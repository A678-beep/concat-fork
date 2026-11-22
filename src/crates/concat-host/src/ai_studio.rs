// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 星籁 AI 工作室（云端）：一键成片 / 动态解说片 / 爆款复刻。
//!
//! 桌面端不打包任何 API Key：这三个能力都由平台在网页工作台里提供，
//! 桌面只是换一个入口，走平台的登录令牌与计费：
//!
//! - `POST /credits/estimate`：生成前的预计积分（管理员账号 `admin_exempt`）
//! - **一键成片**（autoedit）：
//!   `POST /autoedit/understand`（文案 → 分镜工程）
//!   `POST /autoedit/render {project_id, options, presenter}` → `{task_id}`
//!   `GET /autoedit/task/{id}` 轮询（status/progress/stage/url/credits）
//! - **动态解说片**（explainer）：
//!   `GET /explainer/styles`、`POST /explainer/script`、
//!   `POST /explainer/render {script|script_id, style, aspect, resolution, tts, voice}`
//!   `GET /explainer/task/{id}`、`POST /explainer/cancel {task_id}`
//! - **爆款复刻**（replicate）：
//!   `POST /upload`（multipart）→ 视频地址
//!   `POST /replicate/analyze {video_url}` → 镜头 / 文案结构 / 模板
//!   `GET /replicate/library?q=`（站内商用素材）
//!   `POST /replicate/build {analysis_id, duration, slots, voice, bgm, title}`
//!   `GET /replicate/task/{id}`
//!
//! 网络层复用登录那条直连：Windows 上 OpenSSL 直连、不走系统代理（平台的
//! WAF 只放行 OpenSSL 形状的 ClientHello）；下载走 [`crate::direct`]。成片
//! 落到 `%APPDATA%\app.concat.editor\ai-output\`。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::AppDirs;

/// 平台站点；相对地址补全用。
pub const SITE: &str = "https://xinglaiai.com";

/// 三个功能的网页版地址：接口不可用时的兜底入口。
pub const AUTOEDIT_WEB: &str = "https://xinglaiai.com/ai-nav/autoedit/";
pub const EXPLAINER_WEB: &str = "https://xinglaiai.com/ai-nav/explainer/";
pub const REPLICATE_WEB: &str = "https://xinglaiai.com/ai-nav/replicate/";

/// 一键成片最长等这么久（平台实测 20 秒 15s 成片；渲染锁排队时更久）。
const AUTOEDIT_READ: Duration = Duration::from_secs(12 * 60);
/// 动态解说片单条成片 ≤90 秒，渲染通常 20–90 秒。
const EXPLAINER_READ: Duration = Duration::from_secs(8 * 60);
/// 爆款复刻出片比解说片慢（逐镜头配音 + 素材 + 合成）。
const REPLICATE_READ: Duration = Duration::from_secs(10 * 60);
/// 轮询间隔。
const POLL: Duration = Duration::from_secs(2);

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
        _ => Err("请先登录星籁账号（首屏登录后重试）；AI 工作室由平台代扣额度".to_owned()),
    }
}

/// 服务器说不行时翻成一句人话：优先用平台给的中文提示。
fn refusal(status: u16, text: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(text).ok();
    let field = |name: &str| {
        value
            .as_ref()
            .and_then(|value| value.get(name))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let message = field("hint")
        .or_else(|| field("message"))
        .or_else(|| field("error"));
    match status {
        0 => "服务器没有给出回复（连接被中途关闭），请稍后重试".to_owned(),
        401 => "登录已失效，请重新登录星籁账号".to_owned(),
        402 | 403 => message.unwrap_or_else(|| {
            "积分不足或没有这个功能的权限，请在网页工作台充值或升级套餐".to_owned()
        }),
        404 => message.unwrap_or_else(|| "云端接口不存在，请升级到最新版本".to_owned()),
        413 => "上传的文件太大，请压缩后再试".to_owned(),
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
        return Err(value
            .get("hint")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("message").and_then(serde_json::Value::as_str))
            .or_else(|| value.get("error").and_then(serde_json::Value::as_str))
            .unwrap_or("云端 AI 拒绝了这次请求")
            .to_owned());
    }
    Ok(())
}

/// 平台的计费判断：预计积分、是不是管理员免扣。
#[derive(Clone, Debug, Default)]
pub struct Estimate {
    /// 预计扣的积分（≈ 元）。
    pub credits: f64,
    /// 平台标注的费率行（`成片渲染 2 积分/条` 之类）。
    pub lines: Vec<String>,
    /// 余额，未知为 0。
    pub balance: f64,
    /// 管理员（内部账号）免扣。
    pub admin_exempt: bool,
    /// 够不够扣。
    pub enough: bool,
}

impl Estimate {
    /// 界面上一行：`预计 2.0 积分（成片渲染） · 内部账号免扣`。
    pub fn display(&self) -> String {
        if self.credits <= 0.0 && self.lines.is_empty() {
            return String::new();
        }
        let mut text = format!("本次预计消耗 {:.1} 积分", self.credits.max(0.0));
        if !self.lines.is_empty() {
            text.push_str(&format!("（{}）", self.lines.join(" + ")));
        }
        if self.admin_exempt {
            text.push_str(" · 内部账号（管理员）免扣积分，仅记录成本");
        } else if self.balance > 0.0 {
            text.push_str(&format!(" · 余额 {:.1} 积分", self.balance));
        }
        if !self.admin_exempt && !self.enough && self.credits > 0.0 {
            text.push_str(" · 余额可能不足，请先在网页工作台充值");
        }
        text
    }
}

/// 生成前的积分预估（`POST /credits/estimate`）。
///
/// `presenter_mode`：`none` / `chanjing` / `lipsync` / `photo_lipsync`。
/// 未登录也能调（匿名会按普通用户口径给预估）。
pub fn estimate(
    token: Option<&str>,
    presenter_mode: &str,
    duration_seconds: f64,
) -> Result<Estimate, String> {
    let body = serde_json::json!({
        "presenter_mode": presenter_mode,
        "duration_seconds": duration_seconds,
        "render": true,
    })
    .to_string();
    let (status, text) = crate::account::api_call(
        "POST",
        &endpoint("/credits/estimate"),
        "application/json",
        body.as_bytes(),
        token,
        Duration::from_secs(25),
    )?;
    let value = json_reply(status, &text)?;
    check_ok(&value)?;
    let mut lines = Vec::new();
    if let Some(items) = value.get("items").and_then(serde_json::Value::as_array) {
        for item in items {
            let label = item
                .get("label")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let credits = item
                .get("credits")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            if !label.is_empty() && credits > 0.0 {
                lines.push(format!("{label} {credits:.1} 积分"));
            }
        }
    }
    Ok(Estimate {
        credits: value
            .get("credits")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
        lines,
        balance: value
            .get("balance")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
        admin_exempt: value
            .get("admin_exempt")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        enough: value
            .get("enough")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
    })
}

// ─────────────────────────────────────────────────────────────────────────
// 成片的落盘与插入
// ─────────────────────────────────────────────────────────────────────────

/// 成片目录：`%APPDATA%\app.concat.editor\ai-output\`（与配置目录同一份）。
pub fn output_dir(dirs: &AppDirs) -> PathBuf {
    dirs.config.join("ai-output")
}

/// 一条下载好的成片。
#[derive(Clone, Debug, Default)]
pub struct VideoOutput {
    /// 落盘的 mp4。
    pub path: PathBuf,
    /// 平台任务 id。
    pub task_id: String,
    /// 时长，秒。
    pub duration: f64,
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// 字节数。
    pub size: u64,
    /// 计费说明（`已扣 2.0 积分` / `内部账号免扣积分` / 空）。
    pub credits: String,
    /// 平台说这是内部账号免扣。
    pub admin_exempt: bool,
}

impl VideoOutput {
    /// 界面上的一句话结果。
    pub fn summary(&self, feature: &str, inserted: Option<f64>) -> String {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut text = format!(
            "{feature} · {:.1} 秒 · {}×{} · {:.1} MB · 文件 {name}",
            self.duration,
            self.width,
            self.height,
            self.size as f64 / (1024.0 * 1024.0)
        );
        if !self.credits.trim().is_empty() {
            text.push_str(&format!(" · {}", self.credits.trim()));
        }
        match inserted {
            Some(start) => text.push_str(&format!(" · 已插入时间轴 {start:.2}s")),
            None => text.push_str(" · 没有打开的工程，未插入"),
        }
        text
    }
}

/// 毫秒时间戳，给成片起不重名的名字。
pub fn stamp_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis())
}

/// 写一条本地使用日志（`ai-output/usage.jsonl`），便于对账。
pub fn log_usage(dirs: &AppDirs, feature: &str, output: &VideoOutput, detail: &str) {
    let dir = output_dir(dirs);
    let _ = std::fs::create_dir_all(&dir);
    let line = serde_json::json!({
        "ts": stamp_ms() / 1000,
        "feature": feature,
        "task_id": output.task_id,
        "file": output.path.display().to_string(),
        "duration": output.duration,
        "width": output.width,
        "height": output.height,
        "size": output.size,
        "credits": output.credits,
        "admin_exempt": output.admin_exempt,
        "detail": detail,
    })
    .to_string();
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("usage.jsonl"))
    {
        use std::io::Write as _;
        let _ = writeln!(file, "{line}");
    }
}

/// 把平台给的成片地址下载进 `ai-output/`，校验后返回落盘信息。
fn download_output(
    dirs: &AppDirs,
    url: &str,
    prefix: &str,
    task_id: &str,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<(PathBuf, u64), String> {
    if url.trim().is_empty() {
        return Err("任务完成，但平台没有给出成片地址".to_owned());
    }
    let dir = output_dir(dirs);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("建不了 ai-output 目录：{error}"))?;
    let suffix = if task_id.trim().is_empty() {
        stamp_ms().to_string()
    } else {
        task_id.trim().to_owned()
    };
    let dest = dir.join(format!("{prefix}-{}-{suffix}.mp4", stamp_ms()));
    let temporary = dest.with_extension("part");
    crate::direct::download(&absolute(url), &temporary, &mut |fraction, detail| {
        progress(fraction.clamp(0.0, 1.0), detail.to_owned());
    }, cancel)?;
    let size = std::fs::metadata(&temporary).map_or(0, |meta| meta.len());
    if size < 30_000 {
        let _ = std::fs::remove_file(&temporary);
        return Err("下载到的成片不完整（文件太小），请重试".to_owned());
    }
    std::fs::rename(&temporary, &dest).map_err(|error| format!("存不下成片：{error}"))?;
    Ok((dest, size))
}

/// 下载完再探一次时长/分辨率，界面如实显示平台成片。
fn output_of(
    dirs: &AppDirs,
    url: &str,
    prefix: &str,
    task_id: &str,
    credits: String,
    admin_exempt: bool,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<VideoOutput, String> {
    let (path, size) = download_output(dirs, url, prefix, task_id, progress, cancel)?;
    let summary = crate::media::probe(&path.to_string_lossy())?;
    let (width, height) = summary
        .video
        .as_ref()
        .map(|video| (video.width, video.height))
        .unwrap_or((0, 0));
    Ok(VideoOutput {
        path,
        task_id: task_id.to_owned(),
        duration: summary.duration.unwrap_or(0.0),
        width,
        height,
        size,
        credits,
        admin_exempt,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// 一、AI 一键成片（autoedit）
// ─────────────────────────────────────────────────────────────────────────

/// 平台音色（`GET /autoedit/voices`）。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AutoVoice {
    /// 界面上显示的名字。
    #[serde(default)]
    pub id: String,
    /// 请求里要带的 `voice`。
    #[serde(default)]
    pub voice: String,
    /// 一句话说明。
    #[serde(default)]
    pub desc: String,
}

/// 云端数字人形象（蝉镜，`GET /autoedit/presenter/options` 的 chanjing 模式）。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CloudPerson {
    /// 请求里要带的 `person_id`。
    #[serde(default)]
    pub id: String,
    /// 形象名。
    #[serde(default)]
    pub name: String,
    /// 已就绪。
    #[serde(default)]
    pub ready: bool,
    /// `custom` / `public`。
    #[serde(default)]
    pub source: String,
}

impl CloudPerson {
    /// 下拉一行：`公共形象 · 文昊`。
    pub fn label(&self) -> String {
        let source = match self.source.trim() {
            "custom" => "定制形象",
            "public" => "公共形象",
            _ => "形象",
        };
        let name = if self.name.trim().is_empty() {
            self.id.clone()
        } else {
            self.name.trim().to_owned()
        };
        if self.ready {
            format!("{source} · {name}")
        } else {
            format!("{source} · {name}（未就绪）")
        }
    }
}

/// 一键成片用到的平台清单：音色 + 云端数字人形象。
#[derive(Clone, Debug, Default)]
pub struct AutoeditCatalog {
    /// 音色。
    pub voices: Vec<AutoVoice>,
    /// 云端数字人形象（蝉镜）。
    pub persons: Vec<CloudPerson>,
    /// 蝉镜可用（数字人形象能选）。
    pub chanjing: bool,
    /// 平台说明。
    pub note: String,
}

/// 取音色与数字人形象。两者任一失败都不算致命：面板仍能用「无出镜」。
pub fn autoedit_catalog(token: Option<&str>) -> Result<AutoeditCatalog, String> {
    let mut catalog = AutoeditCatalog::default();
    let mut errors = Vec::new();
    let (status, text) = crate::account::api_call(
        "GET",
        &endpoint("/autoedit/voices"),
        "application/json",
        &[],
        token,
        Duration::from_secs(20),
    )?;
    match json_reply(status, &text) {
        Ok(value) => {
            if let Some(rows) = value.get("voices").and_then(serde_json::Value::as_array) {
                for row in rows {
                    if let Ok(voice) = serde_json::from_value::<AutoVoice>(row.clone()) {
                        catalog.voices.push(voice);
                    }
                }
            }
        }
        Err(error) => errors.push(format!("音色列表：{error}")),
    }
    match crate::account::api_call(
        "GET",
        &endpoint("/autoedit/presenter/options"),
        "application/json",
        &[],
        token,
        Duration::from_secs(25),
    ) {
        Ok((status, text)) => match json_reply(status, &text) {
            Ok(value) => {
                if let Some(modes) = value.get("modes").and_then(serde_json::Value::as_array) {
                    if let Some(chanjing) = modes
                        .iter()
                        .find(|mode| mode.get("id").and_then(serde_json::Value::as_str) == Some("chanjing"))
                    {
                        catalog.chanjing = chanjing
                            .get("enabled")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(true);
                        if let Some(persons) =
                            chanjing.get("persons").and_then(serde_json::Value::as_array)
                        {
                            for row in persons {
                                if let Ok(person) = serde_json::from_value::<CloudPerson>(row.clone()) {
                                    catalog.persons.push(person);
                                }
                            }
                        }
                        catalog.note = chanjing
                            .get("note")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                    }
                }
            }
            Err(error) => errors.push(format!("数字人形象：{error}")),
        },
        Err(error) => errors.push(format!("数字人形象：{error}")),
    }
    if catalog.voices.is_empty() && catalog.persons.is_empty() {
        return Err(errors.join("；"));
    }
    if !errors.is_empty() {
        catalog.note = format!("{}（{}）", catalog.note.trim(), errors.join("；"))
            .trim()
            .to_owned();
    }
    Ok(catalog)
}

/// 一键成片的出镜方式。
#[derive(Clone, Debug, Default)]
pub enum Presenter {
    /// 不出镜，纯画面 + 配音。
    #[default]
    None,
    /// 云端数字人（蝉镜形象）。
    Cloud {
        /// 形象 id。
        person_id: String,
        /// 形象名（显示）。
        name: String,
    },
}

impl Presenter {
    /// 请求里的 `presenter` 字段；无出镜返回 `None`。
    pub fn json(&self) -> Option<serde_json::Value> {
        match self {
            Presenter::None => None,
            Presenter::Cloud { person_id, .. } => Some(serde_json::json!({
                "mode": "chanjing",
                "person_id": person_id,
                "layout": "full",
                "bg": {"mode": "none"},
            })),
        }
    }

    /// 计费用的模式名。
    pub fn mode(&self) -> &'static str {
        match self {
            Presenter::None => "none",
            Presenter::Cloud { .. } => "chanjing",
        }
    }

    /// 界面一行。
    pub fn label(&self) -> String {
        match self {
            Presenter::None => "无出镜（纯画面 + 配音）".to_owned(),
            Presenter::Cloud { name, .. } => format!("云端数字人 · {name}"),
        }
    }
}

/// 一次「文案 → 分镜」的结果。
#[derive(Clone, Debug, Default)]
pub struct AutoProject {
    /// 平台工程 id（渲染要它）。
    pub project_id: String,
    /// 平台起的标题。
    pub title: String,
    /// 镜头数。
    pub shots: usize,
    /// 时长，秒。
    pub duration: f64,
    /// 分镜从哪来：`deepseek` / `rule`。
    pub source: String,
}

/// 文案 / 一句需求 → 平台分镜工程（`POST /autoedit/understand`）。
pub fn autoedit_understand(
    token: &str,
    text: &str,
    duration_seconds: f64,
    aspect: &str,
    voice: &str,
) -> Result<AutoProject, String> {
    let text = text.trim();
    if text.chars().count() < 8 {
        return Err("请先写一句需求或粘贴一段文案（至少 8 个字）".to_owned());
    }
    let body = serde_json::json!({
        "text": text.chars().take(3000).collect::<String>(),
        "duration": duration_seconds.clamp(8.0, 180.0),
        "aspect": aspect,
        "voice": voice,
    })
    .to_string();
    let (status, reply) = crate::account::api_call(
        "POST",
        &endpoint("/autoedit/understand"),
        "application/json",
        body.as_bytes(),
        Some(token),
        Duration::from_secs(120),
    )?;
    let value = json_reply(status, &reply)?;
    check_ok(&value)?;
    let project_id = value
        .get("project_id")
        .or_else(|| value.get("id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "分镜生成成功，但服务器没有给出工程 id".to_owned())?
        .to_owned();
    Ok(AutoProject {
        project_id,
        title: value
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        shots: value
            .get("segments")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len),
        duration: value
            .get("duration")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(duration_seconds),
        source: value
            .get("source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

/// 平台任务的公共状态。
#[derive(Clone, Debug, Default)]
struct TaskStatus {
    status: String,
    progress: f32,
    stage: String,
    url: String,
    download_url: String,
    error: String,
    hint: String,
    duration: f64,
    width: u32,
    height: u32,
    credits: String,
    admin_exempt: bool,
}

impl TaskStatus {
    /// 从平台 JSON 取一次任务状态（兼容 `{ok, ...}` 与 `{ok, task:{...}}`）。
    fn parse(value: &serde_json::Value) -> TaskStatus {
        let task = value.get("task").filter(|task| task.is_object());
        let at = |name: &str| task.and_then(|task| task.get(name)).or_else(|| value.get(name));
        let text = |name: &str| {
            at(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let number = |name: &str| at(name).and_then(serde_json::Value::as_f64);
        let credits = at("credits").cloned().unwrap_or_default();
        let admin_exempt = credits
            .get("admin_exempt")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let credit_text = if admin_exempt {
            "内部账号免扣积分（成本照记）".to_owned()
        } else {
            let charged = credits
                .get("charged")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            if charged > 0.0 {
                format!("已扣 {charged:.1} 积分")
            } else if credits
                .get("note")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|note| note.contains("失败不扣费"))
            {
                "失败不扣积分".to_owned()
            } else {
                String::new()
            }
        };
        TaskStatus {
            status: text("status"),
            progress: number("progress").unwrap_or(0.0) as f32 / 100.0,
            stage: text("stage"),
            // 平台不同任务叫法不同：一键成片给 url，解说片 / 复刻给 video_url。
            url: {
                let url = text("url");
                if url.trim().is_empty() {
                    text("video_url")
                } else {
                    url
                }
            },
            download_url: text("download_url"),
            error: text("error"),
            hint: text("hint"),
            duration: number("duration").unwrap_or(0.0),
            width: number("width").unwrap_or(0.0) as u32,
            height: number("height").unwrap_or(0.0) as u32,
            credits: credit_text,
            admin_exempt,
        }
    }

    /// 平台地址：优先 `download_url`（带 ?download=1），其次成片地址。
    fn video_url(&self) -> &str {
        if !self.download_url.trim().is_empty() {
            &self.download_url
        } else {
            &self.url
        }
    }

    /// 失败原因（中文）。
    fn failure(&self) -> String {
        for text in [&self.hint, &self.error, &self.stage] {
            if !text.trim().is_empty() {
                return text.trim().to_owned();
            }
        }
        "平台渲染失败，请稍后重试".to_owned()
    }
}

/// 轮询一个平台任务，直到完成 / 失败 / 取消 / 超时。
fn poll_task(
    token: &str,
    path: &str,
    deadline: Duration,
    feature: &str,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<TaskStatus, String> {
    let began = Instant::now();
    let mut last_error = String::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(format!("已取消（{feature}任务可能仍在平台后台跑完）"));
        }
        if began.elapsed() > deadline {
            return Err(format!(
                "{feature}等待超时（已等 {} 分钟）：平台可能排队中，请稍后到网页工作台查看，或重试",
                began.elapsed().as_secs() / 60
            ));
        }
        match crate::account::api_call(
            "GET",
            &endpoint(path),
            "application/json",
            &[],
            Some(token),
            Duration::from_secs(30),
        ) {
            Ok((status, text)) => match json_reply(status, &text) {
                Ok(value) => {
                    if let Err(error) = check_ok(&value) {
                        return Err(error);
                    }
                    let task = TaskStatus::parse(&value);
                    let waited = began.elapsed().as_secs();
                    let stage = if task.stage.trim().is_empty() {
                        format!("{feature}进行中…已等待 {waited} 秒")
                    } else {
                        format!("{}（已等待 {waited} 秒）", task.stage.trim())
                    };
                    match task.status.as_str() {
                        "done" => {
                            progress(1.0, "下载成片…".to_owned());
                            return Ok(task);
                        }
                        "failed" | "cancelled" | "error" => return Err(task.failure()),
                        _ => {
                            progress(
                                task.progress.clamp(0.02, 0.97),
                                stage,
                            );
                        }
                    }
                }
                Err(error) => last_error = error,
            },
            Err(error) => last_error = error,
        }
        // 轮询的 2 秒等切成 4 段，取消能在半秒内生效。
        for _ in 0..4 {
            if cancel.load(Ordering::Relaxed) {
                return Err(format!("已取消（{feature}任务可能仍在平台后台跑完）"));
            }
            std::thread::sleep(POLL / 4);
        }
        let _ = &last_error;
    }
}

/// 一键成片：提交渲染，轮询到出片，下载进 `ai-output/`。
pub fn autoedit_render(
    token: &str,
    dirs: &AppDirs,
    project_id: &str,
    voice: &str,
    presenter: &Presenter,
    duration_estimate: f64,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<VideoOutput, String> {
    let mut body = serde_json::json!({
        "project_id": project_id,
        "options": { "voice": voice },
    });
    if let Some(presenter) = presenter.json() {
        body["presenter"] = presenter;
    }
    let body = body.to_string();
    progress(0.02, "提交到星籁平台…".to_owned());
    let (status, reply) = crate::account::api_call(
        "POST",
        &endpoint("/autoedit/render"),
        "application/json",
        body.as_bytes(),
        Some(token),
        Duration::from_secs(60),
    )?;
    let value = json_reply(status, &reply)?;
    check_ok(&value)?;
    let task_id = value
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "提交成功，但服务器没有给出任务 id".to_owned())?
        .to_owned();
    let estimate = value
        .get("estimated_seconds")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(duration_estimate * 1.8);
    progress(
        0.05,
        format!("平台渲染中（预计 {estimate:.0} 秒，可取消）…"),
    );
    let task = poll_task(
        token,
        &format!("/autoedit/task/{task_id}"),
        AUTOEDIT_READ,
        "一键成片",
        &mut |fraction, stage| progress(0.05 + fraction * 0.85, stage),
        cancel,
    )?;
    output_of(
        dirs,
        task.video_url(),
        "autoedit",
        &task_id,
        task.credits.clone(),
        task.admin_exempt,
        &mut |fraction, detail| progress(0.92 + fraction * 0.07, detail),
        cancel,
    )
}

// ─────────────────────────────────────────────────────────────────────────
// 二、动态解说片（explainer）
// ─────────────────────────────────────────────────────────────────────────

/// 一套解说片视觉风格。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ExplainerStyle {
    /// 请求里要带的 `style`。
    #[serde(default)]
    pub id: String,
    /// 中文名。
    #[serde(default)]
    pub name: String,
    /// 适用场景。
    #[serde(default)]
    pub tag: String,
}

impl ExplainerStyle {
    /// 下拉一行：`科技蓝 · 发布会 / 科技产品`。
    pub fn label(&self) -> String {
        if self.tag.trim().is_empty() {
            self.name.clone()
        } else {
            format!("{} · {}", self.name, self.tag)
        }
    }
}

/// 解说片音色（`/explainer/styles` 的 `voices`）。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ExplainerVoice {
    /// 请求里要带的 `voice`。
    #[serde(default)]
    pub id: String,
    /// 中文名。
    #[serde(default)]
    pub name: String,
    /// 一句话说明。
    #[serde(default)]
    pub desc: String,
}

impl ExplainerVoice {
    /// 下拉一行。
    pub fn label(&self) -> String {
        if self.desc.trim().is_empty() {
            self.name.clone()
        } else {
            format!("{} · {}", self.name, self.desc)
        }
    }
}

/// 解说片可选项（`GET /explainer/styles`）。
#[derive(Clone, Debug, Default)]
pub struct ExplainerCatalog {
    /// 4 套风格。
    pub styles: Vec<ExplainerStyle>,
    /// 配音音色。
    pub voices: Vec<ExplainerVoice>,
    /// `1080p` / `720p`。
    pub resolutions: Vec<String>,
    /// `16:9` / `9:16`。
    pub aspects: Vec<String>,
}

/// 平台风格、音色、分辨率清单。
pub fn explainer_catalog(token: Option<&str>) -> Result<ExplainerCatalog, String> {
    let (status, text) = crate::account::api_call(
        "GET",
        &endpoint("/explainer/styles"),
        "application/json",
        &[],
        token,
        Duration::from_secs(25),
    )?;
    let value = json_reply(status, &text)?;
    check_ok(&value)?;
    let mut catalog = ExplainerCatalog::default();
    if let Some(rows) = value.get("styles").and_then(serde_json::Value::as_array) {
        for row in rows {
            if let Ok(style) = serde_json::from_value::<ExplainerStyle>(row.clone()) {
                catalog.styles.push(style);
            }
        }
    }
    if let Some(rows) = value.get("voices").and_then(serde_json::Value::as_array) {
        for row in rows {
            if let Ok(voice) = serde_json::from_value::<ExplainerVoice>(row.clone()) {
                catalog.voices.push(voice);
            }
        }
    }
    if let Some(rows) = value.get("resolutions").and_then(serde_json::Value::as_array) {
        for row in rows {
            if let Some(id) = row.get("id").and_then(serde_json::Value::as_str) {
                catalog.resolutions.push(id.to_owned());
            }
        }
    }
    if let Some(rows) = value.get("aspects").and_then(serde_json::Value::as_array) {
        for row in rows {
            if let Some(id) = row.as_str() {
                catalog.aspects.push(id.to_owned());
            }
        }
    }
    if catalog.resolutions.is_empty() {
        catalog.resolutions = vec!["1080p".to_owned(), "720p".to_owned()];
    }
    if catalog.aspects.is_empty() {
        catalog.aspects = vec!["16:9".to_owned(), "9:16".to_owned()];
    }
    Ok(catalog)
}

/// 一次脚本生成的结果：结构化脚本（渲染与编辑都用它）+ 预览行。
#[derive(Clone, Debug, Default)]
pub struct Script {
    /// 平台脚本 id（7 天内可复用）。
    pub script_id: String,
    /// 标题（可改）。
    pub title: String,
    /// 每页一行预览：`1. 标题 — 口播…`。
    pub pages: Vec<String>,
    /// 预估时长，秒。
    pub est_duration: f64,
    /// 生成引擎（`deepseek` / `local-template`）。
    pub engine: String,
    /// 平台说明。
    pub note: String,
    /// 完整脚本 JSON（渲染时原样送回，标题改了也送回）。
    pub raw: serde_json::Value,
}

impl Script {
    /// 从平台回复里读脚本。
    fn parse(value: &serde_json::Value, est: f64) -> Result<Script, String> {
        let raw = value
            .get("script")
            .cloned()
            .ok_or_else(|| "脚本生成成功，但服务器没有给出脚本内容".to_owned())?;
        let title = raw
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut pages = Vec::new();
        if let Some(rows) = raw.get("pages").and_then(serde_json::Value::as_array) {
            for (index, page) in rows.iter().enumerate() {
                let title = page
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let narration = page
                    .get("narration")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let kind = page
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let line = if narration.trim().is_empty() {
                    format!("{}. {title}［{kind}］", index + 1)
                } else {
                    format!("{}. {title} — {narration}", index + 1)
                };
                pages.push(line.chars().take(200).collect());
            }
        }
        Ok(Script {
            script_id: value
                .get("script_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            title,
            pages,
            est_duration: value
                .get("est_duration")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(est),
            engine: value
                .get("engine")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            note: value
                .get("note")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            raw,
        })
    }

    /// 改标题：主标题与首页标题一起改（渲染器两处都读）。
    pub fn rename(&mut self, title: &str) {
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        self.title = title.to_owned();
        if let Some(object) = self.raw.as_object_mut() {
            object.insert("title".to_owned(), serde_json::Value::String(title.to_owned()));
            if let Some(pages) = object.get_mut("pages").and_then(serde_json::Value::as_array_mut) {
                if let Some(first) = pages.first_mut() {
                    if first.get("type").and_then(serde_json::Value::as_str) == Some("title") {
                        first["title"] = serde_json::Value::String(title.to_owned());
                    }
                }
            }
        }
        // 预览第一行跟着改。
        if let Some(first) = self.pages.first_mut() {
            *first = if let Some(rest) = first.split_once('.').map(|(_, rest)| rest) {
                format!("1.{rest}")
            } else {
                first.clone()
            };
        }
    }
}

/// 主题 → 脚本（`POST /explainer/script`）。
pub fn explainer_script(
    token: &str,
    topic: &str,
    duration_seconds: u32,
    aspect: &str,
    style: &str,
) -> Result<Script, String> {
    let topic = topic.trim();
    if topic.is_empty() {
        return Err("请先写一个主题，例如「AI 如何重塑餐饮」".to_owned());
    }
    let body = serde_json::json!({
        "topic": topic.chars().take(80).collect::<String>(),
        "duration": duration_seconds.clamp(15, 90),
        "platform": aspect,
        "style": style,
    })
    .to_string();
    let (status, reply) = crate::account::api_call(
        "POST",
        &endpoint("/explainer/script"),
        "application/json",
        body.as_bytes(),
        Some(token),
        Duration::from_secs(120),
    )?;
    let value = json_reply(status, &reply)?;
    check_ok(&value)?;
    Script::parse(&value, f64::from(duration_seconds))
}

/// 脚本 → 成片（`POST /explainer/render` + 轮询 + 下载）。
pub struct ExplainerRequest<'a> {
    /// 完整脚本 JSON（可带用户改过的标题）。
    pub script: &'a serde_json::Value,
    /// 风格 id。
    pub style: String,
    /// `16:9` / `9:16`。
    pub aspect: String,
    /// `1080p` / `720p`。
    pub resolution: String,
    /// 是否配音。
    pub tts: bool,
    /// 音色 id。
    pub voice: String,
}

/// 渲染解说片。
pub fn explainer_render(
    token: &str,
    dirs: &AppDirs,
    request: &ExplainerRequest,
    progress: &mut dyn FnMut(f32, String),
    on_task: &mut dyn FnMut(String),
    cancel: &AtomicBool,
) -> Result<VideoOutput, String> {
    let body = serde_json::json!({
        "script": request.script,
        "style": request.style,
        "aspect": request.aspect,
        "resolution": request.resolution,
        "tts": request.tts,
        "voice": request.voice,
    })
    .to_string();
    progress(0.02, "提交到动态解说渲染队列…".to_owned());
    let (status, reply) = crate::account::api_call(
        "POST",
        &endpoint("/explainer/render"),
        "application/json",
        body.as_bytes(),
        Some(token),
        Duration::from_secs(90),
    )?;
    let value = json_reply(status, &reply)?;
    check_ok(&value)?;
    let task_id = value
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "提交成功，但服务器没有给出任务 id".to_owned())?
        .to_owned();
    let estimate = value
        .get("est_duration")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(40.0);
    on_task(task_id.clone());
    progress(
        0.05,
        format!("平台逐帧渲染中（成片预计 {estimate:.0} 秒，可取消）…"),
    );
    let task = poll_task(
        token,
        &format!("/explainer/task/{task_id}"),
        EXPLAINER_READ,
        "动态解说",
        &mut |fraction, stage| progress(0.05 + fraction * 0.85, stage),
        cancel,
    )?;
    // 平台任务没有 credits 字段：POC 不扣积分，如实写在结果里。
    output_of(
        dirs,
        task.video_url(),
        "explainer",
        &task_id,
        "解说片为平台 POC：渲染不扣积分（如后续接入计费，以平台返回为准）".to_owned(),
        false,
        &mut |fraction, detail| progress(0.92 + fraction * 0.07, detail),
        cancel,
    )
}

/// 取消一个解说片任务（平台支持；其它两个只能本地停止等待）。
pub fn explainer_cancel(token: &str, task_id: &str) -> Result<(), String> {
    if task_id.trim().is_empty() {
        return Ok(());
    }
    let body = serde_json::json!({ "task_id": task_id }).to_string();
    let (status, reply) = crate::account::api_call(
        "POST",
        &endpoint("/explainer/cancel"),
        "application/json",
        body.as_bytes(),
        Some(token),
        Duration::from_secs(20),
    )?;
    let value = json_reply(status, &reply)?;
    check_ok(&value)
}

// ─────────────────────────────────────────────────────────────────────────
// 三、爆款复刻（replicate）
// ─────────────────────────────────────────────────────────────────────────

/// 一个站内商用素材（`GET /replicate/library`）。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LibraryItem {
    /// 素材 id（`material.asset_id`）。
    #[serde(default)]
    pub id: i64,
    /// 原始标题（多为英文标签）。
    #[serde(default)]
    pub title: String,
    /// 中文标题，可能为空。
    #[serde(default)]
    pub title_zh: String,
    /// 时长，秒。
    #[serde(default)]
    pub duration: f64,
    /// `0:15.1`。
    #[serde(default)]
    pub duration_text: String,
    /// 缩略图地址。
    #[serde(default)]
    pub thumb: String,
    /// 授权。
    #[serde(default)]
    pub license: String,
}

impl LibraryItem {
    /// 展示名。
    pub fn label(&self) -> String {
        if !self.title_zh.trim().is_empty() {
            self.title_zh.clone()
        } else if !self.title.trim().is_empty() {
            self.title.chars().take(40).collect()
        } else {
            format!("素材 #{}", self.id)
        }
    }
}

/// 参考视频里的一个镜头。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Shot {
    /// 第几个镜头（从 1 开始）。
    #[serde(default)]
    pub index: i64,
    /// 起点，秒。
    #[serde(default)]
    pub start: f64,
    /// 终点，秒。
    #[serde(default)]
    pub end: f64,
    /// 时长，秒。
    #[serde(default)]
    pub duration: f64,
    /// 语音识别出的文案（可能为空）。
    #[serde(default)]
    pub text: String,
    /// 景别。
    #[serde(default)]
    pub shot_size: String,
    /// 代表帧地址。
    #[serde(default)]
    pub frame_url: String,
}

impl Shot {
    /// 预览一行。
    pub fn label(&self) -> String {
        let mut line = format!(
            "镜头 {} · {:.1}-{:.1}s（{:.1}s）",
            self.index, self.start, self.end, self.duration
        );
        if !self.shot_size.trim().is_empty() {
            line.push_str(&format!(" · {}", self.shot_size.trim()));
        }
        if !self.text.trim().is_empty() {
            line.push_str(&format!(" — {}", self.text.trim()));
        }
        line.chars().take(180).collect()
    }
}

/// 一次拆解（`POST /replicate/analyze`）。
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    /// 分析 id（出片要它）。
    pub id: String,
    /// 参考视频时长，秒。
    pub duration: f64,
    /// 拆解耗时，秒。
    pub elapsed: f64,
    /// 镜头。
    pub shots: Vec<Shot>,
    /// 节奏说明。
    pub pace: String,
    /// 平均镜头时长。
    pub avg_shot: f64,
    /// 开场钩子。
    pub hook: String,
    /// 正文分段。
    pub body: Vec<String>,
    /// 结尾引导。
    pub cta: String,
    /// 字幕策略说明。
    pub subtitle: String,
    /// 完整模板 JSON（按 `duration_versions` 取出片槽位）。
    pub template: serde_json::Value,
}

impl Analysis {
    /// 拆解完成的一句话。
    pub fn summary(&self) -> String {
        format!(
            "拆解完成：{} 个镜头 · 平均 {:.1} 秒/镜头 · 参考视频 {:.1} 秒 · 节奏 {}",
            self.shots.len(),
            self.avg_shot,
            self.duration,
            if self.pace.trim().is_empty() { "—" } else { self.pace.trim() }
        )
    }

    /// 可选的成片时长版本（模板给的，默认 15/30/60）。
    pub fn versions(&self) -> Vec<u32> {
        let mut versions = Vec::new();
        if let Some(map) = self
            .template
            .get("duration_versions")
            .and_then(serde_json::Value::as_object)
        {
            for key in map.keys() {
                if let Ok(seconds) = key.parse::<u32>() {
                    versions.push(seconds);
                }
            }
        }
        versions.sort_unstable();
        if versions.is_empty() {
            versions = vec![15, 30, 60];
        }
        versions
    }

    /// 某个时长版本的槽位（`slots`）；模板没有就按镜头顺序造一组。
    pub fn slots(&self, seconds: u32) -> Vec<Slot> {
        let key = seconds.to_string();
        if let Some(slots) = self
            .template
            .get("duration_versions")
            .and_then(|versions| versions.get(&key))
            .and_then(|version| version.get("slots"))
            .and_then(serde_json::Value::as_array)
        {
            let mut out = Vec::new();
            for row in slots {
                let duration = row
                    .get("duration")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(2.0);
                out.push(Slot {
                    role: row
                        .get("role")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("body")
                        .to_owned(),
                    duration,
                    copy: row
                        .get("copy")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    shot_size: row
                        .get("shot_size")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
            if !out.is_empty() {
                return out;
            }
        }
        self.shots
            .iter()
            .take(12)
            .enumerate()
            .map(|(index, shot)| Slot {
                role: if index == 0 { "hook" } else { "body" }.to_owned(),
                duration: shot.duration.max(0.8),
                copy: shot.text.clone(),
                shot_size: shot.shot_size.clone(),
            })
            .collect()
    }
}

/// 模板里的一个镜头槽位。
#[derive(Clone, Debug, Default)]
pub struct Slot {
    /// `hook` / `body` / `cta`。
    pub role: String,
    /// 时长，秒。
    pub duration: f64,
    /// 镜头文案（会被配音并作为字幕）。
    pub copy: String,
    /// 景别。
    pub shot_size: String,
}

impl Slot {
    /// 预览一行。
    pub fn label(&self) -> String {
        format!(
            "{} · {:.1}s{} — {}",
            match self.role.as_str() {
                "hook" => "开场钩子",
                "cta" => "结尾 CTA",
                _ => "正文",
            },
            self.duration,
            if self.shot_size.trim().is_empty() {
                String::new()
            } else {
                format!(" · {}", self.shot_size.trim())
            },
            self.copy.trim()
        )
        .chars()
        .take(180)
        .collect()
    }
}

/// 上传参考视频（`POST /upload`，multipart），返回平台地址。
pub fn upload(
    token: &str,
    path: &Path,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|error| format!("参考视频读不出来：{error}"))?;
    if data.len() > 200 * 1024 * 1024 {
        return Err("参考视频超过 200MB，请先裁剪再试".to_owned());
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "reference.mp4".to_owned());
    let mime = match path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("m4v") => "video/x-m4v",
        _ => "video/mp4",
    };
    let boundary = format!("----XingLaiCut{}", stamp_ms());
    let mut body = Vec::with_capacity(data.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    progress(
        0.1,
        format!("上传参考视频（{:.1} MB）…", data.len() as f64 / (1024.0 * 1024.0)),
    );
    if cancel.load(Ordering::Relaxed) {
        return Err("已取消".to_owned());
    }
    let (status, reply) = crate::account::api_call(
        "POST",
        &endpoint("/upload"),
        &format!("multipart/form-data; boundary={boundary}"),
        &body,
        Some(token),
        Duration::from_secs(300),
    )?;
    let value = json_reply(status, &reply)?;
    check_ok(&value)?;
    let url = value
        .get("url")
        .or_else(|| value.get("file_url"))
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "上传成功，但服务器没有给出视频地址".to_owned())?;
    Ok(absolute(url))
}

/// 拆解参考视频（`POST /replicate/analyze`）。异步长任务，不能取消平台侧。
pub fn analyze(
    token: &str,
    video_url: &str,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<Analysis, String> {
    let body = serde_json::json!({ "video_url": absolute(video_url) }).to_string();
    progress(0.25, "AI 拆解中（抽帧 / 场景检测 / 语音识别）…".to_owned());
    let began = Instant::now();
    let (status, reply) = crate::account::api_call(
        "POST",
        &endpoint("/replicate/analyze"),
        "application/json",
        body.as_bytes(),
        Some(token),
        Duration::from_secs(300),
    )?;
    let value = json_reply(status, &reply)?;
    check_ok(&value)?;
    if cancel.load(Ordering::Relaxed) {
        return Err("已取消（平台的拆解任务可能仍在后台跑完）".to_owned());
    }
    let elapsed = value
        .get("elapsed")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or_else(|| began.elapsed().as_secs_f64());
    let mut shots = Vec::new();
    if let Some(rows) = value.get("shots").and_then(serde_json::Value::as_array) {
        for row in rows {
            if let Ok(shot) = serde_json::from_value::<Shot>(row.clone()) {
                shots.push(shot);
            }
        }
    }
    let rhythm = value.get("rhythm").cloned().unwrap_or_default();
    let copy_structure = value.get("copy_structure").cloned().unwrap_or_default();
    let mut body_lines = Vec::new();
    if let Some(rows) = copy_structure
        .get("body")
        .and_then(serde_json::Value::as_array)
    {
        for row in rows {
            let text = row
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if !text.trim().is_empty() {
                body_lines.push(text);
            }
        }
    }
    let subtitle = value
        .get("subtitle")
        .cloned()
        .unwrap_or_default();
    let subtitle_text = if subtitle
        .get("has_subtitle")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        format!(
            "有字幕 · {}",
            subtitle
                .get("color")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("白色")
        )
    } else {
        "未检测到明显字幕".to_owned()
    };
    Ok(Analysis {
        id: value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        duration: value
            .get("video")
            .and_then(|video| video.get("duration"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
        elapsed,
        shots,
        pace: rhythm
            .get("pace_zh")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        avg_shot: rhythm
            .get("avg_shot_duration")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
        hook: copy_structure
            .get("hook")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        body: body_lines,
        cta: copy_structure
            .get("cta")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        subtitle: subtitle_text,
        template: value.get("template").cloned().unwrap_or_default(),
    })
}

/// 搜站内商用素材（`GET /replicate/library`）。
///
/// 平台这个接口在渲染排队时会慢到二十几秒（实测 28 秒），读等待给足；
/// 调用方（[`match_materials`]）还会按关键词缓存、失败重试一次，避免
/// 八九个镜头加起来把一次复刻拖成十分钟。
pub fn library(token: &str, query: &str, limit: u32) -> Result<Vec<LibraryItem>, String> {
    let query = percent_encode(query.trim());
    let path = format!("/replicate/library?q={query}&limit={}", limit.clamp(1, 24));
    let (status, text) = crate::account::api_call(
        "GET",
        &endpoint(&path),
        "application/json",
        &[],
        Some(token),
        Duration::from_secs(60),
    )?;
    let value = json_reply(status, &text)?;
    check_ok(&value)?;
    let mut items = Vec::new();
    if let Some(rows) = value.get("items").and_then(serde_json::Value::as_array) {
        for row in rows {
            if let Ok(item) = serde_json::from_value::<LibraryItem>(row.clone()) {
                items.push(item);
            }
        }
    }
    Ok(items)
}

/// 从镜头文案猜素材关键词（与网页版同一套词表，够用就好）。
fn keywords(slot: &Slot) -> &'static str {
    const KNOWN: &[(&str, &str)] = &[
        ("城市", "城市"),
        ("夜景", "城市 夜景"),
        ("咖啡", "咖啡"),
        ("办公", "办公"),
        ("工作", "办公"),
        ("跑步", "跑步 运动"),
        ("运动", "运动"),
        ("美食", "美食"),
        ("公园", "公园"),
        ("海边", "海边"),
        ("旅行", "旅行"),
        ("人物", "人物"),
        ("团队", "团队"),
        ("门店", "门店"),
        ("产品", "产品"),
        ("学习", "学习"),
        ("宠物", "宠物"),
    ];
    for (hint, query) in KNOWN {
        if slot.copy.contains(hint) {
            return query;
        }
    }
    match slot.role.as_str() {
        "hook" => "人物 表情",
        "cta" => "团队 微笑",
        _ => "生活 场景",
    }
}

/// 给每个槽位自动配一个站内商用素材（与网页版「一键匹配」同口径）。
///
/// 同一个关键词只问平台一次；失败了重试一次、再不行用通用词兜底。平台
/// 的素材接口慢的时候一次要十几二十秒，缓存把八次调用压到三四次。
pub fn match_materials(
    token: &str,
    slots: &[Slot],
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<Vec<LibraryItem>, String> {
    use std::collections::{HashMap, HashSet};
    let mut chosen: Vec<LibraryItem> = Vec::with_capacity(slots.len());
    let mut used: HashSet<i64> = HashSet::new();
    let mut cache: HashMap<&'static str, Vec<LibraryItem>> = HashMap::new();
    let mut general: Option<Vec<LibraryItem>> = None;
    let total = slots.len().max(1);
    let fetch = |query: &'static str, cache: &mut HashMap<&'static str, Vec<LibraryItem>>| {
        if let Some(items) = cache.get(query) {
            return Some(items.clone());
        }
        let mut items = library(token, query, 8).unwrap_or_default();
        if items.is_empty() {
            std::thread::sleep(Duration::from_millis(600));
            items = library(token, query, 8).unwrap_or_default();
        }
        if items.is_empty() {
            return None;
        }
        cache.insert(query, items.clone());
        Some(items)
    };
    for (index, slot) in slots.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消".to_owned());
        }
        progress(
            0.1 + 0.18 * index as f32 / total as f32,
            format!("匹配第 {}/{} 个镜头的素材…", index + 1, total),
        );
        let query = keywords(slot);
        let mut items = fetch(query, &mut cache).unwrap_or_default();
        if items.is_empty() {
            // 通用词兜底：只问一次，之后每个镜头从同一份里挑没用过的。
            if general.is_none() {
                general = fetch("城市 生活", &mut cache);
            }
            items = general.clone().unwrap_or_default();
        }
        let picked = items
            .iter()
            .find(|item| !used.contains(&item.id))
            .or_else(|| items.first())
            .cloned();
        match picked {
            Some(item) => {
                used.insert(item.id);
                chosen.push(item);
            }
            None => {
                return Err(
                    "站内商用素材库暂时取不到素材：请稍后重试，或到网页版手动选素材".to_owned(),
                )
            }
        }
    }
    Ok(chosen)
}

/// 提交复刻成片（`POST /replicate/build` → 轮询 → 下载）。
#[allow(clippy::too_many_arguments)]
pub fn replicate_build(
    token: &str,
    dirs: &AppDirs,
    analysis: &Analysis,
    seconds: u32,
    slots: &[Slot],
    materials: &[LibraryItem],
    title: &str,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<VideoOutput, String> {
    if slots.is_empty() {
        return Err("模板里没有可用的镜头槽位：请重新拆解参考视频".to_owned());
    }
    if materials.len() < slots.len() {
        return Err("素材还没配齐：请重试「一键匹配素材」".to_owned());
    }
    let mut rows = Vec::with_capacity(slots.len());
    for (slot, item) in slots.iter().zip(materials) {
        rows.push(serde_json::json!({
            "role": slot.role,
            "duration": slot.duration,
            "copy": slot.copy,
            "shot_size": slot.shot_size,
            "subtitle": true,
            "material": {
                "asset_id": item.id,
                "thumb": item.thumb,
                "title": item.label(),
            },
        }));
    }
    let body = serde_json::json!({
        "analysis_id": analysis.id,
        "duration": seconds,
        "slots": rows,
        "voice": "zh-CN-XiaoxiaoNeural",
        "bgm": "轻快",
        "title": title.chars().take(40).collect::<String>(),
    })
    .to_string();
    progress(0.35, "提交复刻渲染任务…".to_owned());
    let (status, reply) = crate::account::api_call(
        "POST",
        &endpoint("/replicate/build"),
        "application/json",
        body.as_bytes(),
        Some(token),
        Duration::from_secs(90),
    )?;
    let value = json_reply(status, &reply)?;
    check_ok(&value)?;
    let task_id = value
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "提交成功，但服务器没有给出任务 id".to_owned())?
        .to_owned();
    let estimate = value
        .get("estimated_seconds")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(90.0);
    progress(
        0.4,
        format!("平台渲染复刻成片（预计 {estimate:.0} 秒，可取消）…"),
    );
    let task = poll_task(
        token,
        &format!("/replicate/task/{task_id}"),
        REPLICATE_READ,
        "爆款复刻",
        &mut |fraction, stage| progress(0.4 + fraction * 0.5, stage),
        cancel,
    )?;
    output_of(
        dirs,
        task.video_url(),
        "replicate",
        &task_id,
        task.credits.clone(),
        task.admin_exempt,
        &mut |fraction, detail| progress(0.92 + fraction * 0.07, detail),
        cancel,
    )
}

/// 百分号编码（素材搜索的中文关键词）。
fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_result_url_is_made_absolute() {
        assert_eq!(
            absolute("/ai-nav/studio/files/autoedit/render/a.mp4"),
            "https://xinglaiai.com/ai-nav/studio/files/autoedit/render/a.mp4"
        );
    }

    #[test]
    fn an_estimate_reads_as_the_platform_sends_it() {
        let value = serde_json::json!({
            "ok": true,
            "credits": 2.0,
            "balance": 10.0,
            "enough": true,
            "is_admin": true,
            "admin_exempt": true,
            "items": [{"label": "成片渲染", "credits": 2.0, "quantity": 1}],
        });
        let parse = |value: &serde_json::Value| Estimate {
            credits: value["credits"].as_f64().unwrap_or(0.0),
            lines: vec!["成片渲染 2.0 积分".to_owned()],
            balance: 10.0,
            admin_exempt: value["admin_exempt"].as_bool().unwrap_or(false),
            enough: true,
        };
        let estimate = parse(&value);
        assert!(estimate.display().contains("免扣"));
        assert!(estimate.display().contains("2.0 积分"));
    }

    #[test]
    fn a_task_row_reads_the_credits_it_carries() {
        let value = serde_json::json!({
            "ok": true, "status": "done", "progress": 100, "stage": "完成",
            "url": "/ai-nav/studio/files/autoedit/render/a.mp4", "duration": 15.0,
            "credits": {"charged": 0.0, "status": "admin_exempt", "admin_exempt": true},
        });
        let task = TaskStatus::parse(&value);
        assert_eq!(task.status, "done");
        assert_eq!(task.progress, 1.0);
        assert!(task.credits.contains("免扣"));
        assert!(task.admin_exempt);
        assert_eq!(task.video_url(), "/ai-nav/studio/files/autoedit/render/a.mp4");
    }

    #[test]
    fn a_template_version_gives_its_slots() {
        let analysis = Analysis {
            template: serde_json::json!({
                "duration_versions": {
                    "15": {"slots": [
                        {"role": "hook", "duration": 2.5, "copy": "开场", "shot_size": "近景"},
                        {"role": "body", "duration": 3.0, "copy": "正文", "shot_size": "中景"},
                        {"role": "cta", "duration": 2.0, "copy": "结尾", "shot_size": "全景"}
                    ]}
                }
            }),
            shots: Vec::new(),
            ..Analysis::default()
        };
        assert_eq!(analysis.versions(), vec![15]);
        let slots = analysis.slots(15);
        assert_eq!(slots.len(), 3);
        assert!(slots[0].label().contains("开场钩子"));
        assert_eq!(slots[1].copy, "正文");
    }

    #[test]
    fn a_script_title_can_be_changed_before_rendering() {
        let value = serde_json::json!({
            "ok": true, "script_id": "abc",
            "script": {"title": "旧标题", "pages": [
                {"type": "title", "title": "旧标题", "narration": "开场"},
                {"type": "point", "title": "要点", "narration": "正文"}
            ]},
            "est_duration": 20,
        });
        let mut script = Script::parse(&value, 20.0).unwrap();
        script.rename("新标题");
        assert_eq!(script.title, "新标题");
        assert_eq!(script.raw["title"], "新标题");
        assert_eq!(script.raw["pages"][0]["title"], "新标题");
        assert!(script.pages[0].contains("新标题"));
    }

    #[test]
    fn a_cancelled_poll_stops_at_once() {
        let cancel = AtomicBool::new(true);
        let mut progress = |_: f32, _: String| {};
        let result = poll_task(
            "token",
            "/explainer/task/x",
            Duration::from_secs(1),
            "动态解说",
            &mut progress,
            &cancel,
        );
        assert!(result.unwrap_err().contains("已取消"));
    }

    #[test]
    fn a_cloud_person_reads_as_a_choice() {
        let person = CloudPerson {
            id: "p1".to_owned(),
            name: "文昊".to_owned(),
            ready: true,
            source: "public".to_owned(),
        };
        assert_eq!(person.label(), "公共形象 · 文昊");
    }

    #[test]
    fn a_slot_falls_back_to_the_reference_shots() {
        let analysis = Analysis {
            shots: vec![
                Shot { index: 1, start: 0.0, end: 2.0, duration: 2.0, text: "开场".to_owned(), ..Shot::default() },
                Shot { index: 2, start: 2.0, end: 5.0, duration: 3.0, text: "正文".to_owned(), ..Shot::default() },
            ],
            ..Analysis::default()
        };
        let slots = analysis.slots(15);
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].role, "hook");
        assert_eq!(slots[1].copy, "正文");
    }

    #[test]
    fn a_search_word_is_percent_encoded() {
        assert_eq!(percent_encode("城市 生活"), "%E5%9F%8E%E5%B8%82%20%E7%94%9F%E6%B4%BB");
    }
}
