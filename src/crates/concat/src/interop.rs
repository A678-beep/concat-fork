// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 剪映草稿互通与 XML 导出：文件菜单的几行，与「导出为可编辑草稿」面板。
//!
//! 真正把工程写成别的编辑器能开的文件，在 `concat_project::capcut_export`
//! （剪映 5.x 草稿）与 `concat_project::fcpxml`（FCPXML 1.9 / Premiere
//! xmeml）；这里只做窗口这一半：在事件循环线程上取当前工程，问用户去哪
//! 儿，写完把结果写回 Slint，并给一条通知。
//!
//! 导入走 `concat_host::capcut`：剪映与 CapCut 的草稿文件夹都在它找的范
//! 围里，读出视频片段与字幕做成一个新工程——不是倒进当前时间线，因为草
//! 稿有自己的轨道与画幅，覆盖当前剪辑会丢掉正在做的活。

use crate::host::{self, Shell};
use crate::platform;
use crate::ui::App;

/// 一次导出要的工程快照：工程本身、它所在的文件夹、它的名字。
struct Snapshot {
    project: concat_project::Project,
    folder: String,
    name: String,
}

/// 关掉面板。上次的导出结果与文件夹留着，再打开还能「打开文件夹」。
pub fn close(weak: slint::Weak<App>) {
    if let Some(app) = weak.upgrade() {
        app.set_interop_open(false);
    }
}

/// 导出剪映草稿：问一个文件夹，在它里面建 `<工程名>-剪映草稿`，然后把
/// 结果面板打开——"打开文件夹"与"一键同步"就在那儿。
pub fn export_jianying(weak: slint::Weak<App>) {
    let Some(snapshot) = snapshot() else {
        return refuse(&weak, "先打开一个工程，再导出剪映草稿");
    };
    busy(&weak, "正在选择导出文件夹…");
    let start = export_start(&snapshot);
    host::after_dialog(
        move || platform::pick_folder("选择剪映草稿的导出文件夹", &start),
        move |studio, app, _models, folder| {
            let target = concat_project::capcut_export::free_folder(
                &folder,
                &format!("{}-剪映草稿", safe_name(&snapshot.name)),
            );
            match concat_project::capcut_export::write(&snapshot.project, &snapshot.name, &target) {
                Ok(written) => {
                    let mut status = format!(
                        "已导出剪映草稿：{}\n{} 个片段 · {} 条字幕 · 时长 {:.2} 秒 · 共 {} 个文件",
                        target.display(),
                        written.clips,
                        written.titles,
                        written.duration,
                        written.files.len()
                    );
                    if !written.notes.is_empty() {
                        status.push_str("\n降级说明：");
                        for note in &written.notes {
                            status.push_str("\n· ");
                            status.push_str(note);
                        }
                    }
                    status.push_str("\n可导入剪映二次编辑；部分特效/关键帧可能丢失。");
                    panel(&app);
                    app.set_interop_folder(target.to_string_lossy().into_owned().into());
                    app.set_interop_status(status.into());
                    app.set_interop_busy(false);
                    studio.notify(&format!("剪映草稿已导出：{}", target.display()), false);
                }
                Err(error) => {
                    panel(&app);
                    app.set_interop_busy(false);
                    failed(&app, &error);
                    studio.notify(&error, true);
                }
            }
        },
    );
}

/// 导出 FCPXML 1.9 与 Premiere XML（外加一份 SRT 字幕）到选中的文件夹。
pub fn export_xml(weak: slint::Weak<App>) {
    let Some(snapshot) = snapshot() else {
        return refuse(&weak, "先打开一个工程，再导出 XML");
    };
    if snapshot.project.active().clips.is_empty() {
        return refuse(&weak, "时间线还是空的：先放几段素材再导出 XML");
    }
    busy(&weak, "正在选择导出文件夹…");
    let start = export_start(&snapshot);
    host::after_dialog(
        move || platform::pick_folder("选择 FCPXML / Premiere XML 的导出文件夹", &start),
        move |studio, app, _models, folder| {
            match concat_project::fcpxml::write(&snapshot.project, &snapshot.name, &folder) {
                Ok(written) => {
                    let names: Vec<String> = written
                        .files
                        .iter()
                        .filter_map(|file| file.file_name())
                        .map(|name| name.to_string_lossy().into_owned())
                        .collect();
                    let mut status = format!(
                        "已导出 XML：{}\n{} 个片段 · {} 帧 · {}",
                        folder.display(),
                        written.clips,
                        written.frames,
                        names.join("、")
                    );
                    for note in &written.notes {
                        status.push_str("\n· ");
                        status.push_str(note);
                    }
                    status.push_str(
                        "\n用于 PR / 达芬奇二次编辑：复杂特效、关键帧与剪映专属效果不保证。",
                    );
                    panel(&app);
                    app.set_interop_folder(folder.to_string_lossy().into_owned().into());
                    app.set_interop_status(status.into());
                    app.set_interop_busy(false);
                    studio.notify(&format!("XML 已导出：{}", folder.display()), false);
                }
                Err(error) => {
                    panel(&app);
                    app.set_interop_busy(false);
                    failed(&app, &error);
                    studio.notify(&error, true);
                }
            }
        },
    );
}

/// 在文件管理器里打开上次导出的文件夹。
pub fn reveal(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    let folder = app.get_interop_folder().to_string();
    if folder.trim().is_empty() {
        app.set_interop_status("还没有导出过：先导出一份草稿或 XML。".into());
        return;
    }
    if let Err(error) = platform::reveal(&folder) {
        app.set_interop_status(format!("打不开文件夹：{error}").into());
    }
}

/// 一键把草稿写进剪映的草稿目录：写一个 `<工程名>-剪映草稿` 子文件夹，
/// 剪映下次打开草稿列表就能看见它。剪映 6.x 的草稿加密，这份是 5.x 的
/// 明文格式，说清楚比假装成功好。
pub fn sync_jianying(weak: slint::Weak<App>) {
    let Some(snapshot) = snapshot() else {
        return refuse(&weak, "先打开一个工程，再同步到剪映");
    };
    let Some(app) = weak.upgrade() else { return };
    let Some(root) = concat_host::capcut::jianying_drafts_folder() else {
        app.set_interop_status(
            "没找到剪映的草稿目录（%LOCALAPPDATA%\\JianyingPro\\User Data\\Projects\\com.lveditor.draft）。\
             装好剪映并新建过一个草稿后重试，或改用「导出剪映草稿」再手动放进草稿目录。"
                .into(),
        );
        return;
    };
    busy(&weak, "正在写入剪映草稿目录…");
    let target = concat_project::capcut_export::free_folder(
        &root,
        &format!("{}-剪映草稿", safe_name(&snapshot.name)),
    );
    match concat_project::capcut_export::write(&snapshot.project, &snapshot.name, &target) {
        Ok(written) => {
            app.set_interop_folder(target.to_string_lossy().into_owned().into());
            app.set_interop_status(
                format!(
                    "已同步到剪映草稿目录：{}\n{} 个片段 · {} 条字幕。打开剪映专业版（5.9 或更早）即可在草稿列表看到；\
                     6.x 的草稿加密，读不了这份明文草稿。",
                    target.display(),
                    written.clips,
                    written.titles
                )
                .into(),
            );
            app.set_interop_busy(false);
            Shell::with(|shell, _app| {
                shell
                    .studio
                    .borrow_mut()
                    .notify("已同步到剪映草稿目录，打开剪映即可看到", false);
            });
        }
        Err(error) => {
            failed(&app, &error);
            Shell::with(|shell, _app| {
                shell.studio.borrow_mut().notify(&error, true);
            });
        }
    }
}

/// 导入剪映 / CapCut 草稿：选草稿文件夹，读成新工程再打开。
pub fn import_draft() {
    let drafts = concat_host::capcut::drafts_folder()
        .map(|folder| folder.to_string_lossy().into_owned())
        .or_else(|| {
            let mut found = None;
            Shell::with(|shell, _app| {
                found = shell.studio.borrow().prefs.last_project_location.clone();
            });
            found
        })
        .unwrap_or_default();
    host::after_dialog(
        move || platform::pick_folder("选择剪映 / CapCut 草稿文件夹", &drafts),
        move |studio, app, _models, folder| {
            if !concat_host::capcut::is_draft(&folder) {
                let message = format!(
                    "{} 不是剪映/CapCut 草稿文件夹：里面没有 draft_content.json 或 draft_info.json",
                    folder.display()
                );
                app.set_interop_status(message.clone().into());
                studio.notify(&message, true);
                return;
            }
            let location = studio
                .prefs
                .last_project_location
                .clone()
                .unwrap_or_else(|| studio.start.location.clone());
            match concat_host::capcut::import(&folder, &location) {
                Ok(imported) => {
                    let left = imported.notes.len();
                    match studio.open_project(imported.project) {
                        Ok(()) => {
                            let said = if left == 0 {
                                "已把剪映草稿导入为新工程".to_owned()
                            } else {
                                format!(
                                    "已把剪映草稿导入为新工程，{left} 类内容未带过来（见日志）"
                                )
                            };
                            app.set_interop_status(
                                format!("{said}\n导入的是新工程，当前剪辑没有被覆盖。").into(),
                            );
                            studio.notify(&said, false);
                        }
                        Err(error) => {
                            app.set_interop_status(format!("打开了但没进来：{error}").into());
                            studio.notify(&error, true);
                        }
                    }
                }
                Err(error) => {
                    app.set_interop_status(format!("这份草稿读不了：{error}").into());
                    studio.notify(&error, true);
                }
            }
        },
    );
}

// ── the seams ──

/// 面板打开时共有的那几项：有没有装剪映，和一句提示。上次的导出结果与
/// 文件夹留着，再打开还能「打开文件夹」。
fn panel(app: &App) {
    let jianying = concat_host::capcut::jianying_drafts_folder().is_some();
    app.set_interop_open(true);
    app.set_interop_jianying(jianying);
    app.set_interop_hint(if jianying {
        "检测到剪映：导出后可以直接同步到剪映草稿目录，打开剪映就能看到。"
    } else {
        "未检测到剪映专业版：导出后手动把草稿文件夹放进剪映的草稿目录即可。"
    }
    .into());
}

/// 当前工程的一份快照；启动屏上没有打开的工程就是 `None`。
fn snapshot() -> Option<Snapshot> {
    let mut found = None;
    Shell::with(|shell, _app| {
        let studio = shell.studio.borrow();
        if let Some(session) = studio.session.as_ref() {
            found = Some(Snapshot {
                project: studio.project().clone(),
                folder: session.path().to_owned(),
                name: studio.project_name.clone(),
            });
        }
    });
    found
}

/// 文件对话框从哪儿开：工程自己的文件夹，退而求其次上次新建工程的位置。
fn export_start(snapshot: &Snapshot) -> String {
    if !snapshot.folder.trim().is_empty() {
        return snapshot.folder.clone();
    }
    let mut found = None;
    Shell::with(|shell, _app| {
        found = shell.studio.borrow().prefs.last_project_location.clone();
    });
    found.unwrap_or_default()
}

/// 文件名里不能有的字符换成下划线，空名给一个默认的。
fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| match character {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' | '\t' => '_',
            other => other,
        })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "星籁剪辑".to_owned()
    } else {
        cleaned.chars().take(60).collect()
    }
}

fn busy(weak: &slint::Weak<App>, status: &str) {
    if let Some(app) = weak.upgrade() {
        app.set_interop_busy(true);
        app.set_interop_status(status.into());
    }
}

fn refuse(weak: &slint::Weak<App>, message: &str) {
    log::debug!("interop: {message}");
    if let Some(app) = weak.upgrade() {
        panel(&app);
        app.set_interop_busy(false);
        app.set_interop_status(message.into());
    }
}

/// 失败：面板上一句；调用者再给一条通知（它手里有 Studio 的借用）。
fn failed(app: &App, message: &str) {
    log::error!("interop: {message}");
    app.set_interop_busy(false);
    app.set_interop_status(format!("导出失败：{message}").into());
}
