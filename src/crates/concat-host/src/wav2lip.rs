// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 本地数字人：一张正脸照片 + 一段配音 → 一段对着口型说话的视频。
//!
//! 推理全在装好的 Wav2Lip 插件里：插件自带一个嵌入版 Python（只装了
//! numpy / onnxruntime / Pillow，没有 PyTorch）、Wav2Lip-256 的 ONNX 模型
//! 和 SCRFD 人脸检测模型。桌面端不自己跑推理，只负责起进程、读进度、
//! 把 mp4 交给时间轴——与 ZipVoice 插件同一个做法。
//!
//! 一次生成起一个 `python.exe`（插件目录里的嵌入版），命令行见
//! [`Engine::command`]；脚本 stdout 逐行打印 `@progress` / `@result`，
//! stderr 打印 `@error` / `@warn`，中文；这里把它翻成进度回调与错误句。
//!
//! 离线：不联网、不登录、照片与音频都不出本机。跑不跑得动只看本机：
//! 有 DirectML 走显卡（AMD/NVIDIA/Intel 都行），没有就退 CPU。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::piper::CpuTimer;
use crate::plugins::Plugin;

/// 一次生成最多等这么久：CPU 上 10 秒片段约 1 分钟，留足 15 分钟。
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

/// 输出视频的长边上限，与脚本一致：照片再大也只生成到这个尺寸。
pub const MAX_SIDE: u32 = 1280;

/// 一次生成的参数：照片、配音、先这样，别的不改。
#[derive(Clone, Debug)]
pub struct Request {
    /// 正脸照片（JPG/PNG）。
    pub photo: PathBuf,
    /// 配音 WAV（16k 或其它采样率都行，脚本会用 ffmpeg 转）。
    pub audio: PathBuf,
    /// 输出 mp4。
    pub output: PathBuf,
    /// 帧率。
    pub fps: u32,
    /// 长边上限。
    pub max_side: u32,
}

/// 一次生成的结果，给界面和验收看。
#[derive(Clone, Debug)]
pub struct Rendered {
    /// 写好的 mp4。
    pub path: PathBuf,
    /// 配音时长，秒。
    pub seconds: f64,
    /// 画面宽。
    pub width: u32,
    /// 画面高。
    pub height: u32,
    /// 一共多少帧。
    pub frames: u32,
    /// 推理走的算力：`DirectML` 或 `CPU`。
    pub provider: String,
    /// 墙钟耗时，毫秒。
    pub wall_ms: u64,
    /// 引擎进程的 CPU 时间，毫秒；拿不到是 0。
    pub cpu_ms: u64,
}

impl Rendered {
    /// 音频时长 / 墙钟：几倍实时（越小越快）。
    pub fn real_time_factor(&self) -> f32 {
        if self.wall_ms == 0 {
            0.0
        } else {
            (self.seconds * 1000.0 / self.wall_ms as f64) as f32
        }
    }

    /// 界面上的一句：`9.3 秒 · 870×1018 · 用时 5.2 秒（0.6× 实时，DirectML）`。
    pub fn summary(&self) -> String {
        let mut text = format!("{:.1} 秒 · {}×{}", self.seconds, self.width, self.height);
        if self.wall_ms > 0 {
            text.push_str(&format!(" · 用时 {:.1} 秒", self.wall_ms as f64 / 1000.0));
            let ratio = self.real_time_factor();
            if ratio > 0.0 {
                text.push_str(&format!("（{ratio:.1}× 实时"));
                if !self.provider.trim().is_empty() {
                    text.push_str("，");
                    text.push_str(self.provider.trim());
                }
                text.push('）');
            }
        }
        text
    }
}

/// 一个装好的数字人插件。
#[derive(Clone, Debug)]
pub struct Engine {
    /// 插件根目录，进程的工作目录。
    pub root: PathBuf,
    /// 嵌入版 Python。
    pub exe: PathBuf,
    /// 推理脚本 `infer/wav2lip_local.py`。
    pub script: PathBuf,
    /// Wav2Lip ONNX。
    pub model: PathBuf,
    /// SCRFD 人脸检测 ONNX。
    pub detector: PathBuf,
    /// 插件清单里的名字。
    pub name: String,
}

impl Engine {
    /// 从一个装好的插件里开出引擎；文件缺了就把缺的那一个说出来。
    pub fn load(plugin: &Plugin) -> Result<Engine, String> {
        if plugin.engine() != "wav2lip" {
            return Err("这个插件不是本地数字人（Wav2Lip）插件".to_owned());
        }
        let exe = plugin.executable().ok_or_else(|| {
            "数字人插件里找不到 python\\python.exe，插件包可能不完整：请在模型中心重新下载"
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
        let need = |key: &str| -> Result<PathBuf, String> {
            let relative = extra(key).ok_or_else(|| {
                format!("数字人插件清单缺 {key}：插件包可能不完整，请在模型中心重新下载")
            })?;
            let path = plugin.folder.join(relative);
            if !path.is_file() {
                return Err(format!(
                    "数字人插件里的 {key} 不见了：{}；请在模型中心重新下载插件",
                    path.display()
                ));
            }
            Ok(path)
        };
        Ok(Engine {
            root: plugin.folder.clone(),
            exe,
            script: need("script")?,
            model: need("model")?,
            detector: need("detector")?,
            name: if plugin.manifest.name.trim().is_empty() {
                "本地数字人".to_owned()
            } else {
                plugin.manifest.name.clone()
            },
        })
    }

    /// 桌面端自带的 ffmpeg：跟 `concat.exe` 放在一起。打包时从 deps 里拷进去。
    pub fn ffmpeg() -> Result<PathBuf, String> {
        if let Ok(path) = std::env::var("XINGLAI_FFMPEG") {
            let path = PathBuf::from(path.trim());
            if path.is_file() {
                return Ok(path);
            }
        }
        let beside = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        if let Some(dir) = beside {
            for name in ["ffmpeg.exe", "ffmpeg"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        Err("找不到桌面端自带的 ffmpeg.exe：请确认安装的是星籁剪辑 0.2.14 或更新版本，或重新安装".to_owned())
    }

    /// 引擎能不能跑：Python、脚本、模型、ffmpeg 都在。
    pub fn ready(&self) -> Result<(), String> {
        for path in [&self.exe, &self.script, &self.model, &self.detector] {
            if !path.is_file() {
                return Err(format!(
                    "本地数字人插件缺文件：{}；请在模型中心重新下载插件",
                    path.display()
                ));
            }
        }
        Self::ffmpeg().map(|_| ())
    }

    /// 生成一段对口型视频。
    ///
    /// `progress` 报「0..1 + 一句话」；`cancel` 一置位就把进程杀掉、半截
    /// 文件删掉，返回「已取消」。
    pub fn render(
        &self,
        request: &Request,
        progress: &mut dyn FnMut(f32, &str),
        cancel: &AtomicBool,
    ) -> Result<Rendered, String> {
        if request.photo.as_os_str().is_empty() || !request.photo.is_file() {
            return Err("请先选择一张照片（JPG/PNG）".to_owned());
        }
        if request.audio.as_os_str().is_empty() || !request.audio.is_file() {
            return Err("还没有配音：请先在「配音」页生成一段，或在数字人页输入文案".to_owned());
        }
        let output = &request.output;
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("数字人成片目录建不出来：{error}"))?;
        }
        let _ = std::fs::remove_file(output);
        let ffmpeg = Self::ffmpeg()?;
        let mut command = Command::new(&self.exe);
        command
            .current_dir(&self.root)
            .arg(&self.script)
            .arg("--face")
            .arg(&request.photo)
            .arg("--audio")
            .arg(&request.audio)
            .arg("--out")
            .arg(output)
            .arg("--ffmpeg")
            .arg(&ffmpeg)
            .arg("--model")
            .arg(&self.model)
            .arg("--detector")
            .arg(&self.detector)
            .arg("--fps")
            .arg(request.fps.clamp(15, 30).to_string())
            .arg("--max-side")
            .arg(request.max_side.clamp(256, 1920).to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // 别在编辑窗口后面闪一个控制台。
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let started = std::time::Instant::now();
        let mut child = command
            .spawn()
            .map_err(|error| format!("本地数字人引擎起不来：{error}（插件可能不完整）"))?;
        let timer = CpuTimer::begin(child.id());
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        // stdout 在一条线程里读，主循环 recv_timeout 等它：这样取消和超时
        // 都不会被一行没来的输出卡住。
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
            match receiver.recv_timeout(std::time::Duration::from_millis(200)) {
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
                        log::warn!("wav2lip: {}", rest.trim());
                    } else if let Some(rest) = line.strip_prefix("@error ") {
                        failure = Some(rest.trim().to_owned());
                    }
                }
                // 子进程还活着但暂时没说话：多半在加载模型。
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                // stdout 关了：进程已经退出，最后一行也读完了。
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
                return Err("本地数字人超时（15 分钟）：请把文案改短一点，或换台更快的机器".to_owned());
            }
        }
        let status = child.wait().ok();
        let cpu_ms = timer.cpu_ms();
        let _ = reader.join();
        let err_text = errors.join().unwrap_or_default();
        if cancelled {
            let _ = std::fs::remove_file(output);
            return Err("已取消".to_owned());
        }
        if failure.is_none() {
            // 脚本自己没报错但 stderr 里有 @error（极少数退出路径）。
            failure = err_text
                .lines()
                .find_map(|line| line.strip_prefix("@error ").map(|rest| rest.trim().to_owned()));
        }
        let success = status.is_some_and(|status| status.success());
        if !success || failure.is_some() {
            let _ = std::fs::remove_file(output);
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
            return Err(format!("本地数字人生成失败：{reason}"));
        }
        if !output.is_file() || std::fs::metadata(output).map_or(0, |meta| meta.len()) < 1024 {
            return Err("本地数字人跑完了，但没有得到视频文件".to_owned());
        }
        let number = |value: &serde_json::Value, key: &str| -> f64 {
            value.get(key).and_then(serde_json::Value::as_f64).unwrap_or(0.0)
        };
        let text = |value: &serde_json::Value, key: &str| -> String {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let result = result.unwrap_or(serde_json::Value::Null);
        let wall_ms = started.elapsed().as_millis() as u64;
        let seconds = number(&result, "audio_seconds");
        Ok(Rendered {
            path: output.to_path_buf(),
            seconds,
            width: number(&result, "width") as u32,
            height: number(&result, "height") as u32,
            frames: number(&result, "frames") as u32,
            provider: text(&result, "provider"),
            wall_ms,
            cpu_ms,
        })
    }
}

/// 这次生成离开插件就做不成：给界面解释一句为什么按钮灰着。
pub fn requirement() -> &'static str {
    "需要 Wav2Lip 数字人插件（约 300MB，一次装好、之后完全离线）与 8GB 以上内存"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_counts_time_and_resolution() {
        let rendered = Rendered {
            path: PathBuf::from("a.mp4"),
            seconds: 9.3,
            width: 870,
            height: 1018,
            frames: 230,
            provider: "DirectML".to_owned(),
            wall_ms: 5200,
            cpu_ms: 4000,
        };
        assert!(rendered.summary().contains("9.3 秒"));
        assert!(rendered.summary().contains("870×1018"));
        assert!(rendered.summary().contains("0.6× 实时"));
        assert_eq!(rendered.provider, "DirectML");
    }

    /// 没有 ffmpeg 时给一句能看懂的话，而不是让用户面对一个英文路径。
    #[test]
    fn a_missing_ffmpeg_is_said_in_chinese() {
        let error = Engine::ffmpeg();
        if let Err(message) = error {
            assert!(message.contains("ffmpeg"), "{message}");
            assert!(message.contains("0.2.14"), "{message}");
        }
    }
}
