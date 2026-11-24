// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 影视专业三期面板（0.5.0）：窗口这一半。
//!
//! 八个页签：提示词引擎 / 导演风格库 / 项目资产文件库 / AI 审美总监 /
//! 视频反推 / 自愈剪辑 / 补机位 / 节点画布与导演脑。网络调用都在
//! [`concat_host::pro3`]；这里管面板状态、把动作翻译成请求、把结果写回
//! Slint。
//!
//! 懒加载与 0.3.7 / 0.4.0 面板一致：启动路径上只有一份空的 `Pro3Data`，
//! 没有任何网络请求；打开页签后才按当前页取数，每次请求带 epoch，切页 /
//! 关面板后旧结果直接丢掉。经济模式：只走读接口与免费分析接口，不调付费
//! 视频模型。

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde_json::{json, Value};
use slint::{Image, ModelRc, VecModel};

use crate::host::{self, Shell};
use crate::ui::*;

pub(crate) const TAB_PROMPT: i32 = 0;
pub(crate) const TAB_STYLELIB: i32 = 1;
pub(crate) const TAB_INDEX: i32 = 2;
pub(crate) const TAB_AESTHETIC: i32 = 3;
pub(crate) const TAB_REVERSE: i32 = 4;
pub(crate) const TAB_HEAL: i32 = 5;
pub(crate) const TAB_MULTICAM: i32 = 6;
pub(crate) const TAB_CANVAS: i32 = 7;

const TABS: i32 = TAB_CANVAS;

/// 视频文件选择器的过滤表（选中成片 / 片段）。
const VIDEO_FILTER: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm", "avi"];

/// 验收 / 示例用：平台已存在的样片（0.4.0 实拍装配同一批素材）。
const SAMPLE_VIDEO: &str =
    "https://xinglaiai.com/ai-nav/studio/files/assembly/assets/asm_20261124081708_dcaf/src/01_01_morning_shop.mp4";
/// 验收 / 示例用：审美总监与反推都认的《夜行》32 秒样片。
const AESTHETIC_SAMPLE: &str =
    "https://xinglaiai.com/ai-nav/studio/files/xiaolai_agent/sources/yehang_32s.mp4";
/// 验收 / 示例用：补机位自己的 20 秒样片（这是平台下载门禁放行的样例路径；
/// 一般 studio 文件直链会被 403，面板里如实说明）。
const MULTICAM_SAMPLE: &str =
    "https://xinglaiai.com/ai-nav/studio/files/evidence/multicam-20261124/source/multicam_source_20s.mp4";

/// 页签标题下的一句话：接口 / 数据来源。
fn hint_of(tab: i32) -> String {
    match tab {
        TAB_PROMPT => "提示词引擎：表单 → 12 要素 + 中英提示词 + 成本估算 + 模型模板切换。接口 POST /prompt/build、/prompt/estimate（免费，不调视频模型）。".to_owned(),
        TAB_STYLELIB => "导演风格库 v2：20 个风格卡片 → 详情 → 构图辅助线预览（参数化 SVG/PNG）→ LUT。接口 /api/stylelib/*。风格为参考，非授权/联名，不复刻画面。".to_owned(),
        TAB_INDEX => "项目资产文件库：项目 → 五阶段类型树 → 条目（预览/版本/状态）→ 搜索 → 影响分析 → 导出清单 HTML。接口 /api/index/*。".to_owned(),
        TAB_AESTHETIC => "AI 审美总监：选本地成片或样片 → analyze → 总分 + 分项问题 + 打开 HTML 报告。接口 /api/aesthetic/*；规则 + 启发式（零付费视觉模型），需人工复核。".to_owned(),
        TAB_REVERSE => "视频反推：本地文件 / 链接 → analyze（异步）→ 分镜 + 可复制中英提示词 + 报告。接口 /api/reverse/*；本地 scdet + ASR + 文本 LLM，画面语义需人工复核。".to_owned(),
        TAB_HEAL => "自愈剪辑：选成片 → analyze → 问题清单 / 修复建议 → 补拍清单 / 报告。接口 /api/heal/*；修复只做技术性校正，不改剧情、不换脸。".to_owned(),
        TAB_MULTICAM => "补机位：片段 → 二次构图 7 模式（reframe → 下载 / 插入）→ AI 补机位入口（如实显示余额，本版本不调付费视频模型）。接口 /api/multicam/*。片段需平台可访问的 URL：已预填平台提供的 20s 样片；一般 studio 文件直链会被下载门禁 403。".to_owned(),
        _ => "节点画布 / 导演脑：桌面原生只读列出模板与风格（GET /api/canvas/templates、/api/brain/styles）；完整编排在网页版，入口按「外部页面」打开浏览器。".to_owned(),
    }
}

/// 网页兜底地址（外部页面；标注「外部页面」的地方就用它）。
fn web_of(tab: i32) -> &'static str {
    match tab {
        TAB_PROMPT => "https://xinglaiai.com/ai-nav/prompt/",
        TAB_STYLELIB => "https://xinglaiai.com/ai-nav/stylelib/",
        TAB_INDEX => "https://xinglaiai.com/ai-nav/project/",
        TAB_AESTHETIC => "https://xinglaiai.com/ai-nav/aesthetic/",
        TAB_REVERSE => "https://xinglaiai.com/ai-nav/reverse/",
        TAB_HEAL => "https://xinglaiai.com/ai-nav/",
        TAB_MULTICAM => "https://xinglaiai.com/ai-nav/multicam/",
        _ => "https://xinglaiai.com/ai-nav/canvas/",
    }
}

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

fn rows_model(rows: &[Row]) -> ModelRc<Pro3Row> {
    ModelRc::from(std::rc::Rc::new(VecModel::from(
        rows.iter()
            .map(|row| Pro3Row {
                label: row.label.clone().into(),
                detail: row.detail.clone().into(),
                value: row.value.clone().into(),
                level: row.level.clone().into(),
            })
            .collect::<Vec<_>>(),
    )))
}

/// `index` clamped into a list of `len` entries; an empty list keeps 0.
fn clamp_index(index: i32, len: usize) -> i32 {
    if len == 0 {
        0
    } else {
        index.clamp(0, len as i32 - 1)
    }
}

fn join_text(items: &[String]) -> String {
    items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join("、")
}

fn string_list(value: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        match value.get(*key) {
            Some(Value::Array(items)) => {
                return items
                    .iter()
                    .filter_map(|item| {
                        item.as_str()
                            .map(str::to_owned)
                            .or_else(|| item.get("label").and_then(Value::as_str).map(str::to_owned))
                            .or_else(|| item.get("name").and_then(Value::as_str).map(str::to_owned))
                    })
                    .filter(|item| !item.trim().is_empty())
                    .collect()
            }
            Some(Value::String(text)) => {
                return text
                    .split(['、', ',', '，', '/', ';', '；'])
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(str::to_owned)
                    .collect()
            }
            _ => {}
        }
    }
    Vec::new()
}

fn first_array(value: &Value, keys: &[&str]) -> Vec<Value> {
    for key in keys {
        if let Some(items) = value.get(*key).and_then(Value::as_array) {
            return items.clone();
        }
    }
    Vec::new()
}

// ── 面板状态 ─────────────────────────────────────────────────────────

pub(crate) struct Pro3State {
    pub open: bool,
    pub tab: i32,
    pub signed_in: bool,
    pub epoch: u64,
    pub busy: bool,
    pub status_line: String,
    pub status_level: String,
    pub hint: String,

    // 共用预览（哪个页签取到就显示哪个）
    pub preview: Option<Image>,
    pub pv_line: String,

    // ① 提示词引擎
    pub pp_templates: Vec<(String, String)>,
    pub pp_template: i32,
    pub pp_models: Vec<(String, String)>,
    pub pp_model: i32,
    pub pp_name: String,
    pub pp_look: String,
    pub pp_wardrobe: String,
    pub pp_location: String,
    pub pp_time: String,
    pub pp_weather: String,
    pub pp_mood: String,
    pub pp_duration: String,
    pub pp_aspect: i32,
    pub pp_language: i32,
    pub pp_notes: String,
    pub pp_line: String,
    pub pp_quality: String,
    pub pp_cost: String,
    pub pp_zh: String,
    pub pp_en: String,
    pub pp_model_prompt: String,
    pub pp_elements: Vec<Row>,
    pub pp_costs: Vec<Row>,
    pub pp_variants: Vec<Row>,

    // ② 风格库
    pub sl_style_ids: Vec<String>,
    pub sl_style: i32,
    pub sl_guide_ids: Vec<String>,
    pub sl_guide_names: Vec<String>,
    pub sl_ratio: i32,
    pub sl_guide: i32,
    pub sl_line: String,
    pub sl_detail: String,
    pub sl_count: String,
    pub sl_preview_line: String,
    pub sl_styles: Vec<Row>,
    pub sl_guides: Vec<Row>,

    // ③ 项目资产文件库
    pub ix_project_ids: Vec<String>,
    pub ix_selected: i32,
    pub ix_items_raw: Vec<Vec<Value>>,
    pub ix_item_ids: Vec<String>,
    pub ix_project: i32,
    pub ix_type: i32,
    pub ix_query: String,
    pub ix_line: String,
    pub ix_detail: String,
    pub ix_projects: Vec<Row>,
    pub ix_tree: Vec<Row>,
    pub ix_items: Vec<Row>,

    // ④ AI 审美总监
    pub ae_sample_urls: Vec<String>,
    pub ae_local: String,
    pub ae_source: String,
    pub ae_sample: i32,
    pub ae_line: String,
    pub ae_score: String,
    pub ae_grade: String,
    pub ae_summary: String,
    pub ae_report: String,
    pub ae_cats: Vec<Row>,
    pub ae_issues: Vec<Row>,
    pub ae_sources: Vec<Row>,

    // ⑤ 视频反推
    pub rv_sample_urls: Vec<String>,
    pub rv_local: String,
    pub rv_source: String,
    pub rv_sample: i32,
    pub rv_job: String,
    pub rv_status: String,
    pub rv_line: String,
    pub rv_report: String,
    pub rv_zh: String,
    pub rv_en: String,
    pub rv_sources: Vec<Row>,
    pub rv_shots: Vec<Row>,
    pub rv_done: bool,

    // ⑥ 自愈剪辑
    pub hl_local: String,
    pub hl_source: String,
    pub hl_run: String,
    pub hl_line: String,
    pub hl_counts: String,
    pub hl_report: String,
    pub hl_reshoot_line: String,
    pub hl_issues: Vec<Row>,
    pub hl_plan: Vec<Row>,

    // ⑦ 补机位
    pub mc_mode_ids: Vec<String>,
    pub mc_local: String,
    pub mc_source: String,
    pub mc_mode: i32,
    pub mc_zoom: String,
    pub mc_line: String,
    pub mc_output: String,
    pub mc_output_name: String,
    pub mc_ai_line: String,
    pub mc_poster_line: String,
    pub mc_modes: Vec<Row>,
    pub mc_rows: Vec<Row>,

    // ⑧ 节点画布 / 导演脑
    pub nc_line: String,
    pub nc_brain_line: String,
    pub nc_templates: Vec<Row>,
    pub nc_styles: Vec<Row>,

    pub ticks: u32,
}

impl Default for Pro3State {
    fn default() -> Self {
        Pro3State {
            open: false,
            tab: TAB_PROMPT,
            signed_in: false,
            epoch: 0,
            busy: false,
            status_line: String::new(),
            status_level: "info".to_owned(),
            hint: hint_of(TAB_PROMPT),

            preview: None,
            pv_line: String::new(),

            pp_templates: Vec::new(),
            pp_template: 0,
            pp_models: Vec::new(),
            pp_model: 0,
            pp_name: "林晚".to_owned(),
            pp_look: "28 岁，短发利落，眼白有细密血丝，表情收敛".to_owned(),
            pp_wardrobe: "米白羊毛大衣，衣领袖口有使用痕迹".to_owned(),
            pp_location: "深夜便利店门口".to_owned(),
            pp_time: "深夜".to_owned(),
            pp_weather: "细雨".to_owned(),
            pp_mood: "孤独克制".to_owned(),
            pp_duration: "5".to_owned(),
            pp_aspect: 0,
            pp_language: 2,
            pp_notes: "浅景深，霓虹反光落在肩上；不要出现任何品牌 logo。".to_owned(),
            pp_line: "还没生成。载入模板清单，填好表单后点「生成提示词」。".to_owned(),
            pp_quality: String::new(),
            pp_cost: String::new(),
            pp_zh: String::new(),
            pp_en: String::new(),
            pp_model_prompt: String::new(),
            pp_elements: Vec::new(),
            pp_costs: Vec::new(),
            pp_variants: Vec::new(),

            sl_style_ids: Vec::new(),
            sl_style: 0,
            sl_guide_ids: vec![
                "rule_of_thirds".to_owned(),
                "golden_ratio".to_owned(),
                "center_symmetry".to_owned(),
                "one_point_perspective".to_owned(),
                "safe_area".to_owned(),
                "look_room".to_owned(),
            ],
            sl_guide_names: vec![
                "三分法".to_owned(),
                "黄金比".to_owned(),
                "中心对称".to_owned(),
                "一点透视".to_owned(),
                "安全区".to_owned(),
                "视线空间".to_owned(),
            ],
            sl_ratio: 0,
            sl_guide: 0,
            sl_line: "还没载入风格库。".to_owned(),
            sl_detail: String::new(),
            sl_count: String::new(),
            sl_preview_line: String::new(),
            sl_styles: Vec::new(),
            sl_guides: Vec::new(),

            ix_project_ids: Vec::new(),
            ix_selected: 0,
            ix_items_raw: Vec::new(),
            ix_item_ids: Vec::new(),
            ix_project: 0,
            ix_type: -1,
            ix_query: String::new(),
            ix_line: "还没载入项目。".to_owned(),
            ix_detail: String::new(),
            ix_projects: Vec::new(),
            ix_tree: Vec::new(),
            ix_items: Vec::new(),

            ae_sample_urls: Vec::new(),
            ae_local: String::new(),
            ae_source: AESTHETIC_SAMPLE.to_owned(),
            ae_sample: 0,
            ae_line: "选样片或填片源 URL，点「开始审美分析」。".to_owned(),
            ae_score: String::new(),
            ae_grade: String::new(),
            ae_summary: String::new(),
            ae_report: String::new(),
            ae_cats: Vec::new(),
            ae_issues: Vec::new(),
            ae_sources: Vec::new(),

            rv_sample_urls: Vec::new(),
            rv_local: String::new(),
            rv_source: String::new(),
            rv_sample: 0,
            rv_job: String::new(),
            rv_status: String::new(),
            rv_line: "选样片或本地文件，点「开始反推」。".to_owned(),
            rv_report: String::new(),
            rv_zh: String::new(),
            rv_en: String::new(),
            rv_sources: Vec::new(),
            rv_shots: Vec::new(),
            rv_done: false,

            hl_local: String::new(),
            hl_source: SAMPLE_VIDEO.to_owned(),
            hl_run: String::new(),
            hl_line: "填成片 URL 或选本地成片，点「开始自愈分析」。".to_owned(),
            hl_counts: String::new(),
            hl_report: String::new(),
            hl_reshoot_line: String::new(),
            hl_issues: Vec::new(),
            hl_plan: Vec::new(),

            mc_mode_ids: Vec::new(),
            mc_local: String::new(),
            mc_source: MULTICAM_SAMPLE.to_owned(),
            mc_mode: 0,
            mc_zoom: "0.30".to_owned(),
            mc_line: "载入 7 种模式，选片段后点「二次构图」。".to_owned(),
            mc_output: String::new(),
            mc_output_name: String::new(),
            mc_ai_line: String::new(),
            mc_poster_line: String::new(),
            mc_modes: Vec::new(),
            mc_rows: Vec::new(),

            nc_line: "还没载入画布模板。".to_owned(),
            nc_brain_line: "还没载入导演脑风格。".to_owned(),
            nc_templates: Vec::new(),
            nc_styles: Vec::new(),

            ticks: 0,
        }
    }
}

thread_local! {
    static PRO3: std::cell::RefCell<Pro3State> = std::cell::RefCell::new(Pro3State::default());
}

/// 借用面板状态（短借，不在里面做网络）。
pub(crate) fn with<T>(body: impl FnOnce(&mut Pro3State) -> T) -> T {
    PRO3.with(|cell| body(&mut cell.borrow_mut()))
}

fn say(st: &mut Pro3State, line: impl Into<String>, level: &str) {
    st.status_line = line.into();
    st.status_level = level.to_owned();
}

/// 面板自己的预览目录。
fn preview_dir() -> PathBuf {
    crate::pro2::config_dir().join("pro3-preview")
}

// ── 发布 ─────────────────────────────────────────────────────────────

fn publish(app: &App) {
    PRO3.with(|cell| {
        let st = cell.borrow();
        let has_preview = st.preview.is_some();
        let aspect_label = ["16:9", "9:16", "1:1"]
            .get(st.pp_aspect.clamp(0, 2) as usize)
            .copied()
            .unwrap_or("16:9");
        let language_label = ["中文优先", "英文优先", "中英双语"]
            .get(st.pp_language.clamp(0, 2) as usize)
            .copied()
            .unwrap_or("中英双语");
        let template_label = st
            .pp_templates
            .get(st.pp_template.max(0) as usize)
            .map(|(label, _)| label.clone())
            .unwrap_or_else(|| "未载入".to_owned());
        let model_label = st
            .pp_models
            .get(st.pp_model.max(0) as usize)
            .map(|(label, _)| label.clone())
            .unwrap_or_else(|| "未载入".to_owned());
        let mode_label = st
            .mc_modes
            .get(st.mc_mode.max(0) as usize)
            .map(|row| row.label.clone())
            .unwrap_or_else(|| "未载入".to_owned());
        let project_label = st
            .ix_projects
            .get(st.ix_project.max(0) as usize)
            .map(|row| format!("{}（{}）", row.label, row.detail))
            .unwrap_or_default();
        let type_label = if st.ix_type >= 0 {
            st.ix_tree
                .get(st.ix_type as usize)
                .map(|row| format!("{} {}", row.label, row.detail))
                .unwrap_or_default()
        } else {
            "未选".to_owned()
        };
        let data = Pro3Data {
            open: st.open,
            tab: st.tab,
            busy: st.busy,
            signed_in: st.signed_in,
            status_line: st.status_line.clone().into(),
            status_level: st.status_level.clone().into(),
            hint: st.hint.clone().into(),

            pp_template_label: template_label.into(),
            pp_model_label: model_label.into(),
            pp_name: st.pp_name.clone().into(),
            pp_look: st.pp_look.clone().into(),
            pp_wardrobe: st.pp_wardrobe.clone().into(),
            pp_location: st.pp_location.clone().into(),
            pp_time: st.pp_time.clone().into(),
            pp_weather: st.pp_weather.clone().into(),
            pp_mood: st.pp_mood.clone().into(),
            pp_duration: st.pp_duration.clone().into(),
            pp_aspect: st.pp_aspect,
            pp_aspect_label: aspect_label.into(),
            pp_language: st.pp_language,
            pp_language_label: language_label.into(),
            pp_notes: st.pp_notes.clone().into(),
            pp_line: st.pp_line.clone().into(),
            pp_quality: st.pp_quality.clone().into(),
            pp_cost: st.pp_cost.clone().into(),
            pp_zh: st.pp_zh.clone().into(),
            pp_en: st.pp_en.clone().into(),
            pp_model_prompt: st.pp_model_prompt.clone().into(),

            sl_count: st.sl_count.clone().into(),
            sl_line: st.sl_line.clone().into(),
            sl_detail: st.sl_detail.clone().into(),
            sl_style_id: st
                .sl_style_ids
                .get(st.sl_style.max(0) as usize)
                .cloned()
                .unwrap_or_default()
                .into(),
            sl_guide: st.sl_guide,
            sl_ratio: st.sl_ratio,
            sl_preview_line: st.sl_preview_line.clone().into(),

            ix_project_label: project_label.into(),
            ix_type_label: type_label.into(),
            ix_query: st.ix_query.clone().into(),
            ix_line: st.ix_line.clone().into(),
            ix_detail: st.ix_detail.clone().into(),

            ae_source: st.ae_source.clone().into(),
            ae_sample: st.ae_sample,
            ae_line: st.ae_line.clone().into(),
            ae_score: st.ae_score.clone().into(),
            ae_grade: st.ae_grade.clone().into(),
            ae_summary: st.ae_summary.clone().into(),
            ae_report: st.ae_report.clone().into(),

            rv_source: st.rv_source.clone().into(),
            rv_sample: st.rv_sample,
            rv_line: st.rv_line.clone().into(),
            rv_status: st.rv_status.clone().into(),
            rv_report: st.rv_report.clone().into(),
            rv_prompt_zh: st.rv_zh.clone().into(),
            rv_prompt_en: st.rv_en.clone().into(),

            hl_source: st.hl_source.clone().into(),
            hl_line: st.hl_line.clone().into(),
            hl_run: st.hl_run.clone().into(),
            hl_report: st.hl_report.clone().into(),
            hl_counts: st.hl_counts.clone().into(),
            hl_reshoot_line: st.hl_reshoot_line.clone().into(),

            mc_source: st.mc_source.clone().into(),
            mc_mode: st.mc_mode,
            mc_mode_label: mode_label.into(),
            mc_zoom: st.mc_zoom.clone().into(),
            mc_line: st.mc_line.clone().into(),
            mc_output: st.mc_output.clone().into(),
            mc_ai_line: st.mc_ai_line.clone().into(),
            mc_poster_line: st.mc_poster_line.clone().into(),

            nc_line: st.nc_line.clone().into(),
            nc_brain_line: st.nc_brain_line.clone().into(),
            nc_canvas_web: web_of(TAB_CANVAS).to_owned().into(),
            nc_brain_web: "https://xinglaiai.com/ai-nav/api/brain/".to_owned().into(),

            pv_line: st.pv_line.clone().into(),
        };
        app.set_pro3(data);
        app.set_pro3_preview(st.preview.clone().unwrap_or_default());
        app.set_pro3_has_preview(has_preview);

        app.set_pro3_pp_elements(rows_model(&st.pp_elements));
        app.set_pro3_pp_costs(rows_model(&st.pp_costs));
        app.set_pro3_pp_variants(rows_model(&st.pp_variants));
        app.set_pro3_sl_styles(rows_model(&st.sl_styles));
        app.set_pro3_sl_guides(rows_model(&st.sl_guides));
        app.set_pro3_ix_projects(rows_model(&st.ix_projects));
        app.set_pro3_ix_tree(rows_model(&st.ix_tree));
        app.set_pro3_ix_items(rows_model(&st.ix_items));
        app.set_pro3_ae_samples(rows_model(&st.ae_sources));
        app.set_pro3_ae_cats(rows_model(&st.ae_cats));
        app.set_pro3_ae_issues(rows_model(&st.ae_issues));
        app.set_pro3_rv_samples(rows_model(&st.rv_sources));
        app.set_pro3_rv_shots(rows_model(&st.rv_shots));
        app.set_pro3_hl_issues(rows_model(&st.hl_issues));
        app.set_pro3_hl_plan(rows_model(&st.hl_plan));
        app.set_pro3_mc_modes(rows_model(&st.mc_modes));
        app.set_pro3_mc_rows(rows_model(&st.mc_rows));
        app.set_pro3_nc_templates(rows_model(&st.nc_templates));
        app.set_pro3_nc_styles(rows_model(&st.nc_styles));
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

thread_local! {
    static WANTED: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

pub fn want(tab: i32) {
    WANTED.with(|slot| slot.set(Some(tab.clamp(0, TABS))));
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
        st.tab = tab.clamp(0, TABS);
        st.epoch += 1;
        st.busy = false;
        st.status_line.clear();
        st.status_level = "info".to_owned();
        st.hint = hint_of(st.tab);
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
        TAB_PROMPT => prompt_load(weak),
        TAB_STYLELIB => stylelib_load(weak),
        TAB_INDEX => index_load(weak),
        TAB_AESTHETIC => aesthetic_load(weak),
        TAB_REVERSE => reverse_load(weak),
        TAB_HEAL => heal_load(weak),
        TAB_MULTICAM => multicam_load(weak),
        _ => canvas_load(weak),
    }
}

/// Timer：每 1.5 秒一拍；轮询进行中的反推任务。
pub fn tick(weak: slint::Weak<App>) {
    let (open, tab, job, done) = with(|st| {
        st.ticks = st.ticks.wrapping_add(1);
        (st.open, st.tab, st.rv_job.clone(), st.rv_done)
    });
    if !open || tab != TAB_REVERSE || job.is_empty() || done {
        return;
    }
    let ep = with(|st| st.epoch);
    let job_for_work = job.clone();
    host::spawn(
        move || concat_host::pro3::reverse_job(&job_for_work),
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                match result {
                    Ok(value) => {
                        let status = text_of(&value, &["job.status"]);
                        let stage = text_of(&value, &["job.stage"]);
                        let progress = num_of(&value, &["job.progress"]);
                        st.rv_status = if stage.is_empty() {
                            format!("{status}（{progress:.0}%）")
                        } else {
                            format!("{status} · {stage}（{progress:.0}%）")
                        };
                        st.rv_line = format!("反推任务 {}：{}", job, st.rv_status);
                        if status == "done" {
                            st.rv_done = true;
                            if let Some(result) = value.get("result") {
                                apply_reverse_result(st, result, &job);
                            }
                        } else if status == "failed" || status == "error" {
                            st.rv_done = true;
                            let error = text_of(&value, &["job.error"]);
                            st.rv_line = if error.is_empty() {
                                format!("反推失败：{status}")
                            } else {
                                format!("反推失败：{error}")
                            };
                        }
                        st.busy = false;
                    }
                    Err(error) => {
                        st.rv_line = format!("反推任务状态没取回来：{error}");
                        st.rv_done = true;
                        st.busy = false;
                    }
                }
                st.pv_line = st.rv_status.clone();
            });
            publish(&app);
        },
    );
}

// ── 通用工具 ─────────────────────────────────────────────────────────

/// 取一个字段：`keys` 里每一项是一条路径（`job.status` 这样用点分段），按顺序
/// 找第一个非空值；同层回退就写两条路径（`type_label`、`type`）。
fn text_of(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        let mut node = value;
        let mut found = true;
        for part in key.split('.') {
            match node.get(part) {
                Some(next) => node = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if !found {
            continue;
        }
        if let Some(text) = node.as_str() {
            if !text.trim().is_empty() {
                return text.to_owned();
            }
            continue;
        }
        if let Some(number) = node.as_i64() {
            return number.to_string();
        }
        if let Some(number) = node.as_f64() {
            return number.to_string();
        }
        if node.as_bool() == Some(true) {
            return "true".to_owned();
        }
    }
    String::new()
}

/// 数字字段，路径语义同 [`text_of`]。
fn num_of(value: &Value, keys: &[&str]) -> f64 {
    for key in keys {
        let mut node = value;
        let mut found = true;
        for part in key.split('.') {
            match node.get(part) {
                Some(next) => node = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if !found {
            continue;
        }
        if let Some(number) = node.as_f64() {
            return number;
        }
        if let Some(text) = node.as_str() {
            if let Ok(number) = text.trim().parse::<f64>() {
                return number;
            }
        }
    }
    0.0
}

fn open_url(url: &str) {
    if url.trim().is_empty() {
        return;
    }
    if let Err(error) = opener::open(url.trim()) {
        log::warn!("pro3: 打开地址失败：{error}");
    }
}

fn open_web(tab: i32) {
    open_url(web_of(tab));
}

/// 把远程图片下载到预览目录并以 Slint 图片显示。
fn download_preview(weak: slint::Weak<App>, url: String, tag: &'static str) {
    if url.trim().is_empty() {
        return;
    }
    let url = concat_host::pro3::file_url(&url);
    let dest = preview_dir().join(format!("{tag}.png"));
    let dest_for_job = dest.clone();
    host::spawn(
        move || -> Result<u64, String> {
            let cancel = AtomicBool::new(false);
            concat_host::pro2::fetch(&url, &dest_for_job, &cancel)
        },
        move |_studio, app, _models, result| {
            match result {
                Ok(_) => {
                    with(|st| {
                        st.preview = host::image_at(&dest);
                        st.pv_line = format!("预览已落到本机：{}", dest.display());
                    });
                }
                Err(error) => with(|st| st.pv_line = format!("预览下载失败：{error}")),
            }
            publish(&app);
        },
    );
}

/// 本地视频取一帧当预览（不联网）。
fn local_preview(weak: slint::Weak<App>, path: PathBuf, tag: &'static str) {
    let dir = preview_dir().join(tag);
    host::spawn(
        move || -> Result<PathBuf, String> {
            let frames = concat_host::video_preview::video_frames(&path, &dir, 1, 1, 480)?;
            frames
                .into_iter()
                .next()
                .ok_or_else(|| "没有解出预览帧".to_owned())
        },
        move |_studio, app, _models, result| {
            match result {
                Ok(first) => with(|st| {
                    st.busy = false;
                    st.preview = host::image_at(&first);
                    st.pv_line = format!("本地预览：{}", first.display());
                }),
                Err(error) => with(|st| {
                    st.busy = false;
                    st.pv_line = format!("本地预览没解出来：{error}");
                }),
            }
            publish(&app);
        },
    );
}

/// 让用户选一段本地视频，选中后记到状态里并做本地预览。
fn pick_video(weak: slint::Weak<App>, tab: i32, title: &'static str, tag: &'static str) {
    let weak2 = weak.clone();
    host::pick_files_then(
        title.to_owned(),
        Some(("视频".to_owned(), VIDEO_FILTER)),
        move |_studio, app, _models, paths| {
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let shown = path.display().to_string();
            with(|st| match tab {
                TAB_AESTHETIC => {
                    st.ae_local = shown.clone();
                    st.ae_source = shown.clone();
                }
                TAB_REVERSE => {
                    st.rv_local = shown.clone();
                    st.rv_source = shown.clone();
                }
                TAB_HEAL => {
                    st.hl_local = shown.clone();
                    st.hl_source = shown.clone();
                }
                _ => {
                    st.mc_local = shown.clone();
                    st.mc_source = shown.clone();
                }
            });
            with(|st| {
                say(
                    st,
                    format!("已选本地文件：{shown}（分析时会先上传到星籁换 URL）"),
                    "info",
                );
                st.busy = true;
            });
            publish(&app);
            local_preview(weak2.clone(), path, tag);
        },
    );
}

/// 分析前的片源：已填 URL 就用 URL，选的是本地文件就先上传。
fn resolve_source(source: &str, local: &str) -> Result<String, String> {
    let source = source.trim();
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(source.to_owned());
    }
    let local = local.trim();
    if !local.is_empty() && Path::new(local).is_file() {
        let cancel = AtomicBool::new(false);
        let mut progress = |_p: f32, _line: String| {};
        return concat_host::pro3::upload(Path::new(local), &mut progress, &cancel);
    }
    if source.starts_with('/') || source.starts_with("file:") {
        return Err("本地文件要先点「选本地成片 / 选本地文件」上传，或直接填一个平台可访问的 URL。".to_owned());
    }
    Err("先选本地文件，或填一个平台可访问的片源 URL。".to_owned())
}

// ── ① 提示词引擎 ─────────────────────────────────────────────────────

fn prompt_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        || -> Result<(Value, Value), String> {
            let templates = concat_host::pro3::prompt_templates()?;
            let models = concat_host::pro3::prompt_models().unwrap_or(Value::Null);
            Ok((templates, models))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok((templates, models)) => {
                        st.pp_templates.clear();
                        for item in first_array(&templates, &["templates"]) {
                            st.pp_templates.push((
                                text_of(&item, &["id"]),
                                text_of(&item, &["label"]),
                            ));
                        }
                        st.pp_models.clear();
                        for item in first_array(&models, &["models"]) {
                            st.pp_models.push((
                                text_of(&item, &["id"]),
                                text_of(&item, &["label"]),
                            ));
                        }
                        st.pp_template = st.pp_template.clamp(0, (st.pp_templates.len() as i32 - 1).max(0));
                        st.pp_model = st.pp_model.clamp(0, (st.pp_models.len() as i32 - 1).max(0));
                        st.pp_line = format!(
                            "已取回 {} 个场景模板、{} 个模型模板（列表来自真实接口）。",
                            st.pp_templates.len(),
                            st.pp_models.len()
                        );
                        say(st, "提示词引擎在线。", "ok");
                    }
                    Err(error) => {
                        st.pp_line = format!("提示词引擎没就绪：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn prompt_build(weak: slint::Weak<App>, estimate_only: bool) {
    let (body, template) = with(|st| {
        st.busy = true;
        st.pp_zh.clear();
        st.pp_en.clear();
        st.pp_model_prompt.clear();
        st.pp_elements.clear();
        st.pp_costs.clear();
        st.pp_variants.clear();
        st.pp_quality.clear();
        st.pp_cost.clear();
        st.pp_line = if estimate_only {
            "正在估成本（POST /prompt/estimate）…".to_owned()
        } else {
            "正在生成 12 要素与中英提示词（POST /prompt/build）…".to_owned()
        };
        say(st, st.pp_line.clone(), "info");
        let template = st
            .pp_templates
            .get(st.pp_template.max(0) as usize)
            .map(|(id, _)| id.clone())
            .unwrap_or_default();
        let model = st
            .pp_models
            .get(st.pp_model.max(0) as usize)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| "generic".to_owned());
        let aspect = ["16:9", "9:16", "1:1"]
            .get(st.pp_aspect.clamp(0, 2) as usize)
            .copied()
            .unwrap_or("16:9");
        let language = ["zh", "en", "bilingual"]
            .get(st.pp_language.clamp(0, 2) as usize)
            .copied()
            .unwrap_or("bilingual");
        let body = json!({
            "template_id": template,
            "model": model,
            "language": language,
            "aspect": aspect,
            "duration": st.pp_duration.trim().parse::<f64>().unwrap_or(5.0),
            "mood": st.pp_mood,
            "notes": st.pp_notes,
            "character": {
                "name": st.pp_name,
                "look": st.pp_look,
                "wardrobe": st.pp_wardrobe,
            },
            "scene": {
                "location": st.pp_location,
                "time": st.pp_time,
                "weather": st.pp_weather,
            },
        });
        (body, template)
    });
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        move || -> Result<Value, String> {
            if estimate_only {
                concat_host::pro3::prompt_estimate(&body)
            } else {
                concat_host::pro3::prompt_build(&body)
            }
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        if estimate_only {
                            if let Some(cost) = value.get("estimate") {
                                apply_prompt_cost(st, cost);
                                st.pp_line = format!("成本估算完成（模型模板 {template}）。");
                                say(st, "成本估算完成。", "ok");
                            } else {
                                st.pp_cost = format!("¥{}", num_of(&value, &["total_cny"]));
                                st.pp_line = "成本估算完成。".to_owned();
                                say(st, "成本估算完成。", "ok");
                            }
                        } else {
                            apply_prompt_build(st, &value);
                            say(st, "提示词生成完成（12 要素 + 中英）。", "ok");
                        }
                    }
                    Err(error) => {
                        st.pp_line = format!("提示词生成失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn apply_prompt_cost(st: &mut Pro3State, cost: &Value) {
    st.pp_cost = format!(
        "¥{:.2}（约 {} 积分）{}",
        num_of(cost, &["total_cny"]),
        num_of(cost, &["credits"]) as i64,
        {
            let note = text_of(cost, &["estimated_note"]);
            if note.is_empty() {
                String::new()
            } else {
                format!(" · {note}")
            }
        }
    );
    st.pp_costs.clear();
    for item in first_array(cost, &["items"]) {
        st.pp_costs.push(row(
            text_of(&item, &["name"]),
            text_of(&item, &["model"]),
            format!("¥{:.2}", num_of(&item, &["subtotal_cny"])),
            "info",
        ));
    }
}

fn apply_prompt_build(st: &mut Pro3State, value: &Value) {
    let structured = value.get("structured").cloned().unwrap_or(Value::Null);
    st.pp_elements.clear();
    let field_labels: &[(&str, &str)] = &[
        ("shot", "镜头"),
        ("subject", "主体"),
        ("wardrobe", "服装"),
        ("props", "道具"),
        ("environment", "环境"),
        ("lighting", "光线"),
        ("color", "色彩"),
        ("texture", "质感"),
        ("mood", "情绪"),
        ("camera_motion", "运镜"),
        ("negative", "负向词"),
        ("technical", "技术参数"),
    ];
    for (key, label) in field_labels {
        let block = structured.get(*key).cloned().unwrap_or(Value::Null);
        let mut detail = String::new();
        for sub in ["zh", "en", "emotion", "position", "move", "palette", "medium", "outfit", "location", "resolution"] {
            let text = text_of(&block, &[sub]);
            if !text.trim().is_empty() {
                if !detail.is_empty() {
                    detail.push_str(" · ");
                }
                detail.push_str(text.trim());
            }
        }
        if detail.chars().count() > 240 {
            detail = detail.chars().take(240).collect::<String>() + "…";
        }
        st.pp_elements.push(row(*label, detail, String::new(), "ok"));
    }
    let prompts = value.get("prompts").cloned().unwrap_or(Value::Null);
    st.pp_zh = text_of(&prompts, &["zh"]);
    st.pp_en = text_of(&prompts, &["en"]);
    let model = prompts.get("model").cloned().unwrap_or(Value::Null);
    st.pp_model_prompt = format!(
        "当前模型模板：{} · {}",
        text_of(&model, &["label"]),
        text_of(&model, &["prompt"])
    );
    if let Some(cost) = prompts.get("cost") {
        apply_prompt_cost(st, cost);
    }
    let quality = value.get("quality").cloned().unwrap_or(Value::Null);
    let checks = first_array(&quality, &["checks"]);
    let mut passed = 0;
    let mut total = 0;
    for check in &checks {
        total += 1;
        if check.get("ok").and_then(Value::as_bool) == Some(true) {
            passed += 1;
        }
    }
    st.pp_quality = format!(
        "质量自检 {:.1} 分（{passed}/{total} 项通过）；12 要素{}",
        num_of(&quality, &["score"]),
        if quality.get("elements_complete").and_then(Value::as_bool) == Some(true) {
            "完整"
        } else {
            "缺项（见 /prompt/schema）"
        }
    );
    st.pp_variants.clear();
    for item in first_array(value, &["model_variants"]) {
        st.pp_variants.push(row(
            text_of(&item, &["label", "model"]),
            text_of(&item, &["model"]),
            format!("{} 字", text_of(&item, &["prompt"]).chars().count()),
            "info",
        ));
    }
    st.pp_line = format!(
        "已生成（请求 {}，{} ms）：中文 {} 字 / 英文 {} 字符。提示词只生成原创角色，不点名真实公众人物。",
        text_of(value, &["request_id"]),
        num_of(value, &["elapsed_ms"]) as i64,
        st.pp_zh.chars().count(),
        st.pp_en.chars().count()
    );
}

// ── ② 风格库 ─────────────────────────────────────────────────────────

fn stylelib_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        || concat_host::pro3::stylelib_list(""),
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        st.sl_styles.clear();
                        st.sl_style_ids.clear();
                        for item in first_array(&value, &["profiles"]) {
                            let tags = join_text(&string_list(&item, &["tags"]));
                            st.sl_styles.push(row(
                                text_of(&item, &["display_name"]),
                                format!("{} · {}", text_of(&item, &["subtitle"]), tags),
                                text_of(&item, &["style_id"]),
                                "info",
                            ));
                            st.sl_style_ids.push(text_of(&item, &["style_id"]));
                        }
                        st.sl_count = format!(
                            "已取回 {} / {} 个风格（schema {}）",
                            st.sl_styles.len(),
                            num_of(&value, &["total"]) as i64,
                            text_of(&value, &["schema"])
                        );
                        st.sl_line = format!(
                            "构图辅助线类型：{}。点一张风格卡片看详情。",
                            join_text(&string_list(&value, &["guide_types"]))
                        );
                        st.sl_guides.clear();
                        for item in first_array(&value, &["guide_types"]) {
                            st.sl_guides.push(row(
                                text_of(&item, &["name_zh"]),
                                text_of(&item, &["name_en"]),
                                text_of(&item, &["guide_id"]),
                                "info",
                            ));
                        }
                        if st.sl_guides.is_empty() {
                            st.sl_guides = st
                                .sl_guide_ids
                                .iter()
                                .zip(st.sl_guide_names.iter())
                                .map(|(id, name)| row(name.clone(), String::new(), id.clone(), "info"))
                                .collect();
                        }
                        say(st, "风格库在线（20 个风格档案）。", "ok");
                    }
                    Err(error) => {
                        st.sl_line = format!("风格库没取回来：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn stylelib_pick(weak: slint::Weak<App>, index: i32) {
    let style_id = with(|st| {
        let len = st.sl_style_ids.len();
        st.sl_style = clamp_index(index, len);
        let style_id = st
            .sl_style_ids
            .get(st.sl_style.max(0) as usize)
            .cloned()
            .unwrap_or_default();
        st.busy = true;
        st.sl_detail = format!("正在取 {style_id} 的详情…");
        style_id
    });
    if style_id.is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        move || concat_host::pro3::stylelib_style(&style_id),
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        st.sl_detail = format!(
                            "{} · {}。构图：{}。节奏：{}；镜头 {} 个 / 剪辑率 {} 切每分。色彩：{}。建议辅助线：{}。",
                            text_of(&value, &["display_name"]),
                            text_of(&value, &["subtitle"]),
                            text_of(&value, &["composition.summary"]),
                            text_of(&value, &["pacing_label"]),
                            num_of(&value, &["ref_shot_count"]) as i64,
                            num_of(&value, &["cuts_per_min"]),
                            text_of(&value, &["color.description"]),
                            join_text(&string_list(&value, &["recommended_guides"]))
                        );
                        say(st, "风格详情已取回。", "ok");
                    }
                    Err(error) => {
                        st.sl_detail = format!("详情没取回来：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn stylelib_preview(weak: slint::Weak<App>) {
    let (style_id, guide, guide_name, ratio) = with(|st| {
        let style_id = st
            .sl_style_ids
            .get(st.sl_style.max(0) as usize)
            .cloned()
            .unwrap_or_default();
        let guide = st
            .sl_guide_ids
            .get(st.sl_guide.max(0) as usize)
            .cloned()
            .unwrap_or_else(|| "rule_of_thirds".to_owned());
        let guide_name = st
            .sl_guide_names
            .get(st.sl_guide.max(0) as usize)
            .cloned()
            .unwrap_or_else(|| guide.clone());
        let ratio = ["16:9", "9:16", "2.39:1", "1:1"]
            .get(st.sl_ratio.clamp(0, 3) as usize)
            .copied()
            .unwrap_or("16:9");
        st.busy = true;
        st.sl_preview_line = format!("正在生成构图辅助线（{guide_name} / {ratio}）…");
        (style_id, guide, guide_name, ratio.to_owned())
    });
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    let style_for_job = style_id.clone();
    host::spawn(
        move || -> Result<String, String> {
            let body = json!({
                "style_id": style_for_job,
                "guides": [guide.clone()],
                "ratio": ratio.clone(),
                "width": 1280,
            });
            let value = concat_host::pro3::stylelib_composition_guide(&body)?;
            let guide_data = value
                .get("guides")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .cloned()
                .unwrap_or(Value::Null);
            let title = text_of(&guide_data, &["title"]);
            let notes = join_text(&string_list(&guide_data, &["notes"]));
            let params = text_of(&guide_data, &["params"]);
            let url = concat_host::pro3::stylelib_guide_png_url(
                &text_of(&value, &["style.style_id"]),
                &guide,
                1280,
                &ratio,
            );
            let mut line = format!("构图辅助线已出（{title}）。");
            if !notes.is_empty() {
                line.push_str(&notes);
            }
            if !params.is_empty() {
                line.push_str(&format!(" 参数：{params}"));
            }
            Ok(format!("{url}|{line}"))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            match result {
                Ok(packed) => {
                    let mut parts = packed.splitn(2, '|');
                    let url = parts.next().unwrap_or("").to_owned();
                    let line = parts.next().unwrap_or("").to_owned();
                    with(|st| {
                        st.busy = false;
                        st.sl_preview_line = line;
                    });
                    download_preview(app.as_weak(), url, "stylelib");
                }
                Err(error) => {
                    with(|st| {
                        st.busy = false;
                        st.sl_preview_line = format!("构图辅助线生成失败：{error}");
                        say(st, error, "warn");
                    });
                    publish(&app);
                }
            }
        },
    );
}

fn stylelib_lut(weak: slint::Weak<App>) {
    let (style_id, url) = with(|st| {
        let style_id = st.sl_style_ids.first().cloned().unwrap_or_default();
        let style_id = st
            .sl_style_ids
            .get(st.sl_style.max(0) as usize)
            .cloned()
            .unwrap_or(style_id);
        (style_id.clone(), concat_host::pro3::stylelib_lut_url(&style_id))
    });
    if style_id.is_empty() {
        with(|st| say(st, "先载入风格库再下载 LUT。", "warn"));
        return;
    }
    let folder = dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("星籁")
        .join("风格库");
    let name = format!("{style_id}-17.cube");
    let dest = folder.join(&name);
    let dest_for_job = dest.clone();
    let Some(app) = weak.upgrade() else { return };
    let ep = with(|st| st.epoch);
    with(|st| {
        st.busy = true;
        st.sl_preview_line = format!("正在下载 LUT：{name}…");
    });
    publish(&app);
    host::spawn(
        move || -> Result<u64, String> {
            let cancel = AtomicBool::new(false);
            concat_host::pro2::fetch(&url, &dest_for_job, &cancel)
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(size) => {
                        st.sl_preview_line = format!(
                            "LUT 已下载到 {}（{:.1} KB）",
                            dest.display(),
                            size as f64 / 1024.0
                        );
                        say(st, "LUT 下载完成。", "ok");
                    }
                    Err(error) => {
                        st.sl_preview_line = format!("LUT 下载失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

// ── ③ 项目资产文件库 ─────────────────────────────────────────────────

fn index_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        || -> Result<(Value, Value), String> {
            let projects = concat_host::pro3::index_projects()?;
            let tree = concat_host::pro3::index_tree("")?;
            Ok((projects, tree))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| match result {
                Ok((projects, tree)) => {
                    st.busy = false;
                    st.ix_projects.clear();
                    st.ix_project_ids.clear();
                    for item in first_array(&projects, &["projects"]) {
                        let count = num_of(&item, &["item_count"]) as i64;
                        st.ix_projects.push(row(
                            text_of(&item, &["title"]),
                            text_of(&item, &["project_id"]),
                            format!("{count} 项"),
                            "ok",
                        ));
                        st.ix_project_ids.push(text_of(&item, &["project_id"]));
                    }
                    apply_index_tree(st, &tree);
                    st.ix_line = format!(
                        "已取回 {} 个项目、{} 类资产；数据源为真实 /api/index/*。",
                        st.ix_projects.len(),
                        st.ix_tree.len()
                    );
                    say(st, "项目资产文件库在线。", "ok");
                }
                Err(error) => {
                    st.busy = false;
                    st.ix_line = format!("项目库没取回来：{error}");
                    say(st, error, "warn");
                }
            });
            publish(&app);
        },
    );
}

fn apply_index_tree(st: &mut Pro3State, value: &Value) {
    st.ix_tree.clear();
    st.ix_items_raw.clear();
    st.ix_item_ids.clear();
    st.ix_type = 0;
    let mut nodes_by_type: Vec<Vec<Value>> = Vec::new();
    for stage in first_array(value, &["stages"]) {
        let stage_name = text_of(&stage, &["stage"]);
        for typ in first_array(&stage, &["types"]) {
            let label = text_of(&typ, &["label"]);
            let kind = text_of(&typ, &["type"]);
            let count = num_of(&typ, &["count"]) as i64;
            let status = typ.get("status").cloned().unwrap_or(Value::Null);
            let done = num_of(&status, &["done"]) as i64;
            st.ix_tree.push(row(
                format!("{stage_name} · {label}"),
                format!("{count} 项 · 已完成 {done}"),
                kind,
                "info",
            ));
            nodes_by_type.push(first_array(&typ, &["nodes"]));
        }
    }
    st.ix_items_raw = nodes_by_type;
    st.ix_type = if st.ix_tree.is_empty() { -1 } else { 0 };
    if st.ix_type >= 0 {
        apply_index_type(st, 0);
    }
}

fn apply_index_type(st: &mut Pro3State, index: i32) {
    st.ix_items.clear();
    st.ix_item_ids.clear();
    st.ix_detail.clear();
    let Some(nodes) = st.ix_items_raw.get(index.max(0) as usize) else {
        return;
    };
    for node in nodes {
        let status = text_of(node, &["status_label", "status"]);
        let level = if text_of(node, &["status"]) == "done" { "ok" } else { "info" };
        st.ix_items.push(row(
            text_of(node, &["name"]),
            format!(
                "{} · {} · v{}",
                text_of(node, &["type_label", "type"]),
                status,
                num_of(node, &["current_version"]) as i64
            ),
            text_of(node, &["id"]),
            level,
        ));
        st.ix_item_ids.push(text_of(node, &["id"]));
    }
}

fn index_search(weak: slint::Weak<App>) {
    let (query, project) = with(|st| {
        st.busy = true;
        let project = st
            .ix_project_ids
            .get(st.ix_project.max(0) as usize)
            .cloned()
            .unwrap_or_default();
        (st.ix_query.clone(), project)
    });
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        move || concat_host::pro3::index_search(&query, &project, "", ""),
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| match result {
                Ok(value) => {
                    st.busy = false;
                    st.ix_items.clear();
                    st.ix_item_ids.clear();
                    for item in first_array(&value, &["items"]) {
                        let matched = join_text(&string_list(&item, &["matched_fields"]));
                        st.ix_items.push(row(
                            text_of(&item, &["name"]),
                            format!(
                                "{} · {} · {}",
                                text_of(&item, &["type_label", "type"]),
                                text_of(&item, &["status_label", "status"]),
                                matched
                            ),
                            text_of(&item, &["id"]),
                            "info",
                        ));
                        st.ix_item_ids.push(text_of(&item, &["id"]));
                    }
                    st.ix_line = format!(
                        "搜索「{}」命中 {} 项（真实 /index/search）。点条目看详情 / 版本 / 影响。",
                        text_of(&value, &["q"]),
                        num_of(&value, &["count"]) as i64
                    );
                    say(st, "搜索完成。", "ok");
                }
                Err(error) => {
                    st.busy = false;
                    st.ix_line = format!("搜索失败：{error}");
                    say(st, error, "warn");
                }
            });
            publish(&app);
        },
    );
}

fn index_item(weak: slint::Weak<App>, index: i32, versions: bool, impact: bool) {
    let item = with(|st| st.ix_item_ids.get(index.max(0) as usize).cloned().unwrap_or_default());
    if item.is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    let ep = with(|st| st.epoch);
    with(|st| {
        st.busy = true;
        st.ix_detail = format!("正在取 {item} 的详情…");
    });
    publish(&app);
    host::spawn(
        move || -> Result<(Value, Value), String> {
            let detail = concat_host::pro3::index_item(&item)?;
            if versions {
                Ok((detail, concat_host::pro3::index_versions(&item)?))
            } else if impact {
                Ok((detail, concat_host::pro3::index_impact(&item)?))
            } else {
                Ok((detail, Value::Null))
            }
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok((detail, extra)) => {
                        let item_value = detail.get("item").cloned().unwrap_or(detail);
                        let mut line = format!(
                            "{}（{} / {} / 版本 {}）：{}",
                            text_of(&item_value, &["name"]),
                            text_of(&item_value, &["type_label"]),
                            text_of(&item_value, &["status_label", "status"]),
                            num_of(&item_value, &["current_version"]) as i64,
                            text_of(&item_value, &["note"])
                        );
                        let preview = text_of(&item_value, &["preview.url"]);
                        if !preview.is_empty() {
                            line.push_str(&format!(" 预览：{preview}"));
                        }
                        if versions {
                            let count = num_of(&extra, &["count"]) as i64;
                            let mut names = Vec::new();
                            for item in first_array(&extra, &["versions"]) {
                                names.push(format!(
                                    "v{} {} {}",
                                    num_of(&item, &["version"]) as i64,
                                    text_of(&item, &["name"]),
                                    text_of(&item, &["size_human"])
                                ));
                            }
                            line.push_str(&format!(" 共 {count} 个版本：{}", join_text(&names)));
                        }
                        if impact {
                            line.push_str(&format!(
                                " 影响分析：{}（下游 {} 项）。",
                                text_of(&extra, &["summary"]),
                                num_of(&extra, &["downstream_count"]) as i64
                            ));
                            let groups = first_array(&extra, &["groups"]);
                            if !groups.is_empty() {
                                let mut parts = Vec::new();
                                for group in groups {
                                    parts.push(format!(
                                        "{}×{}",
                                        text_of(&group, &["label"]),
                                        num_of(&group, &["count"]) as i64
                                    ));
                                }
                                line.push_str(&join_text(&parts));
                            }
                        }
                        st.ix_detail = line;
                        say(st, "条目详情已取回。", "ok");
                    }
                    Err(error) => {
                        st.ix_detail = format!("条目详情没取回来：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn index_export(weak: slint::Weak<App>, project: &str) {
    if project.trim().is_empty() {
        with(|st| say(st, "先载入项目再导出。", "warn"));
        return;
    }
    let url = concat_host::pro3::index_export_url(project);
    with(|st| st.ix_line = format!("正在下载资产清单：{url}"));
    open_url(&url);
    let _ = weak;
}

// ── ④ AI 审美总监 ────────────────────────────────────────────────────

fn aesthetic_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        || -> Result<(Value, Value), String> {
            let health = concat_host::pro3::aesthetic_health()?;
            let samples = concat_host::pro3::aesthetic_samples().unwrap_or(Value::Null);
            Ok((health, samples))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok((health, samples)) => {
                        st.ae_sources.clear();
                        st.ae_sample_urls.clear();
                        for item in first_array(&samples, &["samples"]) {
                            st.ae_sources.push(row(
                                text_of(&item, &["label"]),
                                text_of(&item, &["note"]),
                                text_of(&item, &["url"]),
                                "info",
                            ));
                            st.ae_sample_urls.push(text_of(&item, &["url"]));
                        }
                        if st.ae_source.is_empty() {
                            st.ae_source = st.ae_sample_urls.first().cloned().unwrap_or_default();
                        }
                        st.ae_line = format!(
                            "审美引擎在线（{}）：{} 个内置样片；单次分析最长 {}s。{}",
                            text_of(&health, &["version"]),
                            st.ae_sources.len(),
                            num_of(&health, &["limits.max_video_sec"]) as i64,
                            if st.ae_sample_urls.is_empty() {
                                ""
                            } else {
                                "点样片自动填片源。"
                            }
                        );
                        say(st, "AI 审美总监在线。", "ok");
                    }
                    Err(error) => {
                        st.ae_line = format!("审美接口没就绪：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn aesthetic_analyze(weak: slint::Weak<App>) {
    let (source, local) = with(|st| {
        st.busy = true;
        st.ae_score.clear();
        st.ae_grade.clear();
        st.ae_summary.clear();
        st.ae_report.clear();
        st.ae_cats.clear();
        st.ae_issues.clear();
        st.ae_line = "正在上传 + 分析（零付费视觉模型，约 10–60s）…".to_owned();
        say(st, st.ae_line.clone(), "info");
        (st.ae_source.clone(), st.ae_local.clone())
    });
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        move || -> Result<(Value, String), String> {
            let url = resolve_source(&source, &local)?;
            let value = concat_host::pro3::aesthetic_analyze(&json!({ "video_url": url }))?;
            Ok((value, url))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok((value, url)) => {
                        let id = text_of(&value, &["id"]);
                        st.ae_score = format!("{:.1}", num_of(&value, &["total_score"]));
                        st.ae_grade = text_of(&value, &["grade"]);
                        let summary = value.get("summary").cloned().unwrap_or(Value::Null);
                        st.ae_summary = if summary.is_object() {
                            format!(
                                "通过 {} 项 / 高 {} / 中 {} / 低 {}",
                                num_of(&summary, &["passed"]) as i64,
                                num_of(&summary, &["high"]) as i64,
                                num_of(&summary, &["medium"]) as i64,
                                num_of(&summary, &["low"]) as i64
                            )
                        } else {
                            text_of(&value, &["summary"])
                        };
                        st.ae_report = text_of(&value, &["report_url"]);
                        st.ae_report = if st.ae_report.is_empty() {
                            concat_host::pro3::aesthetic_report_url(&id)
                        } else {
                            concat_host::pro3::file_url(&st.ae_report)
                        };
                        st.ae_cats.clear();
                        for item in first_array(&value, &["categories"]) {
                            st.ae_cats.push(row(
                                text_of(&item, &["name", "label", "category"]),
                                text_of(&item, &["summary", "detail"]),
                                format!("{:.1}", num_of(&item, &["score", "value"])),
                                "info",
                            ));
                        }
                        st.ae_issues.clear();
                        for item in first_array(&value, &["issues"]) {
                            let severity = text_of(&item, &["severity", "level"]);
                            let level = if severity == "P0" || severity == "高" || severity == "high" {
                                "fail"
                            } else if severity == "P1" || severity == "中" || severity == "medium" {
                                "warn"
                            } else {
                                "info"
                            };
                            let at = text_of(&item, &["at", "time", "timestamp"]);
                            let name = text_of(&item, &["category_label", "name", "title", "type"]);
                            st.ae_issues.push(row(
                                if at.is_empty() {
                                    name
                                } else {
                                    format!("{at} {name}")
                                },
                                text_of(&item, &["issue", "detail", "description", "suggestion"]),
                                severity,
                                level,
                            ));
                        }
                        st.ae_line = format!(
                            "审美分析完成（id {id}）：总分 {:.1} / 评级 {}；{} 个问题；片源 {}。",
                            num_of(&value, &["total_score"]),
                            text_of(&value, &["grade"]),
                            st.ae_issues.len(),
                            url
                        );
                        st.pv_line = st.ae_line.clone();
                        say(st, "审美分析完成（真实 /aesthetic/analyze）。", "ok");
                    }
                    Err(error) => {
                        st.ae_line = format!("审美分析失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

// ── ⑤ 视频反推 ───────────────────────────────────────────────────────

fn reverse_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        || -> Result<(Value, Value), String> {
            let health = concat_host::pro3::reverse_health()?;
            let samples = concat_host::pro3::reverse_samples().unwrap_or(Value::Null);
            Ok((health, samples))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok((health, samples)) => {
                        st.rv_sources.clear();
                        st.rv_sample_urls.clear();
                        for item in first_array(&samples, &["samples"]) {
                            let label = text_of(&item, &["label"]);
                            st.rv_sources.push(row(
                                label,
                                text_of(&item, &["id"]),
                                text_of(&item, &["bytes"]),
                                "info",
                            ));
                            st.rv_sample_urls.push(text_of(&item, &["id"]));
                        }
                        st.rv_line = format!(
                            "反推引擎在线（{}）：{} 个内置样片。选样片或本地文件后点「开始反推」；ASR 本地、视觉零付费。",
                            text_of(&health, &["version"]),
                            st.rv_sources.len()
                        );
                        say(st, "视频反推引擎在线。", "ok");
                    }
                    Err(error) => {
                        st.rv_line = format!("反推接口没就绪：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn reverse_analyze(weak: slint::Weak<App>) {
    let (source, local, sample_id) = with(|st| {
        st.busy = true;
        st.rv_done = false;
        st.rv_job.clear();
        st.rv_status = "提交中".to_owned();
        st.rv_shots.clear();
        st.rv_zh.clear();
        st.rv_en.clear();
        st.rv_report.clear();
        st.rv_line = "正在提交反推任务（本地文件会先上传换 URL）…".to_owned();
        say(st, st.rv_line.clone(), "info");
        let sample_id = st
            .rv_sample_urls
            .get(st.rv_sample.max(0) as usize)
            .cloned()
            .unwrap_or_default();
        (st.rv_source.clone(), st.rv_local.clone(), sample_id)
    });
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        move || -> Result<Value, String> {
            let mut body = json!({});
            let source = source.trim();
            if source.starts_with("http://") || source.starts_with("https://") {
                body["video_url"] = json!(source);
            } else if !local.trim().is_empty() && Path::new(local.trim()).is_file() {
                let cancel = AtomicBool::new(false);
                let mut progress = |_p: f32, _line: String| {};
                let url = concat_host::pro3::upload(Path::new(local.trim()), &mut progress, &cancel)?;
                body["video_url"] = json!(url);
            } else if !sample_id.is_empty() {
                body["sample_id"] = json!(sample_id);
            } else {
                return Err("先选一个样片，或选本地文件 / 填视频 URL。".to_owned());
            }
            concat_host::pro3::reverse_analyze(&body)
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let job = text_of(&value, &["job_id"]);
                        st.rv_job = job.clone();
                        st.rv_status = text_of(&value, &["status"]);
                        st.rv_line = format!(
                            "反推任务已提交（{job}）：{}。每 1.5 秒轮询 /reverse/job/{{id}}，通常 30–120s。",
                            st.rv_status
                        );
                        say(st, "反推任务已提交（异步）。", "ok");
                    }
                    Err(error) => {
                        st.rv_status = "提交失败".to_owned();
                        st.rv_line = format!("反推提交失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn apply_reverse_result(st: &mut Pro3State, result: &Value, job: &str) {
    st.rv_shots.clear();
    for shot in first_array(result, &["storyboard", "shots"]) {
        st.rv_shots.push(row(
            format!(
                "镜 {} {}",
                num_of(&shot, &["shot_no"]) as i64,
                text_of(&shot, &["time"])
            ),
            format!(
                "{} · {} · {}",
                text_of(&shot, &["shot_size"]),
                text_of(&shot, &["camera_motion"]),
                text_of(&shot, &["action"])
            ),
            text_of(&shot, &["dialogue"]),
            "info",
        ));
    }
    let prompts = result.get("cine_prompt").cloned().unwrap_or(Value::Null);
    st.rv_zh = text_of(&prompts, &["prompts.zh"]);
    st.rv_en = text_of(&prompts, &["prompts.en"]);
    st.rv_report = concat_host::pro3::reverse_report_url(job);
    let cost = result.get("cost").cloned().unwrap_or(Value::Null);
    st.rv_line = format!(
        "反推完成：{} 个分镜；提示词中文 {} 字 / 英文 {} 字符；本次成本约 ¥{:.3}（零付费视觉模型，只算文本 LLM + 本机算力）。",
        st.rv_shots.len(),
        st.rv_zh.chars().count(),
        st.rv_en.chars().count(),
        num_of(&cost, &["total_cny"])
    );
    st.pv_line = st.rv_line.clone();
    say(st, "视频反推完成。", "ok");
}

// ── ⑥ 自愈剪辑 ───────────────────────────────────────────────────────

fn heal_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        || concat_host::pro3::heal_health(),
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        st.hl_line = format!(
                            "自愈引擎在线（{}）：ffmpeg {}，OpenCV {}；单次分析预算 {}s。",
                            text_of(&value, &["version"]),
                            text_of(&value, &["tools.ffmpeg"]),
                            text_of(&value, &["tools.opencv"]),
                            num_of(&value, &["limits.analyze_budget_s"]) as i64
                        );
                        say(st, "自愈剪辑引擎在线。", "ok");
                    }
                    Err(error) => {
                        st.hl_line = format!("自愈接口没就绪：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn heal_analyze(weak: slint::Weak<App>) {
    let (source, local) = with(|st| {
        st.busy = true;
        st.hl_run.clear();
        st.hl_report.clear();
        st.hl_counts.clear();
        st.hl_reshoot_line.clear();
        st.hl_issues.clear();
        st.hl_plan.clear();
        st.hl_line = "正在上传 + 自愈分析（约 20–60s）…".to_owned();
        say(st, st.hl_line.clone(), "info");
        (st.hl_source.clone(), st.hl_local.clone())
    });
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        move || -> Result<Value, String> {
            let url = resolve_source(&source, &local)?;
            concat_host::pro3::heal_analyze(&json!({ "video_url": url }))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let run = text_of(&value, &["run_id"]);
                        st.hl_run = run.clone();
                        st.hl_report = concat_host::pro3::heal_report_url(&run);
                        let counts = value.get("counts").cloned().unwrap_or(Value::Null);
                        st.hl_issues.clear();
                        for item in first_array(&value, &["issues"]) {
                            let severity = text_of(&item, &["severity"]);
                            let level = if severity == "P0" {
                                "fail"
                            } else if severity == "P1" {
                                "warn"
                            } else {
                                "info"
                            };
                            st.hl_issues.push(row(
                                format!(
                                    "{severity} {}",
                                    text_of(&item, &["type_label", "type"])
                                ),
                                text_of(&item, &["reason", "detail", "suggestion"]),
                                text_of(&item, &["at", "time"]),
                                level,
                            ));
                        }
                        let issues = st.hl_issues.len();
                        st.hl_counts = format!(
                            "问题 {} 项（P0 {} / P1 {} / P2 {}）；分析 {:.1}s",
                            issues,
                            num_of(&counts, &["P0"]) as i64,
                            num_of(&counts, &["P1"]) as i64,
                            num_of(&counts, &["P2"]) as i64,
                            num_of(&value, &["analysis_seconds"])
                        );
                        st.hl_line = format!(
                            "自愈分析完成（run {run}）：{issues} 个问题。接下来点「生成修复方案」与「补拍清单」。检测精度为参考值，开拍前请人工复核证据帧。"
                        );
                        st.pv_line = st.hl_line.clone();
                        say(st, "自愈分析完成（真实 /heal/analyze）。", "ok");
                    }
                    Err(error) => {
                        st.hl_line = format!("自愈分析失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn heal_plan(weak: slint::Weak<App>, reshoot: bool) {
    let run = with(|st| {
        st.busy = true;
        st.hl_plan.clear();
        st.hl_reshoot_line.clear();
        if reshoot {
            st.hl_reshoot_line = "正在生成补拍清单…".to_owned();
        } else {
            st.hl_line = "正在生成修复方案（POST /heal/plan）…".to_owned();
        }
        st.hl_run.clone()
    });
    if run.is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        move || -> Result<Value, String> {
            if reshoot {
                let list = concat_host::pro3::heal_reshoot_list(&run)?;
                Ok(json!({ "kind": "reshoot", "value": list }))
            } else {
                let plan = concat_host::pro3::heal_plan(&run)?;
                Ok(json!({ "kind": "plan", "value": plan }))
            }
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(packed) => {
                        let kind = text_of(&packed, &["kind"]);
                        let value = packed.get("value").cloned().unwrap_or(Value::Null);
                        if kind == "reshoot" {
                            let items = first_array(&value, &["items"]);
                            st.hl_reshoot_line = format!(
                                "补拍清单：{} 项（html 报告可打开）。",
                                num_of(&value, &["count"]) as i64
                            );
                            for item in items {
                                st.hl_plan.push(row(
                                    format!(
                                        "{} · {}",
                                        num_of(&item, &["no"]) as i64,
                                        text_of(&item, &["scene"])
                                    ),
                                    text_of(&item, &["suggestion"]),
                                    format!(
                                        "{} {}",
                                        text_of(&item, &["type"]),
                                        text_of(&item, &["priority"])
                                    ),
                                    "warn",
                                ));
                            }
                            say(st, "补拍清单已生成。", "ok");
                        } else {
                            let plan = value.get("plan").cloned().unwrap_or(value);
                            let items = first_array(&plan, &["items"]);
                            let counts = plan.get("counts").cloned().unwrap_or(Value::Null);
                            st.hl_line = format!(
                                "修复方案：{} 项（自动修 {} / 需重生成 {} / 补拍 {} / 阻塞 {}）。",
                                items.len(),
                                num_of(&counts, &["auto_fix"]) as i64,
                                num_of(&counts, &["ai_generate"]) as i64,
                                num_of(&counts, &["reshoot"]) as i64,
                                num_of(&counts, &["blocked"]) as i64
                            );
                            for item in items {
                                let action = text_of(&item, &["action"]);
                                let level = if action == "auto_fix" {
                                    "ok"
                                } else if action == "blocked" {
                                    "fail"
                                } else {
                                    "warn"
                                };
                                st.hl_plan.push(row(
                                    text_of(&item, &["title", "issue_type", "type"]),
                                    text_of(&item, &["reason", "suggestion", "method", "detail"]),
                                    action,
                                    level,
                                ));
                            }
                            say(st, "修复方案已生成。", "ok");
                        }
                    }
                    Err(error) => {
                        if reshoot {
                            st.hl_reshoot_line = format!("补拍清单生成失败：{error}");
                        } else {
                            st.hl_line = format!("修复方案生成失败：{error}");
                        }
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

// ── ⑦ 补机位 ─────────────────────────────────────────────────────────

fn multicam_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        || -> Result<(Value, Value), String> {
            let health = concat_host::pro3::multicam_health()?;
            let modes = concat_host::pro3::multicam_modes()?;
            Ok((health, modes))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok((health, modes)) => {
                        st.mc_modes.clear();
                        st.mc_mode_ids.clear();
                        for item in first_array(&modes, &["modes"]) {
                            st.mc_modes.push(row(
                                text_of(&item, &["mode_cn"]),
                                text_of(&item, &["description"]),
                                text_of(&item, &["mode"]),
                                "info",
                            ));
                            st.mc_mode_ids.push(text_of(&item, &["mode"]));
                        }
                        let ready = health.get("generate").cloned().unwrap_or(Value::Null);
                        st.mc_line = format!(
                            "补机位服务在线（{}）：{} 种二次构图模式；目标机位 {} 个。二次构图是同素材裁剪/变焦/镜像/重构，不是新视角。",
                            text_of(&health, &["version"]),
                            st.mc_modes.len(),
                            first_array(&modes, &["target_angles"]).len()
                        );
                        st.mc_ai_line = format!(
                            "AI 补机位通道：{}（就绪 {} 个）；本版本经济模式只查余额，不发起付费生成。",
                            if ready.get("configured").and_then(Value::as_bool) == Some(true) {
                                "已配置"
                            } else {
                                "未配置 / 无余额"
                            },
                            num_of(&ready, &["ready_providers"]) as i64
                        );
                        say(st, "补机位服务在线（7 种模式）。", "ok");
                    }
                    Err(error) => {
                        st.mc_line = format!("补机位接口没就绪：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn multicam_reframe(weak: slint::Weak<App>) {
    let (source, local, mode, zoom) = with(|st| {
        st.busy = true;
        st.mc_output.clear();
        st.mc_output_name.clear();
        st.mc_rows.clear();
        st.mc_line = "正在上传 + 二次构图渲染（真实 ffmpeg，约 5–40s）…".to_owned();
        say(st, st.mc_line.clone(), "info");
        let mode = st
            .mc_mode_ids
            .get(st.mc_mode.max(0) as usize)
            .cloned()
            .unwrap_or_else(|| "punch_in".to_owned());
        (st.mc_source.clone(), st.mc_local.clone(), mode, st.mc_zoom.clone())
    });
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        move || -> Result<Value, String> {
            let url = resolve_source(&source, &local)?;
            let zoom = zoom.trim().parse::<f64>().unwrap_or(0.3);
            let params = match mode.as_str() {
                "crop_push" => json!({ "zoom": zoom }),
                "punch_in" => json!({ "zoom": if zoom < 1.05 { 1.35 } else { zoom } }),
                "pan_scan" => json!({ "zoom": if zoom < 1.05 { 1.30 } else { zoom }, "axis": "x", "direction": "lr" }),
                "flip" => json!({ "direction": "h" }),
                "tilt_shift" => json!({ "sigma": 10, "band": 0.22 }),
                "vertical_reframe" => json!({ "target_ratio": "9:16" }),
                _ => json!({ "layout": "top_bottom", "gap": 4 }),
            };
            concat_host::pro3::multicam_reframe(&json!({
                "video_url": url,
                "mode": mode,
                "params": params,
            }))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let output = value.get("output").cloned().unwrap_or(Value::Null);
                        let url = text_of(&output, &["url"]);
                        st.mc_output = concat_host::pro3::multicam_media_url(&url);
                        st.mc_output_name = text_of(&output, &["filename"]);
                        st.mc_rows.clear();
                        st.mc_rows.push(row(
                            "模式",
                            text_of(&value, &["mode_cn"]),
                            text_of(&value, &["mode"]),
                            "ok",
                        ));
                        st.mc_rows.push(row(
                            "成片",
                            format!(
                                "{}×{} · {:.1} MB",
                                num_of(&output, &["width"]) as i64,
                                num_of(&output, &["height"]) as i64,
                                num_of(&output, &["bytes"]) / 1024.0 / 1024.0
                            ),
                            text_of(&output, &["filename"]),
                            "ok",
                        ));
                        st.mc_rows.push(row(
                            "诚实标注",
                            text_of(&value, &["honesty.note_cn"]),
                            text_of(&value, &["honesty.label_cn"]),
                            "warn",
                        ));
                        st.mc_line = format!(
                            "二次构图完成（{}）：{}。可下载 / 插入时间线；这不是 AI 新视角。",
                            text_of(&value, &["mode_cn"]),
                            text_of(&output, &["filename"])
                        );
                        st.pv_line = st.mc_line.clone();
                        let poster = text_of(&output, &["poster_url"]);
                        let poster_url = if poster.is_empty() {
                            st.mc_output.clone()
                        } else {
                            concat_host::pro3::multicam_media_url(&poster)
                        };
                        st.mc_poster_line = format!("预览帧：{poster_url}");
                        say(st, "二次构图完成（真实 /multicam/reframe）。", "ok");
                        if !poster_url.is_empty() {
                            download_preview(app.as_weak(), poster_url, "multicam");
                        }
                    }
                    Err(error) => {
                        st.mc_line = format!("二次构图失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn multicam_suggest(weak: slint::Weak<App>) {
    let source = with(|st| {
        st.busy = true;
        st.mc_rows.clear();
        st.mc_line = "正在生成补机位建议（POST /multicam/suggest）…".to_owned();
        st.mc_source.clone()
    });
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        move || {
            concat_host::pro3::multicam_suggest(&json!({
                "fps": 25,
                "shots": [
                    { "scene_id": "S1", "start": 0, "end": 4, "angle": "wide", "source_video": source },
                    { "scene_id": "S1", "start": 4, "end": 7, "angle": "wide", "source_video": source },
                    { "scene_id": "S2", "start": 7, "end": 11, "angle": "close_up", "source_video": source },
                ],
            }))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let summary = value.get("summary").cloned().unwrap_or(Value::Null);
                        st.mc_line = format!(
                            "补机位建议：{} 镜 / {} 场，单机位场景 {} 个（可补 {} 处）；若走 AI 生成预计 ¥{:.2}。A 模式二次构图免费可立即用。",
                            num_of(&summary, &["shots"]) as i64,
                            num_of(&summary, &["scenes"]) as i64,
                            num_of(&summary, &["single_angle_scenes"]) as i64,
                            num_of(&summary, &["supplement_points"]) as i64,
                            num_of(&summary, &["estimated_generate_cost_cny"])
                        );
                        st.mc_rows.clear();
                        for point in first_array(&value, &["points"]) {
                            st.mc_rows.push(row(
                                format!(
                                    "场 {} / {:.1}s",
                                    text_of(&point, &["scene_id"]),
                                    num_of(&point, &["duration_s"])
                                ),
                                text_of(&point, &["problem_cn"]),
                                join_text(&string_list(&point, &["current_angles_cn"])),
                                "warn",
                            ));
                            for suggestion in first_array(&point, &["suggestions"]) {
                                st.mc_rows.push(row(
                                    format!("建议 {}", text_of(&suggestion, &["angle_cn"])),
                                    text_of(&suggestion, &["method_best_cn"]),
                                    format!(
                                        "二次构图 {}",
                                        text_of(&suggestion, &["reframe.mode_cn"])
                                    ),
                                    "info",
                                ));
                            }
                        }
                        say(st, "补机位建议已生成。", "ok");
                    }
                    Err(error) => {
                        st.mc_line = format!("补机位建议失败：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

fn multicam_ai(weak: slint::Weak<App>) {
    with(|st| {
        st.busy = true;
        st.mc_ai_line = "正在只读探测 AI 补机位通道与余额（不发起生成）…".to_owned();
    });
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        || concat_host::pro3::multicam_providers(),
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let ready = value
                            .get("ready_count")
                            .and_then(Value::as_i64)
                            .or_else(|| value.get("configured_count").and_then(Value::as_i64))
                            .unwrap_or(0);
                        let balance = value
                            .get("siliconflow_balance")
                            .cloned()
                            .unwrap_or(Value::Null);
                        let configured = balance
                            .get("configured")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        let balance_text = if !configured {
                            "未开通（SILICONFLOW_KEY 未配置）".to_owned()
                        } else {
                            let number = text_of(&value, &["siliconflow_balance.data.balance"]);
                            if number.is_empty() {
                                text_of(&value, &["siliconflow_balance.hint_cn"])
                            } else {
                                format!("¥{number}")
                            }
                        };
                        st.mc_ai_line = if ready <= 0 {
                            format!(
                                "AI 补机位暂不可用：就绪通道 0 个，余额 {balance_text}。请先到网页版充值 / 配置视频模型；A 模式二次构图免费可用。"
                            )
                        } else {
                            format!(
                                "AI 补机位通道就绪 {ready} 个，余额 {balance_text}。本版本为经济模式（不调用付费视频模型）：入口已接好，生成请到网页版 /multicam 执行；A 模式二次构图免费可用。"
                            )
                        };
                        say(st, "AI 补机位通道状态已取回（未发起付费生成）。", "ok");
                    }
                    Err(error) => {
                        st.mc_ai_line = format!("AI 补机位通道状态没取回来：{error}（A 模式二次构图仍可用）");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

// ── ⑧ 节点画布 / 导演脑 ──────────────────────────────────────────────

fn canvas_load(weak: slint::Weak<App>) {
    with(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let ep = with(|st| st.epoch);
    host::spawn(
        || -> Result<(Value, Value, Value, Value), String> {
            let canvas = concat_host::pro3::canvas_health()?;
            let templates = concat_host::pro3::canvas_templates()?;
            let brain = concat_host::pro3::brain_health()?;
            let styles = concat_host::pro3::brain_styles()?;
            Ok((canvas, templates, brain, styles))
        },
        move |_studio, app, _models, result| {
            if with(|st| st.epoch) != ep {
                return;
            }
            with(|st| {
                st.busy = false;
                match result {
                    Ok((canvas, templates, brain, styles)) => {
                        st.nc_templates.clear();
                        for item in first_array(&templates, &["templates"]) {
                            st.nc_templates.push(row(
                                text_of(&item, &["name", "title", "id"]),
                                format!(
                                    "{} 个节点 · {}",
                                    num_of(&item, &["nodes_count"]),
                                    text_of(&item, &["description", "summary"])
                                ),
                                text_of(&item, &["id"]),
                                "info",
                            ));
                        }
                        st.nc_styles.clear();
                        for item in first_array(&styles, &["styles"]) {
                            st.nc_styles.push(row(
                                text_of(&item, &["name"]),
                                text_of(&item, &["genre"]),
                                text_of(&item, &["style_id"]),
                                "info",
                            ));
                        }
                        st.nc_line = format!(
                            "节点画布在线（{}）：{} 类节点 / {} 个模板；引擎 {}。完整编排在网页版画布。",
                            text_of(&canvas, &["version"]),
                            num_of(&canvas, &["node_types"]) as i64,
                            st.nc_templates.len(),
                            join_text(&string_list(&canvas, &["executors"]))
                        );
                        st.nc_brain_line = format!(
                            "导演脑在线（{}）：{} 个内置导演风格档案；分镜 / 节奏 / LUT / 配乐在网页版编排。",
                            text_of(&brain, &["version"]),
                            st.nc_styles.len()
                        );
                        say(st, "节点画布与导演脑接口在线。", "ok");
                    }
                    Err(error) => {
                        st.nc_line = format!("节点画布 / 导演脑接口没就绪：{error}");
                        say(st, error, "warn");
                    }
                }
            });
            publish(&app);
        },
    );
}

// ── 动作分发 ─────────────────────────────────────────────────────────

/// 面板上的一个手势。
pub fn action(weak: slint::Weak<App>, action: String, index: i32, text: String) {
    let Some(app) = weak.upgrade() else { return };

    match action.as_str() {
        "tab" => {
            let tab = index.clamp(0, TABS);
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

    // 文本输入：先记下来（不联网）。
    match action.as_str() {
        "pp-name" => with(|st| st.pp_name = text),
        "pp-look" => with(|st| st.pp_look = text),
        "pp-wardrobe" => with(|st| st.pp_wardrobe = text),
        "pp-location" => with(|st| st.pp_location = text),
        "pp-time" => with(|st| st.pp_time = text),
        "pp-weather" => with(|st| st.pp_weather = text),
        "pp-mood" => with(|st| st.pp_mood = text),
        "pp-duration" => with(|st| st.pp_duration = text),
        "pp-notes" => with(|st| st.pp_notes = text),
        "ix-query" => with(|st| st.ix_query = text),
        "ae-source" => with(|st| st.ae_source = text),
        "rv-source" => with(|st| st.rv_source = text),
        "hl-source" => with(|st| st.hl_source = text),
        "mc-source" => with(|st| st.mc_source = text),
        "mc-zoom" => with(|st| st.mc_zoom = text),
        _ => {}
    }
    let is_input = matches!(
        action.as_str(),
        "pp-name" | "pp-look" | "pp-wardrobe" | "pp-location" | "pp-time" | "pp-weather"
            | "pp-mood" | "pp-duration" | "pp-notes" | "ix-query" | "ae-source" | "rv-source"
            | "hl-source" | "mc-source" | "mc-zoom"
    );
    if is_input {
        return;
    }

    let tab = with(|st| st.tab);
    match action.as_str() {
        // ① 提示词引擎
        a if a.starts_with("pp-") => match a {
            "pp-load" => prompt_load(weak.clone()),
            "pp-build" => prompt_build(weak.clone(), false),
            "pp-estimate" => prompt_build(weak.clone(), true),
            "pp-next-template" => {
                with(|st| {
                    let len = st.pp_templates.len() as i32;
                    if len > 0 {
                        st.pp_template = (st.pp_template + 1) % len;
                    }
                });
                publish(&app);
            }
            "pp-next-model" => {
                with(|st| {
                    let len = st.pp_models.len() as i32;
                    if len > 0 {
                        st.pp_model = (st.pp_model + 1) % len;
                    }
                });
                publish(&app);
            }
            "pp-next-aspect" => {
                with(|st| st.pp_aspect = (st.pp_aspect + 1) % 3);
                publish(&app);
            }
            "pp-next-language" => {
                with(|st| st.pp_language = (st.pp_language + 1) % 3);
                publish(&app);
            }
            "pp-use-model" => {
                let (detail, value) = with(|st| {
                    st.pp_variants
                        .get(index.max(0) as usize)
                        .map(|row| (row.detail.clone(), row.value.clone()))
                        .unwrap_or_default()
                });
                with(|st| {
                    st.pp_model_prompt = format!("已选模型模板：{detail}（{value}）。按模型改写的提示词在各模型的 model_variants 里，切换模型后再 build 即按新模板出词。");
                });
                publish(&app);
            }
            _ => {}
        },
        // ② 风格库
        a if a.starts_with("sl-") => match a {
            "sl-load" => stylelib_load(weak.clone()),
            "sl-pick" => stylelib_pick(weak.clone(), index),
            "sl-guide" => {
                with(|st| {
                    let len = st.sl_guide_ids.len();
                    st.sl_guide = clamp_index(index, len);
                });
                publish(&app);
            }
            "sl-ratio" => {
                with(|st| st.sl_ratio = index.clamp(0, 3));
                publish(&app);
            }
            "sl-preview" => stylelib_preview(weak.clone()),
            "sl-lut" => stylelib_lut(weak.clone()),
            _ => {}
        },
        // ③ 项目资产文件库
        a if a.starts_with("ix-") => match a {
            "ix-load" => index_load(weak.clone()),
            "ix-project" => {
                let project = with(|st| {
                    let len = st.ix_project_ids.len();
                    st.ix_project = clamp_index(index, len);
                    st.ix_project_ids
                        .get(st.ix_project.max(0) as usize)
                        .cloned()
                        .unwrap_or_default()
                });
                let index_for_job = project.clone();
                let index_for_work = project.clone();
                with(|st| {
                    st.busy = true;
                    st.ix_line = format!("正在取项目 {index_for_job} 的类型树…");
                });
                let ep = with(|st| st.epoch);
                publish(&app);
                host::spawn(
                    move || concat_host::pro3::index_tree(&index_for_work),
                    move |_studio, app, _models, result| {
                        if with(|st| st.epoch) != ep {
                            return;
                        }
                        with(|st| {
                            st.busy = false;
                            match result {
                                Ok(tree) => {
                                    apply_index_tree(st, &tree);
                                    st.ix_line = format!(
                                        "项目 {index_for_job}：{} 类资产。点类型看条目，点条目看详情。",
                                        st.ix_tree.len()
                                    );
                                    say(st, "项目类型树已取回。", "ok");
                                }
                                Err(error) => {
                                    st.ix_line = format!("类型树没取回来：{error}");
                                    say(st, error, "warn");
                                }
                            }
                        });
                        publish(&app);
                    },
                );
            }
            "ix-type" => {
                with(|st| {
                    let len = st.ix_items_raw.len();
                    st.ix_type = clamp_index(index, len);
                    apply_index_type(st, st.ix_type);
                });
                publish(&app);
            }
            "ix-item" => {
                with(|st| st.ix_selected = index.max(0));
                index_item(weak.clone(), index, false, false)
            }
            "ix-search" => index_search(weak.clone()),
            "ix-versions" => {
                let index = with(|st| st.ix_selected);
                index_item(weak.clone(), index, true, false);
            }
            "ix-impact" => {
                let index = with(|st| st.ix_selected);
                index_item(weak.clone(), index, false, true);
            }
            "ix-export" => {
                let project = with(|st| {
                    st.ix_project_ids
                        .get(st.ix_project.max(0) as usize)
                        .cloned()
                        .unwrap_or_default()
                });
                index_export(weak.clone(), &project);
            }
            _ => {}
        },
        // ④ AI 审美总监
        a if a.starts_with("ae-") => match a {
            "ae-load" => aesthetic_load(weak.clone()),
            "ae-sample" => {
                with(|st| {
                    st.ae_sample = index.max(0);
                    if let Some(url) = st.ae_sample_urls.get(index.max(0) as usize) {
                        st.ae_source = url.clone();
                    }
                });
                publish(&app);
            }
            "ae-pick" => pick_video(weak.clone(), TAB_AESTHETIC, "选择本地成片", "aesthetic"),
            "ae-analyze" => aesthetic_analyze(weak.clone()),
            "ae-report" => {
                let url = with(|st| st.ae_report.clone());
                open_url(&url);
            }
            _ => {}
        },
        // ⑤ 视频反推
        a if a.starts_with("rv-") => match a {
            "rv-load" => reverse_load(weak.clone()),
            "rv-sample" => {
                with(|st| {
                    st.rv_sample = index.max(0);
                    if let Some(sample) = st.rv_sample_urls.get(index.max(0) as usize) {
                        st.rv_source = sample.clone();
                    }
                });
                publish(&app);
            }
            "rv-pick" => pick_video(weak.clone(), TAB_REVERSE, "选择要反推的视频", "reverse"),
            "rv-analyze" => reverse_analyze(weak.clone()),
            "rv-report" => {
                let url = with(|st| st.rv_report.clone());
                open_url(&url);
            }
            _ => {}
        },
        // ⑥ 自愈剪辑
        a if a.starts_with("hl-") => match a {
            "hl-load" => heal_load(weak.clone()),
            "hl-pick" => pick_video(weak.clone(), TAB_HEAL, "选择要自愈的成片", "heal"),
            "hl-analyze" => heal_analyze(weak.clone()),
            "hl-plan" => heal_plan(weak.clone(), false),
            "hl-reshoot" => heal_plan(weak.clone(), true),
            "hl-report" => {
                let url = with(|st| st.hl_report.clone());
                open_url(&url);
            }
            _ => {}
        },
        // ⑦ 补机位
        a if a.starts_with("mc-") => match a {
            "mc-load" => multicam_load(weak.clone()),
            "mc-sample" => {
                with(|st| {
                    st.mc_source = MULTICAM_SAMPLE.to_owned();
                    st.mc_line = "已填入平台提供的 20s 补机位样片；点「二次构图（reframe）」即可真实渲染。".to_owned();
                });
                publish(&app);
            }
            "mc-mode" => {
                with(|st| {
                    let len = st.mc_mode_ids.len();
                    st.mc_mode = clamp_index(index, len);
                });
                publish(&app);
            }
            "mc-pick" => pick_video(weak.clone(), TAB_MULTICAM, "选择要补机位的片段", "multicam"),
            "mc-reframe" => multicam_reframe(weak.clone()),
            "mc-suggest" => multicam_suggest(weak.clone()),
            "mc-ai" => multicam_ai(weak.clone()),
            "mc-download" => {
                let (url, name) = with(|st| (st.mc_output.clone(), st.mc_output_name.clone()));
                if !url.is_empty() {
                    let name = if name.is_empty() {
                        format!("二次构图_{}.mp4", crate::pro2::now_secs())
                    } else {
                        name
                    };
                    crate::pro2::download_to(weak.clone(), url, "补机位", name);
                }
            }
            "mc-insert" => {
                let (url, name) = with(|st| (st.mc_output.clone(), st.mc_output_name.clone()));
                if !url.is_empty() {
                    let name = if name.is_empty() {
                        format!("二次构图_{}.mp4", crate::pro2::now_secs())
                    } else {
                        name
                    };
                    crate::pro2::insert_to(weak.clone(), url, "补机位", name);
                }
            }
            _ => {}
        },
        // ⑧ 节点画布 / 导演脑
        a if a.starts_with("nc-") => match a {
            "nc-load" => canvas_load(weak.clone()),
            "nc-canvas-web" => open_url("https://xinglaiai.com/ai-nav/canvas/"),
            "nc-brain-web" => open_url("https://xinglaiai.com/ai-nav/api/brain/"),
            _ => {}
        },
        _ => {
            if tab == TAB_PROMPT {
                // 未知动作不拦面板
            }
        }
    }
}
