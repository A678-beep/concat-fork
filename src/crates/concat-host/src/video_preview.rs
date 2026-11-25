// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai AI and Concat contributors

//! 素材库悬停预览的本地取帧/取声：借随包的 `ffmpeg.exe` 干活。
//!
//! 平台的下载接口只认应用这套 OpenSSL 直连（见 [`crate::direct`]），
//! ffmpeg 自己的 TLS 过不了 WAF，所以网络这一层仍由 [`crate::direct`] 用
//! HTTP Range 只取开头一小段（视频几 MB、音频一两 MB），随包的 ffmpeg 只
//! 负责把这段本地文件解成预览帧/波形样本。这样不整档下载、不占 UI 线程，
//! 也不引新的网络依赖。

use std::path::{Path, PathBuf};

/// 随包/系统里的 ffmpeg。安装包放哪由平台决定：Windows 是 exe 旁边，
/// macOS 是 `.app` 的 `Contents/Resources/bin`；开发机只在 PATH 里也行。
/// 查找规则在 [`crate::tools`]，这里只把名字固定成 ffmpeg。
pub fn ffmpeg() -> PathBuf {
    crate::tools::ffmpeg()
}

/// 把开头片段解成 `count` 张小 JPEG，写到 `dir`，返回按帧序排好的路径。
///
/// 平台素材的 MP4 都是 moov 在文件头（faststart），所以截断的片段也能从
/// 第 0 帧开始解；文件尾缺失时 ffmpeg 退出码非 0，但已经写出的帧仍然有效，
/// 这里按「有没有产出帧」判成功，只把原因带回去。
pub fn video_frames(
    clip: &Path,
    dir: &Path,
    count: u32,
    fps: u32,
    width: u32,
) -> Result<Vec<PathBuf>, String> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    std::fs::create_dir_all(dir).map_err(|error| format!("建不了预览帧目录：{error}"))?;
    let pattern = dir.join("f%03d.jpg");
    let filter = format!("fps={fps},scale={width}:-2");
    let output = std::process::Command::new(ffmpeg())
        .args(["-hide_banner", "-v", "error", "-y", "-i"])
        .arg(clip)
        .args(["-an", "-vf"])
        .arg(filter)
        .args(["-frames:v", &count.to_string(), "-q:v", "5", "-f", "image2"])
        .arg(&pattern)
        .output()
        .map_err(|error| format!("起不了 ffmpeg：{error}"))?;
    let mut frames: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|error| format!("读不了预览帧目录：{error}"))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jpg"))
        .collect();
    frames.sort();
    if frames.is_empty() {
        let reason = String::from_utf8_lossy(&output.stderr);
        let reason = reason.lines().rev().take(3).collect::<Vec<_>>().join(" / ");
        return Err(format!("解不出预览帧：{reason}"));
    }
    Ok(frames)
}

/// 把开头片段转成试听用的 PCM：48k 立体声交错 f32。ffmpeg 的临时 WAV 写完
/// 由这里解析，再删掉；不依赖别的音频库。
pub fn audio_pcm(
    clip: &Path,
    seconds: f64,
    work: &Path,
) -> Result<(Vec<f32>, u32, u16), String> {
    std::fs::create_dir_all(work).map_err(|error| format!("建不了试听目录：{error}"))?;
    let wav = work.join(format!(
        "hover-{}x{}.wav",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let output = std::process::Command::new(ffmpeg())
        .args(["-hide_banner", "-v", "error", "-y", "-i"])
        .arg(clip)
        .args(["-t", &format!("{seconds:.2}"), "-vn", "-ac", "2", "-ar", "48000"])
        .args(["-c:a", "pcm_s16le", "-f", "wav"])
        .arg(&wav)
        .output()
        .map_err(|error| format!("起不了 ffmpeg：{error}"))?;
    let bytes = std::fs::read(&wav).unwrap_or_default();
    let _ = std::fs::remove_file(&wav);
    if bytes.is_empty() {
        let reason = String::from_utf8_lossy(&output.stderr);
        let reason = reason.lines().rev().take(3).collect::<Vec<_>>().join(" / ");
        return Err(format!("解不出试听：{reason}"));
    }
    wav_pcm16(&bytes)
}

/// 只认 ffmpeg 写出来的那种简单 WAV：fmt 里是 PCM 16 位，data 是样本。
fn wav_pcm16(bytes: &[u8]) -> Result<(Vec<f32>, u32, u16), String> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("试听文件不是 WAV".to_owned());
    }
    let mut at = 12usize;
    let mut rate = 48_000u32;
    let mut channels = 2u16;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
            as usize;
        let body = at + 8;
        match id {
            b"fmt " if body + 16 <= bytes.len() => {
                let tag = u16::from_le_bytes([bytes[body], bytes[body + 1]]);
                channels = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]).max(1);
                rate = u32::from_le_bytes([
                    bytes[body + 4],
                    bytes[body + 5],
                    bytes[body + 6],
                    bytes[body + 7],
                ])
                .max(8_000);
                if tag != 1 {
                    // ffmpeg 的 pcm_s16le 一定是 1；别的格式这里不猜。
                }
            }
            b"data" => {
                let end = (body + size).min(bytes.len());
                let pcm: Vec<f32> = bytes[body..end]
                    .chunks_exact(2)
                    .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32_768.0)
                    .collect();
                if pcm.is_empty() {
                    return Err("试听文件里没有声音".to_owned());
                }
                return Ok((pcm, rate, channels));
            }
            _ => {}
        }
        if size == 0 {
            break;
        }
        at = body + size + (size & 1);
    }
    Err("试听文件里没有找到声音数据".to_owned())
}
