// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 本地 AI 插件：装在本机、离线可用的小引擎包（一期 Piper 配音，二期
//! Wav2Lip 数字人）。
//!
//! 一个插件是一个文件夹，放在配置目录的 `ai-plugins/<id>/` 下
//! （Windows：`%APPDATA%\app.concat.editor\ai-plugins\<id>\`），文件夹里
//! 有 `plugin.json` 说明它是什么、可执行文件叫什么、音色/模型在哪。文件夹
//! 就是安装：拷进去即装好，删掉即卸掉，不做注册表、不写系统目录。
//!
//! 包从我们自己的 OSS 下载（[`DOWNLOAD`] 里的地址，运营上传后即用），也可以
//! 由人「导入插件包」——一个 zip，解压到上面那个位置。解压出来的第一层可以
//! 是 `plugin.json`，也可以是一个装着它的子文件夹；没有清单但有
//! `piper.exe` 的老包按 Piper 认，写一份清单补上。
//!
//! 目录扫描是唯一的事实来源：界面打开、导入完成、删掉之后都重新扫一遍，
//! 不缓存「装了什么」。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 插件都放在配置目录下的这个文件夹里。
pub const FOLDER: &str = "ai-plugins";

/// 官方的插件清单：里面给每个插件的下载地址、大小与 sha256，运营换包时
/// 不用改客户端。取不到就回退到 [`DOWNLOAD`] 里的固定地址。
pub const MANIFEST_URL: &str = "https://xinglaiai.com/ai-nav/api/ai-plugins/manifest.json";

/// 官方的两个插件包（清单取不到时的兜底地址；清单里的 `url` 优先）。
pub const DOWNLOAD: [(&str, &str); 2] = [
    (
        "piper",
        "https://xinglaiai.com/ai-nav/api/ai-plugins/piper.zip",
    ),
    (
        "wav2lip",
        "https://xinglaiai.com/ai-nav/api/ai-plugins/wav2lip.zip",
    ),
];

/// 远端清单里一个插件包。
#[derive(Clone, Debug, Default)]
pub struct RemotePackage {
    /// 版本号，只用来显示。
    pub version: String,
    /// 下载地址；空则用固定地址。
    pub url: String,
    /// 字节数，0 表示没说。
    pub size: u64,
    /// 十六进制 sha256；空则不校验。
    pub sha256: String,
}

/// 取一次远端插件清单；取不到或格式不认识就是空表，调用方用固定地址。
///
/// 清单长这样（`plugins` 可省，根上直接放 id 也认）：
/// `{"ok":true,"plugins":{"piper":{"version":"1.0.0","url":"https://…/piper.zip",
/// "size":100003538,"sha256":"950b…"}}}`
pub fn remote_manifest(dirs: &crate::AppDirs) -> std::collections::HashMap<String, RemotePackage> {
    use std::collections::HashMap;
    let mut out = HashMap::new();
    let url = std::env::var("XINGLAI_AI_PLUGIN_MANIFEST")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| MANIFEST_URL.to_owned());
    let folder = root(dirs).join(".downloads");
    if std::fs::create_dir_all(&folder).is_err() {
        return out;
    }
    let temp = folder.join("manifest.json");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    if crate::direct::download(&url, &temp, &mut |_, _| {}, &cancel).is_err() {
        let _ = std::fs::remove_file(&temp);
        return out;
    }
    let text = std::fs::read_to_string(&temp).unwrap_or_default();
    let _ = std::fs::remove_file(&temp);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return out;
    };
    let plugins = value
        .get("plugins")
        .filter(|plugins| plugins.is_object())
        .unwrap_or(&value);
    let Some(map) = plugins.as_object() else {
        return out;
    };
    for (id, row) in map {
        if id == "ok" || row.is_string() {
            continue;
        }
        let field = |name: &str| {
            row.get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        out.insert(
            id.clone(),
            RemotePackage {
                version: field("version"),
                url: field("url"),
                size: row
                    .get("size")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                sha256: field("sha256").to_ascii_lowercase(),
            },
        );
    }
    out
}

/// 一个插件的清单，`plugin.json`。字段都可省，省掉的有默认值。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Manifest {
    /// 文件夹名与稳定 id：`piper`、`wav2lip`。
    pub id: String,
    /// 界面上叫它什么。
    pub name: String,
    /// `tts` 或 `avatar`。
    pub kind: String,
    /// 引擎：`piper`、`wav2lip`。
    pub engine: String,
    /// 版本，`1.0.0`。
    pub version: String,
    /// 一句话说明。
    pub description: String,
    /// 可执行文件，相对插件根：`piper.exe`。
    pub executable: String,
    /// 音色/模型目录，相对插件根：`voices`。
    pub voices_dir: String,
    /// 这份清单对应的下载包，界面上「有新版」时用。
    pub download_url: String,
    /// 音色表；没有就扫 `voices_dir` 里的 `*.onnx`。
    pub voices: Vec<VoiceEntry>,
}

/// 清单里的一条音色。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VoiceEntry {
    /// 稳定 id，存进设置。
    pub id: String,
    /// 界面上的名字，`华言（女·标准）`。
    pub name: String,
    /// 音色分类，"女 · 普通话"。
    pub tag: String,
    /// 模型文件，相对插件根。
    pub model: String,
    /// 模型配置，相对插件根。
    pub config: String,
    /// 质量：`标准`/`轻量`。
    pub quality: String,
}

/// 本机装着的一个插件。
#[derive(Clone, Debug)]
pub struct Plugin {
    /// 它的文件夹。
    pub folder: PathBuf,
    /// 清单。没有清单的文件夹扫描时补不出来，就不算插件。
    pub manifest: Manifest,
    /// 占多少磁盘，字节。
    pub bytes: u64,
    /// 引擎的主程序在不在。
    pub complete: bool,
}

impl Plugin {
    /// 主程序的完整路径。
    pub fn executable(&self) -> Option<PathBuf> {
        let name = if self.manifest.executable.trim().is_empty() {
            "piper.exe"
        } else {
            self.manifest.executable.trim()
        };
        let path = self.folder.join(name);
        path.is_file().then_some(path)
    }

    /// 音色表：清单里的先，扫出来的补在后面，按 id 去重。
    pub fn voices(&self) -> Vec<LocalVoice> {
        let mut voices: Vec<LocalVoice> = Vec::new();
        for entry in &self.manifest.voices {
            voices.push(LocalVoice {
                id: entry.id.clone(),
                name: entry.name.clone(),
                tag: entry.tag.clone(),
                quality: entry.quality.clone(),
                model: self.folder.join(entry.model.trim()),
                config: Some(self.folder.join(entry.config.trim())).filter(|path| path.is_file()),
            });
        }
        for found in self.scanned_voices() {
            if !voices.iter().any(|voice| voice.id == found.id) {
                voices.push(found);
            }
        }
        voices.retain(|voice| voice.model.is_file());
        voices
    }

    /// 引擎目录下扫出来的 `*.onnx`。
    fn scanned_voices(&self) -> Vec<LocalVoice> {
        let dir = if self.manifest.voices_dir.trim().is_empty() {
            self.folder.clone()
        } else {
            self.folder.join(self.manifest.voices_dir.trim())
        };
        let mut files = Vec::new();
        collect_onnx(&dir, 0, &mut files);
        let mut voices: Vec<LocalVoice> = files
            .into_iter()
            .map(|model| local_voice(&model))
            .collect();
        voices.sort_by(|a, b| a.name.cmp(&b.name));
        voices
    }

    /// `piper` / `wav2lip`：按清单，认不出的按 engine 推。
    pub fn engine(&self) -> String {
        let engine = self.manifest.engine.trim().to_ascii_lowercase();
        if !engine.is_empty() {
            return engine;
        }
        if self.manifest.id.eq_ignore_ascii_case("wav2lip") {
            "wav2lip".to_owned()
        } else {
            "piper".to_owned()
        }
    }

    /// 界面上显示的大小。
    pub fn size_label(&self) -> String {
        human_size(self.bytes)
    }
}

/// 一个可用的本地音色。
#[derive(Clone, Debug)]
pub struct LocalVoice {
    /// 稳定 id。
    pub id: String,
    /// 界面上的名字。
    pub name: String,
    /// 分类。
    pub tag: String,
    /// 质量。
    pub quality: String,
    /// 模型文件。
    pub model: PathBuf,
    /// 配置。
    pub config: Option<PathBuf>,
}

/// 插件根目录。
pub fn root(dirs: &crate::AppDirs) -> PathBuf {
    dirs.config.join(FOLDER)
}

/// 重扫一遍本机装了哪些插件。目录不存在就是空。
pub fn scan(dirs: &crate::AppDirs) -> Vec<Plugin> {
    let root = root(dirs);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut plugins = Vec::new();
    for entry in entries.flatten() {
        let folder = entry.path();
        if !folder.is_dir() {
            continue;
        }
        if folder
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if let Some(plugin) = read_plugin(&folder) {
            plugins.push(plugin);
        }
    }
    plugins.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    plugins
}

/// 读一个插件文件夹；没有清单但有 `piper.exe` 的按 Piper 认。
pub fn read_plugin(folder: &Path) -> Option<Plugin> {
    let manifest = read_manifest(folder).or_else(|| synthesized(folder))?;
    let bytes = folder_size(folder);
    let complete = folder
        .join(if manifest.executable.trim().is_empty() {
            "piper.exe"
        } else {
            manifest.executable.trim()
        })
        .is_file();
    Some(Plugin {
        folder: folder.to_path_buf(),
        manifest,
        bytes,
        complete,
    })
}

/// 清空并重写清单；导入时补出来的清单也写回去，下次就不用再猜。
fn write_manifest(folder: &Path, manifest: &Manifest) -> Result<(), String> {
    let text = serde_json::to_string_pretty(manifest)
        .map_err(|error| format!("插件清单写不出来：{error}"))?;
    std::fs::write(folder.join("plugin.json"), text)
        .map_err(|error| format!("插件清单写不出来：{error}"))
}

fn read_manifest(folder: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(folder.join("plugin.json")).ok()?;
    let mut manifest: Manifest = serde_json::from_str(&text).ok()?;
    if manifest.id.trim().is_empty() {
        manifest.id = folder.file_name()?.to_string_lossy().into_owned();
    }
    if manifest.name.trim().is_empty() {
        manifest.name = manifest.id.clone();
    }
    if manifest.kind.trim().is_empty() {
        manifest.kind = if manifest.id.eq_ignore_ascii_case("wav2lip") {
            "avatar".to_owned()
        } else {
            "tts".to_owned()
        };
    }
    if manifest.executable.trim().is_empty() {
        manifest.executable = if folder.join("piper.exe").is_file() {
            "piper.exe".to_owned()
        } else {
            "wav2lip.exe".to_owned()
        };
    }
    Some(manifest)
}

/// 没有清单的老包：认得出 piper.exe 就当 Piper。
fn synthesized(folder: &Path) -> Option<Manifest> {
    if !folder.join("piper.exe").is_file() {
        return None;
    }
    let manifest = Manifest {
        id: "piper".to_owned(),
        name: "Piper 本地配音".to_owned(),
        kind: "tts".to_owned(),
        engine: "piper".to_owned(),
        version: "1.0.0".to_owned(),
        description: "完全离线的中文语音合成（CPU 实时）".to_owned(),
        executable: "piper.exe".to_owned(),
        voices_dir: "voices".to_owned(),
        ..Manifest::default()
    };
    // 补一份清单，下一次扫描就按它来。
    let _ = write_manifest(folder, &manifest);
    Some(manifest)
}

fn collect_onnx(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_onnx(&path, depth + 1, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("onnx"))
        {
            out.push(path);
        }
    }
}

/// 一个 `*.onnx` 音色，名字按文件名认；汉化 huayan 的两个模型给好记的名字。
fn local_voice(model: &Path) -> LocalVoice {
    let stem = model
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "voice".to_owned());
    let lower = stem.to_ascii_lowercase();
    let (name, tag, quality) = if lower.contains("huayan") && lower.contains("x_low") {
        (
            "华言（女·轻量）".to_owned(),
            "女 · 普通话".to_owned(),
            "轻量（更快）".to_owned(),
        )
    } else if lower.contains("huayan") {
        (
            "华言（女·标准）".to_owned(),
            "女 · 普通话".to_owned(),
            "标准".to_owned(),
        )
    } else {
        (stem.clone(), "本地音色".to_owned(), "标准".to_owned())
    };
    let config = model.with_extension("onnx.json");
    LocalVoice {
        id: stem,
        name,
        tag,
        quality,
        model: model.to_path_buf(),
        config: config.is_file().then_some(config),
    }
}

/// 插件占的磁盘。
fn folder_size(folder: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(folder) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += folder_size(&path);
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

/// 导入一个插件 zip：解压到临时目录，找到插件根，再搬到 `ai-plugins/<id>`。
///
/// 覆盖安装（同一个 id 已存在）时旧目录先删掉；解压失败时旧目录不动。
/// `progress` 报「第几个文件 / 一共几个」。
pub fn import_zip(
    dirs: &crate::AppDirs,
    zip_path: &Path,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<Plugin, String> {
    let file = std::fs::File::open(zip_path)
        .map_err(|error| format!("插件包打不开（{}）：{error}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| format!("插件包不是有效的 zip：{error}"))?;
    let count = archive.len();
    if count == 0 {
        return Err("插件包是空的".to_owned());
    }
    if count > 20_000 {
        return Err("插件包里的文件太多，不像是插件".to_owned());
    }
    let root = root(dirs);
    std::fs::create_dir_all(&root).map_err(|error| format!("插件目录建不出来：{error}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis());
    let temp = root.join(format!(".import-{}-{stamp}", std::process::id()));
    if temp.exists() {
        let _ = std::fs::remove_dir_all(&temp);
    }
    std::fs::create_dir_all(&temp).map_err(|error| format!("临时目录建不出来：{error}"))?;
    let outcome = extract(&mut archive, &temp, count, progress);
    if let Err(error) = outcome {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(error);
    }
    let plugin_root = find_plugin_root(&temp).unwrap_or(temp.clone());
    let Some(mut manifest) = read_manifest(&plugin_root).or_else(|| synthesized(&plugin_root)) else {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(
            "插件包里没找到 plugin.json，也没有 piper.exe；请确认选的是本地 AI 插件包".to_owned(),
        );
    };
    if manifest.id.trim().is_empty() {
        manifest.id = plugin_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "plugin".to_owned());
    }
    let id = sanitize(&manifest.id);
    let final_folder = root.join(&id);
    if final_folder.exists() {
        std::fs::remove_dir_all(&final_folder)
            .map_err(|error| format!("旧版插件删不掉（可能被占用）：{error}"))?;
    }
    if plugin_root == temp {
        std::fs::rename(&temp, &final_folder)
            .map_err(|error| format!("插件搬不进插件目录：{error}"))?;
    } else {
        std::fs::rename(&plugin_root, &final_folder)
            .map_err(|error| format!("插件搬不进插件目录：{error}"))?;
        let _ = std::fs::remove_dir_all(&temp);
    }
    read_plugin(&final_folder).ok_or_else(|| "插件解压完了，但读不出清单".to_owned())
}

fn extract(
    archive: &mut zip::ZipArchive<std::fs::File>,
    temp: &Path,
    count: usize,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<(), String> {
    for index in 0..count {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("插件包第 {} 个文件读不出来：{error}", index + 1))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err("插件包里有不安全的路径（.. 或绝对路径）".to_owned());
        };
        let target = temp.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|error| format!("插件目录建不出来：{error}"))?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("插件目录建不出来：{error}"))?;
            }
            let mut out = std::fs::File::create(&target)
                .map_err(|error| format!("插件文件写不进去（{}）：{error}", target.display()))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|error| format!("插件文件写不进去（{}）：{error}", target.display()))?;
        }
        progress(
            (index + 1) as f32 / count as f32,
            &format!("正在解压 {}/{} 个文件", index + 1, count),
        );
    }
    Ok(())
}

/// 解压出来的第一层可能是插件本身，也可能是一个装着它的文件夹。
fn find_plugin_root(temp: &Path) -> Option<PathBuf> {
    if temp.join("plugin.json").is_file() || temp.join("piper.exe").is_file() {
        return Some(temp.to_path_buf());
    }
    let entries = std::fs::read_dir(temp).ok()?;
    for entry in entries.flatten() {
        let folder = entry.path();
        if !folder.is_dir() {
            continue;
        }
        if folder.join("plugin.json").is_file() || folder.join("piper.exe").is_file() {
            return Some(folder);
        }
        // 再深一层也认：压缩时多套了一个版本目录的包。
        if let Ok(inner) = std::fs::read_dir(&folder) {
            for entry in inner.flatten() {
                let deeper = entry.path();
                if deeper.is_dir()
                    && (deeper.join("plugin.json").is_file()
                        || deeper.join("piper.exe").is_file())
                {
                    return Some(deeper);
                }
            }
        }
    }
    None
}

/// 文件夹名只留安全字符。
fn sanitize(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(['-', '.']).to_owned();
    if cleaned.is_empty() {
        "plugin".to_owned()
    } else {
        cleaned
    }
}

/// 删掉一个插件。
pub fn remove(dirs: &crate::AppDirs, id: &str) -> Result<(), String> {
    let folder = root(dirs).join(sanitize(id));
    if !folder.exists() {
        return Err(format!("本机没有装「{id}」这个插件"));
    }
    std::fs::remove_dir_all(&folder).map_err(|error| format!("插件删不掉（可能被占用）：{error}"))
}

/// 下载一个官方插件包到 `ai-plugins/.downloads/<id>.zip`。
///
/// 走平台自己的 HTTPS 直连（[`crate::direct`]），进度按字节报；下完不自动
/// 装，交给 [`import_zip`]，这样下载失败不会弄坏已经装好的版本。
/// `sha256` 非空时校验，不一致删包报错。
pub fn download(
    dirs: &crate::AppDirs,
    id: &str,
    url: &str,
    sha256: &str,
    progress: &mut dyn FnMut(f32, &str),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<PathBuf, String> {
    if url.trim().is_empty() {
        return Err("这个插件的下载地址还没配置：请先用「导入插件包」装上".to_owned());
    }
    let folder = root(dirs).join(".downloads");
    std::fs::create_dir_all(&folder).map_err(|error| format!("下载目录建不出来：{error}"))?;
    let target = folder.join(format!("{}.zip", sanitize(id)));
    let partial = target.with_extension("zip.part");
    let bytes = crate::direct::download(url.trim(), &partial, progress, cancel)?;
    let want = sha256.trim().to_ascii_lowercase();
    if !want.is_empty() {
        let got = sha256_of(&partial)?;
        if got != want {
            let _ = std::fs::remove_file(&partial);
            return Err(format!(
                "插件包校验失败：期望 {want}，实际 {got}；请稍后重试或联系运营"
            ));
        }
        log::info!("ai-plugins: {id} {} bytes verified sha256={want}", bytes);
    }
    std::fs::rename(&partial, &target).map_err(|error| format!("下载包收不好：{error}"))?;
    Ok(target)
}

/// 一个文件的 sha256，十六进制小写。
fn sha256_of(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("下载的文件读不出来：{error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("下载的文件读不出来：{error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 下载地址：官方表里的，或环境变量 `XINGLAI_AI_PLUGIN_URL`（给测试与自建
/// 镜像用）。
pub fn download_url(id: &str) -> String {
    if let Ok(url) = std::env::var("XINGLAI_AI_PLUGIN_URL") {
        if !url.trim().is_empty() {
            return url.trim().to_owned();
        }
    }
    DOWNLOAD
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, url)| (*url).to_owned())
        .unwrap_or_default()
}

/// 包有多大：官方没给，界面上写个大概；有 `Content-Length` 时下载进度更准。
pub fn download_size_hint(id: &str) -> u64 {
    match id {
        "piper" => 110 * 1024 * 1024,
        "wav2lip" => 220 * 1024 * 1024,
        _ => 100 * 1024 * 1024,
    }
}

/// 字节数的人话。
pub fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_name_is_made_safe() {
        assert_eq!(sanitize("piper"), "piper");
        assert_eq!(sanitize("../etc/passwd"), "etc-passwd");
        assert_eq!(sanitize(".."), "plugin");
        assert_eq!(sanitize("a b"), "a-b");
    }

    #[test]
    fn huayan_voices_get_names() {
        let medium = local_voice(Path::new("zh_CN-huayan-medium.onnx"));
        assert_eq!(medium.name, "华言（女·标准）");
        let light = local_voice(Path::new("zh_CN-huayan-x_low.onnx"));
        assert_eq!(light.name, "华言（女·轻量）");
        assert_eq!(light.quality, "轻量（更快）");
    }

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(human_size(1536), "2 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
