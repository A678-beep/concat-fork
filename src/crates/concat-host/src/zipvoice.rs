// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 本地语音克隆：把文字交给装好的 ZipVoice 引擎（sherpa-onnx 的
//! `sherpa-onnx-offline-zeroshot-tts.exe`），克隆参考录音的音色念出来，
//! 得到一条 WAV。
//!
//! 一次生成就是起一个进程：模型文件、参考录音、声音数据都在插件目录里，
//! 不联网、不登录、不把文字发出去。命令行参数见 sherpa-onnx 的 ZipVoice
//! 文档；插件 `plugin.json` 用 `extra` 把文件路径与参考音色写清楚：
//!
//! ```json
//! {
//!   "id": "voice-clone", "engine": "zipvoice",
//!   "executable": "sherpa-onnx-offline-zeroshot-tts.exe",
//!   "extra": {
//!     "textModel": "model/text_encoder.int8.onnx",
//!     "flowMatchingModel": "model/fm_decoder.int8.onnx",
//!     "dataDir": "model/espeak-ng-data",
//!     "pinyinDict": "model/pinyin.raw",
//!     "tokens": "model/tokens.txt",
//!     "vocoder": "model/vocos_24khz.onnx"
//!   },
//!   "voices": [{
//!     "id": "female-news", "name": "新闻女声", "tag": "克隆 · 普通话",
//!     "extra": { "promptAudio": "voices/news-female.wav",
//!                "promptText": "各位村民，大家新年好！……" }
//!   }]
//! }
//! ```
//!
//! 耗时口径与 Piper 一样量：墙钟、进程 CPU 时间、音频时长——CPU 时间除以
//! 墙钟就是这次大约占了几个核，音频时长除以墙钟就是实时率。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::piper::{CpuTimer, Spoken, wav_seconds};
use crate::plugins::Plugin;

/// 一次生成最多等这么久：CPU 上 2000 字可能要很久，给足 20 分钟。
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1200);

/// 一个装好的 ZipVoice 插件。
#[derive(Clone, Debug)]
pub struct Engine {
    /// 插件根目录，进程的工作目录。
    pub root: PathBuf,
    /// 引擎主程序。
    pub exe: PathBuf,
    /// 模型文件（已解析成绝对路径）。
    pub files: Files,
    /// 可用音色。
    pub voices: Vec<Voice>,
    /// 界面上的引擎名。
    pub name: String,
}

/// 命令行要的一组模型文件。
#[derive(Clone, Debug)]
pub struct Files {
    /// ZipVoice 的文本编码器（`--zipvoice-text-model`）。
    pub text_model: PathBuf,
    /// ZipVoice 的流匹配解码器（`--zipvoice-flow-matching-model`）。
    pub flow_matching_model: PathBuf,
    /// espeak-ng 数据目录（英文音素化用）。
    pub data_dir: PathBuf,
    /// cppinyin 的拼音词典（`--zipvoice-pinyin-dict`）。
    pub pinyin_dict: PathBuf,
    /// 音素表。
    pub tokens: PathBuf,
    /// 声码器。
    pub vocoder: PathBuf,
    /// 采样步数，默认 4；小一点更快。
    pub num_steps: u32,
}

/// 一个可克隆的参考音色。
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
    /// 参考录音。
    pub prompt_audio: PathBuf,
    /// 参考录音逐字稿（已知文本，不现转写）。
    pub prompt_text: String,
}

impl Engine {
    /// 从一个装好的插件里开出引擎。
    pub fn load(plugin: &Plugin) -> Result<Engine, String> {
        if plugin.engine() != "zipvoice" {
            return Err("这个插件不是 ZipVoice 语音克隆插件".to_owned());
        }
        let exe = plugin.executable().ok_or_else(|| {
            "插件里找不到 sherpa-onnx-offline-zeroshot-tts.exe，插件包可能不完整".to_owned()
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
            let relative =
                extra(key).ok_or_else(|| format!("插件清单缺 {key}：ZipVoice 引擎要它才知道模型在哪"))?;
            let path = plugin.folder.join(relative);
            if !path.exists() {
                return Err(format!("插件里的 {key} 不见了：{}", path.display()));
            }
            Ok(path)
        };
        let files = Files {
            text_model: need("textModel")?,
            flow_matching_model: need("flowMatchingModel")?,
            data_dir: need("dataDir")?,
            pinyin_dict: need("pinyinDict")?,
            tokens: need("tokens")?,
            vocoder: need("vocoder")?,
            num_steps: extra("numSteps")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(4)
                .clamp(1, 16),
        };
        let mut voices = Vec::new();
        for entry in &plugin.manifest.voices {
            let field = |name: &str| {
                entry
                    .extra
                    .get(name)
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
            };
            let (Some(audio), Some(text)) = (field("promptAudio"), field("promptText")) else {
                continue;
            };
            let prompt_audio = plugin.folder.join(&audio);
            if !prompt_audio.is_file() {
                log::warn!(
                    "ai-plugins: {} 的参考录音不见了：{}",
                    entry.id,
                    prompt_audio.display()
                );
                continue;
            }
            voices.push(Voice {
                id: entry.id.clone(),
                name: if entry.name.trim().is_empty() {
                    entry.id.clone()
                } else {
                    entry.name.clone()
                },
                tag: entry.tag.clone(),
                quality: entry.quality.clone(),
                prompt_audio,
                prompt_text: text,
            });
        }
        if voices.is_empty() {
            return Err(
                "插件里没有可用的参考音色（voices 里缺 promptAudio/promptText）".to_owned(),
            );
        }
        let name = if plugin.manifest.name.trim().is_empty() {
            "本地语音克隆".to_owned()
        } else {
            plugin.manifest.name.clone()
        };
        Ok(Engine {
            root: plugin.folder.clone(),
            exe,
            files,
            voices,
            name,
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
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("配音目录建不出来：{error}"))?;
        }
        let _ = std::fs::remove_file(out);
        let rate = if rate.is_finite() {
            rate.clamp(0.8, 1.3)
        } else {
            1.0
        };
        let mut command = Command::new(&self.exe);
        command
            .current_dir(&self.root)
            .arg(format!(
                "--zipvoice-text-model={}",
                self.files.text_model.display()
            ))
            .arg(format!(
                "--zipvoice-flow-matching-model={}",
                self.files.flow_matching_model.display()
            ))
            .arg(format!(
                "--zipvoice-data-dir={}",
                self.files.data_dir.display()
            ))
            .arg(format!(
                "--zipvoice-pinyin-dict={}",
                self.files.pinyin_dict.display()
            ))
            .arg(format!("--zipvoice-tokens={}", self.files.tokens.display()))
            .arg(format!(
                "--zipvoice-vocoder={}",
                self.files.vocoder.display()
            ))
            .arg(format!("--prompt-audio={}", picked.prompt_audio.display()))
            .arg(format!("--prompt-text={}", picked.prompt_text))
            .arg(format!("--num-steps={}", self.files.num_steps))
            .arg(format!("--speed={rate:.3}"))
            .arg(format!("--output-filename={}", out.display()))
            .arg(text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
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
            .map_err(|error| format!("本地语音克隆引擎起不来：{error}"))?;
        let timer = CpuTimer::begin(child.id());
        let stderr = child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = stderr {
                use std::io::Read;
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
                    return Err(format!("等本地语音克隆引擎时出错：{error}"));
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
                return Err("本地语音克隆超时（20 分钟），请把文字分短一点".to_owned());
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
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
                .find(|line| {
                    line.contains("Error")
                        || line.contains("error")
                        || line.contains("failed")
                        || line.contains("Failed")
                })
                .unwrap_or("引擎退出异常")
                .trim()
                .to_owned();
            return Err(format!("本地语音克隆失败：{reason}"));
        }
        if !out.is_file() {
            return Err("本地语音克隆跑完了，但没有得到音频文件".to_owned());
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
