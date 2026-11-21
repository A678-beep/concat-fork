// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai AI and Concat contributors

//! 星籁免费商用素材库：搜索、行业列表、缩略图、下载与授权说明。
//!
//! 平台接口（2026-11 生产）：
//!
//! - `GET /assets/search?q=&type=&industry=&page=&limit=` →
//!   `{items:[{id,type,title,duration,width,height,thumb_url,license{...},
//!   attribution,url/...}],total,page,limit}`
//! - `GET /assets/industries`、`GET /assets/categories`
//! - `GET /assets/{id}/download` → 302 到 OSS 签名地址（直接跟随下载）
//! - `GET /assets/{id}/thumb` → 缩略图
//! - `GET /assets/{id}/license?format=text` → 授权说明
//!
//! 素材全部可商用；CC BY 一类需要署名，卡片上显示 `attribution`。下载与
//! 缩略图都落在本机的 `asset-cache/`，不重复下载。
//!
//! 网络走登录同一套 OpenSSL 直连（平台的 WAF 只认这个形状的 ClientHello），
//! 下载走 [`crate::direct`]（跟随 302、边下边报进度）。

use std::path::{Path, PathBuf};

use crate::account::API_BASE;
use crate::AppDirs;

/// 素材库接口的根地址，与账号接口同一个平台前缀。
pub fn api(path: &str) -> String {
    let base = std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| API_BASE.to_owned());
    format!("{base}{path}")
}

/// 站点根；缩略图等相对地址补全用。
const SITE: &str = "https://xinglaiai.com";

fn absolute(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        url.to_owned()
    } else if url.starts_with('/') {
        format!("{SITE}{url}")
    } else {
        format!("{SITE}/{url}")
    }
}

/// 本机素材缓存目录：`%APPDATA%\app.concat.editor\asset-cache`。
pub fn cache_dir(dirs: &AppDirs) -> PathBuf {
    dirs.config.join("asset-cache")
}

/// 缩略图缓存目录。
fn thumbs_dir(dirs: &AppDirs) -> PathBuf {
    cache_dir(dirs).join("thumbs")
}

/// 一个可商用素材的授权。
#[derive(Clone, Debug, Default)]
pub struct License {
    pub code: String,
    pub name: String,
    pub commercial_ok: bool,
    pub attribution_required: bool,
}

/// 一条素材。
#[derive(Clone, Debug, Default)]
pub struct AssetItem {
    pub id: String,
    /// video / image / music / sfx / font
    pub kind: String,
    pub title: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub thumb_url: String,
    pub license: License,
    /// 需要署名时署给谁。
    pub attribution: String,
    /// 素材文件地址（按类型的 url 字段之一），下载前用。
    pub file_url: String,
    /// 行业名，平台给的。
    pub industry: String,
    pub category: String,
}

/// 一页搜索结果。
#[derive(Clone, Debug, Default)]
pub struct AssetPage {
    pub items: Vec<AssetItem>,
    pub total: i64,
    pub page: u32,
    pub limit: u32,
}

/// 一个行业，带素材数。
#[derive(Clone, Debug, Default)]
pub struct Industry {
    pub name: String,
    pub count: i64,
}

fn int_of(value: &serde_json::Value, key: &str) -> u32 {
    let raw = value.get(key).cloned().unwrap_or(serde_json::Value::Null);
    match raw {
        serde_json::Value::Number(number) => number.as_u64().unwrap_or(0) as u32,
        serde_json::Value::String(text) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn get(path: &str) -> Result<serde_json::Value, String> {
    let (status, text) = crate::account::api_call(
        "GET",
        &api(path),
        "application/json",
        &[],
        None,
        std::time::Duration::from_secs(20),
    )?;
    if !(200..300).contains(&status) {
        let hint = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("hint")
                    .or_else(|| value.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
        return Err(hint.unwrap_or_else(|| format!("素材库接口返回 HTTP {status}")));
    }
    serde_json::from_str(&text).map_err(|error| format!("素材库的回复不是 JSON：{error}"))
}

/// 把平台的 JSON 读成一条素材；认不出的条目跳过。
fn item_of(value: &serde_json::Value) -> Option<AssetItem> {
    // 平台 id 可能是字符串也可能是数字；数字也要接。
    let id = match value.get("id").or_else(|| value.get("asset_id"))? {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) => number.to_string(),
        _ => return None,
    };
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let number = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0)
    };
    // 授权可能是对象，也可能只是一个字符串（"CC0"、"CC BY 4.0"）。
    let license_value = value
        .get("license")
        .or_else(|| value.get("license_code"))
        .or_else(|| value.get("license_name"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let license = match &license_value {
        serde_json::Value::String(text) => {
            let upper = text.to_ascii_uppercase();
            License {
                code: text.clone(),
                name: text.clone(),
                commercial_ok: !upper.contains("NC") && !text.contains("非商用"),
                attribution_required: upper.contains("BY"),
            }
        }
        _ => License {
            code: license_value
                .get("code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            name: license_value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            commercial_ok: license_value
                .get("commercial_ok")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            attribution_required: license_value
                .get("attribution_required")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        },
    };
    // 文件地址：平台按类型给不同的字段名，取第一个非空的。
    let file_url = [
        "video_url",
        "image_url",
        "music_url",
        "audio_url",
        "sfx_url",
        "font_url",
        "file_url",
        "url",
        "download_url",
    ]
    .iter()
    .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
    .unwrap_or_default()
    .to_owned();
    let kind = if !text("type").is_empty() {
        text("type").to_lowercase()
    } else if !text("kind").is_empty() {
        text("kind").to_lowercase()
    } else {
        text("asset_type").to_lowercase()
    };
    let thumb_url = if !text("thumb_url").is_empty() {
        text("thumb_url")
    } else if !text("thumb").is_empty() {
        text("thumb")
    } else if !text("cover_url").is_empty() {
        text("cover_url")
    } else {
        text("cover")
    };
    let duration = {
        let raw = value.get("duration").cloned().unwrap_or(serde_json::Value::Null);
        match raw {
            serde_json::Value::Number(number) => number.as_f64().unwrap_or(0.0),
            serde_json::Value::String(text) => {
                let mut parts = text.split(':').filter_map(|part| part.trim().parse::<f64>().ok());
                match (parts.next(), parts.next(), parts.next()) {
                    (Some(h), Some(m), Some(s)) => h * 3600.0 + m * 60.0 + s,
                    (Some(m), Some(s), None) => m * 60.0 + s,
                    (Some(s), None, None) => s,
                    _ => 0.0,
                }
            }
            _ => 0.0,
        }
    };
    Some(AssetItem {
        id,
        kind,
        title: if !text("title").is_empty() {
            text("title")
        } else {
            text("name")
        },
        duration,
        width: int_of(value, "width"),
        height: int_of(value, "height"),
        thumb_url,
        license,
        attribution: text("attribution"),
        file_url,
        industry: text("industry"),
        category: text("category"),
    })
}

/// 搜索素材。`kind` 为空是不限类型；`industry` 为空是不限行业。
pub fn search(
    query: &str,
    kind: &str,
    industry: &str,
    category: &str,
    page: u32,
    limit: u32,
) -> Result<AssetPage, String> {
    let mut path = format!(
        "/assets/search?page={}&limit={}",
        page.max(1),
        limit.clamp(1, 60)
    );
    let mut encode = |key: &str, value: &str| {
        if !value.trim().is_empty() {
            path.push('&');
            path.push_str(key);
            path.push('=');
            path.push_str(&urlencode(value.trim()));
        }
    };
    encode("q", query);
    encode("type", kind);
    encode("industry", industry);
    encode("category", category);
    let value = get(&path)?;
    let items_value = ["items", "list", "rows", "results"]
        .iter()
        .find_map(|key| value.get(*key).cloned())
        .or_else(|| {
            ["items", "list", "rows", "results"]
                .iter()
                .find_map(|key| value.get("data")?.get(*key).cloned())
        })
        .or_else(|| value.get("data").filter(|data| data.is_array()).cloned())
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let items: Vec<AssetItem> = items_value
        .as_array()
        .map(|rows| rows.iter().filter_map(item_of).collect())
        .unwrap_or_default();
    let total = ["total", "count", "total_count"]
        .iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_i64))
        .or_else(|| {
            ["total", "count", "total_count"]
                .iter()
                .find_map(|key| value.get("data")?.get(*key)?.as_i64())
        })
        .unwrap_or(items.len() as i64);
    Ok(AssetPage {
        items,
        total,
        page: value
            .get("page")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(u64::from(page)) as u32,
        limit: value
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(u64::from(limit)) as u32,
    })
}

/// 行业列表（含素材数）。
pub fn industries() -> Result<Vec<Industry>, String> {
    let value = get("/assets/industries")?;
    let rows = value
        .get("industries")
        .or_else(|| value.get("list"))
        .or_else(|| value.get("data"))
        .or_else(|| value.get("data").and_then(|data| data.get("industries")))
        .or_else(|| value.get("data").and_then(|data| data.get("list")))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for row in rows {
        if let Some(name) = row.as_str() {
            out.push(Industry {
                name: name.to_owned(),
                count: 0,
            });
            continue;
        }
        let Some(name) = row
            .get("name")
            .or_else(|| row.get("industry"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        out.push(Industry {
            name: name.to_owned(),
            count: row
                .get("count")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
        });
    }
    Ok(out)
}

/// 分类列表（字符串，平台给什么用什么）。
pub fn categories() -> Result<Vec<String>, String> {
    let value = get("/assets/categories")?;
    let rows = value
        .get("categories")
        .or_else(|| value.get("list"))
        .or_else(|| value.get("data"))
        .or_else(|| value.get("data").and_then(|data| data.get("categories")))
        .or_else(|| value.get("data").and_then(|data| data.get("list")))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for row in rows {
        if let Some(name) = row.as_str() {
            out.push(name.to_owned());
            continue;
        }
        if let Some(name) = row
            .get("name")
            .or_else(|| row.get("category"))
            .and_then(serde_json::Value::as_str)
        {
            out.push(name.to_owned());
        }
    }
    Ok(out)
}

/// 一条素材的授权说明正文。
pub fn license_text(id: &str) -> Result<String, String> {
    let value = get(&format!("/assets/{id}/license?format=text"))?;
    if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
        return Ok(text.to_owned());
    }
    if let Some(text) = value.get("license_text").and_then(serde_json::Value::as_str) {
        return Ok(text.to_owned());
    }
    Ok(value.to_string())
}

fn extension_of(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else if bytes.starts_with(b"\x89PNG") {
        "png"
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else {
        "bin"
    }
}

/// 缩略图落盘：已缓存就返回路径，否则从 `/assets/{id}/thumb` 下载。
pub fn thumb(dirs: &AppDirs, item: &AssetItem) -> Result<PathBuf, String> {
    let dir = thumbs_dir(dirs);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("建不了缩略图缓存：{error}"))?;
    for extension in ["jpg", "png", "webp", "gif"] {
        let path = dir.join(format!("{}.{extension}", safe(&item.id)));
        if path.is_file() {
            return Ok(path);
        }
    }
    let url = if item.thumb_url.trim().is_empty() {
        api(&format!("/assets/{}/thumb", item.id))
    } else {
        absolute(&item.thumb_url)
    };
    let temporary = dir.join(format!("{}.part", safe(&item.id)));
    let no_cancel = std::sync::atomic::AtomicBool::new(false);
    crate::direct::download(&url, &temporary, &mut |_, _| {}, &no_cancel)?;
    let bytes = std::fs::read(&temporary).map_err(|error| format!("读不回缩略图：{error}"))?;
    let path = dir.join(format!("{}.{}", safe(&item.id), extension_of(&bytes)));
    std::fs::rename(&temporary, &path).map_err(|error| format!("存不下缩略图：{error}"))?;
    Ok(path)
}

/// 素材文件落盘：已缓存就返回路径，否则跟随 `/assets/{id}/download` 的 302
/// 下载并边下边报进度。返回 (路径, 字节数)。
pub fn download(
    dirs: &AppDirs,
    item: &AssetItem,
    progress: &mut dyn FnMut(f32, &str),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<PathBuf, String> {
    let dir = cache_dir(dirs).join("files");
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("建不了素材缓存：{error}"))?;
    if let Some(path) = cached_file(&dir, &item.id) {
        return Ok(path);
    }
    let url = format!("{}/assets/{}/download", API_BASE, item.id);
    let temporary = dir.join(format!("{}.part", safe(&item.id)));
    crate::direct::download(&url, &temporary, progress, cancel)?;
    let bytes = std::fs::read(&temporary).map_err(|error| format!("读不回素材：{error}"))?;
    if bytes.len() < 128 {
        return Err("下载到的素材文件不完整".to_owned());
    }
    let extension = match item.kind.as_str() {
        "image" | "thumb" => extension_of(&bytes),
        "music" | "sfx" => "mp3",
        "video" => "mp4",
        "font" => "ttf",
        _ => "bin",
    };
    let path = dir.join(format!("{}.{extension}", safe(&item.id)));
    std::fs::rename(&temporary, &path).map_err(|error| format!("存不下素材：{error}"))?;
    Ok(path)
}

/// 缓存里已有的素材文件。
pub fn cached_file(dir: &Path, id: &str) -> Option<PathBuf> {
    let stem = safe(id);
    std::fs::read_dir(dir).ok()?.flatten().map(|entry| entry.path()).find(|path| {
        path.file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == stem)
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext != "part")
    })
}

/// 缓存中这个素材的路径（公开给 Studio 判断要不要下载）。
pub fn cached(dirs: &AppDirs, id: &str) -> Option<PathBuf> {
    cached_file(&cache_dir(dirs).join("files"), id)
}

/// 已下载素材的 id 集合，一次读目录：一页加载完成时把所有卡片的状态一次
/// 定好，已缓存的直接显示「插入时间轴」，不用再点一次下载。
pub fn cached_ids(dirs: &AppDirs) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    let Ok(entries) = std::fs::read_dir(cache_dir(dirs).join("files")) else {
        return ids;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext != "part")
            && let Some(stem) = path.file_stem().and_then(|name| name.to_str())
        {
            ids.insert(stem.to_owned());
        }
    }
    ids
}

/// 一个文件名安全的 id。
fn safe(id: &str) -> String {
    id.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '_' })
        .collect()
}

/// 一个 id 在缓存文件名里的写法；[`cached_ids`] 返回的集合按它比较。
pub fn cache_stem(id: &str) -> String {
    safe(id)
}

/// 查询参数里一个值的转义。
fn urlencode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
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
    fn a_search_query_escapes_chinese_and_spaces() {
        assert_eq!(urlencode("除甲醛 新房"), "%E9%99%A4%E7%94%B2%E9%86%9B%20%E6%96%B0%E6%88%BF");
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn an_unknown_image_starts_as_bin_and_a_png_is_seen() {
        assert_eq!(extension_of(b"not an image"), "bin");
        assert_eq!(extension_of(b"\x89PNG\r\n\x1a\nxxxx"), "png");
    }

    #[test]
    fn an_id_becomes_a_file_name() {
        assert_eq!(safe("im/12 34"), "im_12_34");
    }
}
