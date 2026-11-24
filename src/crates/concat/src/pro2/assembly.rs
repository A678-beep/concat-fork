// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 实拍装配（0.4.0）：导入素材 → ingest → plan（EDL/缺口/置信度）→ 确认 →
//! render 粗剪 → 导入时间线 / 打开成片。
//!
//! 平台接口 `/assembly/*`。桌面按 URL 登记素材（JSON；multipart 留给网页版），
//! 每次提交返回 task_id，面板每秒轮询 `/assembly/task/{id}`。

use std::path::PathBuf;

use serde_json::{json, Value};

use super::{
    num_of, open_url, row, say, text_of, with, Row, TAB_ASSEMBLY,
};
use crate::host;
use crate::ui::App;

/// 打开页签：先看健康与最近批次。
pub fn load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        || -> Result<(Value, Value), String> {
            let health = concat_host::pro2::assembly_health()?;
            let list = concat_host::pro2::assembly_list().unwrap_or(Value::Null);
            Ok((health, list))
        },
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok((health, list)) => {
                        let batches = list
                            .get("batches")
                            .and_then(Value::as_array)
                            .map(Vec::len)
                            .unwrap_or(0);
                        let plans = list
                            .get("plans")
                            .and_then(Value::as_array)
                            .map(Vec::len)
                            .unwrap_or(0);
                        let renders = list
                            .get("renders")
                            .and_then(Value::as_array)
                            .map(Vec::len)
                            .unwrap_or(0);
                        let engine = text_of(&health, &["version"]);
                        st.as_health = format!(
                            "素材批次 {batches} / EDL {plans} / 成片 {renders}（引擎 {}）",
                            if engine.is_empty() { "assembly" } else { engine.as_str() }
                        );
                        st.as_line = format!("装配接口在线：{}。", st.as_health);
                        if st.as_materials.is_empty() {
                            if let Some(batches) = list.get("batches").and_then(Value::as_array) {
                                st.as_materials = batches
                                    .iter()
                                    .take(8)
                                    .map(|batch| {
                                        let stats = batch.get("stats").cloned().unwrap_or(Value::Null);
                                        row(
                                            text_of(batch, &["name", "assets_id"]),
                                            format!(
                                                "{} 镜 · {} 条素材",
                                                num_of(&stats, &["shots"]) as i64,
                                                num_of(&stats, &["assets_ok"]) as i64
                                            ),
                                            text_of(batch, &["assets_id"]),
                                            "ok",
                                        )
                                    })
                                    .collect();
                            }
                        }
                        say(st, "实拍装配接口在线（/assembly/health + /list）。", "ok");
                    }
                    Err(error) => {
                        st.as_line = format!("装配接口没就绪：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn split_urls(text: &str) -> Vec<String> {
    text.lines()
        .flat_map(|line| line.split([' ', ',', '，', ';']))
        .map(str::trim)
        .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
        .map(str::to_owned)
        .collect()
}

/// 导入素材：POST /assembly/ingest（JSON urls）。
fn ingest(weak: slint::Weak<App>) {
    let (body, urls) = with(|st| {
        let urls = split_urls(&st.as_urls);
        st.busy = true;
        st.as_progress = 1.0;
        st.as_status = "导入中".to_owned();
        st.as_task = "正在登记素材…".to_owned();
        st.as_line = format!("登记 {} 条素材（真实 /assembly/ingest）…", urls.len());
        say(st, "正在导入实拍素材（ingest）…", "info");
        let body = json!({
            "urls": urls,
            "name": st.as_name,
            "style": st.as_style,
        });
        (body, urls.len())
    });
    if urls == 0 {
        with(|st| {
            st.busy = false;
            st.as_task.clear();
            st.as_line = "没有素材地址：在「素材 URL」里每行填一个 http(s) 地址。".to_owned();
            say(st, st.as_line.clone(), "warn");
        });
        super::publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        move || concat_host::pro2::assembly_ingest(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let task = text_of(&value, &["task_id"]);
                        let assets = text_of(&value, &["assets_id"]);
                        st.as_assets = assets.clone();
                        st.as_progress = 2.0;
                        st.as_status = "分析中".to_owned();
                        st.as_task = format!("任务 {task} · {assets}");
                        st.as_line = format!(
                            "素材批次 {assets} 已登记，正在分析（元数据/场景/抽帧/ASR/指纹）。"
                        );
                        st.as_task_kind = "ingest".to_owned();
                        st.as_detail = format!("ingest 任务 {task}");
                        say(st, "素材已登记，分析进度看下面的进度条。", "ok");
                    }
                    Err(error) => {
                        st.as_status = "失败".to_owned();
                        st.as_progress = 0.0;
                        st.as_task = error.clone();
                        st.as_line = format!("导入失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

/// 生成 EDL：POST /assembly/plan。
fn plan(weak: slint::Weak<App>) {
    let body = with(|st| {
        st.busy = true;
        st.as_progress = 2.0;
        st.as_status = "规划中".to_owned();
        st.as_task = "DeepSeek 逐场逐 beat 匹配…".to_owned();
        st.as_edl.clear();
        st.as_gaps.clear();
        st.as_confirm_seqs.clear();
        st.as_line = "正在生成 EDL（真实 /assembly/plan，约 5–30s）…".to_owned();
        say(st, "正在生成 EDL（plan）…", "info");
        json!({
            "assets_id": st.as_assets.clone(),
            "script": st.as_script.clone(),
            "style": st.as_style.clone(),
        })
    });
    if with(|st| st.as_assets.clone()).trim().is_empty() {
        with(|st| {
            st.busy = false;
            st.as_line = "还没有 assets_id：先导入素材。".to_owned();
            st.as_status.clear();
            say(st, st.as_line.clone(), "warn");
        });
        super::publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        move || concat_host::pro2::assembly_plan(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let task = text_of(&value, &["task_id"]);
                        let plan = text_of(&value, &["plan_id"]);
                        st.as_plan = plan.clone();
                        st.as_progress = 5.0;
                        st.as_status = "规划中".to_owned();
                        st.as_task = format!("任务 {task} · {plan}");
                        st.as_task_kind = "plan".to_owned();
                        st.as_line = format!(
                            "plan 已提交：{plan}（逐场逐 beat 匹配 + EDL 校验 + 缺口 / 置信度）。"
                        );
                        say(st, "plan 任务已提交。", "ok");
                    }
                    Err(error) => {
                        st.as_status = "失败".to_owned();
                        st.as_progress = 0.0;
                        st.as_task = error.clone();
                        st.as_line = format!("plan 失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

/// 渲染粗剪：POST /assembly/render。
fn render(weak: slint::Weak<App>, confirmed: bool) {
    let body = with(|st| {
        st.busy = true;
        st.as_progress = 5.0;
        st.as_status = "渲染中".to_owned();
        st.as_task = "按 EDL 裁切 / 拼接 / 字幕 / 原声…".to_owned();
        st.as_line = if confirmed {
            "正在渲染已确认片段（真实 /assembly/render mode=confirmed）…".to_owned()
        } else {
            "正在渲染粗剪（真实 /assembly/render）…".to_owned()
        };
        say(st, "正在渲染粗剪（render）…", "info");
        let mut body = json!({
            "plan_id": st.as_plan.clone(),
            "mode": if confirmed { "confirmed" } else { "all" },
            "subtitles": true,
            "keep_audio": true,
        });
        if confirmed {
            let confirmed: Vec<Value> = st
                .as_confirm_seqs
                .iter()
                .map(|seq| json!({ "seq": seq }))
                .collect();
            body["confirmed"] = Value::Array(confirmed);
        }
        body
    });
    if with(|st| st.as_plan.clone()).trim().is_empty() {
        with(|st| {
            st.busy = false;
            st.as_line = "还没有 plan_id：先生成 EDL。".to_owned();
            st.as_status.clear();
            say(st, st.as_line.clone(), "warn");
        });
        super::publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        move || concat_host::pro2::assembly_render(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let task = text_of(&value, &["task_id"]);
                        let render = text_of(&value, &["render_id"]);
                        st.as_render = render.clone();
                        st.as_progress = 8.0;
                        st.as_status = "渲染中".to_owned();
                        st.as_task = format!("任务 {task} · {render}");
                        st.as_task_kind = "render".to_owned();
                        st.as_line = format!("render 已提交：{render}（成片 + 工程包）。");
                        say(st, "render 任务已提交。", "ok");
                    }
                    Err(error) => {
                        st.as_status = "失败".to_owned();
                        st.as_progress = 0.0;
                        st.as_task = error.clone();
                        st.as_line = format!("render 失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

/// 确认 EDL 并重渲染：POST /assembly/apply（confirmed + mode=confirmed）。
fn confirm(weak: slint::Weak<App>) {
    let body = with(|st| {
        st.busy = true;
        st.as_progress = 6.0;
        st.as_status = "确认中".to_owned();
        st.as_task = "把 EDL 标为已确认并重渲染…".to_owned();
        let confirmed: Vec<Value> = st
            .as_confirm_seqs
            .iter()
            .map(|seq| json!({ "seq": seq }))
            .collect();
        let count = confirmed.len();
        st.as_line = format!(
            "确认 {count} 条 EDL（真实 /assembly/apply，confirmed + mode=confirmed）…"
        );
        say(st, "正在确认 EDL 并触发重渲染（apply）…", "info");
        json!({
            "plan_id": st.as_plan.clone(),
            "confirmed": confirmed,
            "mode": "confirmed",
            "render": true,
        })
    });
    if with(|st| st.as_plan.clone()).trim().is_empty() {
        with(|st| {
            st.busy = false;
            st.as_line = "还没有 plan_id：先生成 EDL。".to_owned();
            say(st, st.as_line.clone(), "warn");
        });
        super::publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        move || concat_host::pro2::assembly_apply(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let plan = text_of(&value, &["plan_id"]);
                        let render = text_of(&value, &["render_id"]);
                        let task = text_of(&value, &["render_task_id"]);
                        if !plan.is_empty() {
                            st.as_plan = plan.clone();
                        }
                        st.as_render = render.clone();
                        st.as_progress = 8.0;
                        st.as_status = "重渲染中".to_owned();
                        st.as_task = format!("任务 {task} · {render}");
                        st.as_task_kind = "render".to_owned();
                        describe_plan(st, &value);
                        st.as_line = format!(
                            "已确认并生成新版本 EDL {plan}，正在重渲染（{render}）。"
                        );
                        say(st, "确认完成，EDL 已出新版本。", "ok");
                    }
                    Err(error) => {
                        st.as_status = "失败".to_owned();
                        st.as_progress = 0.0;
                        st.as_task = error.clone();
                        st.as_line = format!("apply 失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

/// 轮询一次任务（ingest / plan / render）。
pub fn poll(weak: slint::Weak<App>, kind: String, task: String) {
    let Some(app) = weak.upgrade() else { return };
    let _ = app;
    let task_for_job = task.clone();
    host::spawn(
        move || concat_host::pro2::assembly_task(&task_for_job),
        move |_studio, app, _models, result| {
            match result {
                Ok(value) => {
                    let task = value
                        .get("task")
                        .cloned()
                        .unwrap_or_else(|| value.clone());
                    let status = text_of(&task, &["status"]);
                    let stage = text_of(&task, &["stage"]);
                    let progress = num_of(&task, &["progress"]).clamp(0.0, 100.0);
                    let error = text_of(&task, &["error"]);
                    let done = matches!(status.as_str(), "done" | "failed" | "partial");
                    let label = if stage.is_empty() { status.clone() } else { stage.clone() };
                    with(|st| {
                        st.as_status = label;
                        st.as_progress = if done { 100.0 } else { progress.max(1.0) as f32 };
                        if !error.is_empty() {
                            st.as_line = format!("任务失败：{error}");
                            say(st, st.as_line.clone(), "fail");
                        }
                    });
                    if done {
                        if status == "failed" {
                            with(|st| {
                                st.as_task_kind.clear();
                                st.as_task.clear();
                            });
                            super::publish(&app);
                            return;
                        }
                        match kind.as_str() {
                            "ingest" => after_ingest(&app),
                            "plan" => after_plan(&app),
                            _ => after_render(&app),
                        }
                    } else {
                        with(|st| st.as_task = format!("任务 {task} · {progress:.0}%"));
                        super::publish(&app);
                    }
                }
                Err(error) => {
                    with(|st| st.as_task = format!("轮询失败：{error}"));
                    super::publish(&app);
                }
            }
        },
    );
}

fn after_ingest(app: &App) {
    let assets = with(|st| st.as_assets.clone());
    with(|st| {
        st.as_task_kind.clear();
        st.as_task.clear();
    });
    host::spawn(
        move || concat_host::pro2::assembly_assets(&assets),
        |_studio, app, _models, result| {
            with(|st| match result {
                Ok(value) => {
                    describe_assets(st, &value);
                    st.as_line = format!(
                        "素材分析完成：{} 个镜头可匹配；可以直接 plan。",
                        st.as_materials.len()
                    );
                    st.as_progress = 100.0;
                    st.as_status = "分析完成".to_owned();
                    say(st, "素材分析完成（ingest done）。", "ok");
                }
                Err(error) => {
                    st.as_line = format!("素材镜头表没取回来：{error}");
                    say(st, error, "warn");
                }
            });
            super::publish(&app);
        },
    );
    let _ = app;
}

fn describe_assets(st: &mut super::Pro2State, value: &Value) {
    st.as_materials.clear();
    let assets = value
        .get("assets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for asset in assets {
        let name = text_of(&asset, &["asset_name", "name", "asset_id"]);
        let duration = num_of(&asset, &["duration"]);
        let shots = num_of(&asset, &["shots", "shot_count"]) as i64;
        let ok = asset
            .get("ingest_status")
            .and_then(Value::as_str)
            .map(|status| status == "ok")
            .unwrap_or(true);
        st.as_materials.push(row(
            name,
            format!("{duration:.1}s · {shots} 镜"),
            text_of(&asset, &["asset_id"]),
            if ok { "ok" } else { "warn" },
        ));
    }
}

fn after_plan(app: &App) {
    let plan = with(|st| st.as_plan.clone());
    with(|st| {
        st.as_task_kind.clear();
        st.as_task.clear();
    });
    host::spawn(
        move || concat_host::pro2::assembly_plan_of(&plan),
        |_studio, app, _models, result| {
            with(|st| match result {
                Ok(value) => {
                    describe_plan(st, &value);
                    st.as_progress = 100.0;
                    st.as_status = "EDL 就绪".to_owned();
                    st.as_line = format!(
                        "EDL 就绪：{} 段 / {} 个缺口；确认后可只渲染已确认片段。",
                        st.as_edl.len(),
                        st.as_gaps.len()
                    );
                    say(st, "EDL 生成完成（plan done）。", "ok");
                }
                Err(error) => {
                    st.as_line = format!("EDL 没取回来：{error}");
                    say(st, error, "warn");
                }
            });
            super::publish(&app);
        },
    );
    let _ = app;
}

fn describe_plan(st: &mut super::Pro2State, value: &Value) {
    st.as_edl.clear();
    st.as_gaps.clear();
    st.as_confirm_seqs.clear();
    if !text_of(value, &["plan_id"]).is_empty() {
        st.as_plan = text_of(value, &["plan_id"]);
    }
    if let Some(edl) = value.get("edl").and_then(Value::as_array) {
        for item in edl {
            let seq = num_of(item, &["seq"]) as i64;
            let scene = num_of(item, &["scene_no"]) as i64;
            let beat = num_of(item, &["beat_no"]) as i64;
            let score = num_of(item, &["match_score"]);
            let duration = num_of(item, &["duration"]);
            let asset = text_of(item, &["asset_name", "asset_id"]);
            let shot = num_of(item, &["shot_no"]) as i64;
            let tin = num_of(item, &["in"]);
            let tout = num_of(item, &["out"]);
            let script = text_of(item, &["script_text"]);
            let reason = text_of(item, &["reason"]);
            let alternatives = item
                .get("alternatives")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            st.as_edl.push(row(
                format!("S{seq} 场{scene}·{beat}"),
                format!(
                    "{script}　→　{asset} S{shot} [{tin:.1}–{tout:.1}]　{reason}{}",
                    if alternatives > 0 {
                        format!("（备选 {alternatives}）")
                    } else {
                        String::new()
                    }
                ),
                format!("{:.0}% · {duration:.1}s", score * 100.0),
                if score >= 0.5 { "ok" } else { "warn" },
            ));
            st.as_confirm_seqs.push(seq);
        }
    }
    if let Some(gaps) = value.get("gaps").and_then(Value::as_array) {
        for gap in gaps {
            let scene = num_of(gap, &["scene_no"]) as i64;
            let beat = num_of(gap, &["beat_no"]) as i64;
            st.as_gaps.push(row(
                format!("场{scene}·{beat}"),
                text_of(gap, &["script_text", "missing"]),
                text_of(gap, &["suggestion"]),
                "warn",
            ));
        }
    }
    let coverage = value.get("coverage").cloned().unwrap_or(Value::Null);
    let confidence = value.get("confidence").cloned().unwrap_or(Value::Null);
    let overall = num_of(&confidence, &["overall"]);
    let coverage_pct = num_of(&coverage, &["coverage"]);
    let stats = value.get("stats").cloned().unwrap_or(Value::Null);
    let total = num_of(&stats, &["total_edl_duration"]);
    st.as_plan_confidence = format!(
        "EDL {} 段 / 缺口 {} / 覆盖率 {:.0}% / 置信度 {:.2} / 总时长 {:.1}s",
        st.as_edl.len(),
        st.as_gaps.len(),
        coverage_pct * 100.0,
        overall,
        total
    );
    st.as_detail = st.as_plan_confidence.clone();
    st.as_assets = {
        let assets = text_of(value, &["assets_id"]);
        if assets.is_empty() {
            st.as_assets.clone()
        } else {
            assets
        }
    };
}

fn after_render(app: &App) {
    let render = with(|st| st.as_render.clone());
    with(|st| {
        st.as_task_kind.clear();
        st.as_task.clear();
    });
    host::spawn(
        move || concat_host::pro2::assembly_render_of(&render),
        |_studio, app, _models, result| {
            with(|st| match result {
                Ok(value) => {
                    let video = text_of(&value, &["video_url"]);
                    let pack = text_of(&value, &["pack_url"]);
                    let duration = num_of(&value, &["duration"]);
                    if !video.is_empty() {
                        st.as_video = video.clone();
                    }
                    if !pack.is_empty() {
                        st.as_pack = pack.clone();
                    }
                    st.as_progress = 100.0;
                    st.as_status = "粗剪完成".to_owned();
                    st.as_line = format!(
                        "粗剪完成：{duration:.1}s 成片 + 工程包（EDL/SRT/project.json）。"
                    );
                    say(st, "粗剪渲染完成（render done）。", "ok");
                }
                Err(error) => {
                    st.as_line = format!("成片信息没取回来：{error}");
                    say(st, error, "warn");
                }
            });
            super::publish(&app);
        },
    );
    let _ = app;
}

/// 面板动作。
pub fn action(weak: slint::Weak<App>, action: &str, index: i32, text: String) {
    match action {
        "as-name" => with(|st| st.as_name = text),
        "as-assets" => with(|st| st.as_assets = text),
        "as-urls" => with(|st| st.as_urls = text),
        "as-script" => with(|st| st.as_script = text),
        "as-style" => with(|st| st.as_style = text),
        "as-plan" => with(|st| st.as_plan = text),
        "as-sample" => {
            with(|st| {
                st.as_urls = super::SAMPLE_ASSETS.join("\n");
                st.as_line = "已填 5 段示例实拍素材（平台已存在，可直接 ingest）。".to_owned();
            });
            super::publish_now();
        }
        "as-ingest" => ingest(weak),
        "as-plan-go" => plan(weak),
        "as-render" => render(weak, false),
        "as-render-confirmed" => render(weak, true),
        "as-confirm" => confirm(weak),
        "as-refresh" => load(weak),
        "as-open" => {
            let video = with(|st| st.as_video.clone());
            if video.is_empty() {
                with(|st| say(st, "还没有成片：先渲染。", "warn"));
                super::publish_now();
            } else {
                open_url(&video);
            }
        }
        "as-video" => {
            let video = with(|st| st.as_video.clone());
            if video.is_empty() {
                with(|st| say(st, "还没有成片地址。", "warn"));
                super::publish_now();
            } else {
                open_url(&video);
            }
        }
        "as-pack" => {
            let pack = with(|st| st.as_pack.clone());
            if pack.is_empty() {
                with(|st| say(st, "还没有工程包地址。", "warn"));
                super::publish_now();
            } else {
                let name = format!("assembly_{}.zip", super::now_secs());
                super::download_to(weak, pack, "实拍装配", name);
            }
        }
        "as-import-timeline" => {
            let video = with(|st| st.as_video.clone());
            super::insert_to(
                weak,
                video,
                "实拍装配",
                format!("实拍粗剪_{}.mp4", super::now_secs()),
            );
        }
        _ => {
            let _ = TAB_ASSEMBLY;
            let _ = index;
            let _: Option<Row> = None;
            let _ = PathBuf::new();
        }
    }
}
