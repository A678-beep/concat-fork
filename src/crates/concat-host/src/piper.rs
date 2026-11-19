// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 本地配音：把文字交给装好的 Piper 引擎，得到一条 WAV。
//!
//! 完全在本机跑：进程、模型、声音数据都在插件目录里，不联网、不登录、
//! 不把文字发出去。一次生成就是起一个 `piper.exe`：文字走 stdin，WAV 走
//! `--output_file`，语速用 `--length_scale`（1/rate）调。
//!
//! 顺带量三样东西给界面和验收看：墙钟耗时、进程的 CPU 时间、以及音频
//! 时长——后两者相除就是「实时率」，CPU 时间除以墙钟就是这次生成大约占了
//! 几个核。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::plugins::Plugin;

/// 一次生成最多等这么久：2000 字的中文约 7 分钟音频，慢机器也要跑完。
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// 一个可用的本地引擎。
#[derive(Clone, Debug)]
pub struct Engine {
    /// 插件根目录，进程的工作目录。
    pub root: PathBuf,
    /// 引擎主程序。
    pub exe: PathBuf,
    /// 可用音色。
    pub voices: Vec<Voice>,
}

/// 引擎的一个音色，已解析成文件路径。
#[derive(Clone, Debug)]
pub struct Voice {
    /// 稳定 id。
    pub id: String,
    /// 界面上的名字。
    pub name: String,
    /// 分类。
    pub tag: String,
    /// 质量。
    pub quality: String,
    /// 模型。
    pub model: PathBuf,
    /// 模型配置。
    pub config: Option<PathBuf>,
}

/// 一次生成的结果。
#[derive(Clone, Debug)]
pub struct Spoken {
    /// 写好的 WAV。
    pub path: PathBuf,
    /// 音频时长，秒。
    pub seconds: f64,
    /// 墙钟耗时，毫秒。
    pub wall_ms: u64,
    /// 进程的 CPU 时间，毫秒；拿不到是 0。
    pub cpu_ms: u64,
    /// 音色名，给标题和通知用。
    pub voice_name: String,
}

impl Spoken {
    /// 音频时长 / 墙钟：几倍速。
    pub fn real_time_factor(&self) -> f32 {
        if self.wall_ms == 0 {
            0.0
        } else {
            (self.seconds * 1000.0 / self.wall_ms as f64) as f32
        }
    }

    /// 这次生成大约占几个核（CPU 时间 / 墙钟）。
    pub fn cores(&self) -> f32 {
        if self.wall_ms == 0 || self.cpu_ms == 0 {
            0.0
        } else {
            self.cpu_ms as f32 / self.wall_ms as f32
        }
    }

    /// 界面上的一句：`10.2 秒音频 · 用时 3.9 秒（2.6× 实时）· 约 1.0 核`。
    pub fn summary(&self) -> String {
        let mut text = format!("{:.1} 秒音频", self.seconds);
        if self.wall_ms > 0 {
            text.push_str(&format!(" · 用时 {:.1} 秒", self.wall_ms as f64 / 1000.0));
        }
        let ratio = self.real_time_factor();
        if ratio > 0.0 {
            text.push_str(&format!("（{ratio:.1}× 实时）"));
        }
        let cores = self.cores();
        if cores > 0.0 {
            text.push_str(&format!(" · 约 {cores:.1} 核 CPU"));
        }
        text
    }
}

impl Engine {
    /// 从一个装好的插件里开出引擎。
    pub fn load(plugin: &Plugin) -> Result<Engine, String> {
        let exe = plugin
            .executable()
            .ok_or_else(|| "插件里找不到 piper.exe，插件包可能不完整".to_owned())?;
        let voices: Vec<Voice> = plugin
            .voices()
            .into_iter()
            .map(|voice| Voice {
                id: voice.id,
                name: voice.name,
                tag: voice.tag,
                quality: voice.quality,
                model: voice.model,
                config: voice.config,
            })
            .collect();
        if voices.is_empty() {
            return Err("插件里没有音色模型（voices/*.onnx）".to_owned());
        }
        Ok(Engine {
            root: plugin.folder.clone(),
            exe,
            voices,
        })
    }

    /// 生成一段语音到 `out`。`rate` 是 0.8–1.3 的语速倍数。
    ///
    /// 取消时进程被杀掉、半截文件删掉，返回「已取消」。
    pub fn speak(
        &self,
        voice: usize,
        text: &str,
        rate: f32,
        out: &Path,
        cancel: &AtomicBool,
    ) -> Result<Spoken, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("请先输入要配音的文字".to_owned());
        }
        if text.chars().count() > 2000 {
            return Err("文字超过 2000 字，请分成几段生成".to_owned());
        }
        let picked = self
            .voices
            .get(voice)
            .or_else(|| self.voices.first())
            .ok_or_else(|| "插件里没有可用音色".to_owned())?;
        if !picked.model.is_file() {
            return Err(format!("音色模型不见了：{}", picked.model.display()));
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("配音目录建不出来：{error}"))?;
        }
        let rate = if rate.is_finite() {
            rate.clamp(0.8, 1.3)
        } else {
            1.0
        };
        // length_scale 是「音素拉多长」：1.3 倍速就是 1/1.3。
        let length_scale = 1.0 / f64::from(rate);
        let mut command = Command::new(&self.exe);
        command
            .current_dir(&self.root)
            .arg("--model")
            .arg(&picked.model)
            .arg("--output_file")
            .arg(out)
            .arg("--length_scale")
            .arg(format!("{length_scale:.4}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // 别在编辑窗口后面闪一个控制台。
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        if let Some(config) = picked.config.as_ref().filter(|path| path.is_file()) {
            command.arg("--config").arg(config);
        }
        let started = std::time::Instant::now();
        let mut child = command
            .spawn()
            .map_err(|error| format!("本地配音引擎起不来：{error}"))?;
        let pid = child.id();
        let timer = CpuTimer::begin(pid);
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                .map_err(|error| format!("要配的文字送不进引擎：{error}"))?;
            // 关掉 stdin，引擎才知道念到哪结束。
            drop(stdin);
        }
        let stderr = child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = stderr {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        });
        let mut cancelled = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    return Err(format!("等本地配音引擎时出错：{error}"));
                }
            }
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                cancelled = true;
                break Default::default();
            }
            if started.elapsed() > TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err("本地配音超时（10 分钟），请把文字分短一点".to_owned());
            }
            std::thread::sleep(std::time::Duration::from_millis(80));
        };
        let cpu_ms = timer.cpu_ms();
        let err_text = reader.join().unwrap_or_default();
        if cancelled {
            let _ = std::fs::remove_file(out);
            return Err("已取消".to_owned());
        }
        if !status.success() {
            let _ = std::fs::remove_file(out);
            let reason = err_text
                .lines()
                .rev()
                .find(|line| line.contains("error") || line.contains("Error"))
                .unwrap_or("引擎退出异常")
                .trim()
                .to_owned();
            return Err(format!("本地配音失败：{reason}"));
        }
        if !out.is_file() {
            return Err("本地配音跑完了，但没有得到音频文件".to_owned());
        }
        let seconds = wav_seconds(out).unwrap_or(0.0);
        Ok(Spoken {
            path: out.to_path_buf(),
            seconds,
            wall_ms: started.elapsed().as_millis() as u64,
            cpu_ms,
            voice_name: picked.name.clone(),
        })
    }
}

/// WAV 的时长：走 chunk，按 data 的大小除以每秒字节数。
pub(crate) fn wav_seconds(path: &Path) -> Option<f64> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12usize;
    let mut byte_rate = 0u32;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = at + 8;
        if id == b"fmt " && body + 16 <= bytes.len() {
            // byte_rate 在 fmt 块的第 8 字节起，四字节。
            byte_rate = u32::from_le_bytes(bytes[body + 8..body + 12].try_into().ok()?);
        }
        if id == b"data" {
            if byte_rate == 0 {
                return None;
            }
            let available = size.min(bytes.len().saturating_sub(body));
            return Some(available as f64 / f64::from(byte_rate));
        }
        at = body + size + (size & 1);
    }
    None
}

/// 进程的 CPU 时间：Windows 上开一个查询句柄，跑完读内核+用户时间。
///
/// 句柄由 [`Drop`] 关；拿不到就是 0，不影响生成。
pub(crate) struct CpuTimer {
    #[cfg(windows)]
    handle: isize,
}

impl CpuTimer {
    pub(crate) fn begin(pid: u32) -> CpuTimer {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            };
            // SAFETY: OpenProcess either gives a handle we own or null, and
            // querying rights is all we ask for.
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            CpuTimer {
                handle: handle as isize,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = pid;
            CpuTimer {}
        }
    }

    /// 已经用掉的 CPU 时间，毫秒。
    pub(crate) fn cpu_ms(&self) -> u64 {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::FILETIME;
            use windows_sys::Win32::System::Threading::GetProcessTimes;
            if self.handle == 0 {
                return 0;
            }
            let mut creation = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let mut exit = creation;
            let mut kernel = creation;
            let mut user = creation;
            // SAFETY: the handle is open and every output points at a live
            // FILETIME.
            let ok = unsafe {
                GetProcessTimes(
                    self.handle as *mut std::ffi::c_void,
                    &mut creation,
                    &mut exit,
                    &mut kernel,
                    &mut user,
                )
            };
            if ok == 0 {
                return 0;
            }
            let to_ms = |time: FILETIME| -> u64 {
                let ticks = ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
                // FILETIME 是 100 纳秒一格。
                ticks / 10_000
            };
            to_ms(kernel) + to_ms(user)
        }
        #[cfg(not(windows))]
        {
            0
        }
    }
}

impl Drop for CpuTimer {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::CloseHandle;
            if self.handle != 0 {
                // SAFETY: the handle came from OpenProcess and is closed once.
                unsafe {
                    CloseHandle(self.handle as *mut std::ffi::c_void);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真的用装好的 Piper 插件合成一段：要本机插件、要写盘，`--ignored` 点名跑。
    ///
    /// 这条走的就是面板「生成配音并插入」的同一个 [`Engine::speak`]。
    #[test]
    #[ignore = "needs the installed Piper plugin"]
    fn live_piper_speaks() {
        let dirs = crate::AppDirs::locate().expect("app dirs");
        let plugins = crate::plugins::scan(&dirs);
        let plugin = plugins
            .iter()
            .find(|plugin| plugin.engine() == "piper")
            .expect("Piper plugin installed");
        let engine = Engine::load(plugin).expect("piper engine");
        let out = std::env::temp_dir().join(format!("xinglai-piper-{}.wav", std::process::id()));
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let spoken = engine
            .speak(
                0,
                "大家好，欢迎来到星籁剪辑。这是完全离线的本地AI配音，不需要登录，也不会把文字上传到云端。",
                1.0,
                &out,
                &cancel,
            )
            .expect("speak");
        println!(
            "LIVE_PIPER voice={} seconds={:.2} wall_ms={} cpu_ms={} rtf={:.1} cores={:.1} path={}",
            spoken.voice_name,
            spoken.seconds,
            spoken.wall_ms,
            spoken.cpu_ms,
            spoken.real_time_factor(),
            spoken.cores(),
            spoken.path.display()
        );
        assert!(spoken.seconds > 5.0, "audio too short: {}", spoken.seconds);
        assert!(spoken.path.is_file());
        let _ = std::fs::remove_file(&spoken.path);
    }

    /// 44 字节头的标准 WAV：44.1kHz 单声道 16 位，一秒钟。
    #[test]
    fn a_wav_states_its_length() {
        let dir = std::env::temp_dir().join(format!("concat-piper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("one.wav");
        let data = vec![0u8; 44100 * 2];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&44100u32.to_le_bytes());
        bytes.extend_from_slice(&88200u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&data);
        std::fs::write(&path, &bytes).unwrap();
        let seconds = wav_seconds(&path).unwrap();
        assert!((seconds - 1.0).abs() < 0.001, "{seconds}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
