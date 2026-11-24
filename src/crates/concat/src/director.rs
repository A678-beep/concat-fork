// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 导演台（0.4.0）：独立窗口 + 同屏浮窗，成片 / 分镜 / 镜头状态板 / 批注。
//!
//! 导演台是同一个 Slint 组件（`DirectorBoard`）的两处实例：主窗口里的浮层，
//! 和 `DirectorWindow` 的独立窗口。两处共用这一份状态；导演的动作直连
//! `/filmgen/monitor/*`（通过 / 退回 / 批注 / 重拍），结果同时刷新主窗口的
//! 监视器面板，主窗口继续编辑不阻塞。关掉导演台只收窗口，主窗口不动。

use std::cell::RefCell;
use std::path::PathBuf;

use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::host::{self, Shell};
use crate::ui::*;

#[derive(Clone, Default)]
struct ShotRow {
    id: String,
    no: String,
    title: String,
    meta: String,
    status: String,
    level: String,
}

#[derive(Clone, Default)]
struct Row {
    label: String,
    detail: String,
    value: String,
    level: String,
}

#[derive(Default)]
struct DirectorState {
    project: String,
    board: Vec<ShotRow>,
    board_ids: Vec<String>,
    takes: Vec<Row>,
    take_ids: Vec<String>,
    notes: Vec<Row>,
    index: i32,
    shot_id: String,
    shot_title: String,
    shot_meta: String,
    board_line: String,
    progress_line: String,
    shot_line: String,
    take_line: String,
    note_text: String,
    note_line: String,
    retake_prompt: String,
    status_line: String,
    status_level: String,
    film_url: String,
    busy: bool,
    open: bool,
    pinned: bool,
    window_open: bool,
    fullscreen: bool,
    loaded: bool,
    epoch: u64,
    ticks: u32,
}

thread_local! {
    /// 从首页导航 / 菜单「导演台」打开：消息处理完之后再开窗口。
    static WANTED_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    static DIRECTOR: RefCell<DirectorState> = RefCell::new(DirectorState {
        project: "desktop-demo-037".to_owned(),
        index: -1,
        ..DirectorState::default()
    });

    /// 独立窗口的实例；Slint 的组件句柄不是 Send，只能留在 UI 线程。
    static WINDOW: RefCell<Option<DirectorWindow>> = const { RefCell::new(None) };
}

fn with<T>(body: impl FnOnce(&mut DirectorState) -> T) -> T {
    DIRECTOR.with(|cell| body(&mut cell.borrow_mut()))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn text_of(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return text.to_owned();
            }
        }
        if let Some(number) = value.get(*key).and_then(Value::as_i64) {
            return number.to_string();
        }
    }
    String::new()
}

fn num_of(value: &Value, keys: &[&str]) -> f64 {
    for key in keys {
        if let Some(number) = value.get(*key).and_then(Value::as_f64) {
            return number;
        }
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            if let Ok(number) = text.trim().parse::<f64>() {
                return number;
            }
        }
    }
    0.0
}

fn board_model(rows: &[ShotRow]) -> ModelRc<DirectorShot> {
    ModelRc::from(std::rc::Rc::new(VecModel::from(
        rows.iter()
            .map(|row| DirectorShot {
                id: row.id.clone().into(),
                no: row.no.clone().into(),
                title: row.title.clone().into(),
                meta: row.meta.clone().into(),
                status: row.status.clone().into(),
                level: row.level.clone().into(),
            })
            .collect::<Vec<_>>(),
    )))
}

fn rows_model(rows: &[Row]) -> ModelRc<DirectorRow> {
    ModelRc::from(std::rc::Rc::new(VecModel::from(
        rows.iter()
            .map(|row| DirectorRow {
                label: row.label.clone().into(),
                detail: row.detail.clone().into(),
                value: row.value.clone().into(),
                level: row.level.clone().into(),
            })
            .collect::<Vec<_>>(),
    )))
}

fn data_of(st: &DirectorState) -> DirectorData {
    DirectorData {
        open: st.open,
        pinned: st.pinned,
        project: st.project.clone().into(),
        index: st.index,
        board_line: st.board_line.clone().into(),
        progress_line: st.progress_line.clone().into(),
        shot_line: st.shot_line.clone().into(),
        shot_title: st.shot_title.clone().into(),
        shot_meta: st.shot_meta.clone().into(),
        take_line: st.take_line.clone().into(),
        note_text: st.note_text.clone().into(),
        note_line: st.note_line.clone().into(),
        retake_prompt: st.retake_prompt.clone().into(),
        status_line: st.status_line.clone().into(),
        status_level: st.status_level.clone().into(),
        film_url: st.film_url.clone().into(),
        busy: st.busy,
    }
}

/// 把状态写进主窗口和独立窗口（窗口开着才写）。
fn publish() {
    Shell::with(|_shell, app| {
        DIRECTOR.with(|cell| {
            let st = cell.borrow();
            app.set_director(data_of(&st));
            app.set_director_board(board_model(&st.board));
            app.set_director_takes(rows_model(&st.takes));
            app.set_director_notes(rows_model(&st.notes));
        });
    });
    WINDOW.with(|cell| {
        if let Some(window) = cell.borrow().as_ref() {
            DIRECTOR.with(|state| {
                let st = state.borrow();
                window.set_data(data_of(&st));
                window.set_board(board_model(&st.board));
                window.set_takes(rows_model(&st.takes));
                window.set_notes(rows_model(&st.notes));
            });
        }
    });
}

/// 从面板 / 菜单改项目 ID（不立刻取数，等刷新）。
pub fn set_project(project: String) {
    with(|st| {
        st.project = project;
        st.loaded = false;
    });
}

/// 请求打开导演台（消息处理完之后由 [`take_wanted_open`] 兑现）。
pub fn want_open() {
    WANTED_OPEN.with(|slot| slot.set(true));
}

/// 窗口消息处理完之后取走「要不要开导演台」。
pub fn take_wanted_open() -> bool {
    WANTED_OPEN.with(|slot| slot.replace(false))
}

/// 启动时写一份空的导演台（窗口没开，也没有网络）。
pub fn initialize(app: &App) {
    let data = DirectorData {
        open: false,
        pinned: false,
        project: "desktop-demo-037".into(),
        index: -1,
        board_line: "导演台没打开：从监视器面板或标题栏「影视专业 → 导演台」打开。".into(),
        progress_line: String::new().into(),
        shot_line: String::new().into(),
        shot_title: String::new().into(),
        shot_meta: String::new().into(),
        take_line: String::new().into(),
        note_text: String::new().into(),
        note_line: String::new().into(),
        retake_prompt: String::new().into(),
        status_line: String::new().into(),
        status_level: "info".into(),
        film_url: String::new().into(),
        busy: false,
    };
    app.set_director(data);
    app.set_director_board(board_model(&[]));
    app.set_director_takes(rows_model(&[]));
    app.set_director_notes(rows_model(&[]));
}

fn say(st: &mut DirectorState, line: impl Into<String>, level: &str) {
    st.status_line = line.into();
    st.status_level = level.to_owned();
}

/// 导演台打开：取状态板。
pub fn refresh(weak: slint::Weak<App>) {
    let project = with(|st| {
        st.open = true;
        st.project.clone()
    });
    let Some(app) = weak.upgrade() else { return };
    with(|st| {
        st.busy = true;
        st.board_line = format!("正在载入监视器状态板：{project}（真实 /filmgen/monitor/board）…");
    });
    publish();
    let _ = app;
    let epoch = with(|st| {
        st.epoch += 1;
        st.epoch
    });
    let project_for_job = project.clone();
    host::spawn(
        move || -> Result<(Value, Value), String> {
            let board = concat_host::pro::monitor_board(&project_for_job, "", 1)?;
            let progress = concat_host::pro::monitor_progress(&project_for_job).unwrap_or(Value::Null);
            Ok((board, progress))
        },
        move |_studio, _app, _models, result| {
            let stale = with(|st| st.epoch != epoch);
            if stale {
                return;
            }
            with(|st| {
                st.busy = false;
                st.loaded = true;
                match result {
                    Ok((board, progress)) => {
                        apply_board(st, &board, &progress);
                        say(
                            st,
                            format!(
                                "状态板已同步：{} 镜 / {}（真实 /filmgen/monitor/board）。",
                                st.board.len(),
                                st.progress_line
                            ),
                            "ok",
                        );
                    }
                    Err(error) => {
                        st.board_line = format!("状态板没取回来：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish();
        },
    );
}

fn find_url(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            if text.starts_with("http") {
                return text.to_owned();
            }
        }
    }
    value
        .get("project")
        .map(|project| find_url(project, keys))
        .unwrap_or_default()
}

fn apply_board(st: &mut DirectorState, board: &Value, progress: &Value) {
    st.board.clear();
    st.board_ids.clear();
    if let Some(project) = board.get("project") {
        let title = text_of(project, &["title"]);
        let shots = num_of(board, &["shots_total", "total"]) as i64;
        st.board_line = format!("项目 {title} · {shots} 镜（接口 /filmgen/monitor/board）");
    }
    if let Some(shots) = board.get("shots").and_then(Value::as_array) {
        for shot in shots {
            let id = text_of(shot, &["shot_id"]);
            let no = format!("S{:02}", num_of(shot, &["shot_no", "no"]) as i64);
            let title = text_of(shot, &["description", "title"]);
            let status = text_of(shot, &["status_label", "status"]);
            let takes = num_of(shot, &["take_count"]) as i64;
            let shot_type = text_of(shot, &["shot_type"]);
            let flags = shot
                .get("continuity")
                .map(|cont| {
                    format!(
                        "连续性 {} warn / {} fail",
                        num_of(cont, &["warn"]) as i64,
                        num_of(cont, &["fail"]) as i64
                    )
                })
                .unwrap_or_default();
            let level = match status.as_str() {
                "已通过" => "ok",
                "已退回" | "重拍" => "fail",
                "待审" | "生成中" => "warn",
                _ => "",
            };
            st.board_ids.push(id.clone());
            st.board.push(ShotRow {
                id,
                no,
                title,
                meta: format!("{status} · take {takes} · {shot_type} {flags}"),
                status,
                level: level.to_owned(),
            });
        }
    }
    let totals = progress.get("totals").cloned().unwrap_or(Value::Null);
    let pct = num_of(&totals, &["pct"]).max(num_of(progress, &["progress", "pct"]));
    let approved = num_of(&totals, &["approved_done"]);
    let total = num_of(&totals, &["shots"]);
    let budget = progress.get("budget").cloned().unwrap_or(Value::Null);
    st.progress_line = format!(
        "进度 {:.0}%（{:.0}/{:.0} 镜）· 预算 ¥{:.2}/¥{:.0}",
        pct,
        approved,
        total,
        num_of(&budget, &["spent_cny"]),
        num_of(&budget, &["cap_cny"]),
    );
    let film = find_url(progress, &["film_url", "video_url", "final_url"]);
    if !film.is_empty() {
        st.film_url = film;
    }
    if st.index >= st.board.len() as i32 {
        st.index = st.board.len() as i32 - 1;
    }
}

/// 选中一镜：取 takes / 批注。
fn load_shot(weak: slint::Weak<App>, index: usize) {
    let id = with(|st| {
        st.index = index as i32;
        st.board_ids.get(index).cloned().unwrap_or_default()
    });
    if id.is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    publish();
    host::spawn(
        move || concat_host::pro::monitor_shot(&id),
        move |_studio, _app, _models, result| {
            with(|st| match result {
                Ok(value) => {
                    let shot = value.get("shot").cloned().unwrap_or(Value::Null);
                    st.shot_id = text_of(&shot, &["shot_id"]);
                    st.shot_title = format!(
                        "{} · {}",
                        st.shot_id,
                        text_of(&shot, &["description", "title"])
                    );
                    st.shot_meta = format!(
                        "第 {} 集 · {} 镜 · 景别 {} · take {} · {}",
                        num_of(&shot, &["episode_no"]) as i64,
                        num_of(&shot, &["shot_no"]) as i64,
                        text_of(&shot, &["shot_type"]),
                        num_of(&shot, &["take_count"]) as i64,
                        text_of(&shot, &["status_label"]),
                    );
                    st.takes.clear();
                    st.take_ids.clear();
                    if let Some(takes) = shot.get("takes").and_then(Value::as_array) {
                        for take in takes {
                            let take_id = text_of(take, &["take_id"]);
                            let status = text_of(take, &["status"]);
                            st.take_ids.push(take_id.clone());
                            st.takes.push(Row {
                                label: format!(
                                    "T{:02} {}",
                                    num_of(take, &["take_no"]) as i64,
                                    match status.as_str() {
                                        "accepted" => "已接受",
                                        "rejected" => "已退回",
                                        "hold" => "待定",
                                        _ => "待审",
                                    }
                                ),
                                detail: format!(
                                    "{} · {:.1}s · {}",
                                    text_of(take, &["model"]),
                                    num_of(take, &["duration_ms"]) / 1000.0,
                                    take_id
                                ),
                                value: String::new(),
                                level: if status == "accepted" { "ok" } else { "" }.to_owned(),
                            });
                        }
                    }
                    st.notes.clear();
                    if let Some(notes) = shot.get("notes").and_then(Value::as_array) {
                        for note in notes {
                            st.notes.push(Row {
                                label: text_of(note, &["severity"]),
                                detail: format!(
                                    "{}（{}）",
                                    text_of(note, &["content"]),
                                    text_of(note, &["created_at"])
                                ),
                                value: String::new(),
                                level: if note.get("resolved").and_then(Value::as_bool) == Some(true)
                                {
                                    "ok"
                                } else {
                                    ""
                                }
                                .to_owned(),
                            });
                        }
                    }
                    st.shot_line = format!(
                        "镜头 {}：{} 条 take / {} 条批注；通过 / 退回 / 批注 / 重拍都写平台审计日志。",
                        st.shot_id,
                        st.takes.len(),
                        st.notes.len()
                    );
                    st.take_line = format!(
                        "已登记 {} 条批注 / {} 条 take；重拍只登记，由导演编排器消费。",
                        st.notes.len(),
                        st.takes.len()
                    );
                    say(st, "单镜详情已取回（真实 /filmgen/monitor/shot/{id}）。", "ok");
                }
                Err(error) => say(st, error, "warn"),
            });
            publish();
        },
    );
    let _ = app;
}

/// 通过 / 退回：写平台审计日志。
fn decide(weak: slint::Weak<App>, decision: &str) {
    let (shot, take) = with(|st| {
        let take = st.take_ids.first().cloned().unwrap_or_default();
        (st.shot_id.clone(), take)
    });
    if shot.is_empty() || take.is_empty() {
        with(|st| {
            say(
                st,
                "这一镜还没有 take：先登记 take 再通过 / 退回（或直接「登记重拍」）。",
                "warn",
            );
        });
        publish();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    let label = if decision == "accept" { "接受" } else { "退回" };
    with(|st| {
        st.busy = true;
        say(st, format!("正在{label}镜头 {shot} 的 take {take}…"), "info");
    });
    publish();
    let _ = app;
    let decision = decision.to_owned();
    host::spawn(
        move || concat_host::pro::monitor_decision(&shot, &take, &decision),
        move |_studio, _app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(_) => {
                        let line = format!("take 已{label}（平台审计日志已记录）。");
                        st.take_line = line.clone();
                        say(st, line, "ok");
                    }
                    Err(error) => say(st, error, "warn"),
                }
            });
            publish();
            if let Some(app) = weak.upgrade() {
                refresh(app.as_weak());
            }
        },
    );
}

fn add_note(weak: slint::Weak<App>) {
    let (shot, content) = with(|st| (st.shot_id.clone(), st.note_text.trim().to_owned()));
    if shot.is_empty() || content.is_empty() {
        with(|st| say(st, "先选一镜、写一条批注。", "warn"));
        publish();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    with(|st| {
        st.busy = true;
        say(st, "正在写批注（真实 /filmgen/monitor/shot/{id}/notes）…", "info");
    });
    publish();
    let _ = app;
    host::spawn(
        move || {
            concat_host::pro::monitor_note(
                &shot,
                &json!({ "content": content, "kind": "text", "severity": "medium" }),
            )
        },
        |_studio, _app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(_) => {
                        st.note_text.clear();
                        st.note_line = "批注已写入平台。".to_owned();
                        say(st, "批注已写入平台审计日志。", "ok");
                    }
                    Err(error) => say(st, error, "warn"),
                }
            });
            publish();
        },
    );
}

fn add_retake(weak: slint::Weak<App>) {
    let (shot, prompt) = with(|st| (st.shot_id.clone(), st.retake_prompt.trim().to_owned()));
    if shot.is_empty() {
        with(|st| say(st, "先选一镜再登记重拍。", "warn"));
        publish();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    with(|st| {
        st.busy = true;
        say(st, "正在登记重拍（真实 /filmgen/monitor/shot/{id}/retake）…", "info");
    });
    publish();
    let _ = app;
    let extra = if prompt.is_empty() {
        "导演台要求重拍".to_owned()
    } else {
        prompt
    };
    host::spawn(
        move || {
            concat_host::pro::monitor_retake(
                &shot,
                &json!({
                    "reason": ["导演台重拍"],
                    "extra_prompt": extra,
                    "tier": "standard",
                }),
            )
        },
        |_studio, _app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(_) => {
                        st.retake_prompt.clear();
                        st.take_line = "已登记重拍：由导演编排器消费，不自动触发真实生成。".to_owned();
                        say(st, "重拍已登记（平台审计日志已记录）。", "ok");
                    }
                    Err(error) => say(st, error, "warn"),
                }
            });
            publish();
        },
    );
}

/// 打开独立窗口（事件循环线程，按钮回调里调用）。
pub fn open_window(weak: slint::Weak<App>) {
    let existing = WINDOW.with(|cell| cell.borrow().is_some());
    if !existing {
        match DirectorWindow::new() {
            Ok(window) => {
                let weak_for_actions = weak.clone();
                window.on_action(move |action, index, text| {
                    action_of(weak_for_actions.clone(), action.to_string(), index, text.to_string());
                });
                let weak_for_close = weak.clone();
                window.on_closed(move || {
                    close_all(weak_for_close.clone());
                });
                let weak_for_request = weak.clone();
                window.window().on_close_requested(move || {
                    close_all(weak_for_request.clone());
                    slint::CloseRequestResponse::HideWindow
                });
                window.show().ok();
                WINDOW.with(|cell| *cell.borrow_mut() = Some(window));
            }
            Err(error) => {
                with(|st| {
                    say(
                        st,
                        format!("独立窗口打不开（{error}）；可以用同屏浮窗。"),
                        "warn",
                    );
                });
                publish();
                return;
            }
        }
    }
    with(|st| {
        st.open = true;
        st.window_open = true;
        st.pinned = false;
        st.status_line.clear();
    });
    publish();
    let Some(app) = weak.upgrade() else { return };
    refresh(app.as_weak());
}

/// 同屏浮窗开关。
pub fn toggle_pin(weak: slint::Weak<App>) {
    let pinned = with(|st| {
        st.pinned = !st.pinned;
        st.open = st.pinned;
        if st.pinned {
            st.status_line.clear();
        }
        st.pinned
    });
    publish();
    if pinned {
        refresh(weak);
    }
}

/// 关闭导演台（独立窗口 + 浮窗都收；主窗口继续编辑）。
pub fn close_all(weak: slint::Weak<App>) {
    WINDOW.with(|cell| {
        if let Some(window) = cell.borrow().as_ref() {
            window.hide().ok();
        }
    });
    with(|st| {
        st.open = false;
        st.pinned = false;
        st.window_open = false;
        st.fullscreen = false;
        st.epoch += 1;
    });
    publish();
    let _ = weak;
}

/// 导演台上的一个手势。
pub fn action_of(weak: slint::Weak<App>, action: String, index: i32, text: String) {
    match action.as_str() {
        "project" => {
            with(|st| {
                st.project = text;
                st.loaded = false;
            });
            refresh(weak);
        }
        "reload" => refresh(weak),
        "select" => load_shot(weak, index.max(0) as usize),
        "pass" => decide(weak, "accept"),
        "reject" => decide(weak, "reject"),
        "note-text" => with(|st| st.note_text = text),
        "note" => add_note(weak),
        "retake-prompt" => with(|st| st.retake_prompt = text),
        "retake" => add_retake(weak),
        "film" => {
            let url = with(|st| st.film_url.clone());
            if !url.is_empty() {
                if let Err(error) = opener::open(&url) {
                    with(|st| say(st, format!("打开成片失败：{error}"), "warn"));
                    publish();
                }
            }
        }
        "web" => {
            let url = concat_host::pro::web_url("monitor");
            if let Err(error) = opener::open(url.as_str()) {
                with(|st| say(st, format!("打开网页版失败：{error}"), "warn"));
                publish();
            }
        }
        "fullscreen" => {
            let fullscreen = with(|st| {
                st.fullscreen = !st.fullscreen;
                st.fullscreen
            });
            WINDOW.with(|cell| {
                if let Some(window) = cell.borrow().as_ref() {
                    window.window().set_fullscreen(fullscreen);
                    window.show().ok();
                }
            });
            with(|st| {
                say(
                    st,
                    if fullscreen {
                        "已切到全屏（Esc / 再点一次还原）"
                    } else {
                        "已退出全屏"
                    },
                    "info",
                );
            });
            publish();
        }
        "pin-close" => close_all(weak),
        "close-window" => close_all(weak),
        _ => {}
    }
}

/// Timer：导演台开着时每秒同步一次（主窗口编辑 / 平台状态都会回流）。
pub fn tick(weak: slint::Weak<App>) {
    let (open, ticks) = with(|st| {
        st.ticks = st.ticks.wrapping_add(1);
        (st.open, st.ticks)
    });
    if !open || ticks % 3 != 0 {
        return;
    }
    // 只在没有请求在飞的时候轻刷一次状态板。
    let busy = with(|st| st.busy);
    if busy {
        return;
    }
    let project = with(|st| st.project.clone());
    let epoch = with(|st| {
        st.epoch += 1;
        st.epoch
    });
    let _ = weak;
    host::spawn(
        move || -> Result<(Value, Value), String> {
            let board = concat_host::pro::monitor_board(&project, "", 1)?;
            let progress = concat_host::pro::monitor_progress(&project).unwrap_or(Value::Null);
            Ok((board, progress))
        },
        move |_studio, _app, _models, result| {
            let stale = with(|st| st.epoch != epoch);
            if stale {
                return;
            }
            if let Ok((board, progress)) = result {
                with(|st| {
                    apply_board(st, &board, &progress);
                    st.loaded = true;
                });
                publish();
            }
        },
    );
}

/// 给「监视器面板」用的摘要（一行说明）。
pub fn summary() -> (String, String, String) {
    with(|st| {
        (
            st.project.clone(),
            st.board_line.clone(),
            st.status_line.clone(),
        )
    })
}

/// 让编译器知道若干类型用过（状态行里放时间戳）。
#[allow(dead_code)]
fn _stamp() -> (i64, PathBuf) {
    (now_secs(), PathBuf::new())
}
