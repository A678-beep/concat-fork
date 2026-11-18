// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 桌面端的「工程云同步」：文件菜单的三行，与云工程面板。
//!
//! 真正连网与压缩在 `concat_host::cloud`；这里只做三件事：在事件循环线程
//! 上读当前工程、把慢活交给线程、回来时把结果写进 Slint（云面板、通知）。
//!
//! 与平台的约定都写在 `docs/INTERFACES.md` 的「工程云同步」一节：
//! 工程文件随工程文件夹走，媒体仍是本机的绝对路径；从云端取回只恢复工程
//! 本身，媒体缺失时由窗口原本的「缺失素材」流程提示。
//!
//! 工程 id 记在工程文件夹的 `cloud.json` 旁（不是 `concat.json`，不改工程
//! 格式）：下一次保存就用 PUT 覆盖，服务端版本 +1；换机器或删掉这个文件
//! 会新建一份，不会把别人的工程覆盖掉。

use std::path::{Path, PathBuf};

use concat_host::cloud as api;
use slint::{ModelRc, VecModel};

use crate::host::Shell;
use crate::ui::{App, CloudItem};

/// 上传上限，与平台一致；本地先拦一次，免得白传。
const MAX_UPLOAD: u64 = 200 * 1024 * 1024;

/// 云端列表，重新取一次（并打开面板）。
pub fn refresh(weak: slint::Weak<App>) {
    let token = match token() {
        Ok(token) => token,
        Err(message) => return reject(&weak, &message),
    };
    Shell::with(|_shell, app| {
        app.set_cloud_open(true);
        app.set_cloud_busy(true);
        app.set_cloud_status("正在获取云端工程列表…".into());
        app.set_cloud_url("".into());
    });
    std::thread::spawn(move || {
        let result = api::list_projects(&token);
        let _ = slint::invoke_from_event_loop(move || {
            if weak.upgrade().is_none() {
                return;
            }
            Shell::with(|shell, app| {
                let mut studio = shell.studio.borrow_mut();
                app.set_cloud_busy(false);
                match result {
                    Ok(projects) => {
                        let count = projects.len();
                        app.set_cloud_items(ModelRc::new(VecModel::from(rows(&projects))));
                        app.set_cloud_status(
                            format!("云端共 {count} 个工程；点「打开」把其中一个恢复到本机。")
                                .into(),
                        );
                    }
                    Err(message) => {
                        studio.notify(&message, true);
                        app.set_cloud_status(message.into());
                    }
                }
                studio.publish_status(&app);
            });
        });
    });
}

/// 保存当前打开的工程：有云端 id 就覆盖（版本 +1），没有就新建。
pub fn save_current(weak: slint::Weak<App>) {
    let Some((folder, name, document)) = current_project() else {
        return reject(&weak, "请先打开一个工程，再保存到云端");
    };
    let token = match token() {
        Ok(token) => token,
        Err(message) => return reject(&weak, &message),
    };
    Shell::with(|_shell, app| {
        app.set_cloud_open(true);
        app.set_cloud_busy(true);
        app.set_cloud_status("正在保存工程到云端…".into());
        app.set_cloud_url("".into());
    });
    let pid = read_pid(Path::new(&folder));
    let meta = serde_json::json!({
        "client": "XingLaiCut",
        "app_version": env!("CARGO_PKG_VERSION"),
    });
    std::thread::spawn(move || {
        let result = api::save_project(&token, pid.as_deref(), &name, &document, Some(&meta));
        let listing = match &result {
            Ok(saved) => {
                remember_saved(&folder, saved);
                api::list_projects(&token).ok()
            }
            Err(_) => None,
        };
        let _ = slint::invoke_from_event_loop(move || {
            if weak.upgrade().is_none() {
                return;
            }
            Shell::with(|shell, app| {
                let mut studio = shell.studio.borrow_mut();
                app.set_cloud_busy(false);
                match result {
                    Ok(saved) => {
                        if let Some(projects) = listing {
                            app.set_cloud_items(ModelRc::new(VecModel::from(rows(&projects))));
                        }
                        app.set_cloud_status(
                            format!(
                                "已保存到云端：{} · 版本 {} · {}",
                                saved.pid, saved.version, saved.updated_at
                            )
                            .into(),
                        );
                        studio.notify(&format!("已保存到云端（版本 {}）", saved.version), false);
                    }
                    Err(message) => {
                        studio.notify(&message, true);
                        app.set_cloud_status(message.into());
                    }
                }
                studio.publish_status(&app);
            });
        });
    });
}

/// 上传当前工程的成片：选一个本机视频，传到云端工程下，拿公网地址。
///
/// 工程还没保存过就先保存一次，成片才有 pid 可挂。
pub fn upload_current(weak: slint::Weak<App>) {
    let Some((folder, name, document)) = current_project() else {
        return reject(&weak, "请先打开一个工程，再上传成片");
    };
    let token = match token() {
        Ok(token) => token,
        Err(message) => return reject(&weak, &message),
    };
    let Some(file) = rfd::FileDialog::new()
        .set_title("选择要上传的成片（≤200MB）")
        .add_filter("视频", &["mp4", "mov", "m4v", "webm", "mkv"])
        .pick_file()
    else {
        return;
    };
    let size = std::fs::metadata(&file).map(|meta| meta.len()).unwrap_or(0);
    if size > MAX_UPLOAD {
        return reject(&weak, "成片超过 200MB 上限，请压缩后再上传");
    }
    let filename = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "成片.mp4".to_owned());
    Shell::with(|_shell, app| {
        app.set_cloud_open(true);
        app.set_cloud_busy(true);
        app.set_cloud_status(format!("正在上传成片：{filename}…").into());
        app.set_cloud_url("".into());
    });
    std::thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            let pid = match read_pid(Path::new(&folder)) {
                Some(pid) => pid,
                None => {
                    let saved = api::save_project(&token, None, &name, &document, None)?;
                    remember_saved(&folder, &saved);
                    saved.pid
                }
            };
            let bytes = std::fs::read(&file).map_err(|error| format!("成片读不出来：{error}"))?;
            api::upload_asset(&token, &pid, &filename, &bytes)
        })();
        let _ = slint::invoke_from_event_loop(move || {
            if weak.upgrade().is_none() {
                return;
            }
            Shell::with(|shell, app| {
                let mut studio = shell.studio.borrow_mut();
                app.set_cloud_busy(false);
                match result {
                    Ok(url) => {
                        app.set_cloud_url(url.as_str().into());
                        app.set_cloud_status(format!("成片已上传，已在工作台可查看：{url}").into());
                        studio.notify("成片已上传，已在工作台可查看", false);
                    }
                    Err(message) => {
                        studio.notify(&message, true);
                        app.set_cloud_status(message.into());
                    }
                }
                studio.publish_status(&app);
            });
        });
    });
}

/// 打开云端列表里的一行：下载工程 JSON，在本机建一个工程文件夹再打开。
pub fn open_selected(weak: slint::Weak<App>, pid: String, display_name: String) {
    let token = match token() {
        Ok(token) => token,
        Err(message) => return reject(&weak, &message),
    };
    let location = last_location();
    Shell::with(|_shell, app| {
        app.set_cloud_open(true);
        app.set_cloud_busy(true);
        app.set_cloud_status(format!("正在从云端打开「{display_name}」…").into());
    });
    std::thread::spawn(move || {
        let result = (|| -> Result<(String, PathBuf, String), String> {
            let (name, project) = api::load_project(&token, &pid)?;
            let folder = project_folder(&location, &name)?;
            let bytes = serde_json::to_vec_pretty(&project)
                .map_err(|error| format!("工程序列化失败：{error}"))?;
            std::fs::write(folder.join("concat.json"), bytes)
                .map_err(|error| format!("工程写不进本机：{error}"))?;
            let folder_text = folder.to_string_lossy().into_owned();
            remember_meta(&folder_text, &pid, &name);
            Ok((name, folder, folder_text))
        })();
        let _ = slint::invoke_from_event_loop(move || {
            if weak.upgrade().is_none() {
                return;
            }
            Shell::with(|shell, app| {
                let mut studio = shell.studio.borrow_mut();
                app.set_cloud_busy(false);
                match result {
                    Ok((name, folder, folder_text)) => {
                        let opened = studio.open_project_at(&folder_text);
                        match opened {
                            Ok(()) => {
                                app.set_cloud_open(false);
                                app.set_cloud_status(
                                    format!("已从云端打开「{name}」：{folder_text}").into(),
                                );
                                studio.notify(&format!("已从云端打开「{name}」"), false);
                            }
                            Err(error) => {
                                studio.notify(
                                    &format!("工程已下载到 {folder_text}，但打开失败：{error}"),
                                    true,
                                );
                                app.set_cloud_status(
                                    format!("工程已下载到 {folder_text}，但打开失败：{error}")
                                        .into(),
                                );
                            }
                        }
                        let _ = folder;
                        studio.refresh_art();
                        studio.publish(&app, &shell.models);
                    }
                    Err(message) => {
                        studio.notify(&message, true);
                        app.set_cloud_status(message.into());
                        studio.publish_status(&app);
                    }
                }
            });
        });
    });
}

// ---------------------------------------------------------------- 小工具

/// 当前登录的令牌；没有就是一句中文。
fn token() -> Result<String, String> {
    match concat_host::account::load() {
        Some(account) if !account.token.trim().is_empty() => Ok(account.token),
        _ => Err("请先登录星籁账号（首屏登录后重试）".to_owned()),
    }
}

/// 当前工程：文件夹、名字、要保存的文档。没有打开的工程就是 `None`。
fn current_project() -> Option<(String, String, serde_json::Value)> {
    let mut found = None;
    Shell::with(|shell, _app| {
        let studio = shell.studio.borrow();
        if let Some(session) = studio.session.as_ref() {
            found = Some((
                session.path().to_owned(),
                studio.project_name.clone(),
                session.document(),
            ));
        }
    });
    found
}

/// 恢复出来的工程放在哪里：上次新建工程的位置，退而求其次用户视频目录。
fn last_location() -> PathBuf {
    let mut remembered = None;
    Shell::with(|shell, _app| {
        remembered = shell.studio.borrow().prefs.last_project_location.clone();
    });
    if let Some(path) = remembered.filter(|path| !path.trim().is_empty()) {
        return PathBuf::from(path);
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        return PathBuf::from(home).join("Videos").join("星籁剪辑");
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// 一个还没被占用的工程文件夹，名字沿用 Concat 的目录名规则。
fn project_folder(location: &Path, name: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(location)
        .map_err(|error| format!("工程目录建不出来（{}）：{error}", location.display()))?;
    let base = concat_host::projects::folder_name(name);
    for attempt in 0..100 {
        let folder = if attempt == 0 {
            location.join(&base)
        } else {
            location.join(format!("{base}-{attempt}"))
        };
        if !folder.exists() {
            std::fs::create_dir_all(&folder)
                .map_err(|error| format!("工程文件夹建不出来：{error}"))?;
            return Ok(folder);
        }
    }
    Err("找不到空余的工程文件夹名".to_owned())
}

/// `cloud.json`：工程文件夹里记着它的云端 id，与工程格式无关。
fn cloud_meta(folder: &Path) -> PathBuf {
    folder.join("cloud.json")
}

fn read_pid(folder: &Path) -> Option<String> {
    let text = std::fs::read_to_string(cloud_meta(folder)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("pid")
        .and_then(serde_json::Value::as_str)
        .filter(|pid| !pid.trim().is_empty())
        .map(str::to_owned)
}

fn remember_meta(folder: &str, pid: &str, name: &str) {
    let path = cloud_meta(Path::new(folder));
    let value = serde_json::json!({
        "provider": "xinglaiai",
        "pid": pid,
        "name": name,
    });
    if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
        let _ = std::fs::write(path, bytes);
    }
}

fn remember_saved(folder: &str, saved: &api::SavedProject) {
    let path = cloud_meta(Path::new(folder));
    let value = serde_json::json!({
        "provider": "xinglaiai",
        "pid": saved.pid.clone(),
        "version": saved.version,
        "updated_at": saved.updated_at,
    });
    if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
        let _ = std::fs::write(path, bytes);
    }
}

fn rows(projects: &[api::CloudProject]) -> Vec<CloudItem> {
    projects
        .iter()
        .map(|project| CloudItem {
            pid: project.pid.as_str().into(),
            name: if project.name.trim().is_empty() {
                project.pid.as_str().into()
            } else {
                project.name.as_str().into()
            },
            size: human_size(project.size),
            version: i32::try_from(project.version).unwrap_or(1),
            updated: project.updated_at.as_str().into(),
        })
        .collect()
}

fn human_size(bytes: u64) -> slint::SharedString {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)).into()
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0).into()
    } else {
        format!("{bytes} B").into()
    }
}

/// 一句失败话：通知面板也写进云面板，再放一条 toast。
fn reject(weak: &slint::Weak<App>, message: &str) {
    let message = message.to_owned();
    Shell::with(|shell, app| {
        let mut studio = shell.studio.borrow_mut();
        studio.notify(&message, true);
        app.set_cloud_busy(false);
        app.set_cloud_status(message.into());
        studio.publish_status(&app);
    });
    let _ = weak;
}
