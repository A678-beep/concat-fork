// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 星籁电影 / 剧集制作向导（0.3.5）：桌面端对接平台的两条生成链。
//!
//! - 电影模式（`/api/movie/*`）：9 类影视预设、从一句话 premise 生成分镜方案
//!   （`/movie/plan`，不产生模型费用）、电影化成片渲染（`/movie/render`，
//!   2.39:1 遮幅 / LUT / 声音设计 / 片头尾+演职人员表 / QC / 工程包）。
//! - AI 导演编排器（`/api/director/*`）：按镜头路由模型、逐镜进度、断点续跑，
//!   专业模式「改 1 个角色 / 1 个分镜只重跑受影响镜头」就靠 `shot_nos` 续跑。
//!
//! 登录态与 AI 工作室同一份（`Authorization: Bearer`，DPAPI 本机加密）；
//! 平台接口走 `account::api_call` 的 OpenSSL 直连（Aliyun WAF 只认 OpenSSL
//! 形状的 ClientHello，见 docs/REBRAND.md 第 4 节），不走系统代理。
//! 这里只发请求与拆 JSON，任何编辑决定都在窗口那一半（`panes::movie`）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

/// 星籁平台网页入口（桌面为主，网页仅备用）。
pub const SITE: &str = "https://xinglaiai.com";

/// 平台接口根地址（与登录同一份，`XINGLAI_API_BASE` 可换）。
fn endpoint(path: &str) -> String {
    let base = std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::account::API_BASE.to_owned());
    format!("{base}{path}")
}

/// 规划类读等待（分镜方案要过一次大模型，通常 10–40 秒）。
const PLAN_READ: Duration = Duration::from_secs(180);
/// 短请求（类型表 / 任务轮询）。
const LIST_READ: Duration = Duration::from_secs(30);

/// 一句合规声明（与 AI 短剧同一句式）。
pub const COMPLIANCE: &str = "我确认：本片使用的素材与人物形象为本人所有或已获得合法授权，遵守平台与法律法规；\
AI 生成的剧本、画面与配音会带「AI 生成」标注，剧情为虚构、不影射真实人物与事件。";

/// 平台的硬限制（照 `/movie/health` 契约，不臆造）。
pub const LIMITS: &str = "平台限制：单任务最多 12 镜、每镜 ≤10 秒、总时长 ≤180 秒；\
2C2G 单机串行，30–60 秒短片（3–6 镜）约 8–20 分钟；渲染任务排队时更久。";

/// 一个影视类型（`GET /movie/types` 的一行）。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct MovieType {
    /// 机器名：film / series / ad …
    #[serde(default)]
    pub id: String,
    /// 中文名。
    #[serde(default)]
    pub label: String,
    /// 分组。
    #[serde(default)]
    pub group: String,
    /// 一句话说明。
    #[serde(default)]
    pub desc: String,
    /// 默认画幅。
    #[serde(default)]
    pub aspect: String,
    /// 默认帧率。
    #[serde(default)]
    pub fps_default: f64,
    /// 默认响度（LUFS）。
    #[serde(default)]
    pub loudness_default: f64,
}

/// 当前登录的 token（没登录就没有）。
pub fn token() -> Option<String> {
    crate::account::load()
        .filter(|account| !account.token.trim().is_empty())
        .map(|account| account.token)
}

/// 是否登录（面板常驻提示用）。
pub fn signed_in() -> bool {
    token().is_some()
}

/// 是否内部（管理员）账号免扣。
pub fn admin() -> bool {
    crate::account::load().is_some_and(|account| account.is_admin)
}

/// 一次 JSON 调用；非 2xx 时把平台的 `hint`/`error` 翻成人话。
fn call(method: &str, path: &str, body: Option<&Value>, read: Duration) -> Result<Value, String> {
    let token = token();
    let payload = match body {
        Some(value) => serde_json::to_vec(value).map_err(|error| format!("请求编码失败：{error}"))?,
        None => Vec::new(),
    };
    let content_type = if body.is_some() {
        "application/json"
    } else {
        "application/json"
    };
    let (status, text) = crate::account::api_call(
        method,
        &endpoint(path),
        content_type,
        &payload,
        token.as_deref(),
        read,
    )
    .map_err(|error| format!("连不上星籁平台（{path}）：{error}"))?;
    let value = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| {
        serde_json::json!({"ok": false, "error": "bad_response", "hint": text.chars().take(160).collect::<String>()})
    });
    if !(200..300).contains(&status) {
        let hint = value
            .get("hint")
            .and_then(Value::as_str)
            .or_else(|| value.get("error").and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("平台返回 HTTP {status}"));
        if status == 401 {
            return Err(format!("请先登录星籁账号：{hint}"));
        }
        if status == 429 {
            return Err(format!("平台忙（429）：{hint}"));
        }
        return Err(format!("平台拒绝了这次请求（HTTP {status}）：{hint}"));
    }
    Ok(value)
}

fn get(path: &str, read: Duration) -> Result<Value, String> {
    call("GET", path, None, read)
}

fn post(path: &str, body: &Value, read: Duration) -> Result<Value, String> {
    call("POST", path, Some(body), read)
}

/// 9 类影视预设（类型表是公开的，不要求登录）。
pub fn types() -> Result<Vec<MovieType>, String> {
    let value = get("/movie/types", LIST_READ)?;
    let rows = value
        .get("types")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for row in rows {
        if let Ok(item) = serde_json::from_value::<MovieType>(row) {
            out.push(item);
        }
    }
    if out.is_empty() {
        return Err("平台的类型表是空的（/movie/types）".to_owned());
    }
    Ok(out)
}

/// 电影模式分镜方案：一句话 premise（`topic`）或明确的分镜脚本文本都能过。
///
/// 入参常用字段：`type`、`tier`（standard/pro/master）、`budget_cny`、
/// `topic`（一句话前提）或 `script`（{title, shots:[{scene,dur,line}]}）。
/// 这一步只做规划，不调用生图/生视频，不产生模型费用。
pub fn plan(body: &Value) -> Result<Value, String> {
    let value = post("/movie/plan", body, PLAN_READ)?;
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(api_error(&value, "分镜方案没生成出来"));
    }
    Ok(value)
}

/// 电影模式渲染任务（异步）：返回 task_id。
pub fn render(body: &Value) -> Result<Value, String> {
    let value = post("/movie/render", body, PLAN_READ)?;
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(api_error(&value, "渲染任务没提交成功"));
    }
    Ok(value)
}

/// 电影模式任务：`GET /movie/task/{id}`。
pub fn task(id: &str) -> Result<Value, String> {
    get(&format!("/movie/task/{id}"), LIST_READ)
}

/// 电影模式任务列表。
pub fn tasks(limit: u32) -> Result<Value, String> {
    get(&format!("/movie/tasks?limit={}", limit.max(1).min(50)), LIST_READ)
}

/// 导演编排器规划（零成本，不调用厂商）。
pub fn director_plan(body: &Value) -> Result<Value, String> {
    let value = post("/director/plan", body, PLAN_READ)?;
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(api_error(&value, "导演编排计划没生成出来"));
    }
    Ok(value)
}

/// 导演编排器执行任务（异步）：返回 task 摘要（含 task_id）。
pub fn director_run(body: &Value) -> Result<Value, String> {
    let value = post("/director/run", body, PLAN_READ)?;
    Ok(value)
}

/// 断点续跑 / 逐镜重生成：只重跑 `shot_nos` 里的镜头，其余镜头复用原任务产物，
/// 再重新合成成片与工程包——专业模式的 DAG 就落在这一条上。
pub fn director_resume(resume_id: &str, shot_nos: &[u32], budget_cny: Option<f64>) -> Result<Value, String> {
    let mut body = serde_json::json!({
        "resume_task_id": resume_id,
        "shot_nos": shot_nos,
    });
    if let Some(budget) = budget_cny {
        body["budget_cny"] = serde_json::json!(budget);
    }
    let value = post("/director/run", &body, PLAN_READ)?;
    Ok(value)
}

/// 导演编排器任务：`GET /director/task/{id}`。
pub fn director_task(id: &str) -> Result<Value, String> {
    get(&format!("/director/task/{id}"), LIST_READ)
}

/// 平台给出的错误一句话。
pub fn api_error(value: &Value, fallback: &str) -> String {
    value
        .get("hint")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
        .map(str::to_owned)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

/// 下载成片 / 工程包到本地（走同一个 OpenSSL 直连）。
pub fn download(url: &str, dest: &std::path::Path, cancel: &AtomicBool) -> Result<u64, String> {
    if url.trim().is_empty() {
        return Err("没有可下载的地址".to_owned());
    }
    let token = token();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("建不了下载目录：{error}"))?;
    }
    crate::account::api_download(url, token.as_deref(), dest, PLAN_READ, cancel)
        .map_err(|error| format!("下载失败：{error}"))
}

/// 一句话把任务状态翻成中文（面板不读英文状态）。
pub fn status_text(status: &str, stage: &str) -> String {
    if !stage.trim().is_empty() {
        return stage.trim().to_owned();
    }
    match status {
        "queued" => "排队中".to_owned(),
        "running" => "生成中".to_owned(),
        "done" => "完成".to_owned(),
        "failed" => "失败".to_owned(),
        "partial" => "部分完成".to_owned(),
        "aborted_budget" => "预算中止".to_owned(),
        "interrupted" => "服务重启中断（可续跑）".to_owned(),
        "cancelled" => "已取消".to_owned(),
        other => other.to_owned(),
    }
}

/// 取消标志的进程内小工具：调用方持 [`std::sync::Arc`]，下载/轮询各自看它。
pub fn cancel_flag() -> std::sync::Arc<AtomicBool> {
    std::sync::Arc::new(AtomicBool::new(false))
}

/// 把取消标志置位（幂等）。
pub fn cancel(cancel: &AtomicBool) {
    cancel.store(true, Ordering::Relaxed);
}

/// 任务的短 id（日志里用，报错给人看的是完整 id）。
pub fn short_id(id: &str) -> String {
    id.chars().take(18).collect()
}
