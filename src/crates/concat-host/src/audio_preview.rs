// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai AI and Concat contributors

//! 素材库悬停试听：一个只播一小段的小播放器。
//!
//! 时间线的声音归 [`crate::playback`]，它按工程、按时间轴混音；素材卡片的
//! 试听只要「把这段 PCM 放出来，移开就淡出停」，和工程无关。所以这里单独
//! 开一条 cpal 输出流：卡里拿到的 PCM 是 48k 立体声交错 f32，回调里做线性
//! 重采样（设备不一定是 48k），淡入淡出用原子音量做，不阻塞 UI 线程。
//!
//! 同一时刻只有一条流、一个声音：调用 [`AudioPreview::play`] 会直接换掉正在
//! 播的那段，不排队；移开卡片调用 [`AudioPreview::stop`]，250ms 内淡出静音。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// 淡入淡出的时间长（毫秒）：试听要听清，也不能突然响起。
const FADE_MS: f32 = 220.0;

#[derive(Default)]
struct Sound {
    pcm: Vec<f32>,
    rate: u32,
    channels: u16,
    /// 当前读到第几个源帧（f64：设备率和源采样率不一定整除）。
    pos: f64,
}

impl Sound {
    fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.pcm.len() / usize::from(self.channels)
        }
    }
}

struct Shared {
    sound: Mutex<Sound>,
    playing: AtomicBool,
    /// 当前音量与目标音量，都以 f32 的位存进原子量。
    gain: AtomicU32,
    target: AtomicU32,
}

fn bits(value: f32) -> u32 {
    value.to_bits()
}

fn from_bits(value: u32) -> f32 {
    f32::from_bits(value)
}

/// 一条输出流 + 一段可换的 PCM。
pub struct AudioPreview {
    stream: cpal::Stream,
    shared: Arc<Shared>,
}

impl AudioPreview {
    /// 开一条输出流。没有输出设备（服务器、无声卡的机器）返回 Err，调用方
    /// 据此把卡片标成「预览不可用」，不影响任何别的东西。
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no audio output device".to_owned())?;
        let config = device
            .default_output_config()
            .map_err(|error| format!("no default output config: {error}"))?;
        let sample_format = config.sample_format();
        let config: cpal::StreamConfig = config.into();
        let shared = Arc::new(Shared {
            sound: Mutex::new(Sound::default()),
            playing: AtomicBool::new(false),
            gain: AtomicU32::new(bits(0.0)),
            target: AtomicU32::new(bits(0.0)),
        });
        let stream = match sample_format {
            cpal::SampleFormat::F32 => stream_for::<f32>(&device, &config, &shared),
            cpal::SampleFormat::I16 => stream_for::<i16>(&device, &config, &shared),
            cpal::SampleFormat::U16 => stream_for::<u16>(&device, &config, &shared),
            other => Err(format!("unsupported sample format {other:?}")),
        }?;
        stream
            .play()
            .map_err(|error| format!("could not start the preview stream: {error}"))?;
        log::info!("assets: hover preview audio out {config:?}");
        Ok(Self { stream, shared })
    }

    /// 换一段声音开始放：从 0 音量淡入。上一条若有，直接被这条换掉。
    pub fn play(&self, pcm: Vec<f32>, rate: u32, channels: u16) {
        if let Ok(mut sound) = self.shared.sound.lock() {
            *sound = Sound {
                pcm,
                rate: rate.max(8_000),
                channels: channels.max(1),
                pos: 0.0,
            };
        }
        self.shared.gain.store(bits(0.0), Ordering::Relaxed);
        self.shared.target.store(bits(1.0), Ordering::Relaxed);
        self.shared.playing.store(true, Ordering::Relaxed);
    }

    /// 淡出并停：250ms 内降到 0，回调随之停播。立刻再 play 会把目标音量拉回
    /// 1，等于在同一条流里交叉淡化。
    pub fn stop(&self) {
        self.shared.target.store(bits(0.0), Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }
}

impl Drop for AudioPreview {
    fn drop(&mut self) {
        self.shared.playing.store(false, Ordering::Relaxed);
        let _ = self.stream.pause();
    }
}

fn stream_for<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    let device_rate = f64::from(config.sample_rate.0);
    let fade_step = 1.0 / (FADE_MS / 1000.0 * device_rate as f32).max(1.0);
    let shared = Arc::clone(shared);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                if !shared.playing.load(Ordering::Relaxed) {
                    data.fill(T::from_sample(0.0));
                    return;
                }
                let target = from_bits(shared.target.load(Ordering::Relaxed)).clamp(0.0, 1.0);
                let mut gain = from_bits(shared.gain.load(Ordering::Relaxed)).clamp(0.0, 1.0);
                // try_lock, not lock: the audio callback must never block. A
                // contended lock (the UI just swapping sounds) plays silence
                // for one buffer and comes back next time.
                let Ok(mut sound) = shared.sound.try_lock() else {
                    data.fill(T::from_sample(0.0));
                    return;
                };
                if sound.pcm.is_empty() || sound.frames() == 0 {
                    data.fill(T::from_sample(0.0));
                    return;
                }
                let step = f64::from(sound.rate) / device_rate;
                let frames = sound.frames();
                let source_channels = usize::from(sound.channels.max(1));
                for frame in data.chunks_mut(channels) {
                    if sound.pos >= frames as f64 {
                        shared.playing.store(false, Ordering::Relaxed);
                        data.fill(T::from_sample(0.0));
                        return;
                    }
                    if (gain - target).abs() > f32::EPSILON {
                        gain += (target - gain).clamp(-fade_step, fade_step);
                        shared.gain.store(bits(gain), Ordering::Relaxed);
                    }
                    if target <= 0.0 && gain <= 0.001 {
                        shared.playing.store(false, Ordering::Relaxed);
                        data.fill(T::from_sample(0.0));
                        return;
                    }
                    let base = sound.pos.floor() as usize;
                    let next = (base + 1).min(frames - 1);
                    let fraction = (sound.pos - base as f64) as f32;
                    let sample = |frame_index: usize, channel: usize| -> f32 {
                        let at = frame_index * source_channels + channel.min(source_channels - 1);
                        sound.pcm.get(at).copied().unwrap_or(0.0)
                    };
                    let source = |channel: usize| -> f32 {
                        let a = sample(base, channel);
                        let b = sample(next, channel);
                        a + (b - a) * fraction
                    };
                    let left = source(0) * gain;
                    let right = if source_channels > 1 { source(1) } else { source(0) } * gain;
                    frame[0] = T::from_sample(left.clamp(-1.0, 1.0));
                    if channels > 1 {
                        frame[1] = T::from_sample(right.clamp(-1.0, 1.0));
                    }
                    for extra in frame.iter_mut().skip(2) {
                        *extra = T::from_sample(0.0);
                    }
                    sound.pos += step;
                }
            },
            move |error| log::warn!("assets: hover preview audio stream: {error}"),
            None,
        )
        .map_err(|error| format!("could not build the preview stream: {error}"))
}
