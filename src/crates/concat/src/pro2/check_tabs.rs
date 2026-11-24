// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 连贯性检查 / 风格匹配 / 素材质量（0.4.0）。

use serde_json::{json, Value};

use super::{num_of, open_url, row, say, text_of, with};
use crate::host;
use crate::ui::App;

// ── 连贯性 ───────────────────────────────────────────────────────────

pub fn continuity_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        || -> Result<(Value, Value), String> {
            let health = concat_host::pro2::continuity_health()?;
            let list = concat_host::pro2::continuity_list(8).unwrap_or(Value::Null);
            Ok((health, list))
        },
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok((health, list)) => {
                        let version = text_of(&health, &["version"]);
                        apply_runs(st, &list);
                        st.ct_line = format!(
                            "连贯性引擎在线（{version}）：最近 {} 次检查。AI/算法辅助判定，结果仅供参考，需人工复核。",
                            st.ct_runs.len()
                        );
                        say(st, "连贯性检查接口在线。", "ok");
                    }
                    Err(error) => {
                        st.ct_line = format!("连贯性接口没就绪：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn apply_runs(st: &mut super::Pro2State, list: &Value) {
    st.ct_runs.clear();
    if let Some(items) = list.get("items").and_then(Value::as_array) {
        for item in items {
            st.ct_runs.push(row(
                text_of(item, &["title", "kind"]),
                text_of(item, &["created_at"]),
                format!(
                    "{} · {:.0}",
                    text_of(item, &["status"]),
                    num_of(item, &["score"])
                ),
                if text_of(item, &["status"]) == "fail" {
                    "warn"
                } else {
                    "ok"
                },
            ));
        }
    }
}

fn find_report(value: &Value) -> String {
    for key in ["report_url", "html_url", "report"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            if text.starts_with("http") || text.starts_with('/') {
                return super::absolute(text);
            }
        }
    }
    let run = text_of(value, &["run_id"]);
    if run.is_empty() {
        String::new()
    } else {
        concat_host::pro2::continuity_report_url(&run)
    }
}

fn audit(weak: slint::Weak<App>) {
    let video = with(|st| {
        let video = st.ct_video.trim().to_owned();
        st.busy = true;
        st.ct_status = "检查中".to_owned();
        st.ct_score.clear();
        st.ct_report.clear();
        st.ct_issues.clear();
        st.ct_line = format!("正在做一键全面检查（真实 /continuity/audit，约 15–60s）：{video}");
        say(st, "正在做连贯性总检…", "info");
        video
    });
    if video.is_empty() {
        with(|st| {
            st.busy = false;
            st.ct_status.clear();
            st.ct_line = "先填成片 URL（平台可下载地址）。".to_owned();
            say(st, st.ct_line.clone(), "warn");
        });
        super::publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    let body = json!({
        "video_url": video,
        "expected": {
            "expect_subtitle": true,
            "expect_ai_label": true,
        },
    });
    host::spawn(
        move || concat_host::pro2::continuity_audit(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let run = text_of(&value, &["run_id"]);
                        st.ct_run = run.clone();
                        let score = num_of(&value, &["score", "overall_score", "total_score"]);
                        st.ct_score = format!("{score:.0}");
                        st.ct_status = text_of(&value, &["status", "verdict"]);
                        if st.ct_status.is_empty() {
                            st.ct_status = "完成".to_owned();
                        }
                        st.ct_report = find_report(&value);
                        st.ct_issues.clear();
                        let items = ["issues", "findings", "problems", "checks", "results"]
                            .iter()
                            .find_map(|key| value.get(*key).and_then(Value::as_array).cloned())
                            .unwrap_or_default();
                        for item in &items {
                            let level = text_of(item, &["level", "severity", "status"]);
                            st.ct_issues.push(row(
                                text_of(item, &["label", "title", "name", "id"]),
                                text_of(item, &["detail", "message", "note", "hint"]),
                                if level.is_empty() {
                                    String::new()
                                } else {
                                    level.clone()
                                },
                                if matches!(level.as_str(), "fail" | "high" | "error") {
                                    "fail"
                                } else if matches!(level.as_str(), "warn" | "medium" | "pending") {
                                    "warn"
                                } else {
                                    "ok"
                                },
                            ));
                        }
                        st.ct_line = format!(
                            "检查完成：{} 分 / {} 条结果{}",
                            score,
                            items.len(),
                            if st.ct_report.is_empty() {
                                String::new()
                            } else {
                                format!("；报告 {}", st.ct_report)
                            }
                        );
                        say(st, "连贯性总检完成（真实 /continuity/audit）。", "ok");
                    }
                    Err(error) => {
                        st.ct_status = "失败".to_owned();
                        st.ct_line = format!("检查失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

pub fn continuity_action(weak: slint::Weak<App>, action: &str, index: i32, text: String) {
    match action {
        "ct-video" => with(|st| st.ct_video = text),
        "ct-audit" => audit(weak),
        "ct-list" => continuity_load(weak),
        "ct-report" => {
            let report = with(|st| {
                if st.ct_report.is_empty() {
                    let run = st.ct_run.clone();
                    if run.is_empty() {
                        String::new()
                    } else {
                        concat_host::pro2::continuity_report_url(&run)
                    }
                } else {
                    st.ct_report.clone()
                }
            });
            if report.is_empty() {
                with(|st| say(st, "还没有报告：先跑一次总检。", "warn"));
                super::publish_now();
            } else {
                open_url(&report);
            }
        }
        _ => {
            let _ = index;
        }
    }
}

// ── 风格匹配 ─────────────────────────────────────────────────────────

pub fn style_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        || -> Result<(Value, Value), String> {
            let health = concat_host::pro2::style_health()?;
            let list = concat_host::pro2::style_list(10)?;
            Ok((health, list))
        },
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok((health, list)) => {
                        st.st_profiles.clear();
                        if let Some(items) = list.get("items").and_then(Value::as_array) {
                            for item in items {
                                let id = text_of(item, &["id"]);
                                let name = text_of(item, &["name", "id"]);
                                st.st_profiles.push((id, name));
                            }
                        }
                        let version = text_of(&health, &["version"]);
                        st.st_line = format!(
                            "风格指纹引擎在线（{version}）：已存 {} 个指纹；参考片只做统计迁移，不复制画面。",
                            st.st_profiles.len()
                        );
                        say(st, "风格匹配接口在线。", "ok");
                    }
                    Err(error) => {
                        st.st_line = format!("风格接口没就绪：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn analyze(weak: slint::Weak<App>) {
    let reference = with(|st| {
        let reference = st.st_ref.trim().to_owned();
        st.busy = true;
        st.st_status = "分析中".to_owned();
        st.st_line = format!("正在提取参考片风格指纹（真实 /style/analyze，约 10–60s）：{reference}");
        st.st_labels.clear();
        say(st, "正在分析参考片风格…", "info");
        reference
    });
    if reference.is_empty() {
        with(|st| {
            st.busy = false;
            st.st_status.clear();
            st.st_line = "先填参考片 URL。".to_owned();
            say(st, st.st_line.clone(), "warn");
        });
        super::publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    let body = json!({
        "reference_video_url": reference,
        "start": 0,
        "duration": 20,
        "name": format!("桌面参考片 {}", super::now_secs()),
        "save": true,
        "describe": false,
    });
    host::spawn(
        move || concat_host::pro2::style_analyze(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let profile = value.get("profile").cloned().unwrap_or_else(|| value.clone());
                        st.st_profile = text_of(&profile, &["id", "profile_id"]);
                        let labels = profile
                            .get("labels")
                            .and_then(Value::as_array)
                            .map(|labels| {
                                labels
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join("、")
                            })
                            .unwrap_or_default();
                        let summary = text_of(&profile, &["summary"]);
                        st.st_labels = if labels.is_empty() {
                            summary.clone()
                        } else {
                            format!("{labels}　{summary}")
                        };
                        st.st_status = "指纹就绪".to_owned();
                        st.st_line = format!(
                            "指纹已提取：{}（节奏 / 运动 / 色彩 / 质感）",
                            st.st_profile
                        );
                        say(st, "风格指纹提取完成（真实 /style/analyze）。", "ok");
                    }
                    Err(error) => {
                        st.st_status = "失败".to_owned();
                        st.st_line = format!("分析失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn apply_color(weak: slint::Weak<App>) {
    let (profile, target, strength) = with(|st| {
        st.busy = true;
        st.st_status = "应用色彩中".to_owned();
        st.st_line = "正在把指纹色彩应用到目标片（真实 /style/apply-color，约 20–60s）…".to_owned();
        say(st, "正在应用色彩…", "info");
        (
            st.st_profile.clone(),
            st.st_target.trim().to_owned(),
            st.st_strength.trim().parse::<f64>().unwrap_or(0.85),
        )
    });
    if profile.is_empty() || target.is_empty() {
        with(|st| {
            st.busy = false;
            st.st_status.clear();
            st.st_line = "先分析参考片（得到指纹），再填目标片 URL。".to_owned();
            say(st, st.st_line.clone(), "warn");
        });
        super::publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    let body = json!({
        "target_video_url": target,
        "profile_id": profile,
        "start": 0,
        "duration": 20,
        "strength": strength.clamp(0.1, 1.0),
        "max_height": 720,
        "lut_size": 33,
    });
    host::spawn(
        move || concat_host::pro2::style_apply_color(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let media = value.get("media").cloned().unwrap_or(Value::Null);
                        let output = text_of(&media, &["output", "video", "output_url"]);
                        if !output.is_empty() {
                            st.st_color = super::absolute(&output);
                        }
                        st.st_metrics.clear();
                        let metrics = value.get("metrics").cloned().unwrap_or(Value::Null);
                        let before = metrics.get("before").cloned().unwrap_or(Value::Null);
                        let after = metrics.get("after").cloned().unwrap_or(Value::Null);
                        for (key, label) in [
                            ("delta_e00_mean", "ΔE00 均色"),
                            ("saturation", "饱和度"),
                            ("contrast", "对比度"),
                            ("mean_luma", "平均亮度"),
                            ("temperature_k", "色温 K"),
                        ] {
                            let before_value = num_of(&before, &[key]);
                            let after_value = num_of(&after, &[key]);
                            if before_value == 0.0 && after_value == 0.0 {
                                continue;
                            }
                            st.st_metrics.push(row(
                                label,
                                format!("{before_value:.3} → {after_value:.3}"),
                                format!(
                                    "{:+.3}",
                                    after_value - before_value
                                ),
                                "info",
                            ));
                        }
                        let improvement = num_of(&metrics, &["improvement"]);
                        let strategy = text_of(&value, &["strategy"]);
                        st.st_metrics_line = format!(
                            "改善 {improvement:+.3}；策略：{}",
                            if strategy.is_empty() { "见平台结果" } else { strategy.as_str() }
                        );
                        st.st_status = "色彩已应用".to_owned();
                        st.st_line = format!(
                            "色彩已应用：{}",
                            if st.st_color.is_empty() {
                                "平台已返回指标".to_owned()
                            } else {
                                st.st_color.clone()
                            }
                        );
                        say(st, "色彩迁移完成（真实 /style/apply-color）。", "ok");
                    }
                    Err(error) => {
                        st.st_status = "失败".to_owned();
                        st.st_line = format!("应用色彩失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn export_lut(weak: slint::Weak<App>) {
    let (profile, strength) = with(|st| {
        st.busy = true;
        st.st_status = "导出 LUT".to_owned();
        st.st_line = "正在生成 .cube LUT（真实 /style/lut）…".to_owned();
        say(st, "正在导出 LUT…", "info");
        (
            st.st_profile.clone(),
            st.st_strength.trim().parse::<f64>().unwrap_or(0.85),
        )
    });
    if profile.is_empty() {
        with(|st| {
            st.busy = false;
            st.st_line = "先分析参考片得到指纹，再导出 LUT。".to_owned();
            say(st, st.st_line.clone(), "warn");
        });
        super::publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    let body = json!({
        "profile_id": profile,
        "lut_size": 33,
        "strength": strength.clamp(0.1, 1.0),
    });
    host::spawn(
        move || concat_host::pro2::style_lut(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let cube = value.get("cube").cloned().unwrap_or(Value::Null);
                        let url = text_of(&cube, &["url", "download_url"]);
                        if !url.is_empty() {
                            st.st_lut = super::absolute(&url);
                        }
                        st.st_status = "LUT 已导出".to_owned();
                        st.st_line = format!(
                            "LUT 已生成：{}",
                            if st.st_lut.is_empty() {
                                "平台已返回".to_owned()
                            } else {
                                st.st_lut.clone()
                            }
                        );
                        say(st, "LUT 导出完成（真实 /style/lut）。", "ok");
                    }
                    Err(error) => {
                        st.st_status = "失败".to_owned();
                        st.st_line = format!("LUT 导出失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

pub fn style_action(weak: slint::Weak<App>, action: &str, index: i32, text: String) {
    match action {
        "st-ref" => with(|st| st.st_ref = text),
        "st-target" => with(|st| st.st_target = text),
        "st-strength" => with(|st| st.st_strength = text),
        "st-analyze" => analyze(weak),
        "st-apply" => apply_color(weak),
        "st-lut" => export_lut(weak),
        "st-open" => {
            let url = with(|st| st.st_color.clone());
            if url.is_empty() {
                with(|st| say(st, "还没有色彩结果：先应用色彩。", "warn"));
                super::publish_now();
            } else {
                open_url(&url);
            }
        }
        "st-pick" => {
            let picked = with(|st| st.st_profiles.get(index.max(0) as usize).cloned());
            if let Some((id, name)) = picked {
                with(|st| {
                    st.st_profile = id.clone();
                    st.st_line = format!("已选指纹：{name}（{id}）；可应用色彩或导出 LUT。");
                    say(st, st.st_line.clone(), "ok");
                });
                super::publish_now();
            }
        }
        _ => {}
    }
}

// ── 素材质量 ─────────────────────────────────────────────────────────

pub fn quality_load(weak: slint::Weak<App>) {
    search(weak);
}

fn search(weak: slint::Weak<App>) {
    let quality = with(|st| match st.ql_filter {
        1 => "cinema_candidate",
        2 => "commercial_hd",
        _ => "",
    });
    with(|st| {
        st.busy = true;
        st.ql_line = "正在按质量档搜素材（真实 /assets/search?quality=…）…".to_owned();
    });
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        move || concat_host::pro2::assets_quality(quality, 1, 24),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        st.ql_items.clear();
                        let total = num_of(&value, &["total"]) as i64;
                        st.ql_total = total.to_string();
                        let summary = value
                            .get("quality_summary")
                            .cloned()
                            .unwrap_or(Value::Null);
                        let filter = value.get("quality_filter").cloned().unwrap_or(Value::Null);
                        let applied = filter
                            .get("applied")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        let grade = text_of(&filter, &["grade"]);
                        st.ql_badge = if applied {
                            if grade.is_empty() {
                                "质量筛选已生效".to_owned()
                            } else {
                                format!("质量档：{grade}")
                            }
                        } else {
                            "全部素材（未按质量筛选）".to_owned()
                        };
                        if let Some(items) = value.get("items").and_then(Value::as_array) {
                            for item in items {
                                let title = {
                                    let zh = text_of(item, &["title_zh"]);
                                    if zh.is_empty() {
                                        text_of(item, &["title", "name"])
                                    } else {
                                        zh
                                    }
                                };
                                let width = num_of(item, &["width"]) as i64;
                                let height = num_of(item, &["height"]) as i64;
                                let resolution = text_of(item, &["resolution"]);
                                let quality = item.get("quality").cloned().unwrap_or(Value::Null);
                                let mut badge = text_of(&quality, &["grade", "label"]);
                                if badge.is_empty() {
                                    badge = text_of(item, &["quality_grade", "quality_label"]);
                                }
                                let score = num_of(&quality, &["score", "total_score"]);
                                let detail = format!(
                                    "{width}×{height} · {resolution}{}{}",
                                    if badge.is_empty() { String::new() } else { format!(" · {badge}") },
                                    if score > 0.0 { format!(" · {score:.0} 分") } else { String::new() },
                                );
                                st.ql_items.push(row(
                                    title,
                                    detail,
                                    text_of(item, &["source_name", "source"]),
                                    if badge.is_empty() { "info" } else { "ok" },
                                ));
                            }
                        }
                        let _ = summary;
                        st.ql_line = format!(
                            "搜索到 {} 条（共 {total} 条）；{}",
                            st.ql_items.len(),
                            st.ql_badge
                        );
                        say(st, "素材质量筛选已取回（真实 /assets/search）。", "ok");
                    }
                    Err(error) => {
                        st.ql_line = format!("素材质量搜索失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

pub fn quality_action(weak: slint::Weak<App>, action: &str, index: i32, text: String) {
    match action {
        "ql-filter" => {
            with(|st| st.ql_filter = index.clamp(0, 2));
            search(weak);
        }
        "ql-search" => search(weak),
        _ => {
            let _ = text;
        }
    }
}
