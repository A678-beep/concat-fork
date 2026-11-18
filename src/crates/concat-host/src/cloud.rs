// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 工程云同步与成片回传：把 Concat 工程保存到星籁平台，或从平台取回。
//!
//! 网络与压缩都在这里：工程正文先 gzip 再 base64（缩小的幅度比省下的
//! 那点带宽更重要，一个 4 分钟工程的 JSON 常有几 MB），用
//! [`crate::account::api_call`] 直连平台 —— Windows 上 OpenSSL、不走系统
//! 代理，平台的 WAF 才放行。
//!
//! 窗口只负责在事件循环线程上发号施令与显示结果；线程、重试与错误话术
//! 都在这里，`project_cloud.rs` 把结果写进 Slint。

use std::io::Write;
use std::time::Duration;

use base64::Engine as _;
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};

/// 连接与读写的等待：上传成片可能有几十 MB，读等待给足。
const READ: Duration = Duration::from_secs(240);

/// 平台接口的根地址，与登录同一份；环境变量 `XINGLAI_API_BASE` 可以换掉。
fn base() -> String {
    std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::account::API_BASE.to_owned())
}

fn endpoint(path: &str) -> String {
    format!("{}{}", base(), path)
}

/// 云端工程列表里的一行。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CloudProject {
    /// 服务端给的工程 id，保存/打开/上传都带着它。
    #[serde(default)]
    pub pid: String,
    /// 工程名，平台上的显示名。
    #[serde(default)]
    pub name: String,
    /// 云端文件大小（gzip 后），字节。
    #[serde(default)]
    pub size: u64,
    /// 第几版；每次覆盖保存 +1。
    #[serde(default)]
    pub version: i64,
    /// 最后更新时间，平台的本机格式 `2026-11-18 17:16:37`。
    #[serde(default)]
    pub updated_at: String,
    /// 这个工程名下已上传的成片/素材数。
    #[serde(default)]
    pub assets: i64,
}

/// 一次保存的结果。
#[derive(Clone, Debug, Default)]
pub struct SavedProject {
    /// 云端工程 id。
    pub pid: String,
    /// 保存后的版本号。
    pub version: i64,
    /// 保存时间，平台的本机格式。
    pub updated_at: String,
    /// 云端文件大小（gzip 后），字节。
    pub size: u64,
    /// 这次是新建（POST）还是覆盖（PUT）。
    pub created: bool,
}

/// 服务器说不行时，翻成一句中文；`hint` 是平台自己的说法，优先用它。
fn refusal(status: u16, text: &str) -> String {
    let hint = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("hint")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    match status {
        0 => "服务器没有给出回复（连接被中途关闭），请稍后重试".to_owned(),
        401 => "登录已失效，请先重新登录星籁账号".to_owned(),
        403 => hint.unwrap_or_else(|| "没有权限访问这个工程".to_owned()),
        404 => hint.unwrap_or_else(|| "云端工程不存在或已被删除".to_owned()),
        413 => "工程或成片超过平台大小上限（工程 20MB，单个文件 200MB）".to_owned(),
        429 => "请求太频繁了，请稍后再试".to_owned(),
        500..=599 => format!("星籁云端服务暂时不可用（HTTP {status}），请稍后再试"),
        _ => hint.unwrap_or_else(|| format!("云端服务返回了 HTTP {status}")),
    }
}

fn json_reply(status: u16, text: &str) -> Result<serde_json::Value, String> {
    if !(200..300).contains(&status) {
        return Err(refusal(status, text));
    }
    serde_json::from_str(text).map_err(|error| format!("服务器的回复不是 JSON：{error}"))
}

/// 工程 JSON → gzip → base64。平台按 `project_json` 收下，解开存盘。
fn packed(project: &serde_json::Value) -> Result<String, String> {
    let bytes = serde_json::to_vec(project).map_err(|error| format!("工程序列化失败：{error}"))?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&bytes)
        .map_err(|error| format!("工程压缩失败：{error}"))?;
    let blob = encoder
        .finish()
        .map_err(|error| format!("工程压缩失败：{error}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(blob))
}

/// 保存一个工程：没有 `pid` 就新建，有就覆盖（服务端版本 +1）。
///
/// `pid` 过期（工程在别处删了）时退回新建，不让人对着 404 发呆。
pub fn save_project(
    token: &str,
    pid: Option<&str>,
    name: &str,
    project: &serde_json::Value,
    meta: Option<&serde_json::Value>,
) -> Result<SavedProject, String> {
    let body = serde_json::json!({
        "name": name,
        "project_json": packed(project)?,
        "meta": meta.cloned().unwrap_or_else(|| serde_json::json!({})),
    })
    .to_string();
    let (mut method, mut path) = match pid {
        Some(pid) if !pid.trim().is_empty() => ("PUT", format!("/projects/{pid}")),
        _ => ("POST", "/projects".to_owned()),
    };
    let (mut status, mut text) = crate::account::api_call(
        method,
        &endpoint(&path),
        "application/json",
        body.as_bytes(),
        Some(token),
        READ,
    )?;
    if method == "PUT" && (status == 404 || status == 405) {
        method = "POST";
        path = "/projects".to_owned();
        (status, text) = crate::account::api_call(
            method,
            &endpoint(&path),
            "application/json",
            body.as_bytes(),
            Some(token),
            READ,
        )?;
    }
    let value = json_reply(status, &text)?;
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
        return Err(value
            .get("hint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("保存到云端失败")
            .to_owned());
    }
    let pid = value
        .get("pid")
        .and_then(serde_json::Value::as_str)
        .filter(|pid| !pid.is_empty())
        .ok_or_else(|| "服务器的回复里没有工程 id".to_owned())?;
    Ok(SavedProject {
        pid: pid.to_owned(),
        version: value
            .get("version")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(1),
        updated_at: value
            .get("updated_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        size: value
            .get("size")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        created: method == "POST",
    })
}

/// 本 workspace 的云端工程，按更新时间从新到旧。
pub fn list_projects(token: &str) -> Result<Vec<CloudProject>, String> {
    let (status, text) = crate::account::api_call(
        "GET",
        &endpoint("/projects"),
        "application/json",
        &[],
        Some(token),
        READ,
    )?;
    let value = json_reply(status, &text)?;
    let rows = value
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut projects = Vec::with_capacity(rows.len());
    for row in rows {
        projects.push(
            serde_json::from_value::<CloudProject>(row)
                .map_err(|error| format!("云端工程列表格式不认识：{error}"))?,
        );
    }
    Ok(projects)
}

/// 读取一个云端工程：工程名与工程 JSON。
pub fn load_project(token: &str, pid: &str) -> Result<(String, serde_json::Value), String> {
    let (status, text) = crate::account::api_call(
        "GET",
        &endpoint(&format!("/projects/{pid}")),
        "application/json",
        &[],
        Some(token),
        READ,
    )?;
    let value = json_reply(status, &text)?;
    let project = value
        .get("project")
        .cloned()
        .filter(|project| project.is_object())
        .ok_or_else(|| "服务器的回复里没有工程数据".to_owned())?;
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("云端工程")
        .to_owned();
    Ok((name, project))
}

/// 文件名里不能进 HTTP 头的东西换掉，中文留给 `filename*` 那一份。
fn ascii_name(name: &str) -> String {
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("成片.mp4")
        .replace(['"', '\r', '\n'], "_");
    let cleaned: String = name
        .chars()
        .map(|ch| if ch.is_ascii() { ch } else { '_' })
        .collect();
    if cleaned.trim().is_empty() || cleaned == "_.mp4" {
        "video.mp4".to_owned()
    } else {
        cleaned
    }
}

/// `filename*=UTF-8''...` 的百分号编码，只留不保留字符。
fn percent(name: &str) -> String {
    let mut out = String::new();
    for byte in name.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn mime_of(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "m4v" => "video/x-m4v",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
}

/// 把一集成片/素材传到工程下，返回公网 URL（平台工作台能看到的那份）。
pub fn upload_asset(
    token: &str,
    pid: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("成片文件是空的，请重新导出".to_owned());
    }
    if bytes.len() > 200 * 1024 * 1024 {
        return Err("成片超过 200MB 上限，请压缩后再上传".to_owned());
    }
    let name = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("成片.mp4")
        .to_owned();
    let boundary = format!("----XingLaiCut{}", std::process::id());
    let mut body = Vec::with_capacity(bytes.len() + 512);
    let head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"; filename*=UTF-8''{}\r\nContent-Type: {}\r\n\r\n",
        ascii_name(&name),
        percent(&name),
        mime_of(&name),
    );
    body.extend_from_slice(head.as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let (status, text) = crate::account::api_call(
        "POST",
        &endpoint(&format!("/projects/{pid}/asset")),
        &format!("multipart/form-data; boundary={boundary}"),
        &body,
        Some(token),
        READ,
    )?;
    let value = json_reply(status, &text)?;
    value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "上传成功，但服务器没有给出公网地址".to_owned())
}

/// 删除一个云端工程。
pub fn delete_project(token: &str, pid: &str) -> Result<(), String> {
    let (status, text) = crate::account::api_call(
        "DELETE",
        &endpoint(&format!("/projects/{pid}")),
        "application/json",
        &[],
        Some(token),
        READ,
    )?;
    json_reply(status, &text).map(|_| ())
}
