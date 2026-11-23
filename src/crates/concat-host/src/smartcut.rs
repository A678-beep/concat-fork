// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 智能粗剪（0.3.2，桌面客户端）：
//!
//! - `POST /smartcut/plan {video_url, requirement, preset, precision}` →
//!   `{plan_id, segments[{start,end,reason,score,role,text}], total_duration,
//!   preview_url, plan{title,cover_text,hook,structure,notes}, stats}`
//! - `POST /smartcut/render {plan_id, subtitle, transition, ratio?}` →
//!   `{task_id, poll_url}`
//! - `GET /smartcut/task/{id}` →
//!   `{status,progress,stage,video_url,full_url,duration,size,error,hint}`
//! - `GET /smartcut/health`、`GET /smartcut/plan/{id}`
//!
//! 上传复用 AI 工作室的 `POST /upload`（[`crate::ai_studio::upload`]）。
//! 后端已上线（2026-11-23 实测：91.6s 口播 18.25s 出 5 段/30.55s，渲染
//! 约 24s）；接口没就绪时把 404/5xx 翻成「接口联调中」+ 具体原因。
//!
//! 桌面不内置 Key，复用星籁账号登录态（`Authorization: Bearer`）。

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// 提交计划的等待：实测 18 秒出计划，给到 2 分钟（高峰排队）。
const PLAN_READ: Duration = Duration::from_secs(120);
/// 一次查询任务的等待。
const TASK_READ: Duration = Duration::from_secs(30);
/// 轮询间隔。
pub const POLL: Duration = Duration::from_secs(2);

/// 平台接口的根地址，与登录、AI 工作室同一份；`XINGLAI_API_BASE` 可换。
fn endpoint(path: &str) -> String {
    let base = std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::account::API_BASE.to_owned());
    format!("{base}{path}")
}

/// 平台直连只在 Windows 栈上接好（见 `account::api_call`）；别的平台由下面
/// 的包装直接说不支持，模块照常编译、面板照常说明。
#[cfg(windows)]
fn api_call(
    method: &str,
    url: &str,
    content_type: &str,
    body: &[u8],
    token: Option<&str>,
    read: Duration,
) -> Result<(u16, String), String> {
    crate::account::api_call(method, url, content_type, body, token, read)
}

#[cfg(not(windows))]
fn api_call(
    _method: &str,
    _url: &str,
    _content_type: &str,
    _body: &[u8],
    _token: Option<&str>,
    _read: Duration,
) -> Result<(u16, String), String> {
    Err("智能粗剪目前只在 Windows 桌面版接入平台。".to_owned())
}
/// 一个粗剪片段。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Segment {
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub end: f64,
    /// 留下它的理由。
    #[serde(default)]
    pub reason: String,
    /// 它在成片里的角色（钩子 / 主体 / 收尾…）。
    #[serde(default)]
    pub role: String,
    /// 这段里的台词或口播。
    #[serde(default)]
    pub text: String,
    /// 平台给的分数，0..1。
    #[serde(default)]
    pub score: f64,
}

/// 一次拆解出来的粗剪计划。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub plan_id: String,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub total_duration: f64,
    #[serde(default)]
    pub preview_url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub hook: String,
    /// 「5 段 / 30.6 秒 · 成本约 0.012 元」这样的回执行。
    #[serde(default)]
    pub stats_line: String,
}

/// 一次渲染任务的回执。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RenderStarted {
    #[serde(default)]
    pub task_id: String,
}

/// 渲染任务的状态。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskStatus {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub stage: String,
    /// 成片地址（平台）。
    #[serde(default)]
    pub video_url: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub size: f64,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub hint: String,
}

impl TaskStatus {
    /// 跑完了。
    pub fn done(&self) -> bool {
        matches!(
            self.status.to_ascii_lowercase().as_str(),
            "done" | "success" | "succeeded" | "finished" | "complete" | "completed"
        )
    }

    /// 失败了，还是平台翻出来的中文原因。
    pub fn failed(&self) -> bool {
        matches!(
            self.status.to_ascii_lowercase().as_str(),
            "error" | "failed" | "failure" | "cancel" | "cancelled"
        )
    }
}

/// 把平台的人话错误摘成一句。
fn detail(text: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        for key in ["detail", "error", "message", "hint"] {
            if let Some(found) = value.get(key).and_then(serde_json::Value::as_str) {
                let found = found.trim();
                if !found.is_empty() {
                    return found.to_owned();
                }
            }
        }
    }
    let text = text.trim();
    if text.is_empty() {
        return "平台没有返回内容".to_owned();
    }
    text.chars().take(200).collect()
}

/// 「接口还没就绪」的说法：状态码 + 一句话。
fn not_ready(status: u16, text: &str) -> String {
    match status {
        0 => format!("接口联调中（平台未返回）。{}", detail(text)),
        404 | 405 => format!("接口联调中（`/api/smartcut/*` 尚未就绪，HTTP {status}）。"),
        401 | 403 => format!("接口联调中（登录态被平台拒绝，HTTP {status}）；请重新登录星籁账号后重试。"),
        status if status >= 500 => {
            format!("接口联调中（平台服务暂不可用，HTTP {status}）。{}", detail(text))
        }
        429 => "平台排队中（HTTP 429）：高峰 2C2G 会限流，等一分钟再试，或改用 AI 工作室成片。".to_owned(),
        status => format!("接口联调中（HTTP {status}）。{}", detail(text)),
    }
}

fn post_json(path: &str, body: &serde_json::Value, token: &str, read: Duration) -> Result<serde_json::Value, String> {
    let encoded = serde_json::to_vec(body).map_err(|error| error.to_string())?;
    let (status, text) = api_call(
        "POST",
        &endpoint(path),
        "application/json",
        &encoded,
        Some(token),
        read,
    )?;
    if !(200..300).contains(&status) {
        return Err(not_ready(status, &text));
    }
    serde_json::from_str(&text).map_err(|_| format!("平台回复不是 JSON：{}", detail(&text)))
}

fn get_json(path: &str, token: &str, read: Duration) -> Result<serde_json::Value, String> {
    let (status, text) = api_call(
        "GET",
        &endpoint(path),
        "application/json",
        &[],
        Some(token),
        read,
    )?;
    if !(200..300).contains(&status) {
        return Err(not_ready(status, &text));
    }
    serde_json::from_str(&text).map_err(|_| format!("平台回复不是 JSON：{}", detail(&text)))
}

fn number(value: &serde_json::Value, keys: &[&str]) -> f64 {
    for key in keys {
        if let Some(found) = value.get(*key).and_then(serde_json::Value::as_f64) {
            return found;
        }
    }
    0.0
}

fn text_of(value: &serde_json::Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(found) = value.get(*key).and_then(serde_json::Value::as_str) {
            return found.to_owned();
        }
    }
    String::new()
}

/// 上传本地视频，返回平台地址（复用 `/upload`）。`progress` 报得粗：0.1 上传
/// 中，0.4 已上传。200MB 上限与 AI 工作室同一份（平台 `/upload` 的硬上限）。
pub fn upload(
    token: &str,
    path: &Path,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<String, String> {
    let size = std::fs::metadata(path).map_or(0, |meta| meta.len());
    if size > 200 * 1024 * 1024 {
        return Err(format!(
            "视频 {:.1} MB，超过平台 200MB 上限：请先裁剪到 3 分钟以内再试",
            size as f64 / (1024.0 * 1024.0)
        ));
    }
    let url = crate::ai_studio::upload(token, path, progress, cancel)?;
    progress(0.4, "视频已上传，平台拆解镜头…".to_owned());
    Ok(url)
}

/// 提交一次粗剪计划。`preset` 是平台约定的六种之一（通用 / 口播片 / 配音解说 /
/// 长视频切片 / VLOG记录 / 混剪），`precision` 是 standard / high。
pub fn plan(
    token: &str,
    video_url: &str,
    requirement: &str,
    preset: &str,
    precision: &str,
) -> Result<Plan, String> {
    let body = serde_json::json!({
        "video_url": video_url,
        "requirement": requirement,
        "preset": preset,
        "precision": precision,
    });
    let began = Instant::now();
    let value = post_json("/smartcut/plan", &body, token, PLAN_READ)?;
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str)
        && !error.trim().is_empty()
    {
        return Err(format!("平台没能拆出计划：{}", detail(&value.to_string())));
    }
    let mut out = Plan {
        plan_id: text_of(&value, &["plan_id", "planId", "id"]),
        total_duration: number(&value, &["total_duration", "totalDuration", "duration"]),
        preview_url: text_of(&value, &["preview_url", "previewUrl"]),
        ..Default::default()
    };
    if let Some(plan) = value.get("plan") {
        out.title = text_of(plan, &["title"]);
        out.hook = text_of(plan, &["hook"]);
    }
    if let Some(rows) = value.get("segments").and_then(serde_json::Value::as_array) {
        for row in rows {
            out.segments.push(Segment {
                start: number(row, &["start"]),
                end: number(row, &["end"]),
                reason: text_of(row, &["reason"]),
                role: text_of(row, &["role"]),
                text: text_of(row, &["text"]),
                score: number(row, &["score"]),
            });
        }
    }
    if out.total_duration <= 0.0 {
        out.total_duration = out
            .segments
            .iter()
            .map(|segment| (segment.end - segment.start).max(0.0))
            .sum();
    }
    let elapsed = began.elapsed().as_secs_f64();
    let cost = number(&value, &["cost_cny", "costCny"]);
    out.stats_line = format!(
        "{} 段 · {:.1} 秒 · {:.1}s 出计划{}",
        out.segments.len(),
        out.total_duration,
        elapsed,
        if cost > 0.0 {
            format!(" · 平台成本约 {cost:.4} 元")
        } else {
            String::new()
        }
    );
    Ok(out)
}

/// 开始渲染计划。`subtitle` 是 burn / none，`transition` 是 none / fade。
pub fn render(token: &str, plan_id: &str, ratio: &str) -> Result<RenderStarted, String> {
    let body = serde_json::json!({
        "plan_id": plan_id,
        "subtitle": "burn",
        "transition": "none",
        "ratio": ratio,
    });
    let value = post_json("/smartcut/render", &body, token, PLAN_READ)?;
    Ok(RenderStarted {
        task_id: text_of(&value, &["task_id", "taskId", "id"]),
    })
}

/// 查询渲染任务一次。
pub fn task(token: &str, task_id: &str) -> Result<TaskStatus, String> {
    let value = get_json(&format!("/smartcut/task/{task_id}"), token, TASK_READ)?;
    Ok(TaskStatus {
        status: text_of(&value, &["status"]),
        progress: {
            let progress = number(&value, &["progress"]);
            if progress > 1.5 { progress / 100.0 } else { progress }
        },
        stage: text_of(&value, &["stage", "message"]),
        video_url: text_of(&value, &["video_url", "videoUrl", "full_url", "fullUrl"]),
        duration: number(&value, &["duration"]),
        size: number(&value, &["size"]),
        error: text_of(&value, &["error"]),
        hint: text_of(&value, &["hint"]),
    })
}

/// 等到一次渲染结束：轮询 `task`，每步报告一次。`cancel` 置位就放弃等待。
pub fn wait_render(
    token: &str,
    task_id: &str,
    mut report: impl FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<TaskStatus, String> {
    let mut misses = 0u32;
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("已取消等待（平台上的任务可能仍在后台跑）".to_owned());
        }
        match task(token, task_id) {
            Ok(status) => {
                misses = 0;
                report(
                    status.progress.clamp(0.0, 1.0) as f32,
                    if status.stage.is_empty() {
                        "渲染中…".to_owned()
                    } else {
                        status.stage.clone()
                    },
                );
                if status.done() {
                    if status.video_url.is_empty() {
                        return Err(format!(
                            "渲染完成但平台没有给成片地址{}",
                            if status.hint.is_empty() {
                                String::new()
                            } else {
                                format!("：{}", status.hint)
                            }
                        ));
                    }
                    return Ok(status);
                }
                if status.failed() {
                    let reason = if !status.error.is_empty() {
                        status.error.clone()
                    } else if !status.hint.is_empty() {
                        status.hint.clone()
                    } else {
                        "平台没有给原因".to_owned()
                    };
                    return Err(format!("渲染失败：{reason}"));
                }
            }
            Err(error) => {
                misses += 1;
                if misses >= 45 {
                    return Err(format!("渲染任务查不到了：{error}"));
                }
            }
        }
        std::thread::sleep(POLL);
    }
}

/// 健康检查（后台联调时给面板探活一次）。
pub fn health(token: Option<&str>) -> bool {
    let Some(token) = token else { return false };
    get_json("/smartcut/health", token, Duration::from_secs(8)).is_ok()
}

/// 把渲染好的成片下载到 AI 输出目录（`ai-output/`，与 AI 工作室同一处）。
/// 返回落盘路径。`url` 可以是平台给的相对地址。
#[cfg(windows)]
pub fn download(
    dirs: &crate::AppDirs,
    token: &str,
    url: &str,
    cancel: &AtomicBool,
) -> Result<std::path::PathBuf, String> {
    let dir = crate::ai_studio::output_dir(dirs);
    std::fs::create_dir_all(&dir).map_err(|error| format!("建不了 ai-output 目录：{error}"))?;
    let stamp = crate::ai_studio::stamp_ms();
    let dest = dir.join(format!("smartcut-{stamp}.mp4"));
    let url = crate::ai_studio::absolute(url);
    let bytes = crate::account::api_download(
        &url,
        Some(token),
        &dest,
        Duration::from_secs(15 * 60),
        cancel,
    )?;
    if bytes < 30_000 {
        let _ = std::fs::remove_file(&dest);
        return Err("下载到的成片不完整（文件太小），请重试".to_owned());
    }
    Ok(dest)
}

/// 非 Windows：见 [`api_call`] 的说明。
#[cfg(not(windows))]
pub fn download(
    _dirs: &crate::AppDirs,
    _token: &str,
    _url: &str,
    _cancel: &AtomicBool,
) -> Result<std::path::PathBuf, String> {
    Err("智能粗剪目前只在 Windows 桌面版接入平台。".to_owned())
}
