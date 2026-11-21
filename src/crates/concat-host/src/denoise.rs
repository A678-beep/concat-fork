// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai AI and Concat contributors

//! 精准去电流声：在选中片段的音频里找「窄带纯音」的峰。
//!
//! 一块坏掉的转换器、或者一个按错采样率设上的低通，留下的是**一个频率上
//! 的纯音**（24kHz 源上误设 12kHz 低通，就得到一个 12kHz 的哨音），而不是
//! 宽带噪声。通用降噪（`afftdn`）对这种纯音只能压下去一点，听感就是「有点
//! 效果但不大」。
//!
//! 这里的做法是 STFT 之后逐帧比较每个频点相对邻域中位数的高度：某个频点
//! 比周围高出 **≥25 dB**、并且这种高度**持续 ≥0.5 秒**，就是一个纯音峰。
//! 分析最多取 4 个峰，交给 `concat.whistle-remove` 的 4 个窄陷波逐个消掉；
//! 陷波只碰那个频点附近，语音不会被压闷。
//!
//! 分析只读音频，不改工程；写链路由 `Studio` 完成（见 `studio/chains.rs`）。

use std::path::Path;

use concat_media::{AudioDecoder, AudioOptions, SampleFormat};

/// 分析用的采样率：与桌面版音频链一致，48kHz。
const RATE: u32 = 48_000;
/// STFT 的窗口长度：8192 点，频率分辨率约 5.86Hz，足够分辨 12kHz 的纯音。
const FFT: usize = 8_192;
/// 窗口之间的步进：一半重叠，长时间纯音不会漏。
const HOP: usize = 4_096;
/// 只看 600Hz 以上：工频（50/100/150Hz）由去工频效果负责。
const LOW_HZ: f64 = 600.0;
/// 只看 18kHz 以下：再高是空气声与超声，人耳与语音都用不到。
const HIGH_HZ: f64 = 18_000.0;
/// 邻域取 ±60 个频点（约 ±350Hz），中位数代表「这一带的正常水平」。
const NEIGHBOUR_BINS: usize = 60;
/// 邻域里最近的几个频点不算，避免峰自己的裙边抬高中位数。
const GUARD_BINS: usize = 3;
/// 高出邻域中位数多少 dB 才算候选峰。
const THRESHOLD_DB: f64 = 25.0;
/// 峰至少要在多大比例的帧里成立：0.5 秒左右。
const SUSTAIN: f64 = 0.5;
/// 最多取几个峰，与 `concat.whistle-remove` 的 4 个陷波对应。
const MAX_PEAKS: usize = 4;
/// 最多分析多少秒：纯音检测不需要整段，20 秒足够且足够快。
const MAX_SECONDS: f64 = 20.0;
/// 峰值频率之间至少隔开多少 Hz，避免同一个峰被拆成几个。
const SEPARATION_HZ: f64 = 60.0;

/// 一次检测的结果。
#[derive(Clone, Debug, Default)]
pub struct WhistleReport {
    /// 检测到的纯音频率，按强度从高到低，最多 4 个。
    pub peaks: Vec<f64>,
    /// 最可信那个峰的置信度，0..=1。
    pub confidence: f64,
    /// 最可信那个峰高出邻域中位数多少 dB。
    pub before_db: f64,
    /// 这个分析覆盖了多少秒音频。
    pub seconds: f64,
}

/// 分析 `path` 从 `start` 秒起的 `duration` 秒声音，找电流声/哨音的纯音峰。
///
/// 慢活（解码 + FFT），在后台线程跑；失败时说清是读不了还是音频太短。
pub fn detect(path: &Path, start: f64, duration: f64) -> Result<WhistleReport, String> {
    let options = AudioOptions {
        start: (start.is_finite() && start > 0.0).then_some(start),
        duration: Some(duration.clamp(0.5, MAX_SECONDS)),
        rate: RATE,
        channels: 1,
        format: SampleFormat::F32,
        ..Default::default()
    };
    let samples = AudioDecoder::open(path, &options)
        .and_then(|mut decoder| decoder.collect_f32())
        .map_err(|error| format!("读不了这段音频：{error}"))?;
    let seconds = samples.len() as f64 / f64::from(RATE);
    if samples.len() < FFT {
        return Err("音频太短，不足一个分析窗口".to_owned());
    }
    let mut report = analyse(&samples)?;
    report.seconds = seconds;
    Ok(report)
}

/// [`detect`] 的分析部分：纯函数，只吃样本，方便单测。
fn analyse(samples: &[f32]) -> Result<WhistleReport, String> {
    use realfft::RealFftPlanner;

    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT);
    let mut input = fft.make_input_vec();
    let mut output = fft.make_output_vec();
    let bins = output.len();
    let frame_rate = f64::from(RATE);
    let bin_hz = frame_rate / FFT as f64;

    // 周期 Hann 窗：重叠一半相加，纯音的幅度不会因窗口形状忽大忽小。
    let window: Vec<f32> = (0..FFT)
        .map(|index| {
            0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / FFT as f64).cos() as f32
        })
        .collect();

    let frames = (samples.len() - FFT) / HOP + 1;
    let mut sum = vec![0.0_f64; bins];
    let mut each_frame: Vec<Vec<f64>> = Vec::with_capacity(frames);
    for frame in 0..frames {
        let slice = &samples[frame * HOP..frame * HOP + FFT];
        for (index, sample) in slice.iter().enumerate() {
            input[index] = sample * window[index];
        }
        fft.process(&mut input, &mut output)
            .map_err(|error| format!("FFT 失败：{error}"))?;
        let mut power = Vec::with_capacity(bins);
        for value in &output {
            power.push(f64::from(value.re) * f64::from(value.re)
                + f64::from(value.im) * f64::from(value.im));
        }
        for (total, value) in sum.iter_mut().zip(&power) {
            *total += value;
        }
        each_frame.push(power);
    }
    let mean: Vec<f64> = sum
        .iter()
        .map(|total| (total / frames as f64).max(1e-20))
        .collect();
    let level: Vec<f64> = mean.iter().map(|power| 10.0 * power.log10()).collect();

    let low = ((LOW_HZ / bin_hz) as usize).max(GUARD_BINS + 1);
    let high = ((HIGH_HZ.min(frame_rate / 2.0 - 100.0) / bin_hz) as usize)
        .min(bins.saturating_sub(NEIGHBOUR_BINS + 1));
    if high <= low {
        return Ok(WhistleReport::default());
    }

    // 逐频点：平均谱上高出邻域 ≥25dB、且是局部最大、且一半以上帧成立。
    let mut found: Vec<(usize, f64, f64)> = Vec::new();
    for bin in low..=high {
        let prominence = level[bin] - local_median(&level, bin);
        if prominence < THRESHOLD_DB {
            continue;
        }
        if level[bin] < level[bin - 1] || level[bin] < level[bin + 1] {
            continue;
        }
        let mut hits = 0usize;
        for frame in &each_frame {
            let frame_level = frame[bin].max(1e-20).log10() * 10.0;
            let mut around = Vec::with_capacity(2 * NEIGHBOUR_BINS);
            for other in bin.saturating_sub(NEIGHBOUR_BINS)..=(bin + NEIGHBOUR_BINS).min(bins - 1)
            {
                if other + GUARD_BINS > bin && other < bin + GUARD_BINS {
                    continue;
                }
                around.push(frame[other].max(1e-20).log10() * 10.0);
            }
            around.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = around[around.len() / 2];
            if frame_level - median >= THRESHOLD_DB {
                hits += 1;
            }
        }
        let sustained = hits as f64 / each_frame.len().max(1) as f64;
        if sustained < SUSTAIN {
            continue;
        }
        let confidence = ((prominence - THRESHOLD_DB) / 25.0).clamp(0.0, 1.0) * 0.6
            + sustained.clamp(0.0, 1.0) * 0.4;
        found.push((bin, prominence, confidence));
    }

    // 强的先要；同一个峰邻近的频点不再重复算。
    found.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut peaks: Vec<(f64, f64, f64)> = Vec::new();
    for (bin, prominence, confidence) in found {
        let frequency = bin as f64 * bin_hz;
        if peaks
            .iter()
            .any(|(taken, ..)| (taken - frequency).abs() < SEPARATION_HZ)
        {
            continue;
        }
        peaks.push((frequency, prominence, confidence));
        if peaks.len() >= MAX_PEAKS {
            break;
        }
    }
    let Some(&(_, before_db, confidence)) = peaks.first() else {
        return Ok(WhistleReport::default());
    };
    peaks.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(WhistleReport {
        peaks: peaks.iter().map(|(frequency, ..)| *frequency).collect(),
        confidence,
        before_db,
        seconds: 0.0,
    })
}

/// `level[bin]` 邻域的中位数，略过紧挨着的几个频点（峰自己的裙边）。
fn local_median(level: &[f64], bin: usize) -> f64 {
    let mut around = Vec::with_capacity(2 * NEIGHBOUR_BINS);
    for other in bin.saturating_sub(NEIGHBOUR_BINS)..=(bin + NEIGHBOUR_BINS).min(level.len() - 1) {
        if other + GUARD_BINS > bin && other < bin + GUARD_BINS {
            continue;
        }
        around.push(level[other]);
    }
    around.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    around.get(around.len() / 2).copied().unwrap_or(level[bin])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tone at `hz` buried in a deterministic noise floor, `seconds` long.
    fn tone(hz: f64, seconds: f64, level: f64) -> Vec<f32> {
        let count = (RATE as f64 * seconds) as usize;
        let mut seed = 0x2f6e_2b1d_u64;
        (0..count)
            .map(|index| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let noise = ((seed >> 40) as f64 / 16_777_216.0 - 0.5) * 0.002;
                let phase = std::f64::consts::TAU * hz * index as f64 / f64::from(RATE);
                (noise + level * phase.sin()) as f32
            })
            .collect()
    }

    /// 一个 12kHz 的纯音（就是那个「低通设错采样率」留下的哨音）能被找到
    /// 在 12kHz 附近，且平均谱上比邻域高出 25dB 以上。
    #[test]
    fn a_twelve_kilohertz_whistle_is_found() {
        let samples = tone(12_000.0, 3.0, 0.1);
        let report = analyse(&samples).expect("analyses");
        assert_eq!(report.peaks.len(), 1, "one tone, one peak: {report:?}");
        assert!(
            (report.peaks[0] - 12_000.0).abs() < 20.0,
            "found {} Hz",
            report.peaks[0]
        );
        assert!(
            report.before_db >= 25.0,
            "prominence {} dB",
            report.before_db
        );
        assert!(report.confidence > 0.5, "confidence {}", report.confidence);
    }

    /// 平稳的语音状信号（只有低频、没有纯音）不该报出哨音。
    #[test]
    fn a_voice_without_a_tone_reports_nothing() {
        let samples = tone(250.0, 3.0, 0.08);
        let report = analyse(&samples).expect("analyses");
        assert!(report.peaks.is_empty(), "no whistle in a hum: {report:?}");
    }
}
