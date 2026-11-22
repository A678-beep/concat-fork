// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 星籁 AI 生视频（云端）：文生视频 / 图生视频 / 多段续写拼接。
//!
//! 桌面端**不打包任何 SiliconFlow / 302 的 Key**：所有生成都走平台自己的
//! 接口（与登录、配音、素材库同一套 TLS 直连）：
//!
//! - `GET  /aivideo/models` → 本账号可用的视频模型、说明、每秒价格
//! - `POST /aivideo {image_url, prompt, duration, model, aspect}` →
//!   `{ok, id, url, request_id, elapsed, duration, width, height, size,
//!     cost_hint:{text,...}, license_note:{...}}`
//!
//! 平台侧是**同步**接口：提交后平台自己轮询 SiliconFlow，最长约 10 分钟才
//! 返回结果（实测 306 秒）。所以这里一次 POST 的读等待给到 11 分钟，并在
//! 等待期间每 2 秒报一次「生成中：已等待 N 秒」，让界面知道还活着。
//!
//! 平台唯一开通的模型 MiniMax-H3 是「图生视频」：`image_url` 必填。文生视频
//! 由桌面端先用 ffmpeg 生成一张纯净底图当首帧（提示词决定画面内容），这一
//! 点如实写在界面说明里，不假装是纯文本直出。
//!
//! 续写拼接：平台每条最多 5 秒，长视频（10/30/60 秒）= 多段 5 秒顺序生成，
//! 上一段的最后一帧（ffmpeg 抽帧）作为下一段的首帧，最后用 ffmpeg 拼接。
//! 质量与一致性可能下降——界面常驻说明。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// 平台站点；相对地址补全用。
pub const SITE: &str = "https://xinglaiai.com";

/// 生成一次最多等这么久：平台实测 306 秒，给到 11 分钟。
const GENERATE_READ: Duration = Duration::from_secs(660);

/// 每个 5 秒片段的平台请求时长（平台只接受 3/5 秒，5 秒是标准）。
pub const SEGMENT_SECONDS: u32 = 5;

/// 一个可用的视频模型（`/aivideo/models` 的一行）。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Model {
    /// 提交时带在 `model` 里的 id。
    #[serde(default)]
    pub id: String,
    /// 中文说明。
    #[serde(default)]
    pub label: String,
    /// 备注（本账号是否开通、费用）。
    #[serde(default)]
    pub note: String,
    /// 本账号已开通。
    #[serde(default)]
    pub enabled: bool,
    /// 平台默认模型。
    #[serde(default)]
    pub default: bool,
    /// 平台判定可用（开通 + 余额/上游正常）。0.2.23 之前的后端没有该字段，
    /// 缺省按可用处理，避免升级后整页模型都被标成不可用。
    #[serde(default = "available_by_default")]
    pub available: bool,
    /// 不可用原因：`upstream_balance` / `model_disabled` / `timeout`，空 = 可用。
    #[serde(default)]
    pub reason: String,
}

fn available_by_default() -> bool {
    true
}

impl Model {
    /// 下拉里的一行：`MiniMax H3（…）· ¥2.5/5s`。
    pub fn display(&self) -> String {
        if self.label.trim().is_empty() {
            self.id.clone()
        } else {
            self.label.clone()
        }
    }

    /// 能不能选来生成：既开通、又没被平台标成不可用。
    pub fn usable(&self) -> bool {
        self.enabled && self.available
    }

    /// 不可用原因的中文说法，给下拉和生成前的提示用。
    pub fn reason_label(&self) -> &str {
        match self.reason.trim() {
            "upstream_balance" => "上游余额不足",
            "model_disabled" => "本账号未开通",
            "timeout" => "上游暂时不可用",
            _ => "",
        }
    }
}

/// `/aivideo/models` 的回复。
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    /// 平台列出的模型（未开通的 `enabled=false`）。
    pub models: Vec<Model>,
    /// 默认模型 id。
    pub default_id: String,
    /// 按输出秒计费，元/秒。
    pub price_per_sec: f64,
    /// 平台自己的说明。
    pub hint: String,
}

impl Inventory {
    /// 能选的下标：平台默认且可用 → 第一个可用 → 第一个开通的；一个都没有返回 None。
    pub fn default_index(&self) -> Option<usize> {
        if let Some(index) = self.models.iter().position(|model| {
            model.id == self.default_id && model.usable()
        }) {
            return Some(index);
        }
        self.models
            .iter()
            .position(|model| model.usable() && model.default)
            .or_else(|| self.models.iter().position(|model| model.usable()))
            .or_else(|| self.models.iter().position(|model| model.enabled))
    }

    /// 选中的模型（必须可用；不可用的留给界面提示原因）。
    pub fn chosen(&self, index: usize) -> Option<&Model> {
        self.models.get(index).filter(|model| model.usable())
    }
}

/// 一次生成要什么。
#[derive(Clone, Debug, Default)]
pub struct Request {
    /// 画面描述。
    pub prompt: String,
    /// 负面提示（当前平台版本会忽略，仍随请求发送）。
    pub negative: String,
    /// 模型 id。
    pub model: String,
    /// `9:16` / `16:9` / `1:1`。
    pub aspect: String,
    /// 首帧：图生视频是用户选的图；文生视频留空，用底图。
    pub image: Option<Vec<u8>>,
    /// 首帧 MIME。
    pub image_mime: String,
}

/// 一个片段的生成结果。
#[derive(Clone, Debug, Default)]
pub struct Segment {
    /// 落盘的 mp4。
    pub path: PathBuf,
    /// 平台的任务 id。
    pub request_id: String,
    /// 平台给的时长，秒。
    pub duration: f64,
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// 字节数。
    pub size: u64,
    /// 平台给的花费说明。
    pub cost_text: String,
}

/// 一次完整生成（一段或拼接后的长视频）的结果。
#[derive(Clone, Debug, Default)]
pub struct Output {
    /// 落盘的 mp4。
    pub path: PathBuf,
    /// 生成了几段。
    pub segments: usize,
    /// 平台的任务 id（多段用逗号连接）。
    pub request_ids: String,
    /// 总时长，秒。
    pub duration: f64,
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// 字节数。
    pub size: u64,
    /// 平台给的花费说明（多段相加成一句）。
    pub cost_text: String,
    /// 本次预估花费，元。
    pub cost_cny: f64,
}

/// 平台接口根地址（与登录同一份，`XINGLAI_API_BASE` 可换）。
fn endpoint(path: &str) -> String {
    let base = std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::account::API_BASE.to_owned());
    format!("{base}{path}")
}

/// 站内相对地址补成绝对地址。
pub fn absolute(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        url.to_owned()
    } else if url.starts_with('/') {
        format!("{SITE}{url}")
    } else {
        format!("{SITE}/{url}")
    }
}

/// 令牌：没有就是一句「请先登录」。
pub fn token() -> Result<String, String> {
    match crate::account::load() {
        Some(account) if !account.token.trim().is_empty() => Ok(account.token),
        _ => Err("请先登录星籁账号（首屏登录后重试）；AI 生视频由平台代扣额度".to_owned()),
    }
}

/// 服务器说不行时翻成一句人话：优先用平台给的中文 `message`（0.2.23 起
/// 两路都失败时会返回 `{"ok":false,"error":"aivideo_unavailable","message":"…"}`），
/// 其次 `hint`，最后才退回按 HTTP 状态码的通用文案。
fn refusal(status: u16, text: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(text).ok();
    let field = |name: &str| {
        value
            .as_ref()
            .and_then(|value| value.get(name))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let message = field("message").or_else(|| field("hint"));
    match status {
        0 => "服务器没有给出回复（连接被中途关闭），请稍后重试".to_owned(),
        401 => "登录已失效，请重新登录星籁账号".to_owned(),
        402 | 403 => message.unwrap_or_else(|| {
            "套餐额度不足或没有这个功能的权限，请在网页工作台升级套餐".to_owned()
        }),
        404 => message.unwrap_or_else(|| "云端接口不存在，请升级到最新版本".to_owned()),
        413 => "上传的首帧图片太大（上限 10MB），请换一张小一点的图".to_owned(),
        429 => message.unwrap_or_else(|| "请求太频繁或额度用完了，请稍后再试".to_owned()),
        500..=599 => message
            .unwrap_or_else(|| format!("星籁云端 AI 暂时不可用（HTTP {status}），请稍后再试")),
        _ => message.unwrap_or_else(|| format!("云端 AI 返回了 HTTP {status}")),
    }
}

fn json_reply(status: u16, text: &str) -> Result<serde_json::Value, String> {
    if !(200..300).contains(&status) {
        return Err(refusal(status, text));
    }
    serde_json::from_str(text).map_err(|error| format!("服务器的回复不是 JSON：{error}"))
}

fn check_ok(value: &serde_json::Value) -> Result<(), String> {
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
        let hint = value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("hint").and_then(serde_json::Value::as_str))
            .or_else(|| value.get("detail").and_then(serde_json::Value::as_str))
            .unwrap_or("云端 AI 拒绝了这次请求");
        return Err(hint.to_owned());
    }
    Ok(())
}

/// 本账号可用的视频模型。
pub fn models(token: Option<&str>) -> Result<Inventory, String> {
    let (status, text) = crate::account::api_call(
        "GET",
        &endpoint("/aivideo/models"),
        "application/json",
        &[],
        token,
        Duration::from_secs(25),
    )?;
    let value = json_reply(status, &text)?;
    check_ok(&value)?;
    let mut models = Vec::new();
    if let Some(rows) = value.get("models").and_then(serde_json::Value::as_array) {
        for row in rows {
            models.push(
                serde_json::from_value(row.clone())
                    .map_err(|error| format!("视频模型列表格式不认识：{error}"))?,
            );
        }
    }
    Ok(Inventory {
        models,
        default_id: value
            .get("default")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        price_per_sec: value
            .get("price_per_sec")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
        hint: value
            .get("hint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

/// 桌面端自带的 ffmpeg：跟 `concat.exe` 放在一起。
pub fn ffmpeg() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("XINGLAI_FFMPEG") {
        let path = PathBuf::from(path.trim());
        if path.is_file() {
            return Ok(path);
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for part in std::env::split_paths(&path) {
            for name in ["ffmpeg.exe", "ffmpeg"] {
                let candidate = part.join(name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
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
    Err("找不到桌面端自带的 ffmpeg.exe：请确认安装的是星籁剪辑 0.2.22 或更新版本".to_owned())
}

/// 生成一张纯净底图（文生视频的首帧）。返回 JPEG 字节。
pub fn poster_frame(aspect: &str) -> Result<Vec<u8>, String> {
    let size = match aspect {
        "16:9" => "1280x720",
        "1:1" => "960x960",
        _ => "720x1280",
    };
    let ffmpeg = ffmpeg()?;
    let dir = std::env::temp_dir().join("xinglai-aivideo");
    std::fs::create_dir_all(&dir).map_err(|error| format!("建不了临时目录：{error}"))?;
    let out = dir.join(format!("poster-{}.jpg", stamp_ms()));
    let source = format!(
        "gradients=s={size}:c0=0x16233b:c1=0x9fc6e8:x0=0:y0=0:x1={w}:y1={h}:d=1",
        w = size.split('x').next().unwrap_or("720"),
        h = size.split('x').nth(1).unwrap_or("1280")
    );
    let status = std::process::Command::new(&ffmpeg)
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &source])
        .args(["-frames:v", "1", "-q:v", "3"])
        .arg(&out)
        .status()
        .map_err(|error| format!("调不动 ffmpeg：{error}"))?;
    if !status.success() {
        return Err("底图生成失败（ffmpeg 返回错误）".to_owned());
    }
    let bytes = std::fs::read(&out).map_err(|error| format!("读不回底图：{error}"))?;
    let _ = std::fs::remove_file(&out);
    if bytes.len() < 1024 {
        return Err("底图生成失败（文件太小）".to_owned());
    }
    Ok(bytes)
}

/// 从视频里抽最后一帧（续写拼接用）。返回 JPEG 字节。
pub fn last_frame(video: &Path, aspect: &str) -> Result<Vec<u8>, String> {
    let ffmpeg = ffmpeg()?;
    let dir = std::env::temp_dir().join("xinglai-aivideo");
    std::fs::create_dir_all(&dir).map_err(|error| format!("建不了临时目录：{error}"))?;
    let out = dir.join(format!("last-{}.jpg", stamp_ms()));
    let status = std::process::Command::new(&ffmpeg)
        .args(["-y", "-loglevel", "error", "-sseof", "-0.35", "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-q:v", "2"])
        .arg(&out)
        .status()
        .map_err(|error| format!("调不动 ffmpeg：{error}"))?;
    if !status.success() || !out.is_file() {
        return Err("抽最后一帧失败（视频可能损坏）".to_owned());
    }
    let bytes = std::fs::read(&out).map_err(|error| format!("读不回最后一帧：{error}"))?;
    let _ = std::fs::remove_file(&out);
    if bytes.len() < 1024 {
        return Err("抽最后一帧失败（图片太小）".to_owned());
    }
    let _ = aspect;
    Ok(bytes)
}

/// 一段视频：提交、等平台生成、下载。
fn one_segment(
    token: &str,
    dirs: &crate::AppDirs,
    request: &Request,
    image: &[u8],
    image_mime: &str,
    start_fraction: f32,
    span: f32,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<Segment, String> {
    use base64::Engine as _;
    if image.len() > 10 * 1024 * 1024 {
        return Err("首帧图片超过 10MB，请换一张小一点的图".to_owned());
    }
    let data_uri = format!(
        "data:{image_mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(image)
    );
    let body = serde_json::json!({
        "image_url": data_uri,
        "prompt": request.prompt.chars().take(600).collect::<String>(),
        "negative_prompt": request.negative.chars().take(300).collect::<String>(),
        "duration": SEGMENT_SECONDS,
        "model": request.model.clone(),
        "aspect": request.aspect.clone(),
    })
    .to_string();

    progress(start_fraction, "提交到星籁平台…".to_owned());
    let url = endpoint("/aivideo");
    // 线程要活到平台回来，令牌先拷一份成拥有的，不借调用方的引用。
    let token = token.to_owned();
    let (sender, receiver) = mpsc::channel::<Result<(u16, String), String>>();
    let generation = std::thread::Builder::new()
        .name("concat-aivideo".into())
        .spawn(move || {
            let result = crate::account::api_call(
                "POST",
                &url,
                "application/json",
                body.as_bytes(),
                Some(&token),
                GENERATE_READ,
            );
            let _ = sender.send(result);
        })
        .map_err(|error| format!("起不了生成线程：{error}"))?;
    let began = Instant::now();
    let (status, reply) = loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消（平台的任务可能仍在后台跑完）".to_owned());
        }
        match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(reply)) => break reply,
            Ok(Err(error)) => return Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let waited = began.elapsed().as_secs();
                let fraction = (start_fraction + span * 0.05).min(start_fraction + span * 0.9);
                progress(
                    fraction,
                    format!("云端生成中…已等待 {waited} 秒（通常 1–6 分钟，可取消）"),
                );
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("生成线程意外退出，请重试".to_owned());
            }
        }
    };
    // 线程已经发完结果，join 一下释放资源。
    let _ = generation.join();
    let value = json_reply(status, &reply)?;
    check_ok(&value)?;
    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let url = value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "生成成功，但服务器没有给出视频地址".to_owned())?;
    let url = absolute(url);
    let out_dir = dirs.config.join("ai-video");
    std::fs::create_dir_all(&out_dir).map_err(|error| format!("建不了 ai-video 目录：{error}"))?;
    let suffix = if request_id.is_empty() {
        stamp_ms().to_string()
    } else {
        request_id.clone()
    };
    let dest = out_dir.join(format!("aivideo-{}-{suffix}.mp4", stamp_ms()));
    let temporary = dest.with_extension("part");
    progress(start_fraction + span * 0.85, "下载成片…".to_owned());
    crate::direct::download(&url, &temporary, &mut |fraction, _| {
        let mapped = start_fraction + span * (0.85 + 0.15 * fraction.clamp(0.0, 1.0));
        progress(mapped, format!("下载成片…{:.0}%", fraction * 100.0));
    }, cancel)?;
    let size = std::fs::metadata(&temporary).map_or(0, |meta| meta.len());
    if size < 50_000 {
        let _ = std::fs::remove_file(&temporary);
        return Err("下载到的视频不完整（文件太小），请重试".to_owned());
    }
    std::fs::rename(&temporary, &dest).map_err(|error| format!("存不下成片：{error}"))?;
    let summary = crate::media::probe(&dest.to_string_lossy())?;
    let cost_text = value
        .get("cost_hint")
        .and_then(|cost| cost.get("text"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok(Segment {
        path: dest,
        request_id,
        duration: summary.duration.unwrap_or(f64::from(SEGMENT_SECONDS)),
        width: value
            .get("width")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32,
        height: value
            .get("height")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32,
        size,
        cost_text,
    })
}

/// 把多段拼成一个文件（ffmpeg concat，重编码一次，保证时间戳/音轨一致）。
pub fn concat(parts: &[PathBuf], dest: &Path) -> Result<(), String> {
    if parts.len() == 1 {
        std::fs::copy(&parts[0], dest).map_err(|error| format!("存不下成片：{error}"))?;
        return Ok(());
    }
    let ffmpeg = ffmpeg()?;
    let dir = dest
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir).map_err(|error| format!("建不了目录：{error}"))?;
    let list = dir.join(format!("concat-{}.txt", stamp_ms()));
    let mut text = String::new();
    for part in parts {
        let path = part.display().to_string().replace('\\', "/");
        text.push_str(&format!("file '{path}'\n"));
    }
    std::fs::write(&list, text).map_err(|error| format!("写不了拼接清单：{error}"))?;
    let status = std::process::Command::new(&ffmpeg)
        .args(["-y", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-movflags",
            "+faststart",
        ])
        .arg(dest)
        .status()
        .map_err(|error| format!("调不动 ffmpeg：{error}"))?;
    let _ = std::fs::remove_file(&list);
    if !status.success() {
        return Err("多段拼接失败（ffmpeg 返回错误）".to_owned());
    }
    if !dest.is_file() || std::fs::metadata(dest).map_or(0, |meta| meta.len()) < 50_000 {
        return Err("多段拼接失败（成片文件不完整）".to_owned());
    }
    Ok(())
}

/// 生成长视频：`segments` 段 5 秒顺序续写，按最后帧接下一段，最后拼起来。
///
/// `segments == 1` 就是普通的单条生成。
#[allow(clippy::too_many_arguments)]
pub fn generate(
    token: &str,
    dirs: &crate::AppDirs,
    request: &Request,
    segments: usize,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<Output, String> {
    let segments = segments.clamp(1, 16);
    let out_dir = dirs.config.join("ai-video");
    std::fs::create_dir_all(&out_dir).map_err(|error| format!("建不了 ai-video 目录：{error}"))?;
    let (first, first_mime) = match request.image.as_ref().filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => {
            let mime = if request.image_mime.trim().is_empty() {
                "image/jpeg".to_owned()
            } else {
                request.image_mime.clone()
            };
            (bytes.clone(), mime)
        }
        None => (poster_frame(&request.aspect)?, "image/jpeg".to_owned()),
    };
    let mut parts: Vec<PathBuf> = Vec::with_capacity(segments);
    let mut ids: Vec<String> = Vec::new();
    let mut costs: Vec<String> = Vec::new();
    let mut image = first;
    let mut image_mime = first_mime;
    let mut total_duration = 0.0;
    let mut width = 0;
    let mut height = 0;
    let span = 1.0 / segments as f32;
    for index in 0..segments {
        if cancel.load(Ordering::Relaxed) {
            cleanup(&parts);
            return Err("已取消（平台的任务可能仍在后台跑完）".to_owned());
        }
        let start = index as f32 * span;
        let label = if segments == 1 {
            "生成中".to_owned()
        } else {
            format!("第 {}/{} 段（续写拼接）", index + 1, segments)
        };
        progress(start, format!("{label}：准备首帧…"));
        let mut inner = |fraction: f32, text: String| {
            progress(start + span * fraction.clamp(0.0, 1.0), format!("{label}：{text}"));
        };
        let segment = one_segment(
            token,
            dirs,
            request,
            &image,
            &image_mime,
            start,
            span,
            &mut inner,
            cancel,
        )
        .map_err(|error| {
            cleanup(&parts);
            if segments > 1 {
                format!("{error}（已生成 {} 段，未拼接）", parts.len())
            } else {
                error
            }
        })?;
        total_duration += segment.duration;
        width = segment.width;
        height = segment.height;
        ids.push(segment.request_id.clone());
        if !segment.cost_text.trim().is_empty() {
            costs.push(segment.cost_text.clone());
        }
        parts.push(segment.path.clone());
        if index + 1 < segments {
            progress(start + span * 0.99, format!("{label}：抽取最后一帧续写…"));
            image = last_frame(&segment.path, &request.aspect).map_err(|error| {
                cleanup(&parts);
                error
            })?;
            image_mime = "image/jpeg".to_owned();
        }
    }
    progress(0.97, "拼接成片…".to_owned());
    let dest = out_dir.join(format!("aivideo-{}.mp4", stamp_ms()));
    concat(&parts, &dest).map_err(|error| {
        cleanup(&parts);
        error
    })?;
    cleanup(&parts);
    let summary = crate::media::probe(&dest.to_string_lossy())?;
    let price = cost_per_second(&costs).unwrap_or(0.5);
    let cost_cny = f64::from(SEGMENT_SECONDS) * segments as f64 * price;
    progress(1.0, "完成".to_owned());
    let size = std::fs::metadata(&dest).map_or(0, |meta| meta.len());
    Ok(Output {
        path: dest,
        segments,
        request_ids: ids.join(","),
        duration: summary.duration.unwrap_or(total_duration),
        width,
        height,
        size,
        cost_text: if costs.is_empty() {
            format!("约 ¥{cost_cny:.1}（{segments} 段 × {SEGMENT_SECONDS} 秒）")
        } else {
            format!("{}（共 {} 段）", costs[0], segments)
        },
        cost_cny,
    })
}

/// 从平台的 cost_hint 文本里恢复单价（`¥0.50/秒`）；恢复不了就按 0.5。
fn cost_per_second(costs: &[String]) -> Option<f64> {
    let text = costs.first()?;
    let at = text.find('/')?;
    let before = &text[..at];
    let price = before.rsplit(['¥', ' ']).next()?.trim().parse::<f64>().ok()?;
    (price > 0.0).then_some(price)
}

fn cleanup(parts: &[PathBuf]) {
    for part in parts {
        let _ = std::fs::remove_file(part);
    }
}

/// 一次生成要几段：5 秒 1 段，10 秒 2 段，30 秒 6 段，60 秒 12 段。
pub fn segments_for(seconds: f64) -> usize {
    ((seconds / f64::from(SEGMENT_SECONDS)).round() as usize).clamp(1, 16)
}

/// 预计花费，元。
pub fn estimate(seconds: f64, price_per_sec: f64) -> f64 {
    let price = if price_per_sec > 0.0 {
        price_per_sec
    } else {
        0.5
    };
    (seconds / f64::from(SEGMENT_SECONDS)).ceil() * f64::from(SEGMENT_SECONDS) * price
}

/// 本地模型目录的一行（**预留**：等待模型包发布，不做假下载）。
#[derive(Clone, Debug, Default)]
pub struct LocalModel {
    /// 稳定 id。
    pub id: String,
    /// 名字。
    pub name: String,
    /// 一句话说明。
    pub summary: String,
    /// 下载体积。
    pub size: String,
    /// 最低显存。
    pub vram: String,
    /// 本机是否满足。
    pub fit: bool,
    /// 本机结论。
    pub fit_note: String,
    /// 能不能下载（本版本一律 false：等待模型包发布）。
    pub downloadable: bool,
    /// 状态。
    pub status: String,
}

/// 本地视频模型目录（预留）。
pub fn local_models(hardware: &crate::hardware::Hardware) -> Vec<LocalModel> {
    let vram = hardware.gpu.vram_mb;
    let discrete = hardware.gpu.discrete;
    let row = |id: &str, name: &str, summary: &str, size: &str, need_gb: u64| LocalModel {
        id: id.to_owned(),
        name: name.to_owned(),
        summary: summary.to_owned(),
        size: size.to_owned(),
        vram: format!("{need_gb}GB"),
        fit: discrete && vram >= need_gb * 1024,
        fit_note: if !discrete {
            "未检测到独立显卡：本地生成不可用".to_owned()
        } else if vram >= need_gb * 1024 {
            format!("本机显存 {}GB，满足最低 {}GB", vram / 1024, need_gb)
        } else {
            format!("本机显存 {}GB，低于最低 {}GB", vram / 1024, need_gb)
        },
        downloadable: false,
        status: "即将开放：等待模型包发布".to_owned(),
    };
    vec![
        row(
            "wan2.2-i2v-a14b",
            "Wan2.2-I2V-A14B",
            "通义万相 2.2 图生视频（A14B 双专家，720P，中文提示词友好）",
            "约 28GB",
            24,
        ),
        row(
            "ltx-video",
            "LTX-Video",
            "轻量实时级文生/图生视频（1216×704，速度快、显存要求低）",
            "约 20GB",
            16,
        ),
    ]
}

/// 本地推理接口（预留）：等模型包发布后接入，现在如实报「未开放」。
pub fn local_generate(_request: &Request, _dirs: &crate::AppDirs) -> Result<Output, String> {
    Err("本地推理接口已预留：等待 Wan2.2-I2V-A14B / LTX-Video 模型包发布后接入；本版本不提供下载、不做假下载，也不伪造本地生成结果。云端生成现在就能用。".to_owned())
}

/// 毫秒时间戳，给临时文件和成片起不重名的名字。
pub fn stamp_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis())
}

/// 写一条本地使用日志（`ai-video/usage.jsonl`），便于对账。
pub fn log_usage(dirs: &crate::AppDirs, output: &Output, model: &str, aspect: &str) {
    let dir = dirs.config.join("ai-video");
    let path = dir.join("usage.jsonl");
    let line = serde_json::json!({
        "ts": stamp_ms() / 1000,
        "file": output.path.display().to_string(),
        "segments": output.segments,
        "duration": output.duration,
        "width": output.width,
        "height": output.height,
        "size": output.size,
        "model": model,
        "aspect": aspect,
        "request_ids": output.request_ids,
        "cost_cny": output.cost_cny,
    })
    .to_string();
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_result_url_is_made_absolute() {
        assert_eq!(
            absolute("/ai-nav/studio/files/aivideo/a.mp4"),
            "https://xinglaiai.com/ai-nav/studio/files/aivideo/a.mp4"
        );
        assert_eq!(
            absolute("https://s3.example/a.mp4"),
            "https://s3.example/a.mp4"
        );
    }

    #[test]
    fn segments_follow_the_five_second_floor() {
        assert_eq!(segments_for(5.0), 1);
        assert_eq!(segments_for(10.0), 2);
        assert_eq!(segments_for(30.0), 6);
        assert_eq!(segments_for(60.0), 12);
        assert_eq!(segments_for(1.0), 1);
    }

    #[test]
    fn the_estimate_charges_by_five_second_blocks() {
        assert!((estimate(5.0, 0.5) - 2.5).abs() < 1e-9);
        assert!((estimate(10.0, 0.5) - 5.0).abs() < 1e-9);
        assert!((estimate(60.0, 0.5) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn a_model_row_reads_as_the_platform_sends_it() {
        let row = Model {
            id: "MiniMaxAI/MiniMax-H3".to_owned(),
            label: "MiniMax H3（图生/文生视频）".to_owned(),
            note: "¥0.5/秒".to_owned(),
            enabled: true,
            default: true,
            ..Model::default()
        };
        assert!(row.display().contains("MiniMax"));
        assert!(row.usable());
    }

    #[test]
    fn the_local_catalogue_never_offers_a_fake_download() {
        let hardware = crate::hardware::Hardware::default();
        for model in local_models(&hardware) {
            assert!(!model.downloadable);
            assert!(model.status.contains("等待模型包发布"));
        }
        assert!(local_generate(&Request::default(), &crate::AppDirs::under(Path::new("."))).is_err());
    }
}
