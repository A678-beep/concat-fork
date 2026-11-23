// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! AI 工程包导入（0.3.5）：把平台 `project_pack v1` 的 zip 变成桌面工程。
//!
//! 工程包格式见 `docs/AI工程包格式-v1.md`：zip 里有 `project.json`（完整交接
//! 清单）、`project.xinglai.json`（星籁剪辑原生工程文档）、`assets/`（视频/音频/
//! 图片，相对路径）、`subtitles.srt`、`meta.json`。原生工程文档就是 0.3.x 的
//! 工程正文：把 `media[].path` 从包内相对路径改成本地解压目录的绝对路径，
//! 再放到工程文件夹的 `concat.json` 里，就是一个能直接打开、能播的工程。
//!
//! 这里只做解压与路径改写；打开工程、插入时间轴由窗口那一半完成。任一步失败
//! 都返回中文一句话，界面如实显示（不做假成功）。

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// 一次导入的结果。
#[derive(Clone, Debug, Default)]
pub struct ImportedPack {
    /// 新工程文件夹（`open_project_at` 用它）。
    pub project_path: String,
    /// 工程名（包的标题）。
    pub title: String,
    /// 视频轨条数。
    pub video_tracks: u32,
    /// 音频轨条数。
    pub audio_tracks: u32,
    /// 片段总数。
    pub clips: u32,
    /// 字幕条数（SRT / cues）。
    pub subtitles: u32,
    /// AI 标识（「AI 生成」）。
    pub ai_label: String,
    /// 工程包 id。
    pub pack_id: String,
    /// 素材相对路径 → 本地绝对路径的条数。
    pub assets: u32,
    /// 包里没带上、只留了远程地址的素材（只能提示，不假装有）。
    pub missing: u32,
    /// 解压出来、但工程文档里没登记的旁路文件（如 extras/）。
    pub extras: Vec<String>,
    /// 不致命的问题。
    pub warnings: Vec<String>,
}

impl ImportedPack {
    /// 一行给界面看的结果。
    pub fn summary(&self) -> String {
        format!(
            "已导入「{}」：{} 条视频轨 / {} 条音频轨 · {} 个片段 · {} 条字幕 · {} 个素材{}",
            if self.title.trim().is_empty() { "AI 工程包" } else { self.title.trim() },
            self.video_tracks,
            self.audio_tracks,
            self.clips,
            self.subtitles,
            self.assets,
            if self.missing > 0 {
                format!("；{} 个素材未随包（只有远程地址）", self.missing)
            } else {
                String::new()
            }
        )
    }
}

/// 文件夹名里的坏字符换掉，Windows 上留不出问题的名字。
fn safe_folder(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars().take(48) {
        if ch.is_ascii_control() || "<>:\"/\\|?*".contains(ch) {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim().trim_matches('.').trim().to_owned();
    if trimmed.is_empty() {
        "星籁AI工程包".to_owned()
    } else {
        trimmed
    }
}

/// 不重名的工程文件夹：同名时加 `-2`、`-3`…
fn free_folder(parent: &Path, name: &str) -> PathBuf {
    let base = safe_folder(name);
    let mut candidate = parent.join(&base);
    let mut n = 2u32;
    while candidate.exists() {
        candidate = parent.join(format!("{base}-{n}"));
        n += 1;
    }
    candidate
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 解压包内一个条目；路径必须落在 `root` 里，绝不写到外面。
fn extract_entry(
    archive: &mut zip::ZipArchive<std::fs::File>,
    index: usize,
    root: &Path,
) -> Result<Option<PathBuf>, String> {
    let mut entry = archive
        .by_index(index)
        .map_err(|error| format!("读 zip 失败：{error}"))?;
    if entry.is_dir() {
        return Ok(None);
    }
    let Some(relative) = entry.enclosed_name() else {
        return Err(format!("工程包里有不安全的路径：{}", entry.name()));
    };
    let dest = root.join(&relative);
    if !dest.starts_with(root) {
        return Err(format!("工程包里有不安全的路径：{}", entry.name()));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("建不了解压目录 {}：{error}", parent.display()))?;
    }
    let mut out = std::fs::File::create(&dest)
        .map_err(|error| format!("写不了 {}：{error}", dest.display()))?;
    std::io::copy(&mut entry, &mut out)
        .map_err(|error| format!("解压 {} 失败：{error}", dest.display()))?;
    Ok(Some(dest))
}

/// 工程包里的一个 zip 是否是星籁 AI 工程包（有原生工程文档或 project.json）。
pub fn is_pack(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return false;
    };
    let mut native = false;
    for index in 0..archive.len() {
        if let Ok(entry) = archive.by_index(index) {
            let name = entry.name().trim_start_matches("./").to_owned();
            if name == "project.xinglai.json" || name == "project.json" {
                native = true;
                break;
            }
        }
    }
    native
}

/// 递归删掉对象里的 null 字段（数组里的 null 保留）。工程包的素材条目用它。
fn strip_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key, strip_nulls(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

/// 把工程包解压成桌面工程，返回结果（不打开工程）。
///
/// `dest_parent` 为 None 时解压到 zip 所在目录；`title_override` 为空时用包里的
/// 标题。任何失败都是中文一句话。
pub fn import(
    zip_path: &Path,
    dest_parent: Option<&Path>,
    title_override: &str,
) -> Result<ImportedPack, String> {
    let file = std::fs::File::open(zip_path)
        .map_err(|error| format!("打不开工程包 {}：{error}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("这不是有效的 .zip 工程包：{error}"))?;

    // 标题优先用包里的 meta.json，其次文件名。
    let mut meta_title = String::new();
    let mut pack_id = String::new();
    for index in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        let name = entry.name().trim_start_matches("./").to_owned();
        if name == "meta.json" || name == "project.json" {
            let mut text = String::new();
            if entry.read_to_string(&mut text).is_ok() {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    if let Some(title) = value.get("title").and_then(Value::as_str) {
                        if !title.trim().is_empty() {
                            meta_title = title.trim().to_owned();
                        }
                    }
                    if let Some(id) = value.get("pack_id").and_then(Value::as_str) {
                        pack_id = id.to_owned();
                    }
                }
            }
        }
    }

    let stem = zip_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "AI工程包".to_owned());
    let title = if !title_override.trim().is_empty() {
        title_override.trim().to_owned()
    } else if !meta_title.is_empty() {
        meta_title
    } else {
        stem.clone()
    };
    let parent = dest_parent
        .map(Path::to_path_buf)
        .or_else(|| zip_path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let folder = free_folder(&parent, &title);
    std::fs::create_dir_all(&folder)
        .map_err(|error| format!("建不了工程文件夹 {}：{error}", folder.display()))?;

    let mut files: Vec<String> = Vec::new();
    for index in 0..archive.len() {
        if let Some(path) = extract_entry(&mut archive, index, &folder)? {
            if let Ok(relative) = path.strip_prefix(&folder) {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    if files.is_empty() {
        return Err("工程包是空的".to_owned());
    }

    // 原生工程文档：优先 project.xinglai.json，退而求其次用 project.json 的
    // 内联字段（本版只认原生文档；没有就如实说）。
    let native = folder.join("project.xinglai.json");
    let mut warnings = Vec::new();
    let document = if native.is_file() {
        read_json(&native)
            .ok_or_else(|| "工程包的 project.xinglai.json 不是有效 JSON".to_owned())?
    } else {
        return Err("不是星籁 AI 工程包：缺少 project.xinglai.json（0.3.x 原生工程文档）"
            .to_owned());
    };

    let mut document = document;
    let mut assets = 0u32;
    let mut missing = 0u32;
    if let Some(media) = document.get_mut("media").and_then(Value::as_array_mut) {
        for item in media.iter_mut() {
            let Some(path) = item.get("path").and_then(Value::as_str).map(str::to_owned) else {
                if item.get("source_url").and_then(Value::as_str).is_some_and(|u| !u.is_empty()) {
                    missing += 1;
                }
                continue;
            };
            if path.trim().is_empty() {
                if item.get("source_url").and_then(Value::as_str).is_some_and(|u| !u.is_empty()) {
                    missing += 1;
                }
                continue;
            }
            if Path::new(&path).is_absolute() {
                missing += 1;
                continue;
            }
            let absolute = folder.join(path.trim_start_matches("./"));
            if absolute.is_file() {
                let shown = absolute.to_string_lossy().into_owned();
                item["path"] = Value::String(shown);
                assets += 1;
            } else {
                missing += 1;
            }
        }
    } else {
        return Err("工程文档里没有 media 列表".to_owned());
    }

    // 工程名与写入方版本：manifest 也认这两个字段。
    let doc_title = document
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| title.clone());
    document["name"] = Value::String(doc_title.clone());
    if document.get("concat").and_then(Value::as_str).is_none() {
        document["concat"] = Value::String(env!("CARGO_PKG_VERSION").to_owned());
    }
    if document.get("video").and_then(Value::as_object).is_none() {
        document["video"] = serde_json::json!({
            "width": 1920, "height": 1080, "rateNum": 30, "rateDen": 1
        });
        warnings.push("工程文档没写画布，按 1920×1080/30fps 打开".to_owned());
    }
    // 字体表指向的是平台字体目录，本地没有：留空让系统字体接管。
    if document.get("fonts").and_then(Value::as_array).is_some_and(|list| !list.is_empty()) {
        document["fonts"] = serde_json::json!([]);
        warnings.push("工程包里的自定义字体未随包，字幕用系统字体".to_owned());
    }

    // 工程包里的素材条目会把没探测过的字段写成 null（duration/width/…）。引擎的
    // MediaItem 用具体类型读这些字段，null 会让它整条读不出来（于是片段也跟着
    // 读不出来）。把 null 去掉，缺字段就走引擎的默认值。
    let document = strip_nulls(document);
    let encoded = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("工程文档编码失败：{error}"))?;
    let manifest = folder.join("concat.json");
    std::fs::write(&manifest, encoded)
        .map_err(|error| format!("写不了 {}：{error}", manifest.display()))?;

    // 统计给界面看：轨、片段、字幕。
    let mut video_tracks = 0u32;
    let mut audio_tracks = 0u32;
    let mut clips = 0u32;
    if let Some(tracks) = document
        .get("timelines")
        .and_then(Value::as_array)
        .and_then(|timelines| timelines.first())
        .and_then(|timeline| timeline.get("tracks"))
        .and_then(Value::as_array)
    {
        for track in tracks {
            match track.get("type").and_then(Value::as_str) {
                Some("audio") => audio_tracks += 1,
                _ => video_tracks += 1,
            }
            clips += track
                .get("clips")
                .and_then(Value::as_array)
                .map(|rows| rows.len() as u32)
                .unwrap_or(0);
        }
    } else if let Some(rows) = document.get("clips").and_then(Value::as_array) {
        clips = rows.len() as u32;
    }

    let mut subtitles = 0u32;
    if let Some(srt) = folder.join("subtitles.srt").to_str() {
        if let Ok(text) = std::fs::read_to_string(srt) {
            subtitles = text.matches(" --> ").count() as u32;
        }
    }
    if subtitles == 0 {
        if let Ok(project) = std::fs::read_to_string(folder.join("project.json")) {
            if let Ok(value) = serde_json::from_str::<Value>(&project) {
                subtitles = value
                    .get("subtitles")
                    .and_then(|s| s.get("cues"))
                    .and_then(Value::as_array)
                    .map(|cues| cues.len() as u32)
                    .unwrap_or(0);
            }
        }
    }

    let ai_label = if let Ok(project) = std::fs::read_to_string(folder.join("project.json")) {
        serde_json::from_str::<Value>(&project)
            .ok()
            .and_then(|value| {
                value
                    .get("ai")
                    .and_then(|ai| ai.get("label"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "AI 生成".to_owned())
    } else {
        "AI 生成".to_owned()
    };

    // extras/ 之类没进工程文档的文件列出来（EDL/OTIO/QC 在这里）。
    let mut extras = Vec::new();
    for file in &files {
        if file.starts_with("extras/") || file == "README.txt" {
            extras.push(file.clone());
        }
    }
    if assets == 0 {
        return Err("工程包里的素材路径都不存在（解压不完整或包内只有远程素材）".to_owned());
    }
    if missing > 0 {
        warnings.push(format!(
            "{missing} 个素材未随包（只有远程地址，画面已合成在成片里）"
        ));
    }
    if clips == 0 {
        warnings.push("工程文档里没有片段（只有素材）".to_owned());
    }

    Ok(ImportedPack {
        project_path: folder.to_string_lossy().into_owned(),
        title: doc_title,
        video_tracks,
        audio_tracks,
        clips,
        subtitles,
        ai_label,
        pack_id,
        assets,
        missing,
        extras,
        warnings,
    })
}
