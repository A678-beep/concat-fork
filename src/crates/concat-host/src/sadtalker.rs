// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 本地数字人（SadTalker 版）：照片 + 配音 → 有头部动作和眨眼的说话视频。
//!
//! 插件里是 SadTalker 官方推理代码 + PyTorch CPU + 官方 checkpoint，桌面端起
//! 一个进程跑 `infer/run_sadtalker.py`，参数按产品约定（size 512、preprocess
//! crop、batch 2、expression_scale 1.0、pose_style 0）。脚本 stdout 逐行打
//! `@progress` / `@result`，stderr 打 `@error` / `@warn`，这里翻成进度与中文
//! 错误；取消/超时杀进程并删掉半截文件。
//!
//! 与 Wav2Lip 相比：口型与头部动作都更自然（会点头、眨眼），但只在 CPU 上
//! 跑，慢得多；所以数字人页让用户自己选。

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::piper::CpuTimer;
use crate::plugins::Plugin;
use crate::wav2lip::Rendered;

/// SadTalker 在 CPU 上很慢：一条 10 秒视频允许跑到 45 分钟。
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45 * 60);

/// 一次生成的参数。
#[derive(Clone, Debug)]
pub struct Request {
    /// 正脸照片。
    pub photo: PathBuf,
    /// 配音 WAV。
    pub audio: PathBuf,
    /// 输出 mp4。
    pub output: PathBuf,
}

/// 一个装好的 SadTalker 插件。
#[derive(Clone, Debug)]
pub struct Engine {
    /// 插件根目录，进程的工作目录。
    pub root: PathBuf,
    /// 嵌入版 Python。
    pub exe: PathBuf,
    /// 包装脚本 `infer/run_sadtalker.py`。
    pub script: PathBuf,
    /// SadTalker 官方代码目录。
    pub repo: PathBuf,
    /// checkpoint 目录。
    pub checkpoints: PathBuf,
    /// 插件清单里的名字。
    pub name: String,
}

impl Engine {
    /// 从一个装好的插件里开出引擎。
    pub fn load(plugin: &Plugin) -> Result<Engine, String> {
        if plugin.engine() != "sadtalker" {
            return Err("这个插件不是 SadTalker 数字人插件".to_owned());
        }
        let exe = plugin.executable().ok_or_else(|| {
            "SadTalker 插件里找不到 python\\python.exe，插件包可能不完整：请在模型中心重新下载"
                .to_owned()
        })?;
        let extra = |key: &str| {
            plugin
                .manifest
                .extra
                .get(key)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let path = |key: &str| -> Result<PathBuf, String> {
            let relative = extra(key).ok_or_else(|| {
                format!("SadTalker 插件清单缺 {key}：插件包可能不完整，请在模型中心重新下载")
            })?;
            Ok(plugin.folder.join(relative))
        };
        let script = path("script")?;
        if !script.is_file() {
            return Err(format!(
                "SadTalker 插件里的推理脚本不见了：{}；请在模型中心重新下载",
                script.display()
            ));
        }
        let repo = path("repo")?;
        if !repo.is_dir() {
            return Err(format!(
                "SadTalker 程序目录不见了：{}；请在模型中心重新下载",
                repo.display()
            ));
        }
        let checkpoints = path("checkpoints")?;
        if !checkpoints.is_dir() {
            return Err(format!(
                "SadTalker 模型目录不见了：{}；请在模型中心重新下载",
                checkpoints.display()
            ));
        }
        Ok(Engine {
            root: plugin.folder.clone(),
            exe,
            script,
            repo,
            checkpoints,
            name: if plugin.manifest.name.trim().is_empty() {
                "本地数字人 · SadTalker".to_owned()
            } else {
                plugin.manifest.name.clone()
            },
        })
    }

    /// SadTalker 用到的两个 512 模型都在。
    fn models_present(&self) -> bool {
        self.checkpoints
            .join("SadTalker_V0.0.2_512.safetensors")
            .is_file()
            && self
                .checkpoints
                .join("mapping_00229-model.pth.tar")
                .is_file()
    }

    /// 引擎能不能跑。
    pub fn ready(&self) -> Result<(), String> {
        if !self.exe.is_file() || !self.script.is_file() || !self.repo.is_dir() {
            return Err(
                "SadTalker 插件缺文件（python.exe / 脚本 / 程序目录）；请在模型中心重新下载"
                    .to_owned(),
            );
        }
        if !self.models_present() {
            return Err(
                "SadTalker 的 512 模型文件缺失（SadTalker_V0.0.2_512）；请在模型中心重新下载"
                    .to_owned(),
            );
        }
        crate::wav2lip::Engine::ffmpeg().map(|_| ())
    }

    /// 生成一段视频。
    pub fn render(
        &self,
        request: &Request,
        progress: &mut dyn FnMut(f32, &str),
        cancel: &AtomicBool,
    ) -> Result<Rendered, String> {
        if !request.photo.is_file() {
            return Err("请先选择一张照片（JPG/PNG）".to_owned());
        }
        if !request.audio.is_file() {
            return Err("还没有配音：请先在「配音」页生成一段，或在数字人页输入文案".to_owned());
        }
        if let Some(parent) = request.output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("数字人成片目录建不出来：{error}"))?;
        }
        let _ = std::fs::remove_file(&request.output);
        let ffmpeg = crate::wav2lip::Engine::ffmpeg()?;
        let ffprobe = ffmpeg
            .parent()
            .map(|dir| dir.join("ffprobe.exe"))
            .filter(|path| path.is_file())
            .unwrap_or_default();
        let mut command = Command::new(&self.exe);
        command
            .current_dir(&self.root)
            .arg(&self.script)
            .arg("--source-image")
            .arg(&request.photo)
            .arg("--driven-audio")
            .arg(&request.audio)
            .arg("--out")
            .arg(&request.output)
            .arg("--ffmpeg")
            .arg(&ffmpeg)
            .arg("--ffprobe")
            .arg(&ffprobe)
            .arg("--repo")
            .arg(&self.repo)
            .arg("--checkpoints")
            .arg(&self.checkpoints)
            .arg("--size")
            .arg("512")
            .arg("--batch-size")
            .arg("2")
            .arg("--expression-scale")
            .arg("1.0")
            .arg("--pose-style")
            .arg("0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let started = std::time::Instant::now();
        let mut child = command
            .spawn()
            .map_err(|error| format!("SadTalker 引擎起不来：{error}（插件可能不完整）"))?;
        let timer = CpuTimer::begin(child.id());
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (sender, receiver) = std::sync::mpsc::channel::<String>();
        let reader = std::thread::spawn(move || {
            if let Some(pipe) = stdout {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            }
        });
        let errors = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = stderr {
                use std::io::Read;
                let _ = pipe.read_to_string(&mut text);
            }
            text
        });
        let mut cancelled = false;
        let mut result: Option<serde_json::Value> = None;
        let mut failure: Option<String> = None;
        loop {
            match receiver.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(line) => {
                    if let Some(rest) = line.strip_prefix("@progress ") {
                        let mut parts = rest.splitn(2, ' ');
                        let fraction = parts
                            .next()
                            .and_then(|value| value.trim().parse::<f32>().ok())
                            .unwrap_or(0.0);
                        let stage = parts.next().unwrap_or_default().trim().to_owned();
                        progress(fraction.clamp(0.0, 1.0), &stage);
                    } else if let Some(rest) = line.strip_prefix("@result ") {
                        result = serde_json::from_str(rest.trim()).ok();
                    } else if let Some(rest) = line.strip_prefix("@warn ") {
                        log::warn!("sadtalker: {}", rest.trim());
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                cancelled = true;
                break;
            }
            if started.elapsed() > TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    "SadTalker 超时（45 分钟）：CPU 上确实很慢，建议把文案改短或改用 Wav2Lip"
                        .to_owned(),
                );
            }
        }
        let status = child.wait().ok();
        let cpu_ms = timer.cpu_ms();
        let _ = reader.join();
        let err_text = errors.join().unwrap_or_default();
        if cancelled {
            let _ = std::fs::remove_file(&request.output);
            return Err("已取消".to_owned());
        }
        if failure.is_none() {
            failure = err_text
                .lines()
                .find_map(|line| line.strip_prefix("@error ").map(|rest| rest.trim().to_owned()));
        }
        let success = status.is_some_and(|status| status.success());
        if !success || failure.is_some() {
            let _ = std::fs::remove_file(&request.output);
            let reason = failure.filter(|reason| !reason.trim().is_empty()).unwrap_or_else(|| {
                let last = err_text
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .next_back()
                    .unwrap_or("引擎退出异常")
                    .trim()
                    .to_owned();
                if last.is_empty() {
                    "引擎退出异常".to_owned()
                } else {
                    last
                }
            });
            return Err(format!("SadTalker 生成失败：{reason}"));
        }
        if !request.output.is_file()
            || std::fs::metadata(&request.output).map_or(0, |meta| meta.len()) < 1024
        {
            return Err("SadTalker 跑完了，但没有得到视频文件".to_owned());
        }
        let result = result.unwrap_or(serde_json::Value::Null);
        let number = |value: &serde_json::Value, key: &str| -> f64 {
            value.get(key).and_then(serde_json::Value::as_f64).unwrap_or(0.0)
        };
        Ok(Rendered {
            path: request.output.clone(),
            seconds: number(&result, "audio_seconds"),
            width: number(&result, "width") as u32,
            height: number(&result, "height") as u32,
            frames: number(&result, "frames") as u32,
            provider: "CPU".to_owned(),
            wall_ms: started.elapsed().as_millis() as u64,
            cpu_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_engine_names_itself() {
        let plugin = Plugin {
            folder: PathBuf::from("."),
            manifest: crate::plugins::Manifest {
                id: "piper".to_owned(),
                engine: "piper".to_owned(),
                ..crate::plugins::Manifest::default()
            },
            bytes: 0,
            complete: false,
        };
        let error = Engine::load(&plugin).unwrap_err();
        assert!(error.contains("SadTalker"), "{error}");
    }
}
