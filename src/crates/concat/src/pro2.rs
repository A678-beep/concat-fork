// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 影视专业二期面板（0.4.0）：窗口这一半。
//!
//! 九个页签：导演台（双屏独立窗口）/ 片头片尾 / 实拍装配 / 特效 / 配乐 /
//! 配音 / 连贯性检查 / 风格匹配 / 素材质量。网络调用都在
//! [`concat_host::pro2`]；这里管面板状态、把动作翻译成请求、把结果写回
//! Slint。
//!
//! 懒加载与 0.3.7 面板一致：启动路径上只有一份空的 `Pro2Data`，没有任何
//! 网络请求；打开页签后才按当前页取数，每次请求带 epoch，切页 / 关面板后
//! 旧结果直接丢掉。

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use slint::{Image, ModelRc, SharedString, VecModel};

use crate::host::{self, Shell};
use crate::studio::Studio;
use crate::ui::*;

mod assembly;
mod check_tabs;
mod credits;
mod media_tabs;

pub(crate) const TAB_DIRECTOR: i32 = 0;
pub(crate) const TAB_CREDITS: i32 = 1;
pub(crate) const TAB_ASSEMBLY: i32 = 2;
pub(crate) const TAB_VFX: i32 = 3;
pub(crate) const TAB_MUSIC: i32 = 4;
pub(crate) const TAB_VOICE: i32 = 5;
pub(crate) const TAB_CONTINUITY: i32 = 6;
pub(crate) const TAB_STYLE: i32 = 7;
pub(crate) const TAB_QUALITY: i32 = 8;

/// 页签标题下的一句话：接口 / 数据来源。
fn hint_of(tab: i32) -> String {
    match tab {
        TAB_DIRECTOR => "导演台（0.4.0）：独立窗口（可拖到第二显示器 / 可全屏）+ 同屏浮窗；成片、分镜、镜头状态板、批注同步，主窗口继续编辑不阻塞。接口 /filmgen/monitor/*。".to_owned(),
        TAB_CREDITS => "片头片尾：名单编辑（出品/制片/导演/主演 billing/crew 部门）→ 6 种模板 → 生成片头/片尾 → 预览 → 导出（MP4/ASS/SRT/zip）。接口 /credits/titles/*。".to_owned(),
        TAB_ASSEMBLY => "实拍装配：导入素材 + 剧本 → ingest 进度 → plan（EDL / 缺口 / 置信度 / 备选）→ 确认 → render 粗剪 → 导入时间线 / 打开成片。接口 /assembly/*。".to_owned(),
        TAB_VFX => "特效：9 类 27 预设，参数 schema 由 /vfx/library 下发；渲染 3s 预览（ProRes4444 / WebM / PNG，带 alpha）→ 下载 / 插入时间线。接口 /vfx/*。".to_owned(),
        TAB_MUSIC => "配乐：28 风格 + 168 成品 cue 试听；按场景时间轴 / 情绪点 → /music/score 出段落与卡点 → 生成 → 插入。100% 程序化原创。接口 /music/*。".to_owned(),
        TAB_VOICE => "配音：40 个角色音色；台词情感标注 → /voice/dub 逐句合成 → 试听。真实通道目前 edge-tts 兜底并逐句标注降级。接口 /voice/*。".to_owned(),
        TAB_CONTINUITY => "连贯性检查：角色一致性 / 跨镜连贯 / 穿帮 / 光照时间 → 分数 + 问题列表 + HTML 报告。接口 /continuity/*；AI 辅助判定，需人工复核。".to_owned(),
        TAB_STYLE => "风格匹配：参考片 → 风格指纹（节奏/运动/色彩/质感）→ 应用色彩（ΔE00 前后对比）→ 导出 .cube LUT。接口 /style/*。".to_owned(),
        _ => "素材质量：/api/assets/search?quality=cinema_candidate|commercial_hd 筛选 + 平台质量徽章。".to_owned(),
    }
}

/// 网页兜底页（接口不通 / 想看网页版时）。
fn web_of(tab: i32) -> &'static str {
    match tab {
        TAB_DIRECTOR => "monitor",
        TAB_CREDITS => "credits",
        TAB_ASSEMBLY => "assembly",
        TAB_VFX => "vfx",
        TAB_MUSIC => "music",
        TAB_VOICE => "voice",
        TAB_CONTINUITY => "continuity",
        TAB_STYLE => "style",
        _ => "assets",
    }
}

/// 验收 / 示例用：平台已存在的实拍素材（5 段，共 9 镜），导入后能真出 EDL。
pub(crate) const SAMPLE_ASSETS: [&str; 5] = [
    "https://xinglaiai.com/ai-nav/studio/files/assembly/assets/asm_20261124081708_dcaf/src/01_01_morning_shop.mp4",
    "https://xinglaiai.com/ai-nav/studio/files/assembly/assets/asm_20261124081708_dcaf/src/02_02_office_meeting.mp4",
    "https://xinglaiai.com/ai-nav/studio/files/assembly/assets/asm_20261124081708_dcaf/src/03_03_park_afternoon.mp4",
    "https://xinglaiai.com/ai-nav/studio/files/assembly/assets/asm_20261124081708_dcaf/src/04_04_night_shop.mp4",
    "https://xinglaiai.com/ai-nav/studio/files/assembly/assets/asm_20261124081708_dcaf/src/05_05_rain_window_broll.mp4",
];

/// 验收 / 示例用：一段平台可下载的样片（连贯性检查 / 风格指纹 / 色彩迁移）。
pub(crate) const SAMPLE_VIDEO: &str =
    "https://xinglaiai.com/ai-nav/studio/files/assembly/assets/asm_20261124081708_dcaf/src/01_01_morning_shop.mp4";

// ── 行数据（发布到 Slint） ───────────────────────────────────────────

#[derive(Clone, Default)]
pub(crate) struct Row {
    pub label: String,
    pub detail: String,
    pub value: String,
    pub level: String,
}

pub(crate) fn row(label: impl Into<String>, detail: impl Into<String>, value: impl Into<String>, level: &str) -> Row {
    Row {
        label: label.into(),
        detail: detail.into(),
        value: value.into(),
        level: level.to_owned(),
    }
}

pub(crate) fn rows_model(rows: &[Row]) -> ModelRc<Pro2Row> {
    ModelRc::from(std::rc::Rc::new(VecModel::from(
        rows.iter()
            .map(|row| Pro2Row {
                label: row.label.clone().into(),
                detail: row.detail.clone().into(),
                value: row.value.clone().into(),
                level: row.level.clone().into(),
            })
            .collect::<Vec<_>>(),
    )))
}

fn strings_model(items: &[String]) -> ModelRc<SharedString> {
    ModelRc::from(std::rc::Rc::new(VecModel::from(
        items.iter().map(|item| SharedString::from(item.as_str())).collect::<Vec<_>>(),
    )))
}

// ── 面板状态 ─────────────────────────────────────────────────────────

/// 0.4.0 面板的全部状态。每个页签一段，命名前缀区分。
pub(crate) struct Pro2State {
    pub open: bool,
    pub tab: i32,
    pub signed_in: bool,
    pub epoch: u64,
    pub busy: bool,
    pub status_line: String,
    pub status_level: String,
    pub hint: String,

    // 片头片尾
    pub cr_styles: Vec<String>,
    pub cr_style: i32,
    pub cr_kind: i32,
    pub cr_res: i32,
    pub cr_title: String,
    pub cr_studios: String,
    pub cr_producers: String,
    pub cr_director: String,
    pub cr_cast: String,
    pub cr_crew: String,
    pub cr_year: String,
    pub cr_ai: bool,
    pub cr_license: String,
    pub cr_record: String,
    pub cr_project: String,
    pub cr_demo: Option<Value>,
    pub cr_line: String,
    pub cr_task: String,
    pub cr_progress: f32,
    pub cr_status: String,
    pub cr_preview_line: String,
    pub cr_preview: Option<Image>,
    pub cr_mp4: String,
    pub cr_zip: String,
    pub cr_qc_line: String,
    pub cr_pending: Option<String>,

    // 实拍装配
    pub as_name: String,
    pub as_assets: String,
    pub as_urls: String,
    pub as_script: String,
    pub as_style: String,
    pub as_line: String,
    pub as_detail: String,
    pub as_task: String,
    pub as_task_kind: String,
    pub as_progress: f32,
    pub as_status: String,
    pub as_plan: String,
    pub as_render: String,
    pub as_video: String,
    pub as_pack: String,
    pub as_materials: Vec<Row>,
    pub as_edl: Vec<Row>,
    pub as_gaps: Vec<Row>,
    pub as_health: String,
    pub as_confirm_seqs: Vec<i64>,
    pub as_plan_confidence: String,

    // 特效
    pub vfx_effects: Vec<(String, String)>,
    pub vfx_effect: i32,
    pub vfx_presets: Vec<(String, String)>,
    pub vfx_preset: i32,
    pub vfx_size: i32,
    pub vfx_format: i32,
    pub vfx_duration: String,
    pub vfx_line: String,
    pub vfx_params: String,
    pub vfx_task: String,
    pub vfx_progress: f32,
    pub vfx_status: String,
    pub vfx_video: String,
    pub vfx_jobs: Vec<Row>,
    pub vfx_library: Option<Value>,
    pub vfx_insert_name: String,

    // 配乐
    pub mu_styles: Vec<String>,
    pub mu_style: i32,
    pub mu_mood: i32,
    pub mu_minutes: String,
    pub mu_line: String,
    pub mu_task: String,
    pub mu_progress: f32,
    pub mu_status: String,
    pub mu_audio: String,
    pub mu_timeline: String,
    pub mu_cues: Vec<(Row, String)>,
    pub mu_library: Option<Value>,

    // 配音
    pub vo_roles: Vec<(String, String)>,
    pub vo_role: i32,
    pub vo_emotion: i32,
    pub vo_text: String,
    pub vo_line: String,
    pub vo_task: String,
    pub vo_progress: f32,
    pub vo_status: String,
    pub vo_audio: String,
    pub vo_advice: String,
    pub vo_raw: Option<Value>,
    pub vo_cast_line: String,

    // 连贯性
    pub ct_video: String,
    pub ct_line: String,
    pub ct_status: String,
    pub ct_score: String,
    pub ct_report: String,
    pub ct_run: String,
    pub ct_issues: Vec<Row>,
    pub ct_runs: Vec<Row>,

    // 风格匹配
    pub st_ref: String,
    pub st_target: String,
    pub st_strength: String,
    pub st_line: String,
    pub st_status: String,
    pub st_profile: String,
    pub st_labels: String,
    pub st_color: String,
    pub st_lut: String,
    pub st_metrics_line: String,
    pub st_metrics: Vec<Row>,
    pub st_profiles: Vec<(String, String)>,

    // 素材质量
    pub ql_filter: i32,
    pub ql_line: String,
    pub ql_badge: String,
    pub ql_items: Vec<Row>,
    pub ql_total: String,

    pub ticks: u32,
}

impl Default for Pro2State {
    fn default() -> Self {
        Pro2State {
            open: false,
            tab: TAB_DIRECTOR,
            signed_in: false,
            epoch: 0,
            busy: false,
            status_line: String::new(),
            status_level: "info".to_owned(),
            hint: hint_of(TAB_DIRECTOR),

            cr_styles: Vec::new(),
            cr_style: 0,
            cr_kind: 2,
            cr_res: 0,
            cr_title: "星海长歌".to_owned(),
            cr_studios: "星籁影业、深空传媒".to_owned(),
            cr_producers: "陈默、周知".to_owned(),
            cr_director: "林知远".to_owned(),
            cr_cast: "领衔主演=周砚、沈栖、陈叙；特别出演=贺远".to_owned(),
            cr_crew: "摄影=摄影指导：王砚\n灯光=灯光指导：周行\n美术=美术指导：苏棠".to_owned(),
            cr_year: "2026".to_owned(),
            cr_ai: false,
            cr_license: String::new(),
            cr_record: String::new(),
            cr_project: "desktop-demo-037".to_owned(),
            cr_demo: None,
            cr_line: "还没载入模板与示例名单。".to_owned(),
            cr_task: String::new(),
            cr_progress: 0.0,
            cr_status: String::new(),
            cr_preview_line: String::new(),
            cr_preview: None,
            cr_mp4: String::new(),
            cr_zip: String::new(),
            cr_qc_line: String::new(),
            cr_pending: None,

            as_name: String::new(),
            as_assets: String::new(),
            as_urls: SAMPLE_ASSETS.join("\n"),
            as_script: "第1场 清晨·豆浆铺\n阿婆：早啊，今天还是老样子？\n小满：嗯，谢谢阿婆。\n\n第2场 白天·会议室\n主管：这个月的目标，大家心里有数。\n\n第3场 傍晚·公园\n小满：我想，慢一点也没关系。\n\n第4场 夜里·老屋院子\n阿婆（画外）：人啊，总要记得回家的路。\n\n第5场 深夜·小店\n旁白：城市睡了，还有一盏灯亮着。".to_owned(),
            as_style: "纪实".to_owned(),
            as_line: "先导入素材（ingest），再生成 EDL（plan）。".to_owned(),
            as_detail: String::new(),
            as_task: String::new(),
            as_task_kind: String::new(),
            as_progress: 0.0,
            as_status: String::new(),
            as_plan: String::new(),
            as_render: String::new(),
            as_video: String::new(),
            as_pack: String::new(),
            as_materials: Vec::new(),
            as_edl: Vec::new(),
            as_gaps: Vec::new(),
            as_health: String::new(),
            as_confirm_seqs: Vec::new(),
            as_plan_confidence: String::new(),

            vfx_effects: Vec::new(),
            vfx_effect: 0,
            vfx_presets: Vec::new(),
            vfx_preset: 0,
            vfx_size: 0,
            vfx_format: 1,
            vfx_duration: "3".to_owned(),
            vfx_line: String::new(),
            vfx_params: String::new(),
            vfx_task: String::new(),
            vfx_progress: 0.0,
            vfx_status: String::new(),
            vfx_video: String::new(),
            vfx_jobs: Vec::new(),
            vfx_library: None,
            vfx_insert_name: String::new(),

            mu_styles: Vec::new(),
            mu_style: 0,
            mu_mood: 2,
            mu_minutes: "2".to_owned(),
            mu_line: String::new(),
            mu_task: String::new(),
            mu_progress: 0.0,
            mu_status: String::new(),
            mu_audio: String::new(),
            mu_timeline: String::new(),
            mu_cues: Vec::new(),
            mu_library: None,

            vo_roles: Vec::new(),
            vo_role: 0,
            vo_emotion: 0,
            vo_text: "这条街，我走了十年，今晚却觉得陌生。".to_owned(),
            vo_line: String::new(),
            vo_task: String::new(),
            vo_progress: 0.0,
            vo_status: String::new(),
            vo_audio: String::new(),
            vo_advice: String::new(),
            vo_raw: None,
            vo_cast_line: String::new(),

            ct_video: SAMPLE_VIDEO.to_owned(),
            ct_line: String::new(),
            ct_status: String::new(),
            ct_score: String::new(),
            ct_report: String::new(),
            ct_run: String::new(),
            ct_issues: Vec::new(),
            ct_runs: Vec::new(),

            st_ref: SAMPLE_VIDEO.to_owned(),
            st_target: SAMPLE_VIDEO.to_owned(),
            st_strength: "0.85".to_owned(),
            st_line: String::new(),
            st_status: String::new(),
            st_profile: String::new(),
            st_labels: String::new(),
            st_color: String::new(),
            st_lut: String::new(),
            st_metrics_line: String::new(),
            st_metrics: Vec::new(),
            st_profiles: Vec::new(),

            ql_filter: 0,
            ql_line: String::new(),
            ql_badge: String::new(),
            ql_items: Vec::new(),
            ql_total: String::new(),

            ticks: 0,
        }
    }
}

thread_local! {
    static PRO2: std::cell::RefCell<Pro2State> = std::cell::RefCell::new(Pro2State::default());
}

/// 借用面板状态（短借，不在里面做网络）。
pub(crate) fn with<T>(body: impl FnOnce(&mut Pro2State) -> T) -> T {
    PRO2.with(|cell| body(&mut cell.borrow_mut()))
}

/// 现在的 Unix 秒。
pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── 发布 ─────────────────────────────────────────────────────────────

fn publish(app: &App) {
    let (dr_project, dr_line, dr_status) = crate::director::summary();
    PRO2.with(|cell| {
        let st = cell.borrow();
        let data = Pro2Data {
            dr_project: dr_project.clone().into(),
            dr_line: dr_line.clone().into(),
            dr_status: dr_status.clone().into(),
            open: st.open,
            tab: st.tab,
            busy: st.busy,
            signed_in: st.signed_in,
            status_line: st.status_line.clone().into(),
            status_level: st.status_level.clone().into(),
            hint: st.hint.clone().into(),

            cr_style: st.cr_style,
            cr_kind: st.cr_kind,
            cr_res: st.cr_res,
            cr_title: st.cr_title.clone().into(),
            cr_studios: st.cr_studios.clone().into(),
            cr_producers: st.cr_producers.clone().into(),
            cr_director: st.cr_director.clone().into(),
            cr_cast: st.cr_cast.clone().into(),
            cr_crew: st.cr_crew.clone().into(),
            cr_year: st.cr_year.clone().into(),
            cr_ai: st.cr_ai,
            cr_license: st.cr_license.clone().into(),
            cr_record: st.cr_record.clone().into(),
            cr_line: st.cr_line.clone().into(),
            cr_task: st.cr_task.clone().into(),
            cr_progress: st.cr_progress,
            cr_status: st.cr_status.clone().into(),
            cr_preview_line: st.cr_preview_line.clone().into(),
            cr_project: st.cr_project.clone().into(),
            cr_mp4: st.cr_mp4.clone().into(),
            cr_zip: st.cr_zip.clone().into(),
            cr_qc_line: st.cr_qc_line.clone().into(),

            as_assets: st.as_assets.clone().into(),
            as_name: st.as_name.clone().into(),
            as_urls: st.as_urls.clone().into(),
            as_script: st.as_script.clone().into(),
            as_style: st.as_style.clone().into(),
            as_line: st.as_line.clone().into(),
            as_task: st.as_task.clone().into(),
            as_progress: st.as_progress,
            as_status: st.as_status.clone().into(),
            as_plan: st.as_plan.clone().into(),
            as_render: st.as_render.clone().into(),
            as_video: st.as_video.clone().into(),
            as_pack: st.as_pack.clone().into(),
            as_line_detail: st.as_detail.clone().into(),

            vfx_effect: st.vfx_effect,
            vfx_preset: st.vfx_preset,
            vfx_size: st.vfx_size,
            vfx_format: st.vfx_format,
            vfx_duration: st.vfx_duration.clone().into(),
            vfx_line: st.vfx_line.clone().into(),
            vfx_task: st.vfx_task.clone().into(),
            vfx_progress: st.vfx_progress,
            vfx_status: st.vfx_status.clone().into(),
            vfx_video: st.vfx_video.clone().into(),
            vfx_params: st.vfx_params.clone().into(),

            mu_style: st.mu_style,
            mu_mood: st.mu_mood,
            mu_minutes: st.mu_minutes.clone().into(),
            mu_line: st.mu_line.clone().into(),
            mu_task: st.mu_task.clone().into(),
            mu_progress: st.mu_progress,
            mu_status: st.mu_status.clone().into(),
            mu_audio: st.mu_audio.clone().into(),
            mu_timeline: st.mu_timeline.clone().into(),

            vo_role: st.vo_role,
            vo_emotion: st.vo_emotion,
            vo_text: st.vo_text.clone().into(),
            vo_line: st.vo_line.clone().into(),
            vo_task: st.vo_task.clone().into(),
            vo_progress: st.vo_progress,
            vo_status: st.vo_status.clone().into(),
            vo_audio: st.vo_audio.clone().into(),
            vo_advice: st.vo_advice.clone().into(),

            ct_video: st.ct_video.clone().into(),
            ct_line: st.ct_line.clone().into(),
            ct_status: st.ct_status.clone().into(),
            ct_score: st.ct_score.clone().into(),
            ct_report: st.ct_report.clone().into(),
            ct_runs_line: st.ct_runs.len().to_string().into(),

            st_ref: st.st_ref.clone().into(),
            st_target: st.st_target.clone().into(),
            st_strength: st.st_strength.clone().into(),
            st_line: st.st_line.clone().into(),
            st_status: st.st_status.clone().into(),
            st_profile: st.st_profile.clone().into(),
            st_labels: st.st_labels.clone().into(),
            st_color: st.st_color.clone().into(),
            st_lut: st.st_lut.clone().into(),
            st_metrics_line: st.st_metrics_line.clone().into(),

            ql_filter: st.ql_filter,
            ql_line: st.ql_line.clone().into(),
            ql_badge: st.ql_badge.clone().into(),
        };
        app.set_pro2(data);
        app.set_pro2_preview(st.cr_preview.clone().unwrap_or_default());
        app.set_pro2_has_preview(st.cr_preview.is_some());

        app.set_pro2_credits_templates(rows_model(
            &st.cr_styles
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    row(
                        name.clone(),
                        String::new(),
                        String::new(),
                        if index as i32 == st.cr_style { "ok" } else { "" },
                    )
                })
                .collect::<Vec<_>>(),
        ));
        app.set_pro2_assembly_materials(rows_model(&st.as_materials));
        app.set_pro2_assembly_edl(rows_model(&st.as_edl));
        app.set_pro2_assembly_gaps(rows_model(&st.as_gaps));
        app.set_pro2_vfx_effects(strings_model(
            &st.vfx_effects.iter().map(|(_, label)| label.clone()).collect::<Vec<_>>(),
        ));
        app.set_pro2_vfx_presets(strings_model(
            &st.vfx_presets.iter().map(|(_, label)| label.clone()).collect::<Vec<_>>(),
        ));
        app.set_pro2_vfx_jobs(rows_model(&st.vfx_jobs));
        app.set_pro2_music_styles(strings_model(&st.mu_styles));
        app.set_pro2_music_cues(rows_model(
            &st.mu_cues.iter().map(|(row, _)| row.clone()).collect::<Vec<_>>(),
        ));
        app.set_pro2_voice_roles(strings_model(
            &st.vo_roles.iter().map(|(_, label)| label.clone()).collect::<Vec<_>>(),
        ));
        app.set_pro2_continuity_issues(rows_model(&st.ct_issues));
        app.set_pro2_continuity_runs(rows_model(&st.ct_runs));
        app.set_pro2_style_profiles(strings_model(
            &st.st_profiles.iter().map(|(_, label)| label.clone()).collect::<Vec<_>>(),
        ));
        app.set_pro2_style_metrics(rows_model(&st.st_metrics));
        app.set_pro2_assets(rows_model(&st.ql_items));
    });
}

/// 在事件循环线程上把状态写回 Slint。
pub(crate) fn publish_now() {
    Shell::with(|_shell, app| publish(&app));
}

/// 启动时写一份空表：面板没开，也没有任何网络工作。
pub fn initialize(app: &App) {
    publish(app);
}

// ── 打开 / 关闭 / 页签 ───────────────────────────────────────────────

/// 放进队列，等窗口消息处理完再开（先借后还，不在 Studio 借用里开面板）。
thread_local! {
    static WANTED: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

pub fn want(tab: i32) {
    WANTED.with(|slot| slot.set(Some(tab.clamp(0, TAB_QUALITY))));
}

pub fn take_wanted() -> Option<i32> {
    WANTED.with(|slot| slot.take())
}

/// 打开面板（事件循环线程、Studio 借用之外调用）。
pub fn open(weak: slint::Weak<App>, tab: i32) {
    let signed = concat_host::pro::signed_in();
    with(|st| {
        st.signed_in = signed;
        st.open = true;
        st.tab = tab.clamp(0, TAB_QUALITY);
        st.epoch += 1;
        st.status_line.clear();
        st.status_level = "info".to_owned();
        st.hint = hint_of(st.tab);
        st.busy = false;
    });
    let tab = with(|st| st.tab);
    publish_now();
    load_tab(weak, tab);
}

/// 关闭面板。
pub fn close(weak: slint::Weak<App>) {
    if let Some(app) = weak.upgrade() {
        with(|st| {
            st.open = false;
            st.epoch += 1;
        });
        publish(&app);
    }
}

/// 切页签：取这一页该取的数据。
fn load_tab(weak: slint::Weak<App>, tab: i32) {
    match tab {
        TAB_DIRECTOR => crate::director::refresh(weak),
        TAB_CREDITS => credits::load(weak),
        TAB_ASSEMBLY => assembly::load(weak),
        TAB_VFX => media_tabs::vfx_load(weak),
        TAB_MUSIC => media_tabs::music_load(weak),
        TAB_VOICE => media_tabs::voice_load(weak),
        TAB_CONTINUITY => check_tabs::continuity_load(weak),
        TAB_STYLE => check_tabs::style_load(weak),
        _ => check_tabs::quality_load(weak),
    }
}

/// Timer：每秒一拍。轮询进行中的任务。
pub fn tick(weak: slint::Weak<App>) {
    let (kind, id, open) = with(|st| {
        st.ticks = st.ticks.wrapping_add(1);
        (st.as_task_kind.clone(), st.as_task.clone(), st.open)
    });
    if !open {
        return;
    }
    if !id.is_empty() {
        assembly::poll(weak.clone(), kind, id);
    }
    let cr = with(|st| st.cr_pending.clone());
    if let Some(id) = cr {
        credits::poll(weak.clone(), id);
    }
    let vfx = with(|st| st.vfx_task.clone());
    if !vfx.is_empty() && with(|st| st.vfx_progress < 100.0) {
        media_tabs::vfx_poll(weak.clone(), vfx);
    }
    let mu = with(|st| st.mu_task.clone());
    if !mu.is_empty() && with(|st| st.mu_progress < 100.0) {
        media_tabs::music_poll(weak.clone(), mu);
    }
    let vo = with(|st| st.vo_task.clone());
    if !vo.is_empty() && with(|st| st.vo_progress < 100.0) {
        media_tabs::voice_poll(weak.clone(), vo);
    }
}

// ── 动作分发 ─────────────────────────────────────────────────────────

fn say(st: &mut Pro2State, line: impl Into<String>, level: &str) {
    st.status_line = line.into();
    st.status_level = level.to_owned();
}

fn open_web(tab: i32) {
    let panel = web_of(tab);
    let url = concat_host::pro::web_url(panel);
    if let Err(error) = opener::open(url.as_str()) {
        log::warn!("pro2: 打开网页版失败：{error}");
    }
}

/// 面板上的一个手势。
pub fn action(weak: slint::Weak<App>, action: String, index: i32, text: String) {
    let Some(app) = weak.upgrade() else { return };

    match action.as_str() {
        "tab" => {
            let tab = index.clamp(0, TAB_QUALITY);
            let changed = with(|st| {
                if st.tab == tab {
                    return false;
                }
                st.tab = tab;
                st.epoch += 1;
                st.busy = false;
                st.status_line.clear();
                st.status_level = "info".to_owned();
                st.hint = hint_of(tab);
                true
            });
            if !changed {
                return;
            }
            publish(&app);
            load_tab(weak, tab);
            return;
        }
        "close" => {
            close(weak);
            return;
        }
        "web" => {
            open_web(index);
            return;
        }
        _ => {}
    }

    let tab = with(|st| st.tab);
    match action.as_str() {
        // 导演台：独立窗口 / 同屏浮窗。
        a if a.starts_with("dir-") => match a {
            "dir-project" => {
                with(|st| st.cr_project = text.clone());
                crate::director::set_project(text);
            }
            "dir-open" => {
                crate::director::open_window(weak.clone());
            }
            "dir-pin" => {
                crate::director::toggle_pin(weak.clone());
            }
            "dir-close" => {
                crate::director::close_all(weak.clone());
            }
            _ => {}
        },
        // 片头片尾
        a if a.starts_with("cr-") => credits::action(weak, a, index, text),
        // 实拍装配
        a if a.starts_with("as-") => assembly::action(weak, a, index, text),
        // 特效 / 配乐 / 配音
        a if a.starts_with("vfx-") => media_tabs::vfx_action(weak, a, index, text),
        a if a.starts_with("mu-") => media_tabs::music_action(weak, a, index, text),
        a if a.starts_with("vo-") => media_tabs::voice_action(weak, a, index, text),
        // 连贯性 / 风格 / 素材质量
        a if a.starts_with("ct-") => check_tabs::continuity_action(weak, a, index, text),
        a if a.starts_with("st-") => check_tabs::style_action(weak, a, index, text),
        a if a.starts_with("ql-") => check_tabs::quality_action(weak, a, index, text),
        _ => {
            let _ = tab;
        }
    }
}

// ── 通用工具 ─────────────────────────────────────────────────────────

pub(crate) fn text_of(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return text.to_owned();
            }
        }
        if let Some(text) = value.get(*key).and_then(Value::as_i64) {
            return text.to_string();
        }
        if let Some(text) = value.get(*key).and_then(Value::as_f64) {
            return text.to_string();
        }
    }
    String::new()
}

pub(crate) fn num_of(value: &Value, keys: &[&str]) -> f64 {
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

pub(crate) fn money(cny: f64) -> String {
    format!("¥{cny:.2}")
}

pub(crate) fn pct(value: f64) -> String {
    format!("{:.1}%", value.clamp(0.0, 100.0))
}

/// 面板自己的配置目录（下载落盘用）。
pub(crate) fn config_dir() -> PathBuf {
    let mut config = PathBuf::new();
    Shell::with(|shell, _| {
        config = shell.studio.borrow().host.dirs.config.clone();
    });
    if config.as_os_str().is_empty() {
        config = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("app.concat.editor");
    }
    config
}

/// 把状态写回并弹一条通知，然后重新发布面板。
pub(crate) fn finish(studio: &mut Studio, app: &App, line: String, level: &str, warn: bool) {
    with(|st| say(st, line.clone(), level));
    if warn {
        studio.notify(&line, true);
    }
    publish(app);
}

/// 下载地址到「视频 / 星籁 / <sub> / <name>」。
pub(crate) fn download_to(weak: slint::Weak<App>, url: String, sub: &'static str, name: String) {
    if url.trim().is_empty() {
        return;
    }
    let folder = dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("星籁")
        .join(sub);
    let dest = folder.join(name);
    let dest_for_job = dest.clone();
    host::spawn(
        move || {
            let cancel = AtomicBool::new(false);
            concat_host::pro2::fetch(&url, &dest_for_job, &cancel)
        },
        move |studio, app, _models, result| match result {
            Ok(size) => {
                let line = format!(
                    "已下载到 {}（{:.1} MB）",
                    dest.display(),
                    size as f64 / 1024.0 / 1024.0
                );
                finish(studio, app, line, "ok", false);
                let _ = opener::open(dest.to_string_lossy().as_ref());
            }
            Err(error) => finish(studio, app, error, "fail", true),
        },
    );
}

/// 下载 → 探测 → 进媒体库 → 插到第一条空轨（与素材库卡片同一条路）。
pub(crate) fn insert_to(weak: slint::Weak<App>, url: String, sub: &'static str, name: String) {
    if url.trim().is_empty() {
        return;
    }
    let folder = dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("星籁")
        .join(sub);
    let dest = folder.join(&name);
    let dest_for_job = dest.clone();
    let display = name.clone();
    host::spawn(
        move || -> Result<PathBuf, String> {
            let cancel = AtomicBool::new(false);
            if !dest_for_job.is_file() {
                concat_host::pro2::fetch(&url, &dest_for_job, &cancel)?;
            }
            Ok(dest_for_job)
        },
        move |studio, app, _models, result| match result {
            Ok(path) => match insert_media(studio, &path, &display) {
                Ok(line) => finish(studio, app, line, "ok", false),
                Err(error) => finish(studio, app, error, "fail", true),
            },
            Err(error) => finish(studio, app, error, "fail", true),
        },
    );
}

/// 把一个本地文件加进工程并插到第一条空轨。
fn insert_media(studio: &mut Studio, path: &Path, name: &str) -> Result<String, String> {
    use concat_project::Command;
    let summary = concat_host::media::probe(path.to_string_lossy().as_ref())?;
    let mut media = summary.to_new_media();
    media.name = name.to_owned();
    let created = studio.apply(Command::AddMedia { item: media });
    let media_id = created.or_else(|| {
        studio
            .project()
            .media
            .iter()
            .find(|media| media.path == summary.path)
            .map(|media| media.id.clone())
    });
    let Some(media_id) = media_id else {
        return Err("插入失败：工程没有打开或媒体库不接受这个文件".to_owned());
    };
    let start = f64::from(studio.playhead);
    studio.apply(Command::AddClipAtFirstFree { media_id, start });
    Ok(format!("已插入时间线：{name}"))
}

/// 用默认浏览器/播放器打开一个地址。
pub(crate) fn open_url(url: &str) {
    if url.trim().is_empty() {
        return;
    }
    if let Err(error) = opener::open(url.trim()) {
        log::warn!("pro2: 打开地址失败：{error}");
    }
}

/// 让平台 URL 变完整。
pub(crate) fn absolute(url: &str) -> String {
    concat_host::pro::absolute_url(url)
}

/// 把「0-20 悬疑 0.25」这类行解析成场景数组；空行跳过。
pub(crate) fn parse_scenes(text: &str, fallback_mood: &str, minutes: f64) -> Vec<Value> {
    let moods = ["悬疑", "紧张", "动作", "悲伤", "温情", "史诗", "科幻", "黑暗"];
    let mut scenes = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let range = parts.next().unwrap_or_default();
        let (start, end) = match range.split_once('-') {
            Some((start, end)) => (
                start.parse::<f64>().unwrap_or(0.0),
                end.parse::<f64>().unwrap_or(0.0),
            ),
            None => (0.0, 0.0),
        };
        let mood = parts.next().unwrap_or(fallback_mood);
        let mood = if moods.contains(&mood) { mood } else { fallback_mood };
        let energy = parts.next().and_then(|value| value.parse::<f64>().ok()).unwrap_or(0.5);
        if end <= start {
            continue;
        }
        scenes.push(json!({
            "start_sec": start,
            "end_sec": end,
            "emotion": mood,
            "energy": energy,
        }));
    }
    if scenes.is_empty() {
        let total = (minutes.max(0.5) * 60.0).round();
        let steps = 4.0;
        for index in 0..4 {
            let start = total * f64::from(index) / steps;
            let end = total * f64::from(index + 1) / steps;
            let mood = moods[((index as usize) + 1) % moods.len()];
            scenes.push(json!({
                "start_sec": start,
                "end_sec": end,
                "emotion": mood,
                "energy": 0.3 + 0.15 * f64::from(index),
            }));
        }
    }
    scenes
}
