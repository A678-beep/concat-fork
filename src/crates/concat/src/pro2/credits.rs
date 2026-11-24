// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 片头片尾（0.4.0）：名单编辑 → 模板 → 预览 → 生成 → 导出。
//!
//! 平台接口 `/credits/titles/*`：模板 / 示例名单 / 同步预览帧 / 异步渲染 /
//! 任务进度 / 排版 QC / zip 导出。项目 JSON 只在这一层拼，不改平台任何
//! 默认值；AI 声明与许可证号 / 备案号字段默认关，开了才写进项目。

use std::path::PathBuf;

use serde_json::{json, Value};

use super::{
    absolute, config_dir, money, num_of, open_url, publish_now, row, say, text_of, with,
    Pro2State, TAB_CREDITS,
};
use crate::host::{self, Shell};
use crate::ui::App;

/// 一页模板 / 示例名单 / 引擎状态。
pub fn load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        || -> Result<(Value, Value, Value), String> {
            let templates = concat_host::pro2::credits_templates()?;
            let demo = concat_host::pro2::credits_demo().unwrap_or(Value::Null);
            let health = concat_host::pro2::credits_health().unwrap_or(Value::Null);
            Ok((templates, demo, health))
        },
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok((templates, demo, health)) => {
                        remember_styles(&templates);
                        apply_templates(st, &templates);
                        if !demo.is_null() {
                            apply_demo(st, &demo);
                        }
                        let engine = text_of(&health, &["version", "engine"]);
                        let count = st.cr_styles.len();
                        st.cr_line = format!(
                            "已取回 {count} 种模板与示例名单（真实 /credits/titles/templates + /demo{}）。",
                            if engine.is_empty() {
                                String::new()
                            } else {
                                format!("；引擎 {engine}")
                            }
                        );
                        say(st, "片头片尾接口在线：模板 / 示例名单已载入，可直接预览或生成。", "ok");
                    }
                    Err(error) => {
                        st.cr_line = "模板没取回来：可重试，或打开网页版片头片尾。".to_owned();
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn apply_templates(st: &mut Pro2State, value: &Value) {
    st.cr_styles.clear();
    if let Some(templates) = value.get("templates").and_then(Value::as_array) {
        for template in templates {
            let label = text_of(template, &["label", "label_en", "id"]);
            st.cr_styles.push(label);
        }
    }
    st.cr_style = st.cr_style.clamp(0, (st.cr_styles.len() as i32 - 1).max(0));
}

fn billing_label(billing: &str) -> &'static str {
    match billing {
        "lead" => "领衔主演",
        "special" => "特别出演",
        "guest" => "特邀出演",
        "friendly" => "友情出演",
        "voice" => "配音",
        "support" => "联合主演",
        _ => "主演",
    }
}

fn apply_demo(st: &mut Pro2State, value: &Value) {
    let Some(project) = value.get("project") else { return };
    st.cr_demo = Some(project.clone());
    let title = project
        .get("title")
        .map(|title| text_of(title, &["zh", "en", "name"]))
        .unwrap_or_default();
    if !title.is_empty() {
        st.cr_title = title;
    }
    if let Some(year) = project.get("year").and_then(Value::as_i64) {
        st.cr_year = year.to_string();
    }
    let studios: Vec<String> = project
        .get("studios")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.get("name")
                        .map(|name| text_of(name, &["zh", "en"]))
                        .unwrap_or_default()
                })
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if !studios.is_empty() {
        st.cr_studios = studios.join("、");
    }
    let producers: Vec<String> = project
        .get("producers")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.get("name")
                        .map(|name| text_of(name, &["zh", "en"]))
                        .unwrap_or_default()
                })
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if !producers.is_empty() {
        st.cr_producers = producers.join("、");
    }
    let director = project
        .get("director")
        .map(|name| text_of(name, &["zh", "en", "name"]))
        .unwrap_or_default();
    if !director.is_empty() {
        st.cr_director = director;
    }
    if let Some(cast) = project.get("cast").and_then(Value::as_array) {
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for item in cast {
            let actor = item
                .get("actor")
                .map(|actor| text_of(actor, &["zh", "en"]))
                .unwrap_or_default();
            if actor.is_empty() {
                continue;
            }
            let billing = text_of(item, &["billing"]);
            let label = billing_label(&billing).to_owned();
            match groups.iter_mut().find(|(group, _)| *group == label) {
                Some((_, names)) => names.push(actor),
                None => groups.push((label, vec![actor])),
            }
        }
        let text = groups
            .iter()
            .map(|(label, names)| format!("{label}={}", names.join("、")))
            .collect::<Vec<_>>()
            .join("；");
        if !text.is_empty() {
            st.cr_cast = text;
        }
    }
    if let Some(crew) = project.get("crew").and_then(Value::as_array) {
        let mut lines = Vec::new();
        for item in crew {
            let dept = item
                .get("dept")
                .map(|dept| text_of(dept, &["zh", "en"]))
                .unwrap_or_default();
            let role = item
                .get("role")
                .map(|role| text_of(role, &["zh", "en"]))
                .unwrap_or_default();
            let names: Vec<String> = item
                .get("names")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .map(|name| text_of(name, &["zh", "en"]))
                        .filter(|name| !name.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if dept.is_empty() && role.is_empty() {
                continue;
            }
            lines.push(format!("{dept}={role}：{}", names.join("、")));
        }
        if !lines.is_empty() {
            st.cr_crew = lines.join("\n");
        }
    }
}

/// 名单里的一行「甲、乙」拆成名字数组。
fn split_names(text: &str) -> Vec<String> {
    text.split(['、', ',', '，', ';', '；', '/', '|'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

fn zh(name: &str) -> Value {
    json!({ "zh": name, "en": "" })
}

/// 面板上编辑的名单 → 平台项目 JSON（只给 norm_project 的宽松字段）。
fn project_json(st: &Pro2State) -> Value {
    let title = if st.cr_title.trim().is_empty() {
        "未命名影片"
    } else {
        st.cr_title.trim()
    };
    let studios: Vec<Value> = st
        .cr_studios
        .split(['、', ',', '，', ';', '；'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .enumerate()
        .map(|(index, name)| {
            json!({
                "name": zh(name),
                "role": if index == 0 { "presenter" } else { "co_presenter" },
            })
        })
        .collect();
    let producers: Vec<Value> = split_names(&st.cr_producers)
        .iter()
        .map(|name| {
            json!({
                "name": zh(name),
                "title": json!({ "zh": "制片人", "en": "Producer" }),
            })
        })
        .collect();
    let mut cast: Vec<Value> = Vec::new();
    let mut order = 1;
    for group in st.cr_cast.split([';', '；']) {
        let group = group.trim();
        if group.is_empty() {
            continue;
        }
        let (label, names) = match group.split_once(['=', '＝', ':']) {
            Some((label, names)) => (label.trim(), names),
            None => ("主演", group),
        };
        let billing = match label {
            "领衔主演" => "lead",
            "特别出演" => "special",
            "特邀出演" | "特邀" => "guest",
            "友情出演" | "友情" => "friendly",
            "配音" => "voice",
            "联合主演" => "support",
            _ => "lead",
        };
        for name in split_names(names) {
            cast.push(json!({
                "actor": zh(&name),
                "role": zh(""),
                "billing_order": order,
                "billing": billing,
            }));
            order += 1;
        }
    }
    let mut crew: Vec<Value> = Vec::new();
    for line in st.cr_crew.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // 部门=职位：姓名1、姓名2  或  部门：职位：姓名
        let (dept, rest) = match line.split_once(['=', '＝']) {
            Some((dept, rest)) => (dept.trim(), rest.trim()),
            None => ("制作", line),
        };
        let (role, names) = match rest.split_once([':', '：']) {
            Some((role, names)) => (role.trim(), names.trim()),
            None => ("", rest),
        };
        let names: Vec<Value> = split_names(names).iter().map(|name| zh(name)).collect();
        if names.is_empty() {
            continue;
        }
        crew.push(json!({
            "dept": zh(dept),
            "role": zh(role),
            "names": names,
        }));
    }
    let mut settings = json!({
        "fps": 24,
        "opening_duration": 8.0,
        "show_license": !st.cr_license.trim().is_empty(),
        "show_ai_notice": st.cr_ai,
        "license_position": "bottom_left",
        "logo_mode": "placeholder",
    });
    if st.cr_style < st.cr_styles.len() as i32 {
        // 模板 id 由模板序号的英文 id 决定；见 style_id()。
        if let Some(style) = style_id(st) {
            settings["style"] = json!(style);
        }
    }
    let mut project = json!({
        "project_id": format!("cpr_d{}", super::now_secs()),
        "title": { "zh": title, "en": "" },
        "title_short": { "zh": title, "en": "" },
        "year": st.cr_year.trim().parse::<i64>().unwrap_or(2026),
        "studios": studios,
        "producers": producers,
        "director": zh(st.cr_director.trim()),
        "cast": cast,
        "crew": crew,
        "settings": settings,
        "license_no": st.cr_license.trim(),
        "filing_no": st.cr_record.trim(),
        "program_no": st.cr_record.trim(),
        "ai_generated": st.cr_ai,
    });
    if st.cr_ai {
        project["ai_statement"] = json!({
            "zh": "本片部分内容由 AI 生成 / 部分环节使用人工智能辅助制作。",
            "en": "Some content in this film was generated or assisted by artificial intelligence.",
        });
    }
    project
}

/// 模板序号 → 平台模板 id（templates 响应里的 id，按顺序缓存）。
thread_local! {
    static STYLE_IDS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn remember_styles(value: &Value) {
    let mut ids = Vec::new();
    if let Some(templates) = value.get("templates").and_then(Value::as_array) {
        for template in templates {
            ids.push(text_of(template, &["id"]));
        }
    }
    STYLE_IDS.with(|cell| *cell.borrow_mut() = ids);
}

fn style_id(st: &Pro2State) -> Option<String> {
    STYLE_IDS.with(|cell| {
        cell.borrow()
            .get(st.cr_style.clamp(0, i32::MAX) as usize)
            .filter(|id| !id.is_empty())
            .cloned()
    })
}

fn current_style(st: &Pro2State) -> String {
    style_id(st).unwrap_or_else(|| "gold_black".to_owned())
}

fn resolution(st: &Pro2State) -> &'static str {
    match st.cr_res {
        1 => "4k",
        2 => "vertical",
        3 => "2.39",
        _ => "1080p",
    }
}

/// 预览：同步出 5 帧，把第一帧下载到本机，画在面板里。
fn preview(weak: slint::Weak<App>) {
    let (body, style, res) = with(|st| {
        st.busy = true;
        st.cr_preview_line = "正在出预览帧（平台同步渲染 5 帧）…".to_owned();
        say(st, "正在预览片头帧（真实 /credits/titles/preview）…", "info");
        let body = json!({
            "project": project_json(st),
            "kind": if st.cr_kind == 1 { "crawl" } else { "opening" },
            "style": current_style(st),
            "aspect": if st.cr_res == 3 { "2.39:1" } else { "16:9" },
            "width": if st.cr_res == 2 { 1080 } else { 1920 },
            "height": if st.cr_res == 2 { 1920 } else { 1080 },
        });
        (body, current_style(st), resolution(st))
    });
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        move || concat_host::pro2::credits_preview(&body),
        move |_studio, app, _models, result| {
            match result {
                Ok(value) => {
                    let frames = value
                        .get("frames")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let first = frames
                        .first()
                        .map(|frame| text_of(frame, &["url"]))
                        .unwrap_or_default();
                    let dur = inner_num(&value, &["layout", "duration"]);
                    let count = frames.len();
                    with(|st| {
                        st.busy = false;
                        st.cr_preview_line = format!(
                            "{count} 帧已出（{style} / {res}，时长 {dur:.1}s），正在下载第一帧…"
                        );
                        say(st, "预览帧已生成，正在落到本机。", "ok");
                    });
                    super::publish(&app);
                    if first.is_empty() {
                        with(|st| {
                            st.cr_preview_line = "预览帧生成了，但响应里没有地址。".to_owned();
                        });
                        super::publish(&app);
                        return;
                    }
                    let url = absolute(&first);
                    let dest = config_dir().join("pro2-preview").join("credits-preview.png");
                    let dest_for_job = dest.clone();
                    host::spawn(
                        move || -> Result<u64, String> {
                            let cancel = std::sync::atomic::AtomicBool::new(false);
                            concat_host::pro2::fetch(&url, &dest_for_job, &cancel)
                        },
                        move |_studio, app, _models, result| {
                            with(|st| {
                                st.busy = false;
                                match result {
                                    Ok(_) => {
                                        st.cr_preview = host::image_at(&dest);
                                        st.cr_preview_line = format!(
                                            "预览已就绪：{count} 帧 / {dur:.1}s（第一帧在本机，全部帧可打开网页版看）"
                                        );
                                        say(st, "预览帧已下载到本机。", "ok");
                                    }
                                    Err(error) => {
                                        st.cr_preview_line = format!("预览帧下载失败：{error}");
                                        say(st, error, "warn");
                                    }
                                }
                            });
                            super::publish(&app);
                        },
                    );
                }
                Err(error) => {
                    with(|st| {
                        st.busy = false;
                        st.cr_preview_line = format!("预览失败：{error}");
                        say(st, error, "warn");
                    });
                    super::publish(&app);
                }
            }
        },
    );
}

fn inner_num(value: &Value, path: &[&str]) -> f64 {
    let mut node = value;
    for key in path {
        match node.get(*key) {
            Some(next) => node = next,
            None => return 0.0,
        }
    }
    node.as_f64().unwrap_or(0.0)
}

/// 生成：POST /credits/titles/render（异步），返回 task_id 后轮询。
fn render(weak: slint::Weak<App>) {
    let body = with(|st| {
        st.busy = true;
        st.cr_progress = 1.0;
        st.cr_status = "提交中".to_owned();
        st.cr_task = "正在提交渲染任务…".to_owned();
        st.cr_mp4.clear();
        st.cr_zip.clear();
        say(st, "正在提交片头片尾渲染（真实 /credits/titles/render）…", "info");
        let opening = json!({
            "style": current_style(st),
            "resolutions": [resolution(st)],
            "enabled": st.cr_kind != 1,
        });
        let crawl = json!({
            "style": current_style(st),
            "resolutions": [resolution(st)],
            "enabled": st.cr_kind != 0,
        });
        json!({
            "project": project_json(st),
            "opening": opening,
            "crawl": crawl,
        })
    });
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        move || concat_host::pro2::credits_render(&body),
        |_studio, app, _models, result| {
            match result {
                Ok(value) => {
                    let task = text_of(&value, &["task_id"]);
                    let project = text_of(&value, &["project_id"]);
                    let variants = value
                        .get("variants")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    with(|st| {
                        st.busy = false;
                        st.cr_status = "渲染中".to_owned();
                        st.cr_project = project.clone();
                        st.cr_task = if task.is_empty() {
                            "提交成功，但没有返回 task_id".to_owned()
                        } else {
                            format!("任务 {task} · {variants} 个变体")
                        };
                        st.cr_line = format!(
                            "已提交渲染：项目 {project}，{variants} 个变体（2C2G 串行队列，可能要几分钟）。"
                        );
                        say(st, "渲染任务已提交，进度每秒刷新。", "ok");
                        st.cr_pending = if task.is_empty() { None } else { Some(task) };
                    });
                }
                Err(error) => {
                    with(|st| {
                        st.busy = false;
                        st.cr_status = "提交失败".to_owned();
                        st.cr_progress = 0.0;
                        st.cr_task = error.clone();
                        say(st, error, "warn");
                    });
                }
            }
            super::publish(&app);
        },
    );
}

/// 轮询一次渲染任务。
pub fn poll(weak: slint::Weak<App>, task: String) {
    let Some(app) = weak.upgrade() else { return };
    let _ = app;
    let task_for_job = task.clone();
    host::spawn(
        move || concat_host::pro2::credits_task(&task_for_job),
        move |_studio, app, _models, result| {
            match result {
                Ok(value) => {
                    let status = text_of(&value, &["status"]);
                    let stage = text_of(&value, &["stage"]);
                    let progress = num_of(&value, &["progress"]).clamp(0.0, 100.0);
                    let project = text_of(&value, &["project_id"]);
                    let assets = value
                        .get("assets")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let done = status == "done" || status == "partial" || status == "failed";
                    let label = if stage.is_empty() { status.clone() } else { stage };
                    with(|st| {
                        st.cr_status = label.clone();
                        st.cr_progress = if done { 100.0 } else { progress as f32 };
                        st.cr_task = if done {
                            format!("任务完成：{} 个产物", assets.len())
                        } else {
                            format!("任务 {task} · {progress:.0}%")
                        };
                        if !project.is_empty() {
                            st.cr_project = project.clone();
                        }
                        if !assets.is_empty() {
                            describe_assets(st, &assets);
                        }
                        if status == "failed" {
                            let error = text_of(&value, &["error"]);
                            st.cr_line = if error.is_empty() {
                                "渲染失败（平台没给原因）。".to_owned()
                            } else {
                                format!("渲染失败：{error}")
                            };
                            say(st, st.cr_line.clone(), "fail");
                        } else if done {
                            st.cr_line = format!(
                                "渲染完成：{} 个产物；可「打开 MP4」或「导出 zip」。",
                                assets.len()
                            );
                            say(st, "片头片尾渲染完成（真实产物）。", "ok");
                        }
                        if done {
                            st.cr_pending = None;
                        }
                    });
                }
                Err(error) => {
                    with(|st| {
                        st.cr_task = format!("轮询失败：{error}");
                    });
                }
            }
            super::publish(&app);
        },
    );
}

fn describe_assets(st: &mut Pro2State, assets: &[Value]) {
    let mut opening = String::new();
    let mut crawl = String::new();
    for asset in assets {
        let kind = text_of(asset, &["kind"]);
        let label = text_of(asset, &["label"]);
        let mp4 = text_of(asset, &["mp4_url"]);
        let dur = num_of(asset, &["duration"]);
        let line = format!("{label} · {dur:.1}s");
        if kind == "opening" && opening.is_empty() {
            opening = line;
            if !mp4.is_empty() {
                st.cr_mp4 = mp4;
            }
        } else if kind == "crawl" && crawl.is_empty() {
            crawl = line;
            if st.cr_mp4.is_empty() && !mp4.is_empty() {
                st.cr_mp4 = mp4;
            }
        }
    }
    st.cr_qc_line = [opening, crawl]
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("　|　");
}

/// 导出 zip：GET /credits/export/{pid}（服务端直接回文件）。
fn export(weak: slint::Weak<App>) {
    let project = with(|st| st.cr_project.clone());
    if project.is_empty() {
        with(|st| say(st, "还没有渲染过的项目：先生成一次再导出。", "warn"));
        super::publish_now();
        return;
    }
    let url = concat_host::pro2::credits_export_url(&project);
    let name = format!("credits_{project}.zip");
    let dest = dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("星籁")
        .join("片头片尾")
        .join(&name);
    with(|st| {
        st.busy = true;
        st.cr_zip = "导出中…".to_owned();
        say(st, "正在导出 zip（MP4 + PNG + ASS + SRT + 名单 + manifest）…", "info");
    });
    super::publish_now();
    let dest_for_job = dest.clone();
    host::spawn(
        move || {
            let cancel = std::sync::atomic::AtomicBool::new(false);
            concat_host::pro2::fetch(&url, &dest_for_job, &cancel)
        },
        move |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(size) => {
                        st.cr_zip = dest.display().to_string();
                        st.cr_line = format!(
                            "导出完成：{}（{:.1} MB）。",
                            dest.display(),
                            size as f64 / 1024.0 / 1024.0
                        );
                        say(st, "导出 zip 完成。", "ok");
                        let _ = open_url(&dest.to_string_lossy());
                    }
                    Err(error) => {
                        st.cr_zip.clear();
                        st.cr_line = format!("导出失败：{error}");
                        say(st, st.cr_line.clone(), "fail");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

/// 排版 QC。
fn qc(weak: slint::Weak<App>) {
    let project = with(|st| st.cr_project.clone());
    if project.is_empty() {
        with(|st| {
            st.cr_qc_line = "还没有渲染过的项目：先生成一次再 QC。".to_owned();
            say(st, "还没有渲染过的项目。", "warn");
        });
        super::publish_now();
        return;
    }
    host::spawn(
        move || concat_host::pro2::credits_qc(&project),
        |_studio, app, _models, result| {
            match result {
                Ok(value) => {
                    let qc = value.get("qc").cloned().unwrap_or_else(|| value.clone());
                    let score = num_of(&qc, &["score"]);
                    let status = text_of(&qc, &["status"]);
                    let checks = qc
                        .get("checks")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    let compliance = value.get("compliance").cloned().unwrap_or(Value::Null);
                    let license = text_of(&compliance, &["license_no"]);
                    let ai = compliance
                        .get("ai_generated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    with(|st| {
                        st.cr_qc_line = format!(
                            "QC {status} · 得分 {score:.0} · {checks} 项检查 · AI 声明 {} · 许可证号 {}",
                            if ai { "已开" } else { "未开" },
                            if license.is_empty() { "未填" } else { license.as_str() }
                        );
                        st.cr_project = text_of(&value, &["project_id"]);
                        say(st, "排版 QC 已取回（真实 /credits/titles/qc）。", "ok");
                    });
                }
                Err(error) => {
                    with(|st| {
                        st.cr_qc_line = format!("QC 失败：{error}");
                        say(st, error, "warn");
                    });
                }
            }
            super::publish(&app);
        },
    );
}

/// 面板动作。
pub fn action(weak: slint::Weak<App>, action: &str, index: i32, text: String) {
    match action {
        "cr-style" => {
            with(|st| {
                st.cr_style = index.clamp(0, 5);
                if let Some(id) = style_id(st) {
                    st.cr_line = format!("模板已选：{id}（生成时使用）。");
                }
            });
            super::publish_now();
        }
        "cr-kind" => {
            with(|st| st.cr_kind = index.clamp(0, 2));
            super::publish_now();
        }
        "cr-res" => {
            with(|st| st.cr_res = index.clamp(0, 3));
            super::publish_now();
        }
        "cr-title" => with(|st| st.cr_title = text),
        "cr-studios" => with(|st| st.cr_studios = text),
        "cr-producers" => with(|st| st.cr_producers = text),
        "cr-director" => with(|st| st.cr_director = text),
        "cr-cast" => with(|st| st.cr_cast = text),
        "cr-crew" => with(|st| st.cr_crew = text),
        "cr-year" => with(|st| st.cr_year = text),
        "cr-license" => with(|st| st.cr_license = text),
        "cr-record" => with(|st| st.cr_record = text),
        "cr-ai" => {
            with(|st| st.cr_ai = !st.cr_ai);
            super::publish_now();
        }
        "cr-demo" => {
            let demo = with(|st| st.cr_demo.clone());
            let Some(value) = demo else {
                with(|st| say(st, "示例名单还没载入：等模板接口返回，或点重试。", "warn"));
                super::publish_now();
                return;
            };
            with(|st| {
                apply_demo(st, &json!({ "project": value }));
                st.cr_line = "已重新载入示例名单（20 主演 + 部门 crew）。".to_owned();
                say(st, "示例名单已载入。", "ok");
            });
            super::publish_now();
        }
        "cr-preview" => preview(weak),
        "cr-render" => render(weak),
        "cr-export" => export(weak),
        "cr-qc" => qc(weak),
        "cr-open-mp4" => {
            let url = with(|st| st.cr_mp4.clone());
            if url.is_empty() {
                with(|st| say(st, "还没有 MP4：先生成一次。", "warn"));
                super::publish_now();
            } else {
                open_url(&url);
            }
        }
        _ => {
            let _ = TAB_CREDITS;
            let _ = money(0.0);
        }
    }
}

/// 让编译器知道 Shell 已用（部分闭包只在 cfg 下用）。
#[allow(dead_code)]
fn _shell_is_used() {
    Shell::with(|_shell, _app| {});
    let _ = publish_now;
    let _ = row("", "", "", "");
}
