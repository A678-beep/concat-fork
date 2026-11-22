// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 剪映助手同款的常驻 AI 助手（0.3.1，桌面简版）。
//!
//! 走平台 `/xiaolai/chat` 的文本模式（与网页小来同一份对话能力）：发一句话，
//! 读 `reply_text`。桌面端**只显示回复与建议**，不替用户自动改时间线；回复里
//! 平台若顺手触发了工具任务（小来会按意图自动执行），这里把工具名与成本如实
//! 带给界面，由人去 AI 工作室查看。
//!
//! 不登录也能问（平台匿名口径会给普通回答）；登录后按账号计费。

use std::time::Duration;

/// 一次对话的回复。
#[derive(Clone, Debug, Default)]
pub struct Reply {
    /// 小来说的话。
    pub text: String,
    /// 平台顺手执行的工具（`none` / `autoedit` / `asset_search` ...）。
    pub tool: String,
    /// 平台的成本估算（元），没有就是空。
    pub cost: String,
    /// 会话 id（平台跨轮上下文用）。
    pub session: String,
    /// 一句人话提醒：工具触发时告诉用户去哪儿看成片。
    pub note: String,
}

/// 平台给的耗时/计费是数字，界面只要一句话。
fn money(value: Option<f64>) -> String {
    match value {
        Some(cost) if cost > 0.0 => format!("平台本次成本约 {cost:.3} 元"),
        _ => String::new(),
    }
}

/// `POST /xiaolai/chat {text, session_id}` —— 文本模式。`session_id` 由调用方
/// 固定成本窗口期的同一个串（换窗口就换会话也行，平台自己兜底）。
pub fn chat(token: Option<&str>, text: &str, session_id: &str) -> Result<Reply, String> {
    let body = serde_json::json!({
        "text": text,
        "session_id": session_id,
        "mode": "text",
    })
    .to_string();
    let base = std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::account::API_BASE.to_owned());
    let url = format!("{base}/xiaolai/chat");
    let (status, reply) = crate::account::api_call(
        "POST",
        &url,
        "application/json",
        body.as_bytes(),
        token,
        Duration::from_secs(75),
    )?;
    if !(200..300).contains(&status) {
        let value = serde_json::from_str::<serde_json::Value>(&reply).ok();
        let hint = value
            .as_ref()
            .and_then(|value| value.get("hint"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                value
                    .as_ref()
                    .and_then(|value| value.get("error"))
                    .and_then(serde_json::Value::as_str)
            });
        let message = match status {
            0 => "连不上小来（网络或服务暂时不可用），稍后再试".to_owned(),
            401 => "登录已失效：重新登录后小来可用".to_owned(),
            429 => "小来有点忙（请求太频繁），过一会儿再问".to_owned(),
            500..=599 => "小来正在维护（服务端暂时不可用），稍后再问".to_owned(),
            _ => hint
                .map(str::to_owned)
                .unwrap_or_else(|| format!("小来没有回答（HTTP {status}）")),
        };
        return Err(message);
    }
    let value = serde_json::from_str::<serde_json::Value>(&reply)
        .map_err(|error| format!("小来的回复不是 JSON：{error}"))?;
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
        let hint = value
            .get("hint")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("error").and_then(serde_json::Value::as_str))
            .unwrap_or("小来这次没接上，再问一次");
        return Err(hint.to_owned());
    }
    let tool = value
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none")
        .to_owned();
    let mut note = String::new();
    if value
        .get("tool")
        .and_then(|tool| tool.get("ok"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        note = "小来按对话顺手在平台上建了一个任务：桌面端只显示回复，成片请到 AI 工作室查看；\
不确定要不要花积分时，先在小来里问清楚再让它执行。"
            .to_owned();
    }
    Ok(Reply {
        text: value
            .get("reply_text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("嗯，我在。")
            .trim()
            .to_owned(),
        tool: if tool == "none" { String::new() } else { tool },
        cost: money(value.get("cost_est_cny").and_then(serde_json::Value::as_f64)),
        session: value
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(session_id)
            .to_owned(),
        note,
    })
}
