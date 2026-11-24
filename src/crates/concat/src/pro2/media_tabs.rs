// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 特效 / 配乐 / 配音（0.4.0）：三个页签的取数、生成与插入。

use serde_json::{json, Value};

use super::{absolute, num_of, open_url, parse_scenes, row, say, text_of, with};
use crate::host;
use crate::ui::App;

// ── 特效 ─────────────────────────────────────────────────────────────

/// GET /vfx/library：14 特效 + 参数 schema + 预设。
pub fn vfx_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        || concat_host::pro2::vfx_library(),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        apply_vfx(st, &value);
                        say(
                            st,
                            format!(
                                "特效库已取回：{} 个特效 / {} 组预设（真实 /vfx/library）。",
                                st.vfx_effects.len(),
                                st.vfx_presets.len()
                            ),
                            "ok",
                        );
                    }
                    Err(error) => {
                        st.vfx_line = format!("特效库没取回来：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn apply_vfx(st: &mut super::Pro2State, value: &Value) {
    st.vfx_library = Some(value.clone());
    st.vfx_effects.clear();
    if let Some(effects) = value.get("effects").and_then(Value::as_array) {
        for effect in effects {
            let id = text_of(effect, &["effect_id", "id"]);
            let name = text_of(effect, &["name", "name_en", "effect_id"]);
            let category = text_of(effect, &["category"]);
            st.vfx_effects.push((id, format!("{name} · {category}")));
        }
    }
    st.vfx_effect = st.vfx_effect.clamp(0, (st.vfx_effects.len() as i32 - 1).max(0));
    refresh_vfx_presets(st);
}

fn refresh_vfx_presets(st: &mut super::Pro2State) {
    st.vfx_presets.clear();
    st.vfx_preset = 0;
    let Some(library) = st.vfx_library.clone() else { return };
    let index = st.vfx_effect.max(0) as usize;
    let Some(effect) = library
        .get("effects")
        .and_then(Value::as_array)
        .and_then(|effects| effects.get(index))
    else {
        return;
    };
    if let Some(presets) = effect.get("presets").and_then(Value::as_array) {
        for preset in presets {
            let id = if preset.is_string() {
                preset.as_str().unwrap_or_default().to_owned()
            } else {
                text_of(preset, &["id", "preset_id"])
            };
            let name = if preset.is_string() {
                id.clone()
            } else {
                text_of(preset, &["name", "label", "id"])
            };
            st.vfx_presets.push((id, name));
        }
    }
    let params = effect
        .get("params_schema")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let line = params
        .iter()
        .take(9)
        .map(|param| {
            format!(
                "{}={}",
                text_of(param, &["label", "key"]),
                text_of(param, &["default"])
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let name = text_of(effect, &["name", "effect_id"]);
    let desc = text_of(effect, &["desc"]);
    st.vfx_params = format!("{name}：{desc}　参数：{line}");
}

fn vfx_apply_preset(st: &mut super::Pro2State) {
    refresh_vfx_presets(st);
}

fn vfx_render(weak: slint::Weak<App>) {
    let body = with(|st| {
        st.busy = true;
        st.vfx_progress = 2.0;
        st.vfx_status = "提交中".to_owned();
        st.vfx_task = "正在提交渲染…".to_owned();
        st.vfx_video.clear();
        let effect = st
            .vfx_effects
            .get(st.vfx_effect.max(0) as usize)
            .map(|(id, _)| id.clone())
            .unwrap_or_default();
        let preset = st
            .vfx_presets
            .get(st.vfx_preset.max(0) as usize)
            .map(|(id, _)| id.clone())
            .filter(|id| !id.is_empty());
        let size = ["720p", "1080p", "2k"][st.vfx_size.clamp(0, 2) as usize];
        let format = ["prores4444", "webm_vp9", "png"][st.vfx_format.clamp(0, 2) as usize];
        let duration = st
            .vfx_duration
            .trim()
            .parse::<f64>()
            .unwrap_or(3.0)
            .clamp(0.5, 10.0);
        st.vfx_line = format!(
            "正在渲染 {effect} · {size} · {format} · {duration:.1}s（真实 /vfx/render）…"
        );
        say(st, "正在渲染特效预览…", "info");
        let mut body = json!({
            "effect_id": effect,
            "duration": duration,
            "fps": 24,
            "size": size,
            "format": format,
            "seed": 20261124,
        });
        if let Some(preset) = preset {
            body["preset"] = json!(preset);
        }
        if format == "png" {
            body["png_zip"] = json!(false);
        }
        body
    });
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        move || concat_host::pro2::vfx_render(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let task = text_of(&value, &["task_id"]);
                        st.vfx_task = task.clone();
                        st.vfx_progress = 4.0;
                        st.vfx_status = "排队中".to_owned();
                        st.vfx_line = format!("渲染任务已提交：{task}");
                        say(st, "特效渲染任务已提交，进度每秒刷新。", "ok");
                    }
                    Err(error) => {
                        st.vfx_progress = 0.0;
                        st.vfx_status = "失败".to_owned();
                        st.vfx_task = error.clone();
                        st.vfx_line = format!("特效渲染失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

pub fn vfx_poll(weak: slint::Weak<App>, task: String) {
    let Some(app) = weak.upgrade() else { return };
    let _ = app;
    let task_for_job = task.clone();
    host::spawn(
        move || concat_host::pro2::vfx_task(&task_for_job),
        move |_studio, app, _models, result| {
            match result {
                Ok(value) => {
                    let inner = value.get("task").cloned().unwrap_or_else(|| value.clone());
                    let status = text_of(&inner, &["status"]);
                    let progress = num_of(&inner, &["progress"]).clamp(0.0, 100.0);
                    let url = find_url(&inner, &["url", "video_url", "output_url", "download_url"]);
                    let error = text_of(&inner, &["error"]);
                    let done = matches!(status.as_str(), "done" | "failed");
                    with(|st| {
                        st.vfx_progress = if done { 100.0 } else { progress.max(2.0) as f32 };
                        st.vfx_status = if status.is_empty() {
                            "渲染中".to_owned()
                        } else {
                            status.clone()
                        };
                        if !error.is_empty() {
                            st.vfx_line = format!("渲染失败：{error}");
                            say(st, st.vfx_line.clone(), "fail");
                        } else if !url.is_empty() {
                            st.vfx_video = url.clone();
                            st.vfx_line = format!("产物：{url}");
                        }
                        if done {
                            if status == "done" {
                                say(st, "特效渲染完成（真实产物）。", "ok");
                            }
                        } else {
                            st.vfx_task = format!("任务 {task} · {progress:.0}%");
                        }
                    });
                }
                Err(error) => {
                    with(|st| st.vfx_line = format!("轮询失败：{error}"));
                }
            }
            super::publish(&app);
        },
    );
}

fn find_url(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            if text.starts_with("http") || text.starts_with('/') {
                return absolute(text);
            }
        }
    }
    for node in value.as_object().into_iter().flat_map(|object| object.values()) {
        match node {
            Value::Object(_) => {
                let found = find_url(node, keys);
                if !found.is_empty() {
                    return found;
                }
            }
            Value::Array(items) => {
                for item in items {
                    let found = find_url(item, keys);
                    if !found.is_empty() {
                        return found;
                    }
                }
            }
            _ => {}
        }
    }
    String::new()
}

pub fn vfx_action(weak: slint::Weak<App>, action: &str, index: i32, text: String) {
    match action {
        "vfx-effect" => {
            with(|st| {
                st.vfx_effect = index.clamp(0, (st.vfx_effects.len() as i32 - 1).max(0));
                vfx_apply_preset(st);
            });
            super::publish_now();
        }
        "vfx-preset" => {
            with(|st| st.vfx_preset = index.max(0));
            super::publish_now();
        }
        "vfx-size" => {
            with(|st| st.vfx_size = index.clamp(0, 2));
            super::publish_now();
        }
        "vfx-format" => {
            with(|st| st.vfx_format = index.clamp(0, 2));
            super::publish_now();
        }
        "vfx-duration" => with(|st| st.vfx_duration = text),
        "vfx-load" => vfx_load(weak),
        "vfx-render" => vfx_render(weak),
        "vfx-open" => {
            let url = with(|st| st.vfx_video.clone());
            if url.is_empty() {
                with(|st| say(st, "还没有产物：先渲染一次。", "warn"));
                super::publish_now();
            } else {
                open_url(&url);
            }
        }
        "vfx-insert" => {
            let (url, name) = with(|st| {
                let effect = st
                    .vfx_effects
                    .get(st.vfx_effect.max(0) as usize)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| "特效".to_owned());
                let ext = match st.vfx_format {
                    0 => "mov",
                    1 => "webm",
                    _ => "zip",
                };
                (st.vfx_video.clone(), format!("特效_{effect}_{}.{ext}", super::now_secs()))
            });
            if url.is_empty() {
                with(|st| say(st, "还没有产物：先渲染一次。", "warn"));
                super::publish_now();
            } else if url.ends_with(".zip") {
                super::download_to(weak, url, "特效", name);
            } else {
                super::insert_to(weak, url, "特效", name);
            }
        }
        _ => {
            let _ = index;
        }
    }
}

// ── 配乐 ─────────────────────────────────────────────────────────────

pub fn music_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        || concat_host::pro2::music_library(),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        st.mu_library = Some(value.clone());
                        st.mu_styles.clear();
                        if let Some(styles) = value.get("styles").and_then(Value::as_array) {
                            for style in styles {
                                let name = text_of(style, &["name", "id"]);
                                st.mu_styles.push(name);
                            }
                        }
                        st.mu_style =
                            st.mu_style.clamp(0, (st.mu_styles.len() as i32 - 1).max(0));
                        st.mu_cues.clear();
                        let library = value.get("library").cloned().unwrap_or(Value::Null);
                        if let Some(cues) = library.get("cues").and_then(Value::as_array) {
                            for cue in cues.iter().take(14) {
                                let title = text_of(cue, &["title", "id"]);
                                let style = text_of(cue, &["style_name", "style"]);
                                let bpm = num_of(cue, &["bpm"]);
                                let duration = num_of(cue, &["duration_sec"]);
                                let url = text_of(cue, &["url"]);
                                st.mu_cues.push((
                                    row(
                                        title,
                                        format!("{style} · {bpm:.0} BPM"),
                                        format!("{:.0}s", duration),
                                        "info",
                                    ),
                                    url,
                                ));
                            }
                        }
                        st.mu_line = format!(
                            "曲库已取回：{} 风格 / {} 条成品 cue（真实 /music/library）。",
                            st.mu_styles.len(),
                            st.mu_cues.len()
                        );
                        say(st, "配乐接口在线：风格与成品曲库已载入。", "ok");
                    }
                    Err(error) => {
                        st.mu_line = format!("配乐库没取回来：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn music_score(weak: slint::Weak<App>) {
    let body = with(|st| {
        st.busy = true;
        st.mu_progress = 2.0;
        st.mu_status = "提交中".to_owned();
        st.mu_task = "正在生成配乐时间线…".to_owned();
        st.mu_audio.clear();
        st.mu_timeline.clear();
        let moods = ["史诗", "悬疑", "紧张", "动作", "悲伤", "温情", "科幻", "黑暗"];
        let mood = moods[st.mu_mood.clamp(0, 7) as usize];
        let minutes = st.mu_minutes.trim().parse::<f64>().unwrap_or(2.0).clamp(0.5, 5.0);
        let style = st
            .mu_library
            .as_ref()
            .and_then(|library| library.get("styles"))
            .and_then(Value::as_array)
            .and_then(|styles| styles.get(st.mu_style.max(0) as usize))
            .map(|style| text_of(style, &["id"]))
            .unwrap_or_default();
        let scenes = parse_scenes("", mood, minutes);
        st.mu_line = format!(
            "正在按场景配乐：{mood} / {minutes:.1} 分钟（真实 /music/score）…"
        );
        say(st, "正在生成配乐（score）…", "info");
        json!({
            "scenes": scenes,
            "style": style,
            "duration_min": minutes,
            "stems": false,
            "format": "mp3",
            "seed": 20261124,
        })
    });
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        move || concat_host::pro2::music_score(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let task = text_of(&value, &["task_id"]);
                        st.mu_task = task.clone();
                        st.mu_progress = 4.0;
                        st.mu_status = "生成中".to_owned();
                        let timeline = value.get("timeline").cloned().unwrap_or(Value::Null);
                        describe_timeline(st, &timeline);
                        st.mu_line = format!("配乐任务已提交：{task}（段落/卡点已回）");
                        say(st, "配乐任务已提交，进度每秒刷新。", "ok");
                    }
                    Err(error) => {
                        st.mu_progress = 0.0;
                        st.mu_status = "失败".to_owned();
                        st.mu_task = error.clone();
                        st.mu_line = format!("配乐失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn describe_timeline(st: &mut super::Pro2State, timeline: &Value) {
    st.mu_timeline.clear();
    let sections = timeline
        .get("sections")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let markers = timeline
        .get("markers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if sections.is_empty() && markers.is_empty() {
        return;
    }
    let sections_line = sections
        .iter()
        .take(6)
        .map(|section| {
            format!(
                "{} {:.0}s-{:.0}s",
                text_of(section, &["label", "type", "name"]),
                num_of(section, &["start_sec", "start"]),
                num_of(section, &["end_sec", "end"])
            )
        })
        .collect::<Vec<_>>()
        .join("；");
    let markers_line = markers
        .iter()
        .take(8)
        .map(|marker| {
            format!(
                "{}({:.0}s)",
                text_of(marker, &["type", "label"]),
                num_of(marker, &["t", "sec", "time"])
            )
        })
        .collect::<Vec<_>>()
        .join("、");
    st.mu_timeline = format!("段落：{sections_line}　卡点：{markers_line}");
}

pub fn music_poll(weak: slint::Weak<App>, task: String) {
    let Some(app) = weak.upgrade() else { return };
    let _ = app;
    let task_for_job = task.clone();
    host::spawn(
        move || concat_host::pro2::music_task(&task_for_job),
        move |_studio, app, _models, result| {
            match result {
                Ok(value) => {
                    let inner = value.get("task").cloned().unwrap_or_else(|| value.clone());
                    let status = text_of(&inner, &["status"]);
                    let progress = num_of(&inner, &["progress"]).clamp(0.0, 100.0);
                    let error = text_of(&inner, &["error"]);
                    let mut url = find_url(&inner, &["mp3", "audio_url", "url", "wav"]);
                    if url.is_empty() {
                        for key in ["outputs", "result"] {
                            if let Some(outputs) = inner.get(key) {
                                url = find_url(outputs, &["mp3", "audio_url", "url", "wav"]);
                                if !url.is_empty() {
                                    break;
                                }
                            }
                        }
                    }
                    let done = matches!(status.as_str(), "done" | "failed");
                    with(|st| {
                        st.mu_progress = if done { 100.0 } else { progress.max(2.0) as f32 };
                        st.mu_status = if status.is_empty() {
                            "生成中".to_owned()
                        } else {
                            status.clone()
                        };
                        if !error.is_empty() {
                            st.mu_line = format!("配乐失败：{error}");
                            say(st, st.mu_line.clone(), "fail");
                        } else if !url.is_empty() {
                            st.mu_audio = url.clone();
                            st.mu_line = format!("配乐成品：{url}");
                        }
                        if done {
                            st.mu_task.clear();
                            if status == "done" {
                                say(st, "配乐生成完成（真实产物）。", "ok");
                            }
                        } else {
                            st.mu_task = format!("任务 {task} · {progress:.0}%");
                        }
                    });
                }
                Err(error) => {
                    with(|st| st.mu_line = format!("轮询失败：{error}"));
                }
            }
            super::publish(&app);
        },
    );
}

pub fn music_action(weak: slint::Weak<App>, action: &str, index: i32, text: String) {
    match action {
        "mu-style" => {
            with(|st| st.mu_style = index.clamp(0, (st.mu_styles.len() as i32 - 1).max(0)));
            super::publish_now();
        }
        "mu-mood" => {
            with(|st| st.mu_mood = index.clamp(0, 7));
            super::publish_now();
        }
        "mu-minutes" => with(|st| st.mu_minutes = text),
        "mu-score" => music_score(weak),
        "mu-open" => {
            let url = with(|st| st.mu_audio.clone());
            if url.is_empty() {
                with(|st| say(st, "还没有配乐：先按场景配乐。", "warn"));
                super::publish_now();
            } else {
                open_url(&url);
            }
        }
        "mu-insert" => {
            let url = with(|st| st.mu_audio.clone());
            if url.is_empty() {
                with(|st| say(st, "还没有配乐：先按场景配乐。", "warn"));
                super::publish_now();
            } else {
                super::insert_to(weak, url, "配乐", format!("配乐_{}.mp3", super::now_secs()));
            }
        }
        "mu-cue-play" => {
            let cue = with(|st| st.mu_cues.get(index.max(0) as usize).cloned());
            if let Some((row, url)) = cue {
                let name = format!("{}.mp3", sanitize(&row.label));
                super::download_to(weak, absolute(&url), "配乐/曲库", name);
            }
        }
        _ => {}
    }
}

fn sanitize(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => ch,
        })
        .take(48)
        .collect()
}

// ── 配音 ─────────────────────────────────────────────────────────────

const EMOTIONS: [(&str, &str); 8] = [
    ("neutral", "平静"),
    ("happy", "开心"),
    ("sad", "悲伤"),
    ("angry", "愤怒"),
    ("fear", "恐惧"),
    ("tender", "温柔"),
    ("excited", "激动"),
    ("whisper", "耳语"),
];

pub fn voice_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    super::publish(&app);
    host::spawn(
        || concat_host::pro2::voice_roles(),
        |_studio, app, _models, result| {
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        st.vo_raw = Some(value.clone());
                        st.vo_roles.clear();
                        if let Some(roles) = value.get("roles").and_then(Value::as_array) {
                            for role in roles {
                                let preset = text_of(role, &["preset_id", "id"]);
                                let name = text_of(role, &["name"]);
                                let category = text_of(role, &["category"]);
                                st.vo_roles.push((preset, format!("{name} · {category}")));
                            }
                        }
                        st.vo_line = format!(
                            "角色音色库已取回：{} 个角色（真实 /voice/roles）。",
                            st.vo_roles.len()
                        );
                        say(st, "配音接口在线：角色音色库已载入。", "ok");
                    }
                    Err(error) => {
                        st.vo_line = format!("角色库没取回来：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn role_of(st: &super::Pro2State) -> (String, String) {
    st.vo_roles
        .get(st.vo_role.max(0) as usize)
        .cloned()
        .unwrap_or_else(|| (String::new(), String::new()))
}

fn voice_advise(weak: slint::Weak<App>) {
    let (text, preset) = with(|st| (st.vo_text.clone(), role_of(st).0));
    if text.trim().is_empty() {
        with(|st| say(st, "先写一句台词。", "warn"));
        super::publish_now();
        return;
    }
    let body = json!({
        "text": text,
        "scene_context": "桌面配音导演：单句情感标注",
        "preset_id": preset,
    });
    with(|st| {
        st.vo_progress = 10.0;
        st.vo_status = "标注中".to_owned();
        st.vo_advice = "正在围绕台词标注情感与强度（真实 /voice/advise）…".to_owned();
        say(st, "正在做台词情感标注…", "info");
    });
    super::publish_now();
    host::spawn(
        move || concat_host::pro2::voice_advise(&body),
        |_studio, app, _models, result| {
            with(|st| {
                st.vo_progress = 100.0;
                match result {
                    Ok(value) => {
                        let lines = value
                            .get("lines")
                            .or_else(|| value.get("advice").and_then(|advice| advice.get("lines")))
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        if let Some(line) = lines.first() {
                            let emotion = text_of(line, &["emotion"]);
                            let intensity = num_of(line, &["intensity"]);
                            let note = text_of(line, &["note", "reason"]);
                            st.vo_advice = format!(
                                "建议：emotion={emotion} / intensity={intensity:.2}{}",
                                if note.is_empty() {
                                    String::new()
                                } else {
                                    format!(" / {note}")
                                }
                            );
                            for (index, (id, _)) in EMOTIONS.iter().enumerate() {
                                if *id == emotion {
                                    st.vo_emotion = index as i32;
                                }
                            }
                        } else {
                            st.vo_advice = "平台没有返回逐句情感（可直接生成试听）。".to_owned();
                        }
                        st.vo_status = "标注完成".to_owned();
                        say(st, "台词情感标注完成。", "ok");
                    }
                    Err(error) => {
                        st.vo_status = "标注失败（可直接生成）".to_owned();
                        st.vo_advice = format!("情感标注失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

fn voice_dub(weak: slint::Weak<App>) {
    let (text, preset, name, emotion) = with(|st| {
        let (preset, name) = role_of(st);
        (
            st.vo_text.clone(),
            preset,
            name,
            EMOTIONS[st.vo_emotion.clamp(0, 7) as usize].0.to_owned(),
        )
    });
    if text.trim().is_empty() {
        with(|st| say(st, "先写一句台词。", "warn"));
        super::publish_now();
        return;
    }
    let body = json!({
        "title": "桌面配音试听",
        "scene_context": "桌面单句试听",
        "roles": [{ "name": "角色", "preset_id": preset, "voice_label": name }],
        "lines": [{
            "text": text,
            "role": "角色",
            "emotion": emotion,
            "intensity": 0.7,
        }],
        "takes": 1,
    });
    with(|st| {
        st.vo_progress = 5.0;
        st.vo_status = "提交中".to_owned();
        st.vo_task = "正在提交配音任务…".to_owned();
        st.vo_audio.clear();
        st.vo_line = "正在生成试听（真实 /voice/dub，edge-tts 兜底）…".to_owned();
        say(st, "正在生成配音试听…", "info");
    });
    super::publish_now();
    host::spawn(
        move || concat_host::pro2::voice_dub(&body),
        |_studio, app, _models, result| {
            with(|st| {
                match result {
                    Ok(value) => {
                        let task = text_of(&value, &["task_id"]);
                        st.vo_task = task.clone();
                        st.vo_progress = 8.0;
                        st.vo_status = "生成中".to_owned();
                        st.vo_line = format!("配音任务已提交：{task}（逐句合成 + 混音）");
                        say(st, "配音任务已提交，进度每秒刷新。", "ok");
                    }
                    Err(error) => {
                        st.vo_progress = 0.0;
                        st.vo_status = "失败".to_owned();
                        st.vo_task = error.clone();
                        st.vo_line = format!("配音失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            super::publish(&app);
        },
    );
}

pub fn voice_poll(weak: slint::Weak<App>, task: String) {
    let Some(app) = weak.upgrade() else { return };
    let _ = app;
    let task_for_job = task.clone();
    host::spawn(
        move || concat_host::pro2::voice_task(&task_for_job),
        move |_studio, app, _models, result| {
            match result {
                Ok(value) => {
                    let inner = value.get("task").cloned().unwrap_or_else(|| value.clone());
                    let status = text_of(&inner, &["status"]);
                    let progress = num_of(&inner, &["progress"]).clamp(0.0, 100.0);
                    let error = text_of(&inner, &["error"]);
                    let mut url = find_url(&inner, &["mp3", "mix_url", "audio_url", "url"]);
                    if url.is_empty() {
                        if let Some(outputs) = inner.get("outputs") {
                            url = find_url(outputs, &["mp3", "mix_url", "audio_url", "url"]);
                        }
                    }
                    let done = matches!(status.as_str(), "done" | "failed");
                    with(|st| {
                        st.vo_progress = if done { 100.0 } else { progress.max(2.0) as f32 };
                        st.vo_status = if status.is_empty() {
                            "生成中".to_owned()
                        } else {
                            status.clone()
                        };
                        if !error.is_empty() {
                            st.vo_line = format!("配音失败：{error}");
                            say(st, st.vo_line.clone(), "fail");
                        } else if !url.is_empty() {
                            st.vo_audio = url.clone();
                            st.vo_line = format!("试听成品：{url}");
                        }
                        if done {
                            st.vo_task.clear();
                            if status == "done" {
                                say(st, "配音试听生成完成。", "ok");
                            }
                        } else {
                            st.vo_task = format!("任务 {task} · {progress:.0}%");
                        }
                    });
                }
                Err(error) => {
                    with(|st| st.vo_line = format!("轮询失败：{error}"));
                }
            }
            super::publish(&app);
        },
    );
}

pub fn voice_action(weak: slint::Weak<App>, action: &str, index: i32, text: String) {
    match action {
        "vo-role" => {
            with(|st| st.vo_role = index.clamp(0, (st.vo_roles.len() as i32 - 1).max(0)));
            super::publish_now();
        }
        "vo-emotion" => {
            with(|st| st.vo_emotion = index.clamp(0, 7));
            super::publish_now();
        }
        "vo-text" => with(|st| st.vo_text = text),
        "vo-advise" => voice_advise(weak),
        "vo-dub" => voice_dub(weak),
        "vo-open" => {
            let url = with(|st| st.vo_audio.clone());
            if url.is_empty() {
                with(|st| say(st, "还没有试听：先生成。", "warn"));
                super::publish_now();
            } else {
                open_url(&url);
            }
        }
        "vo-insert" => {
            let url = with(|st| st.vo_audio.clone());
            if url.is_empty() {
                with(|st| say(st, "还没有试听：先生成。", "warn"));
                super::publish_now();
            } else {
                super::insert_to(weak, url, "配音", format!("配音_{}.mp3", super::now_secs()));
            }
        }
        _ => {}
    }
}
