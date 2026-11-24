// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 影视专业面板（0.3.7）：窗口这一半。
//!
//! 九块面板：导演编排器 / 导演监视器 / 发行中心 / 授权护照 / 合规沙盘 /
//! 插件市场 / 素材库独立页 / 模板市场 / 统一任务中心。网络调用都在
//! [`concat_host::pro`]；这里管面板状态、把动作翻译成请求、把结果写回
//! Slint，并把每一次真实生成记到本机任务表（`pro-tasks.json`）里。
//!
//! 面板懒加载：启动路径上只有一份空的 `ProData`，没有任何网络请求；
//! 面板打开后才按当前页取数。每个后台请求都带「这次是第几次打开」的
//! 序号，切换页签 / 关闭面板后旧结果直接丢掉，不会覆盖新状态。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use slint::{ModelRc, SharedString, VecModel};

use crate::host::{self, Shell};
use crate::platform;
use crate::studio::Studio;
use crate::ui::*;

const TAB_ORCH: i32 = 0;
const TAB_MONITOR: i32 = 1;
const TAB_DIST: i32 = 2;
const TAB_PASS: i32 = 3;
const TAB_SANDBOX: i32 = 4;
const TAB_PLUGIN: i32 = 5;
const TAB_ASSETS: i32 = 6;
const TAB_TEMPLATE: i32 = 7;
const TAB_TASKS: i32 = 8;

/// 面板标题下的一句话：接口 / 数据来源。
fn hint_of(tab: i32) -> String {
    match tab {
        TAB_ORCH => "导演编排器：分镜 → 平台路由计划（每镜模型 / 成本 / 降级警告）→ 真实生成 → 逐镜进度 → 成片 / 工程包。接口已上线，桌面直连。".to_owned(),
        TAB_MONITOR => "导演监视器：镜头状态板 / take 审片 / 批注 / 一键重拍 / 连续性检查 / 进度预算，全部写平台审计日志。".to_owned(),
        TAB_DIST => "发行中心：短剧 → 平台 / 语言 → 发行包（进度 / 文件清单）→ 下载 → 发布步骤 → 数据回填。".to_owned(),
        TAB_PASS => "授权护照：素材 / 工程 → 护照（内容哈希 + 台账哈希链 + Ed25519 签名）→ 查看 / 校验 / 导出 zip。".to_owned(),
        TAB_SANDBOX => "合规沙盘：规则库 135 条 + AI 语义辅助；风险分 / 问题清单 / 修改建议 / 整改复评。结果仅供参考，不构成法律意见。".to_owned(),
        TAB_PLUGIN => "插件市场：安装 / 启用 / 按 config_schema 配置 / 平台沙箱运行 / 运行历史；资源型插件不执行脚本。".to_owned(),
        TAB_ASSETS => "素材库：星籁免费商用素材（/api/assets/*），搜索 / 行业 / 类型 / 授权角标 / 下载 / 插入时间线。".to_owned(),
        TAB_TEMPLATE => "模板市场：行业 / 场景筛选 → 详情 → 一键套用（POST /api/templates-market/use）→ 成片 / 工程包回桌面。".to_owned(),
        _ => "统一任务中心：本机发起的所有 AI / 渲染任务（本地记录 + 轮询远端），状态 / 进度 / 成本，可打开文件。".to_owned(),
    }
}

/// 网页兜底页（接口不通 / 想看网页版时）。
fn web_of(tab: i32) -> &'static str {
    match tab {
        TAB_ORCH => "director",
        TAB_MONITOR => "monitor",
        TAB_DIST => "publish",
        TAB_PASS => "passport",
        TAB_SANDBOX => "sandbox",
        TAB_PLUGIN => "plugins",
        TAB_ASSETS => "assets",
        TAB_TEMPLATE => "templates",
        _ => "tasks",
    }
}

// ── 行数据（发布到 Slint） ──────────────────────────────────────────

#[derive(Clone, Default)]
struct Row {
    label: String,
    detail: String,
    value: String,
    level: String,
}

#[derive(Clone, Default)]
struct ShotEdit {
    label: String,
    no: i32,
    scene: String,
    line: String,
    dur: String,
}

#[derive(Clone, Default)]
struct PlanShot {
    label: String,
    no: i32,
    title: String,
    model: String,
    cost: String,
    detail: String,
    level: String,
}

#[derive(Clone, Default)]
struct ModelRow {
    group: String,
    name: String,
    vendor: String,
    status: String,
    detail: String,
    level: String,
}

#[derive(Clone, Default)]
struct TierRow {
    key: String,
    label: String,
    desc: String,
    per_shot: String,
    ep120: String,
    ep180: String,
    effective: String,
    downgrade: String,
    hot: bool,
}

#[derive(Clone, Default)]
struct TaskRow {
    id: String,
    module: String,
    title: String,
    status: String,
    progress: f32,
    cost: String,
    detail: String,
    level: String,
    has_file: bool,
    can_open: bool,
}

#[derive(Clone, Default)]
struct BoardShot {
    id: String,
    no: String,
    title: String,
    meta: String,
    status: String,
    flags: String,
    level: String,
}

#[derive(Clone, Default)]
struct TemplateRow {
    id: String,
    name: String,
    meta: String,
    detail: String,
    level: String,
}

#[derive(Clone, Default)]
struct PluginRow {
    id: String,
    name: String,
    meta: String,
    description: String,
    quality: String,
    installed: bool,
    official: bool,
}

#[derive(Clone, Default)]
struct Finding {
    level: String,
    title: String,
    category: String,
    position: String,
    quote: String,
    basis: String,
    why: String,
    suggest: String,
}

// ── 面板状态 ─────────────────────────────────────────────────────────

struct ProState {
    open: bool,
    tab: i32,
    /// 打开面板时算一次登录态：不在每次 publish 里解 DPAPI。
    signed_in: bool,
    /// 这次打开 / 切页的序号：旧请求回来时对不上就丢掉。
    epoch: u64,
    busy: bool,
    status_line: String,
    status_level: String,
    hint: String,

    // 编排器
    tier: i32,
    budget: String,
    title: String,
    text_shots: Vec<ShotEdit>,
    plan: Option<Value>,
    plan_line: String,
    plan_budget: String,
    budget_ok: bool,
    plan_shots: Vec<PlanShot>,
    models: Vec<ModelRow>,
    tiers: Vec<TierRow>,
    run_task: Option<String>,
    run_line: String,
    run_status: String,
    run_progress: f32,
    film_url: String,
    pack_url: String,

    // 监视器
    project: String,
    episode: String,
    type_index: i32,
    type_line: String,
    mon_types: Vec<Row>,
    type_ids: Vec<String>,
    type_detail: String,
    board_line: String,
    board_progress: f32,
    budget_line: String,
    counts: String,
    board: Vec<BoardShot>,
    episodes: Vec<Row>,
    board_ids: Vec<String>,
    shot: Option<Value>,
    shot_line: String,
    shot_title: String,
    shot_meta: String,
    takes: Vec<Row>,
    take_ids: Vec<String>,
    note_ids: Vec<String>,
    notes: Vec<Row>,
    take_url: String,
    take_line: String,
    note_text: String,
    note_line: String,
    retake_prompt: String,
    retake_tier: i32,
    retake_reasons: [bool; 5],
    retake_line: String,
    cont_line: String,
    cont_rows: Vec<Row>,
    retake_queue: Vec<Row>,
    audit_rows: Vec<Row>,
    queue_line: String,

    // 发行
    dramas: Vec<Value>,
    drama_index: i32,
    dists: Vec<Value>,
    dist_pick: i32,
    platform: i32,
    lang: i32,
    dist_line: String,
    dist_files: Vec<Row>,
    dist_url: String,
    dist_views: String,
    dist_likes: String,
    dist_conversions: String,
    dist_publish_url: String,
    dist_stats_line: String,
    dist_steps: Vec<Row>,

    // 护照
    pass_mode: i32,
    pass_asset_id: String,
    pass_asset_url: String,
    pass_project: String,
    pass_pack: String,
    pass_title: String,
    pass_client: String,
    pass_line: String,
    pass_rows: Vec<Row>,
    pass_verify_line: String,
    pass_verify_rows: Vec<Row>,
    pass_verify_id: String,
    pass_sha: String,
    pass_id: String,
    pass_zip_url: String,
    pass_json_path: String,
    pass_recent: Vec<Row>,
    pass_last: Option<Value>,

    // 沙盘
    sb_content: String,
    sb_type: i32,
    sb_platform: i32,
    sb_ai: bool,
    sb_line: String,
    sb_score: i32,
    sb_grade: String,
    sb_summary: String,
    sb_stats: String,
    sb_rewrite: String,
    sb_ai_line: String,
    sb_history_line: String,
    sb_check_id: String,
    sb_findings: Vec<Finding>,
    sb_platforms: Vec<Row>,
    sb_history: Vec<Row>,

    // 插件
    plug_query: String,
    plug_market: Vec<Value>,
    plug_installed: Vec<Value>,
    plug_selected: String,
    plug_detail: Option<Value>,
    plug_line: String,
    plug_detail_line: String,
    plug_detail_desc: String,
    plug_detail_meta: String,
    plug_schema: String,
    plug_config_json: String,
    plug_run_json: String,
    plug_output: String,
    plug_runs_line: String,
    plug_installed_flag: bool,
    plug_enabled: bool,
    plug_runnable: bool,

    // 模板
    tpl_industries: Vec<String>,
    tpl_industry_keys: Vec<String>,
    tpl_scenes: Vec<String>,
    tpl_scene_keys: Vec<String>,
    tpl_industry: i32,
    tpl_scene: i32,
    tpl_list: Vec<Value>,
    tpl_selected: String,
    tpl_detail: Option<Value>,
    tpl_line: String,
    tpl_detail_line: String,
    tpl_detail_desc: String,
    tpl_store: String,
    tpl_product: String,
    tpl_points: String,
    tpl_copy: String,
    tpl_duration: i32,
    tpl_quality: i32,
    tpl_task: Option<String>,
    tpl_gen_line: String,
    tpl_gen_progress: f32,
    tpl_video_url: String,
    tpl_download_url: String,
    tpl_fav: bool,

    // 素材库（列表状态在 AssetsPane 里；这里只要一行说明）
    asset_line: String,

    // 任务中心
    tasks: Vec<concat_host::pro::LocalTask>,
    task_line: String,
    tasks_loaded: bool,

    /// 面板 Timer 的计数（每 5 拍刷新一次远端任务表）。
    ticks: u32,
}

impl Default for ProState {
    fn default() -> Self {
        let mut st = ProState {
            open: false,
            tab: TAB_ORCH,
            signed_in: false,
            epoch: 0,
            busy: false,
            status_line: String::new(),
            status_level: "info".to_owned(),
            hint: hint_of(TAB_ORCH),
            tier: 0,
            budget: "10".to_owned(),
            title: "AI 导演成片".to_owned(),
            text_shots: Vec::new(),
            plan: None,
            plan_line: String::new(),
            plan_budget: String::new(),
            budget_ok: false,
            plan_shots: Vec::new(),
            models: Vec::new(),
            tiers: Vec::new(),
            run_task: None,
            run_line: String::new(),
            run_status: String::new(),
            run_progress: 0.0,
            film_url: String::new(),
            pack_url: String::new(),
            project: String::new(),
            episode: String::new(),
            type_index: -1,
            type_line: String::new(),
            mon_types: Vec::new(),
            type_ids: Vec::new(),
            type_detail: String::new(),
            board_line: String::new(),
            board_progress: 0.0,
            budget_line: String::new(),
            counts: String::new(),
            board: Vec::new(),
            episodes: Vec::new(),
            board_ids: Vec::new(),
            shot: None,
            shot_line: String::new(),
            shot_title: "选一镜看 take / 批注 / 重拍".to_owned(),
            shot_meta: String::new(),
            takes: Vec::new(),
            take_ids: Vec::new(),
            note_ids: Vec::new(),
            notes: Vec::new(),
            take_url: String::new(),
            take_line: String::new(),
            note_text: String::new(),
            note_line: String::new(),
            retake_prompt: String::new(),
            retake_tier: 0,
            retake_reasons: [true, false, false, false, false],
            retake_line: String::new(),
            cont_line: String::new(),
            cont_rows: Vec::new(),
            retake_queue: Vec::new(),
            audit_rows: Vec::new(),
            queue_line: String::new(),
            dramas: Vec::new(),
            drama_index: 0,
            dists: Vec::new(),
            dist_pick: -1,
            platform: 0,
            lang: 0,
            dist_line: String::new(),
            dist_files: Vec::new(),
            dist_url: String::new(),
            dist_views: "0".to_owned(),
            dist_likes: "0".to_owned(),
            dist_conversions: "0".to_owned(),
            dist_publish_url: String::new(),
            dist_stats_line: String::new(),
            dist_steps: default_steps(),
            pass_mode: 0,
            pass_asset_id: String::new(),
            pass_asset_url: String::new(),
            pass_project: String::new(),
            pass_pack: String::new(),
            pass_title: String::new(),
            pass_client: String::new(),
            pass_line: String::new(),
            pass_rows: Vec::new(),
            pass_verify_line: String::new(),
            pass_verify_rows: Vec::new(),
            pass_verify_id: String::new(),
            pass_sha: String::new(),
            pass_id: String::new(),
            pass_zip_url: String::new(),
            pass_json_path: String::new(),
            pass_recent: Vec::new(),
            pass_last: None,
            sb_content: "这款面膜三天祛斑、根治痘痘，敏感肌也能用，纯天然无副作用，医院同款，7天见效。".to_owned(),
            sb_type: 0,
            sb_platform: 0,
            sb_ai: true,
            sb_line: String::new(),
            sb_score: -1,
            sb_grade: "未检查".to_owned(),
            sb_summary: "粘贴要发布的文案 / 脚本，选平台后点「开始彩排」。".to_owned(),
            sb_stats: String::new(),
            sb_rewrite: String::new(),
            sb_ai_line: String::new(),
            sb_history_line: String::new(),
            sb_check_id: String::new(),
            sb_findings: Vec::new(),
            sb_platforms: Vec::new(),
            sb_history: Vec::new(),
            plug_query: String::new(),
            plug_market: Vec::new(),
            plug_installed: Vec::new(),
            plug_selected: String::new(),
            plug_detail: None,
            plug_line: String::new(),
            plug_detail_line: "选一个插件看详情".to_owned(),
            plug_detail_desc: String::new(),
            plug_detail_meta: String::new(),
            plug_schema: String::new(),
            plug_config_json: "{}".to_owned(),
            plug_run_json: "{}".to_owned(),
            plug_output: String::new(),
            plug_runs_line: String::new(),
            plug_installed_flag: false,
            plug_enabled: false,
            plug_runnable: false,
            tpl_industries: Vec::new(),
            tpl_industry_keys: Vec::new(),
            tpl_scenes: Vec::new(),
            tpl_scene_keys: Vec::new(),
            tpl_industry: 0,
            tpl_scene: 0,
            tpl_list: Vec::new(),
            tpl_selected: String::new(),
            tpl_detail: None,
            tpl_line: String::new(),
            tpl_detail_line: "选一个模板看详情".to_owned(),
            tpl_detail_desc: String::new(),
            tpl_store: String::new(),
            tpl_product: String::new(),
            tpl_points: String::new(),
            tpl_copy: String::new(),
            tpl_duration: 0,
            tpl_quality: 0,
            tpl_task: None,
            tpl_gen_line: String::new(),
            tpl_gen_progress: 0.0,
            tpl_video_url: String::new(),
            tpl_download_url: String::new(),
            tpl_fav: false,
            asset_line: String::new(),
            tasks: Vec::new(),
            task_line: String::new(),
            tasks_loaded: false,
            ticks: 0,
        };
        // 默认 3 镜：打开面板就能直接 plan，不用先打字。
        st.text_shots = vec![
            ShotEdit {
                label: "S01".to_owned(),
                no: 1,
                scene: "雨夜，老城区巷口，外卖箱上的雨珠特写，路灯昏黄".to_owned(),
                line: String::new(),
                dur: "4".to_owned(),
            },
            ShotEdit {
                label: "S02".to_owned(),
                no: 2,
                scene: "外卖员推门进店，抬头看见柜台后的旧照片，眼神一滞".to_owned(),
                line: "这张照片……哪来的？".to_owned(),
                dur: "5".to_owned(),
            },
            ShotEdit {
                label: "S03".to_owned(),
                no: 3,
                scene: "骑电动车冲进雨幕，后视镜里店门灯光慢慢变暗".to_owned(),
                line: String::new(),
                dur: "4".to_owned(),
            },
        ];
        st
    }
}

/// 发行中心的发布步骤（与平台发行说明一致，静态清单；平台审核口径会变）。
fn default_steps() -> Vec<Row> {
    let step = |label: &str, detail: &str| Row {
        label: label.to_owned(),
        detail: detail.to_owned(),
        value: String::new(),
        level: "".to_owned(),
    };
    vec![
        step("1. 下载发行包", "zip 里有视频（竖版 / 横版）、字幕 SRT/VTT、封面、剧本、metadata 与发行说明"),
        step("2. 检查合规", "先在「合规沙盘」过一遍；AI 生成内容按《AI 生成合成内容标识办法》显著标识"),
        step("3. 上传平台", "抖音 / 快手 / 小红书：创作者平台后台上传；TikTok / YouTube 用发布链接登记"),
        step("4. 填写标题与话题", "标题按平台限长；标签用行业词 + 剧情词；避免绝对化用语与站外导流"),
        step("5. 登记发布链接", "把发布链接回填到「数据回填」的 publish_url，方便后续对账"),
        step("6. 回填数据", "播放 / 点赞 / 转化 / 评论从平台后台抄回，POST 回台账"),
    ]
}

thread_local! {
    static PRO: std::cell::RefCell<ProState> = std::cell::RefCell::new(ProState::default());
}

fn with_pro<T>(f: impl FnOnce(&mut ProState) -> T) -> T {
    PRO.with(|cell| f(&mut cell.borrow_mut()))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 短文本：取第一个非空字符串字段。
fn text_of(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return text.to_owned();
            }
        }
    }
    String::new()
}

/// 数字字段（接受字符串数字）。
fn num_of(value: &Value, keys: &[&str]) -> f64 {
    for key in keys {
        if let Some(number) = value.get(*key) {
            if let Some(number) = number.as_f64() {
                return number;
            }
            if let Some(text) = number.as_str()
                && let Ok(number) = text.trim().parse::<f64>()
            {
                return number;
            }
        }
    }
    0.0
}

fn money(cny: f64) -> String {
    if cny <= 0.0 {
        "¥0".to_owned()
    } else if cny < 1.0 {
        format!("¥{cny:.2}")
    } else {
        format!("¥{cny:.2}")
    }
}

fn pct(value: f64) -> String {
    format!("{:.0}%", value.clamp(0.0, 100.0))
}

// ── 发布到 Slint ─────────────────────────────────────────────────────

fn rows_model(rows: &[Row]) -> ModelRc<ProRow> {
    ModelRc::from(std::rc::Rc::new(VecModel::from(
        rows.iter()
            .map(|row| ProRow {
                label: row.label.clone().into(),
                detail: row.detail.clone().into(),
                value: row.value.clone().into(),
                level: row.level.clone().into(),
            })
            .collect::<Vec<_>>(),
    )))
}

fn publish(app: &App) {
    PRO.with(|cell| {
        let st = cell.borrow();
        let data = ProData {
            open: st.open,
            tab: st.tab,
            busy: st.busy,
            signed_in: st.signed_in,
            status_line: st.status_line.clone().into(),
            status_level: st.status_level.clone().into(),
            hint: st.hint.clone().into(),
            orch_tier: st.tier,
            orch_budget: st.budget.clone().into(),
            orch_title: st.title.clone().into(),
            orch_plan_line: st.plan_line.clone().into(),
            orch_plan_budget: st.plan_budget.clone().into(),
            orch_plan_level: st.status_level.clone().into(),
            orch_budget_ok: st.budget_ok,
            orch_run_line: st.run_line.clone().into(),
            orch_run_progress: st.run_progress,
            orch_run_status: st.run_status.clone().into(),
            orch_task_line: st.run_task.clone().unwrap_or_default().into(),
            orch_film_url: st.film_url.clone().into(),
            orch_pack_url: st.pack_url.clone().into(),
            orch_shot_count: st.text_shots.len() as i32,
            mon_project: st.project.clone().into(),
            mon_episode: st.episode.clone().into(),
            mon_type: st.type_index,
            mon_type_line: st.type_line.clone().into(),
            mon_board_line: st.board_line.clone().into(),
            mon_progress: st.board_progress,
            mon_budget_line: st.budget_line.clone().into(),
            mon_counts: st.counts.clone().into(),
            mon_shot_line: st.shot_line.clone().into(),
            mon_shot_title: st.shot_title.clone().into(),
            mon_shot_meta: st.shot_meta.clone().into(),
            mon_take_url: st.take_url.clone().into(),
            mon_take_line: st.take_line.clone().into(),
            mon_note_text: st.note_text.clone().into(),
            mon_note_line: st.note_line.clone().into(),
            mon_retake_prompt: st.retake_prompt.clone().into(),
            mon_retake_tier: st.retake_tier,
            mon_retake_line: st.retake_line.clone().into(),
            mon_cont_line: st.cont_line.clone().into(),
            mon_queue_line: st.queue_line.clone().into(),
            mon_audit_line: String::new().into(),
            dist_drama: st.drama_index,
            dist_pick: st.dist_pick,
            dist_platform: st.platform,
            dist_lang: st.lang,
            dist_line: st.dist_line.clone().into(),
            dist_file_line: st
                .dramas
                .get(st.drama_index.max(0) as usize)
                .map(|drama| text_of(drama, &["title", "logline"]))
                .unwrap_or_default()
                .into(),
            dist_url: st.dist_url.clone().into(),
            dist_views: st.dist_views.clone().into(),
            dist_likes: st.dist_likes.clone().into(),
            dist_conversions: st.dist_conversions.clone().into(),
            dist_publish_url: st.dist_publish_url.clone().into(),
            dist_stats_line: st.dist_stats_line.clone().into(),
            pass_mode: st.pass_mode,
            pass_asset_id: st.pass_asset_id.clone().into(),
            pass_asset_url: st.pass_asset_url.clone().into(),
            pass_project: st.pass_project.clone().into(),
            pass_pack: st.pass_pack.clone().into(),
            pass_title: st.pass_title.clone().into(),
            pass_client: st.pass_client.clone().into(),
            pass_line: st.pass_line.clone().into(),
            pass_verify_line: st.pass_verify_line.clone().into(),
            pass_verify_id: st.pass_verify_id.clone().into(),
            pass_sha: st.pass_sha.clone().into(),
            pass_id: st.pass_id.clone().into(),
            pass_zip_url: st.pass_zip_url.clone().into(),
            pass_json_path: st.pass_json_path.clone().into(),
            sb_content: st.sb_content.clone().into(),
            sb_type: st.sb_type,
            sb_platform: st.sb_platform,
            sb_ai: st.sb_ai,
            sb_line: st.sb_line.clone().into(),
            sb_score: st.sb_score,
            sb_grade: st.sb_grade.clone().into(),
            sb_summary: st.sb_summary.clone().into(),
            sb_stats: st.sb_stats.clone().into(),
            sb_rewrite: st.sb_rewrite.clone().into(),
            sb_ai_line: st.sb_ai_line.clone().into(),
            sb_history_line: st.sb_history_line.clone().into(),
            sb_check_id: st.sb_check_id.clone().into(),
            plug_query: st.plug_query.clone().into(),
            plug_line: st.plug_line.clone().into(),
            plug_selected: st.plug_selected.clone().into(),
            plug_detail_line: st.plug_detail_line.clone().into(),
            plug_detail_desc: st.plug_detail_desc.clone().into(),
            plug_detail_meta: st.plug_detail_meta.clone().into(),
            plug_schema: st.plug_schema.clone().into(),
            plug_config_json: st.plug_config_json.clone().into(),
            plug_run_json: st.plug_run_json.clone().into(),
            plug_output: st.plug_output.clone().into(),
            plug_runs_line: st.plug_runs_line.clone().into(),
            plug_installed: st.plug_installed_flag,
            plug_enabled: st.plug_enabled,
            plug_runnable: st.plug_runnable,
            asset_industry: 0,
            asset_line: st.asset_line.clone().into(),
            tpl_industry: st.tpl_industry,
            tpl_scene: st.tpl_scene,
            tpl_line: st.tpl_line.clone().into(),
            tpl_detail_line: st.tpl_detail_line.clone().into(),
            tpl_detail_desc: st.tpl_detail_desc.clone().into(),
            tpl_store: st.tpl_store.clone().into(),
            tpl_product: st.tpl_product.clone().into(),
            tpl_points: st.tpl_points.clone().into(),
            tpl_copy: st.tpl_copy.clone().into(),
            tpl_duration: st.tpl_duration,
            tpl_quality: st.tpl_quality,
            tpl_gen_line: st.tpl_gen_line.clone().into(),
            tpl_gen_progress: st.tpl_gen_progress,
            tpl_run: if st.tpl_task.is_some() { 1 } else { 0 },
            tpl_video_url: st.tpl_video_url.clone().into(),
            tpl_download_url: st.tpl_download_url.clone().into(),
            tpl_fav: st.tpl_fav,
            tpl_selected: st.tpl_selected.clone().into(),
            task_line: st.task_line.clone().into(),
            task_count: st.tasks.len() as i32,
        };
        app.set_pro(data);

        // 编排器
        app.set_pro_text_shots(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.text_shots
                .iter()
                .map(|shot| ProShotRow {
                    no: shot.no,
                    label: shot.label.clone().into(),
                    scene: shot.scene.clone().into(),
                    line: shot.line.clone().into(),
                    dur: shot.dur.clone().into(),
                })
                .collect::<Vec<_>>(),
        ))));
        app.set_pro_plan_shots(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.plan_shots
                .iter()
                .map(|shot| ProPlanShot {
                    no: shot.no,
                    label: shot.label.clone().into(),
                    title: shot.title.clone().into(),
                    model: shot.model.clone().into(),
                    cost: shot.cost.clone().into(),
                    detail: shot.detail.clone().into(),
                    level: shot.level.clone().into(),
                })
                .collect::<Vec<_>>(),
        ))));
        app.set_pro_models(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.models
                .iter()
                .map(|model| ProModelRow {
                    group: model.group.clone().into(),
                    name: model.name.clone().into(),
                    vendor: model.vendor.clone().into(),
                    status: model.status.clone().into(),
                    detail: model.detail.clone().into(),
                    level: model.level.clone().into(),
                })
                .collect::<Vec<_>>(),
        ))));
        app.set_pro_tiers(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.tiers
                .iter()
                .map(|tier| ProTierRow {
                    key: tier.key.clone().into(),
                    label: tier.label.clone().into(),
                    desc: tier.desc.clone().into(),
                    per_shot: tier.per_shot.clone().into(),
                    ep120: tier.ep120.clone().into(),
                    ep180: tier.ep180.clone().into(),
                    effective: tier.effective.clone().into(),
                    downgrade: tier.downgrade.clone().into(),
                    hot: tier.hot,
                })
                .collect::<Vec<_>>(),
        ))));

        // 监视器
        app.set_pro_mon_types(rows_model(&st.mon_types));
        app.set_pro_mon_episodes(rows_model(&st.episodes));
        app.set_pro_board(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.board
                .iter()
                .map(|shot| ProBoardShot {
                    id: shot.id.clone().into(),
                    no: shot.no.clone().into(),
                    title: shot.title.clone().into(),
                    meta: shot.meta.clone().into(),
                    status: shot.status.clone().into(),
                    flags: shot.flags.clone().into(),
                    level: shot.level.clone().into(),
                })
                .collect::<Vec<_>>(),
        ))));
        app.set_pro_takes(rows_model(&st.takes));
        app.set_pro_notes(rows_model(&st.notes));
        app.set_pro_cont_rows(rows_model(&st.cont_rows));
        app.set_pro_retake_reasons(rows_model(&retake_reason_rows(&st.retake_reasons)));
        app.set_pro_retake_queue(rows_model(&st.retake_queue));
        app.set_pro_audit_rows(rows_model(&st.audit_rows));

        // 发行
        app.set_pro_dramas(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.dramas
                .iter()
                .enumerate()
                .map(|(index, drama)| ProRow {
                    label: text_of(drama, &["title"]).into(),
                    detail: format!(
                        "{} 集 · {} · {}",
                        num_of(drama, &["episodes_n", "episodes"]) as i64,
                        text_of(drama, &["status"]),
                        text_of(drama, &["updated_at", "created_at"])
                    )
                    .into(),
                    value: (index as i32).to_string().into(),
                    level: if index == st.drama_index.max(0) as usize {
                        "on"
                    } else {
                        ""
                    }
                    .into(),
                })
                .collect::<Vec<_>>(),
        ))));
        app.set_pro_dists(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.dists
                .iter()
                .enumerate()
                .map(|(index, dist)| ProRow {
                    label: text_of(dist, &["drama_title", "title"]).into(),
                    detail: format!(
                        "{} · {} · {} 个文件 · {}",
                        text_of(dist, &["platform"]),
                        text_of(dist, &["language", "lang"]),
                        num_of(dist, &["file_count"]) as i64,
                        text_of(dist, &["created_at"])
                    )
                    .into(),
                    value: text_of(dist, &["status"]).into(),
                    level: if index == st.dist_pick.max(0) as usize {
                        "on"
                    } else {
                        ""
                    }
                    .into(),
                })
                .collect::<Vec<_>>(),
        ))));
        app.set_pro_dist_files(rows_model(&st.dist_files));
        app.set_pro_dist_steps(rows_model(&st.dist_steps));
        app.set_pro_dist_drama_labels(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.dramas
                .iter()
                .map(|drama| SharedString::from(text_of(drama, &["title"])))
                .collect::<Vec<_>>(),
        ))));

        // 护照
        app.set_pro_pass_rows(rows_model(&st.pass_rows));
        app.set_pro_pass_verify_rows(rows_model(&st.pass_verify_rows));
        app.set_pro_pass_recent(rows_model(&st.pass_recent));

        // 沙盘
        app.set_pro_findings(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.sb_findings
                .iter()
                .map(|finding| ProFinding {
                    level: finding.level.clone().into(),
                    title: finding.title.clone().into(),
                    category: finding.category.clone().into(),
                    position: finding.position.clone().into(),
                    quote: finding.quote.clone().into(),
                    basis: finding.basis.clone().into(),
                    why: finding.why.clone().into(),
                    suggest: finding.suggest.clone().into(),
                })
                .collect::<Vec<_>>(),
        ))));
        app.set_pro_sb_platforms(rows_model(&st.sb_platforms));
        app.set_pro_sb_history(rows_model(&st.sb_history));

        // 插件
        app.set_pro_plugins(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.plug_market
                .iter()
                .map(|plugin| ProPluginRow {
                    id: text_of(plugin, &["market_id", "plugin_id", "id"]).into(),
                    name: text_of(plugin, &["name"]).into(),
                    meta: format!(
                        "{} · {} · 下载 {} · {}",
                        text_of(plugin, &["category"]),
                        text_of(plugin, &["type_label"]).if_empty("插件"),
                        num_of(plugin, &["downloads"]) as i64,
                        text_of(plugin, &["quality_label"]).if_empty("待认证"),
                    )
                    .into(),
                    description: text_of(plugin, &["description"]).into(),
                    quality: text_of(plugin, &["quality_label"]).if_empty("待认证").into(),
                    installed: plugin
                        .get("installed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    official: plugin
                        .get("official")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
                .collect::<Vec<_>>(),
        ))));
        app.set_pro_plugin_mine(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.plug_installed
                .iter()
                .map(|plugin| ProRow {
                    label: text_of(plugin, &["name"]).into(),
                    detail: format!(
                        "{} · v{} · {}",
                        text_of(plugin, &["type_label"]),
                        text_of(plugin, &["version"]),
                        text_of(plugin, &["author"])
                    )
                    .into(),
                    value: text_of(plugin, &["id"]).into(),
                    level: if text_of(plugin, &["id"]) == st.plug_selected {
                        "on"
                    } else {
                        ""
                    }
                    .into(),
                })
                .collect::<Vec<_>>(),
        ))));

        // 模板
        app.set_pro_templates(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.tpl_list
                .iter()
                .map(|template| ProTemplateRow {
                    id: text_of(template, &["id"]).into(),
                    name: text_of(template, &["name"]).into(),
                    meta: format!(
                        "{} · {} · {} · 用过 {} 次",
                        text_of(template, &["industry_name"]),
                        text_of(template, &["scene_label"]),
                        text_of(template, &["duration_label"]),
                        num_of(template, &["use_count"]) as i64
                    )
                    .into(),
                    detail: text_of(template, &["desc"]).into(),
                    level: if text_of(template, &["id"]) == st.tpl_selected {
                        "on"
                    } else {
                        ""
                    }
                    .into(),
                })
                .collect::<Vec<_>>(),
        ))));
        app.set_pro_tpl_industries(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.tpl_industries
                .iter()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        ))));
        app.set_pro_tpl_scenes(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.tpl_scenes
                .iter()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        ))));

        // 任务中心
        app.set_pro_tasks(ModelRc::from(std::rc::Rc::new(VecModel::from(
            st.tasks
                .iter()
                .map(|task| ProTaskRow {
                    id: task.id.clone().into(),
                    module: task.module.clone().into(),
                    title: task.title.clone().into(),
                    status: task.status.clone().into(),
                    progress: task.progress as f32,
                    cost: if task.cost > 0.0 {
                        money(task.cost)
                    } else {
                        "—".to_owned()
                    }
                    .into(),
                    detail: {
                        let mut detail = task.detail.clone();
                        if detail.is_empty() {
                            detail = task.url.clone();
                        }
                        detail.into()
                    },
                    level: if task.status == "失败" || task.status == "failed" {
                        "fail"
                    } else if task.finished() {
                        "ok"
                    } else {
                        "info"
                    }
                    .into(),
                    has_file: !task.file.is_empty(),
                    can_open: task.finished(),
                })
                .collect::<Vec<_>>(),
        ))));
    });
}

/// 空字符串换一个默认值。
trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}
impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.trim().is_empty() {
            fallback.to_owned()
        } else {
            self
        }
    }
}

fn retake_reason_rows(on: &[bool; 5]) -> Vec<Row> {
    const NAMES: [&str; 5] = ["画面质量", "连续性", "口型 / 台词", "时长节奏", "其他"];
    NAMES
        .iter()
        .enumerate()
        .map(|(index, name)| Row {
            label: (*name).to_owned(),
            detail: String::new(),
            value: String::new(),
            level: if on[index] { "on" } else { "" }.to_owned(),
        })
        .collect()
}

// ── 打开 / 关闭 / 定时 ───────────────────────────────────────────────

/// 从首页创作中心 / 菜单请求打开（在 Studio 的借还之外调用）。
pub fn request(tab: i32) {
    PRO.with(|cell| {
        let mut st = cell.borrow_mut();
        st.open = true;
        st.tab = tab.clamp(0, TAB_TASKS);
        st.epoch += 1;
        st.status_line.clear();
        st.status_level = "info".to_owned();
        st.hint = hint_of(st.tab);
    });
}

/// 把「请打开第 N 页」放进队列；窗口的消息处理在 Studio 借用放掉后开面板。
thread_local! {
    static WANTED: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

/// 请求打开第 `tab` 页（消息处理之后由 [`take_wanted`] 兑现）。
pub fn want(tab: i32) {
    WANTED.with(|slot| slot.set(Some(tab.clamp(0, TAB_TASKS))));
}

/// 窗口消息处理完之后取走「要开哪一页」。
pub fn take_wanted() -> Option<i32> {
    WANTED.with(|slot| slot.take())
}

/// 打开面板（在事件循环线程、Studio 借用之外调用）。
pub fn open(weak: slint::Weak<App>, tab: i32) {
    let signed = concat_host::pro::signed_in();
    with_pro(|st| st.signed_in = signed);
    request(tab);
    let tab = tab.clamp(0, TAB_TASKS);
    if tab == TAB_ASSETS {
        // 素材库页共用媒体面板的 AssetsPane：打开时让它取一次数。
        Shell::with(|shell, _| {
            shell
                .studio
                .borrow_mut()
                .handle(crate::panes::Msg::Assets(crate::panes::assets::AssetsMsg::Shown(
                    true,
                )));
        });
    }
    publish_now();
    // 按当前页取数（懒加载：启动路径上没有这些请求）。
    match tab {
        TAB_ORCH => load_models(weak.clone()),
        TAB_MONITOR => load_monitor_types(weak.clone()),
        TAB_DIST => load_distribution(weak.clone()),
        TAB_PASS => load_passport(weak.clone()),
        TAB_SANDBOX => load_sandbox(weak.clone()),
        TAB_PLUGIN => load_plugins(weak.clone()),
        TAB_TEMPLATE => load_templates(weak.clone()),
        TAB_TASKS => refresh_tasks(weak.clone(), true),
        _ => {}
    }
}

fn publish_now() {
    Shell::with(|_shell, app| publish(&app));
}

/// 启动时把一份空表写进 Slint：面板没开，也没有任何网络工作。
pub fn initialize(app: &App) {
    publish(app);
}

/// 关闭面板。
pub fn close(weak: slint::Weak<App>) {
    if let Some(app) = weak.upgrade() {
        with_pro(|st| {
            st.open = false;
            st.epoch += 1;
        });
        publish(&app);
    }
}

/// Timer：每秒一拍。轮询进行中的导演 / 模板任务，每 5 拍刷一次任务中心。
pub fn tick(weak: slint::Weak<App>) {
    let (run_task, tpl_task, count) = with_pro(|st| {
        if !st.open {
            return (None, None, 1);
        }
        st.ticks = st.ticks.wrapping_add(1);
        (st.run_task.clone(), st.tpl_task.clone(), st.ticks % 5)
    });
    if weak.upgrade().is_none() {
        return;
    }
    if let Some(task) = run_task {
        spawn_task_poll(weak.clone(), task, false);
    }
    if let Some(task) = tpl_task {
        spawn_task_poll(weak.clone(), task, true);
    }
    if count == 0 {
        refresh_tasks(weak, false);
    }
}

// ── 通用：设置状态行 ─────────────────────────────────────────────────

fn say(st: &mut ProState, line: impl Into<String>, level: &str) {
    st.status_line = line.into();
    st.status_level = level.to_owned();
}

// ── ① 导演编排器 ─────────────────────────────────────────────────────

fn apply_models(st: &mut ProState, value: &Value) {
    st.models.clear();
    if let Some(models) = value.get("models").and_then(Value::as_array) {
        for model in models {
            let capability = text_of(model, &["capability"]);
            let group = match capability.as_str() {
                "i2v" => "视频",
                "image_gen" => "生图",
                "tts" => "配音",
                "lipsync" => "口型",
                other => other,
            };
            let status = text_of(model, &["status"]);
            let level = match status.as_str() {
                "ready" => "ready",
                "configured" => "configured",
                "needs_activation" => "warn",
                _ => "fail",
            };
            let price = model
                .get("price")
                .map(|price| {
                    if let Some(object) = price.as_object() {
                        object
                            .iter()
                            .map(|(key, value)| format!("{key} ¥{}", value))
                            .collect::<Vec<_>>()
                            .join(" / ")
                    } else {
                        String::new()
                    }
                })
                .unwrap_or_default();
            st.models.push(ModelRow {
                group: group.to_owned(),
                name: text_of(model, &["label", "id"]),
                vendor: text_of(model, &["vendor_label", "vendor"]),
                status: match status.as_str() {
                    "ready" => "已实测".to_owned(),
                    "configured" => "已配置".to_owned(),
                    "needs_activation" => "待开通".to_owned(),
                    _ => "未配置".to_owned(),
                },
                detail: format!(
                    "{} · {} · {}",
                    text_of(model, &["api_model"]),
                    price,
                    text_of(model, &["hint_cn", "price_note"])
                ),
                level: level.to_owned(),
            });
        }
    }
    // 三档定价：优先 models 响应里的 tier_pricing，缺了就用 pricing 响应。
    let pricing = value
        .get("tier_pricing")
        .cloned()
        .or_else(|| value.get("pricing").cloned());
    apply_pricing(st, pricing.as_ref());
}

fn apply_pricing(st: &mut ProState, pricing: Option<&Value>) {
    st.tiers.clear();
    let Some(pricing) = pricing else { return };
    let tiers = match pricing.get("tiers").and_then(Value::as_object) {
        Some(tiers) => tiers,
        None => return,
    };
    for key in ["standard", "pro", "master"] {
        let Some(tier) = tiers.get(key) else { continue };
        let ep120 = tier.get("episode_120s").or_else(|| tier.get("target_episode_120s"));
        let ep180 = tier.get("episode_180s").or_else(|| tier.get("target_episode_180s"));
        let effective = tier.get("effective_models").cloned().unwrap_or(Value::Null);
        let down = tier
            .get("downgraded")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| text_of(row, &["model"]))
                    .filter(|name| !name.is_empty())
                    .collect::<Vec<_>>()
                    .join("、")
            })
            .unwrap_or_default();
        st.tiers.push(TierRow {
            key: key.to_owned(),
            label: text_of(tier, &["label"]).if_empty(key),
            desc: text_of(tier, &["desc"]),
            per_shot: format!("{:.2}", num_of(tier, &["per_shot_cny"])),
            ep120: ep120
                .map(|value| {
                    format!(
                        "{} / {} 积分",
                        money(num_of(value, &["cost_cny"])),
                        num_of(value, &["credits"]) as i64
                    )
                })
                .unwrap_or_default(),
            ep180: ep180
                .map(|value| {
                    format!(
                        "{} / {} 积分",
                        money(num_of(value, &["cost_cny"])),
                        num_of(value, &["credits"]) as i64
                    )
                })
                .unwrap_or_default(),
            effective: format!(
                "视频 {} · 生图 {} · 配音 {}{}",
                text_of(&effective, &["video"]),
                text_of(&effective, &["image"]),
                text_of(&effective, &["tts"]),
                {
                    let lip = text_of(&effective, &["lipsync"]);
                    if lip.is_empty() {
                        String::new()
                    } else {
                        format!(" · 口型 {lip}")
                    }
                }
            ),
            downgrade: down,
            hot: key == "standard",
        });
    }
}

fn load_models(weak: slint::Weak<App>) {
    with_pro(|st| st.busy = true);
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    host::spawn(
        || -> Result<(Value, Value), String> {
            let models = concat_host::pro::director_models()?;
            let pricing = concat_host::pro::director_pricing().unwrap_or(Value::Null);
            Ok((models, pricing))
        },
        |_studio, app, _models, result| {
            with_pro(|st| {
                st.busy = false;
                match result {
                    Ok((models, pricing)) => {
                        apply_models(st, &models);
                        if !pricing.is_null() {
                            apply_pricing(st, Some(&pricing));
                        }
                        let tiers = st.tiers.len();
                        let models = st.models.len();
                        let ready = st
                            .models
                            .iter()
                            .filter(|model| model.level == "ready")
                            .count();
                        st.plan_budget = format!(
                            "模型矩阵：{models} 个型号（已实测 {ready} 个）· 三档 {tiers} 档；1 积分 = ¥1，实际结算以积分账单为准。"
                        );
                        say(
                            st,
                            "导演编排器接口在线：模型矩阵与三档定价已取回（真实 /director/models + /pricing）。",
                            "ok",
                        );
                    }
                    Err(error) => {
                        st.plan_budget = "模型矩阵没取回来：可重试，或打开网页版看当前接入状态。".to_owned();
                        say(st, error, "warn");
                    }
                }
            });
            publish(app);
        },
    );
}

fn plan_body(st: &ProState) -> Value {
    let tier = ["standard", "pro", "master"][st.tier.clamp(0, 2) as usize];
    let budget = st.budget.trim().parse::<f64>().unwrap_or(10.0);
    let shots: Vec<Value> = st
        .text_shots
        .iter()
        .filter(|shot| !shot.scene.trim().is_empty())
        .map(|shot| {
            json!({
                "no": shot.no,
                "id": format!("S{}", shot.no),
                "scene": shot.scene.trim(),
                "desc": shot.scene.trim(),
                "line": shot.line.trim(),
                "dur": shot.dur.trim().parse::<f64>().unwrap_or(5.0),
            })
        })
        .collect();
    json!({
        "tier": tier,
        "budget_cny": budget,
        "aspect": "16:9",
        "style": "写实",
        "title": if st.title.trim().is_empty() { "AI 导演成片" } else { st.title.trim() },
        "script": {
            "title": if st.title.trim().is_empty() { "AI 导演成片" } else { st.title.trim() },
            "aspect": "16:9",
            "style": "写实",
            "shots": shots,
        }
    })
}

fn run_plan(weak: slint::Weak<App>) {
    let body = with_pro(|st| {
        st.busy = true;
        st.run_line.clear();
        st.film_url.clear();
        st.pack_url.clear();
        st.plan_shots.clear();
        st.plan = None;
        st.plan_line = "正在生成路由计划（不调用厂商、不产生费用）…".to_owned();
        plan_body(st)
    });
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    host::spawn(
        move || concat_host::pro::director_plan(&body),
        |_studio, app, _models, result| {
            with_pro(|st| {
                st.busy = false;
                match result {
                    Ok(plan) => {
                        let shots = plan
                            .get("shots")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        st.plan_shots = shots
                            .iter()
                            .map(|shot| {
                                let downgraded: String = shot
                                    .get("downgraded_from")
                                    .and_then(Value::as_array)
                                    .map(|rows| {
                                        rows.iter()
                                            .map(|row| {
                                                let from = text_of(row, &["model"]);
                                                let status = text_of(row, &["status"]);
                                                format!("{from}（{status}）")
                                            })
                                            .collect::<Vec<_>>()
                                            .join("、")
                                    })
                                    .unwrap_or_default();
                                let warnings = shot
                                    .get("warnings")
                                    .and_then(Value::as_array)
                                    .map(|rows| {
                                        rows.iter()
                                            .map(|row| row.as_str().unwrap_or_default().to_owned())
                                            .filter(|text| !text.is_empty())
                                            .collect::<Vec<_>>()
                                            .join("；")
                                    })
                                    .unwrap_or_default();
                                let detail = [
                                    text_of(shot, &["reason"]),
                                    if downgraded.is_empty() {
                                        String::new()
                                    } else {
                                        format!("降级：{downgraded}")
                                    },
                                    warnings,
                                ]
                                .into_iter()
                                .filter(|text| !text.is_empty())
                                .collect::<Vec<_>>()
                                .join("　");
                                let no = num_of(shot, &["no"]) as i32;
                                PlanShot {
                                    no,
                                    label: format!("S{no:02}"),
                                    title: text_of(shot, &["desc"]),
                                    model: format!(
                                        "{}@{}",
                                        text_of(shot, &["model"]),
                                        text_of(shot, &["resolution"])
                                    ),
                                    cost: money(num_of(shot, &["est_cost"])),
                                    detail,
                                    level: match text_of(shot, &["route_status"]).as_str() {
                                        "ready" => "ok",
                                        "configured" => "info",
                                        _ => "warn",
                                    }
                                    .to_owned(),
                                }
                            })
                            .collect();
                        st.plan = Some(plan.clone());
                        let shot_count = num_of(&plan, &["shot_count"]) as i64;
                        let duration = num_of(&plan, &["total_duration"]);
                        let cost = num_of(&plan, &["total_est_cost"]);
                        let budget = num_of(&plan, &["budget_cny"]);
                        st.budget_ok = plan
                            .get("budget_ok")
                            .and_then(Value::as_bool)
                            .unwrap_or(true);
                        st.plan_line = format!(
                            "计划完成：{shot_count} 镜 / {duration:.1}s，预估 {}（预算 ¥{budget:.0}）· 档位 {}",
                            money(cost),
                            text_of(&plan, &["tier_label"])
                        );
                        st.plan_budget = if st.budget_ok {
                            format!(
                                "预算检查通过：预估 {} ≤ 预算 ¥{budget:.0}。生成按逐镜路由调用模型。",
                                money(cost)
                            )
                        } else {
                            format!(
                                "预算超限：预估 {} 超过预算 ¥{budget:.0}。调高预算或降档后再生成（按钮已禁用）。",
                                money(cost)
                            )
                        };
                        let warnings = plan
                            .get("warnings")
                            .and_then(Value::as_array)
                            .map(|rows| rows.len())
                            .unwrap_or(0);
                        say(
                            st,
                            format!(
                                "路由计划已生成：{} 镜 / 预估 {} / 预算{}。{}",
                                shot_count,
                                money(cost),
                                if st.budget_ok { "通过" } else { "超限" },
                                if warnings > 0 {
                                    format!("{warnings} 条警告见每镜说明。")
                                } else {
                                    String::new()
                                }
                            ),
                            if st.budget_ok { "ok" } else { "warn" },
                        );
                        // 计划出来时把镜头数写进任务标题（还没提交，不记账）。
                    }
                    Err(error) => {
                        st.plan_line = "路由计划没生成出来。".to_owned();
                        st.plan_budget = "可重试；或打开网页版导演编排器查看平台当前接入状态。".to_owned();
                        say(st, error, "fail");
                    }
                }
            });
            publish(app);
        },
    );
}

fn run_director(weak: slint::Weak<App>, studio_config: PathBuf) {
    let Some(body) = with_pro(|st| {
        let Some(plan) = st.plan.clone() else {
            say(st, "先生成路由计划再开始真实生成。", "warn");
            return None;
        };
        let tier = plan
            .get("tier")
            .and_then(Value::as_str)
            .unwrap_or("standard")
            .to_owned();
        let budget = st.budget.trim().parse::<f64>().unwrap_or(10.0);
        st.busy = true;
        st.run_status = "正在提交导演任务…".to_owned();
        Some(json!({
            "plan": plan,
            "tier": tier,
            "budget_cny": budget,
            "batch_size": 6,
            "transition": "none",
        }))
    }) else {
        publish_now();
        return;
    };
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    let shots = with_pro(|st| st.plan_shots.len() as i64);
    host::spawn(
        move || concat_host::pro::director_run(&body),
        {
            let config = studio_config.clone();
            move |_studio, app, _models, result| {
                let mut record: Option<concat_host::pro::LocalTask> = None;
                with_pro(|st| {
                    st.busy = false;
                    match result {
                        Ok(task) => {
                            let task_id = text_of(&task, &["task_id"]);
                            st.run_task = if task_id.is_empty() {
                                None
                            } else {
                                Some(task_id.clone())
                            };
                            st.run_status = format!(
                                "任务 {} 已进入后台（{}）。平台 2C2G 串行，逐镜进度会自动刷新。",
                                task_id,
                                text_of(&task, &["status"])
                            );
                            st.run_progress = num_of(&task, &["progress"]) as f32;
                            st.run_line = "已提交导演编排任务，逐镜生成中（真实调用模型，按实际用量结算）。".to_owned();
                            say(
                                st,
                                format!("导演任务已提交：{task_id}（{} 镜，预算 {}）", shots, money(num_of(&task, &["budget_cny"]))),
                                "ok",
                            );
                            record = Some(concat_host::pro::LocalTask {
                                id: format!("director-{task_id}"),
                                module: "导演编排器".to_owned(),
                                title: format!("真实生成 {} 镜", shots),
                                kind: "director".to_owned(),
                                remote: task_id,
                                status: "生成中".to_owned(),
                                progress: 0.0,
                                cost: 0.0,
                                detail: "逐镜生成中".to_owned(),
                                file: String::new(),
                                url: String::new(),
                                created: now_secs(),
                                updated: now_secs(),
                            });
                        }
                        Err(error) => {
                            st.run_status = "提交失败。".to_owned();
                            st.run_line.clear();
                            say(st, error, "fail");
                        }
                    }
                });
                if let Some(task) = record {
                    push_task(&config, task, app.as_weak());
                }
                publish(app);
            }
        },
    );
}

fn task_apply(st: &mut ProState, task_id: &str, task: &Value) {
    let _ = task_id;
    st.run_progress = num_of(task, &["progress"]) as f32;
    st.run_status = format!(
        "{} · 已完成 {} / {} 镜 · 成本 {}",
        text_of(task, &["stage_label", "stage"]).if_empty("生成中"),
        num_of(task, &["shots_ok"]) as i64,
        num_of(task, &["total_shots"]) as i64,
        money(num_of(task, &["actual_cost_cny"]))
    );
    st.film_url = text_of(task, &["video_url"]);
    st.pack_url = text_of(task, &["pack_url"]);
    let status = text_of(task, &["status"]);
    if st.film_url.is_empty() && st.pack_url.is_empty() && status == "done" {
        st.run_line = "任务完成：平台没有返回成片地址（可能只产出工程包）。".to_owned();
    } else if !st.film_url.is_empty() {
        st.run_line = format!(
            "成片已就绪：{} · 工程包：{}",
            st.film_url,
            if st.pack_url.is_empty() { "无" } else { "有" }
        );
    }
    if matches!(status.as_str(), "done" | "failed" | "partial" | "aborted_budget" | "cancelled") {
        st.run_task = None;
        if status != "done" && status != "partial" {
            if st.run_line.is_empty() {
                st.run_line = format!("任务{}：{}", if status == "cancelled" { "已取消" } else { "失败" }, text_of(task, &["hint", "error"]));
            }
        }
    }
}

fn spawn_task_poll(weak: slint::Weak<App>, task_id: String, template: bool) {
    let config = config_dir();
    let poll_id = task_id.clone();
    host::spawn(
        move || -> Result<Value, String> {
            if template {
                concat_host::pro::template_task(&poll_id)
            } else {
                concat_host::pro::director_task(&poll_id)
            }
        },
        move |_studio, app, _models, result: Result<Value, String>| {
            let remote = match &result {
                Ok(task) => Some(task.clone()),
                Err(error) => {
                    with_pro(|st| say(st, format!("任务轮询失败：{error}"), "warn"));
                    None
                }
            };
            if let Some(task) = remote {
                with_pro(|st| {
                    if template {
                        st.tpl_gen_progress = num_of(&task, &["progress"]) as f32;
                        st.tpl_gen_line = format!(
                            "{} · {}% · {}",
                            text_of(&task, &["stage"]).if_empty(&text_of(&task, &["status"])),
                            num_of(&task, &["progress"]) as i64,
                            text_of(&task, &["hint", "error"])
                        );
                        st.tpl_video_url = text_of(&task, &["video_url", "download_url", "url"]);
                        st.tpl_download_url =
                            text_of(&task, &["download_url", "video_url", "url"]);
                        let status = text_of(&task, &["status"]);
                        if matches!(status.as_str(), "done" | "failed") {
                            st.tpl_task = None;
                        }
                    } else {
                        task_apply(st, &task_id, &task);
                    }
                });
                update_local_task(&config, &task_id, &task, template);
            }
            publish(app);
        },
    );
}

// ── 本机任务表 ───────────────────────────────────────────────────────

fn config_dir() -> PathBuf {
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

/// 记一条任务（写回 `pro-tasks.json`），并刷新任务页。
fn push_task(config: &Path, mut task: concat_host::pro::LocalTask, ui: slint::Weak<App>) {
    let mut tasks = concat_host::pro::load_tasks(config);
    task.updated = now_secs();
    if task.created == 0 {
        task.created = task.updated;
    }
    tasks.retain(|row| row.id != task.id);
    tasks.push(task);
    tasks.sort_by(|a, b| b.updated.cmp(&a.updated));
    tasks.truncate(200);
    if let Err(error) = concat_host::pro::save_tasks(config, &tasks) {
        log::warn!("pro: 任务记录写不下：{error}");
    }
    with_pro(|st| st.tasks = tasks);
    if let Some(app) = ui.upgrade() {
        publish(&app);
    }
}

/// 轮询回来的任务状态写回本机记录。
fn update_local_task(config: &Path, remote_id: &str, task: &Value, template: bool) {
    let mut tasks = concat_host::pro::load_tasks(config);
    let mut changed = false;
    for row in tasks.iter_mut() {
        if row.remote != remote_id {
            continue;
        }
        let status = text_of(task, &["status"]);
        row.progress = num_of(task, &["progress"]).clamp(0.0, 100.0);
        row.status = if template {
            concat_host::pro::status_text(&status, &text_of(task, &["stage"]))
        } else {
            concat_host::pro::status_text(&status, &text_of(task, &["stage_label", "stage"]))
        };
        row.cost = num_of(task, &["actual_cost_cny", "cost_cny"]);
        let video = text_of(task, &["video_url", "download_url"]);
        if !video.is_empty() {
            row.url = video;
        }
        let detail = text_of(task, &["hint", "error"]);
        if !detail.is_empty() {
            row.detail = detail;
        }
        row.updated = now_secs();
        changed = true;
        break;
    }
    if changed {
        let _ = concat_host::pro::save_tasks(config, &tasks);
        with_pro(|st| st.tasks = tasks);
    }
}

/// 刷新任务中心：本机记录 + 远端任务表（导演 / 电影）。
fn refresh_tasks(weak: slint::Weak<App>, announce: bool) {
    let config = config_dir();
    let mut local = concat_host::pro::load_tasks(&config);
    let Some(app) = weak.upgrade() else { return };
    if announce {
        with_pro(|st| {
            st.task_line = "正在合并本机记录与远端任务表（/director/tasks + /movie/tasks）…".to_owned();
        });
        publish(&app);
    }
    host::spawn(
        move || {
            let director = concat_host::pro::director_tasks(20).unwrap_or(Value::Null);
            let movie = concat_host::pro::movie_tasks(20).unwrap_or(Value::Null);
            (director, movie)
        },
        move |_studio, app, _models, (director, movie)| {
            let mut remote: Vec<concat_host::pro::LocalTask> = Vec::new();
            if let Some(tasks) = director.get("tasks").and_then(Value::as_array) {
                for task in tasks {
                    let id = text_of(task, &["task_id"]);
                    if id.is_empty() {
                        continue;
                    }
                    remote.push(concat_host::pro::LocalTask {
                        id: format!("remote-director-{id}"),
                        module: "导演编排器".to_owned(),
                        title: format!(
                            "{} · {} 镜",
                            text_of(task, &["tier_label"]).if_empty("导演任务"),
                            num_of(task, &["total_shots"]) as i64
                        ),
                        kind: "director".to_owned(),
                        remote: id,
                        status: concat_host::pro::status_text(&text_of(task, &["status"]), ""),
                        progress: num_of(task, &["progress"]),
                        cost: num_of(task, &["actual_cost_cny"]),
                        detail: text_of(task, &["created_at"]),
                        file: String::new(),
                        url: text_of(task, &["video_url"]),
                        created: 0,
                        updated: now_secs(),
                    });
                }
            }
            if let Some(tasks) = movie.get("tasks").and_then(Value::as_array) {
                for task in tasks {
                    let id = text_of(task, &["task_id"]);
                    if id.is_empty() {
                        continue;
                    }
                    remote.push(concat_host::pro::LocalTask {
                        id: format!("remote-movie-{id}"),
                        module: "电影成片管线".to_owned(),
                        title: format!(
                            "{} · {}",
                            text_of(task, &["type"]).if_empty("电影任务"),
                            text_of(task, &["stage"])
                        ),
                        kind: "none".to_owned(),
                        remote: id,
                        status: concat_host::pro::status_text(
                            &text_of(task, &["status"]),
                            &text_of(task, &["stage"]),
                        ),
                        progress: num_of(task, &["progress"]),
                        cost: num_of(task, &["cost_cny"]),
                        detail: format!(
                            "预算 {} · {}",
                            money(num_of(task, &["budget_cny"])),
                            text_of(task, &["created_at"])
                        ),
                        file: String::new(),
                        url: String::new(),
                        created: 0,
                        updated: now_secs(),
                    });
                }
            }
            local.retain(|row| row.kind != "director" && row.kind != "template" || !row.finished());
            for task in remote {
                if !local.iter().any(|row| row.remote == task.remote) {
                    local.push(task);
                }
            }
            local.sort_by(|a, b| b.updated.cmp(&a.updated));
            let count = local.len();
            with_pro(|st| {
                st.tasks = local;
                st.tasks_loaded = true;
                st.task_line = format!(
                    "共 {count} 条：本机记录（生成 / 下载 / 检查）+ 平台最近任务（导演 / 电影）；每 5 秒刷新一次远端状态。"
                );
                if announce {
                    say(st, format!("任务中心已取回 {count} 条任务。"), "ok");
                }
            });
            publish(&app);
        },
    );
}

// ── ② 导演监视器 ─────────────────────────────────────────────────────

fn load_monitor_types(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    host::spawn(
        || concat_host::pro::monitor_types(),
        |_studio, app, _models, result| {
            with_pro(|st| {
                match result {
                    Ok(value) => {
                        let rows = value
                            .get("types")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        st.mon_types.clear();
                        st.type_ids.clear();
                        for row in &rows {
                            let id = text_of(row, &["mode_id", "id"]);
                            st.type_ids.push(id);
                            st.mon_types.push(Row {
                                label: text_of(row, &["name"]),
                                detail: text_of(row, &["category"]),
                                value: String::new(),
                                level: "".to_owned(),
                            });
                        }
                        st.type_line = format!(
                            "类型表：{} 类影视 / 短剧模板（{}）。",
                            rows.len(),
                            rows.iter()
                                .map(|row| text_of(row, &["name"]))
                                .filter(|name| !name.is_empty())
                                .collect::<Vec<_>>()
                                .join(" / ")
                        );
                        say(
                            st,
                            format!("导演监视器接口在线：类型表 {} 类已取回（真实 /filmgen/monitor/types）。", rows.len()),
                            "ok",
                        );
                    }
                    Err(error) => {
                        say(st, error, "fail");
                    }
                }
            });
            publish(app);
        },
    );
}

fn load_type_detail(weak: slint::Weak<App>, index: usize) {
    let id = with_pro(|st| {
        st.type_index = index as i32;
        st.type_ids.get(index).cloned().unwrap_or_default()
    });
    if id.is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    host::spawn(
        move || concat_host::pro::monitor_type(&id),
        |_studio, app, _models, result| {
            with_pro(|st| match result {
                Ok(value) => {
                    let kind = value.get("type").cloned().unwrap_or(Value::Null);
                    let steps = kind
                        .get("steps")
                        .and_then(Value::as_array)
                        .map(|rows| {
                            rows.iter()
                                .map(|row| {
                                    format!(
                                        "{}. {}（{}）",
                                        num_of(row, &["step", "no"]) as i64,
                                        text_of(row, &["name", "title"]),
                                        text_of(row, &["role", "owner"])
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("　")
                        })
                        .unwrap_or_default();
                    st.type_detail = steps.clone();
                    st.type_line = format!(
                        "{} · {} · 默认画幅 {} / {}fps · 每集 {}–{} 镜 · 每镜 {} take。{}",
                        text_of(&kind, &["name"]),
                        text_of(&kind, &["summary"]),
                        text_of(&kind, &["aspect_default"]),
                        num_of(&kind, &["fps_default"]) as i64,
                        num_of(&kind, &["shots_per_episode"]),
                        num_of(&kind, &["shots_per_episode"]),
                        num_of(&kind, &["takes_per_shot"]) as i64,
                        if steps.is_empty() {
                            String::new()
                        } else {
                            format!("\n15 步：{steps}")
                        }
                    );
                }
                Err(error) => say(st, error, "warn"),
            });
            publish(app);
        },
    );
}

fn write_board(st: &mut ProState, value: &Value) {
    let project = value.get("project").cloned().unwrap_or(Value::Null);
    let progress = value.get("progress").cloned().unwrap_or(Value::Null);
    st.board_line = format!(
        "{} · {} · {} · {} 镜{}",
        text_of(&project, &["title", "project_id"]),
        text_of(&project, &["type_name", "type_profile_id"]),
        text_of(&project, &["duration_tier"]),
        num_of(value, &["shots_total"]) as i64,
        {
            let ep = num_of(&project, &["episode_count"]) as i64;
            if ep > 0 {
                format!(" · {ep} 集")
            } else {
                String::new()
            }
        }
    );
    st.board_progress = num_of(&progress, &["pct"]) as f32;
    st.counts = format!(
        "已通过 {} / {} · 待审 {} · 重拍 {} · 未生成 {} · 运行中 {}",
        num_of(&progress, &["approved"]) as i64,
        num_of(&progress, &["total"]) as i64,
        num_of(
            value.get("counts").unwrap_or(&Value::Null),
            &["pending_review"]
        ) as i64,
        num_of(value.get("counts").unwrap_or(&Value::Null), &["reshoot"]) as i64,
        num_of(
            value.get("counts").unwrap_or(&Value::Null),
            &["not_generated"]
        ) as i64,
        num_of(value.get("counts").unwrap_or(&Value::Null), &["generating"]) as i64,
    );
    st.budget_line = format!(
        "预算 {} / {}（{}）",
        money(num_of(&progress, &["spent_cny"])),
        money(num_of(&progress, &["budget_cap_cny"])),
        pct(num_of(&progress, &["budget_pct"]))
    );
    st.board.clear();
    st.board_ids.clear();
    if let Some(shots) = value.get("shots").and_then(Value::as_array) {
        for shot in shots {
            let id = text_of(shot, &["shot_id"]);
            st.board_ids.push(id.clone());
            let status = text_of(shot, &["status"]);
            let level = match status.as_str() {
                "approved" | "locked" => "ok",
                "rejected" | "reshoot" => "fail",
                "pending_review" | "generating" => "warn",
                _ => "",
            };
            let continuity = shot.get("continuity").cloned().unwrap_or(Value::Null);
            let flags = [
                shot.get("note_count")
                    .and_then(Value::as_i64)
                    .filter(|count| *count > 0)
                    .map(|count| format!("批注 {count}")),
                num_of(&continuity, &["fail"])
                    .gt(&0.0)
                    .then(|| "连续性 fail".to_owned()),
                num_of(&continuity, &["warn"])
                    .gt(&0.0)
                    .then(|| "连续性 warn".to_owned()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            st.board.push(BoardShot {
                id,
                no: format!("S{:02}", num_of(shot, &["shot_no", "no"]) as i64),
                title: text_of(shot, &["description", "title"]),
                meta: format!(
                    "{} · {} · take {} · {}s",
                    text_of(shot, &["coverage_grade"]),
                    text_of(shot, &["shot_type"]),
                    num_of(shot, &["take_count"]) as i64,
                    num_of(shot, &["duration_ms"]) / 1000.0
                ),
                status: text_of(shot, &["status_label"]).if_empty(&status),
                flags,
                level: level.to_owned(),
            });
        }
    }
    st.episodes.clear();
    if let Some(episodes) = value.get("episodes").and_then(Value::as_array) {
        for episode in episodes {
            st.episodes.push(Row {
                label: format!("第 {} 集", num_of(episode, &["episode_no"]) as i64),
                detail: pct(num_of(episode, &["pct"])),
                value: String::new(),
                level: if num_of(episode, &["pct"]) >= 100.0 {
                    "on"
                } else {
                    ""
                }
                .to_owned(),
            });
        }
    }
}

fn load_board(weak: slint::Weak<App>) {
    let (project, episode) = with_pro(|st| {
        st.busy = true;
        st.board_line = "正在取镜头状态板…".to_owned();
        (st.project.clone(), st.episode.clone())
    });
    if project.trim().is_empty() {
        with_pro(|st| {
            st.busy = false;
            say(st, "先在项目框里填 project_id，再点「载入状态板」。", "warn");
        });
        publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    host::spawn(
        move || concat_host::pro::monitor_board(&project, &episode, 1),
        |_studio, app, _models, result| {
            with_pro(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        write_board(st, &value);
                        say(
                            st,
                            format!(
                                "状态板已载入：{} 镜（真实 /filmgen/monitor/board）。",
                                st.board.len()
                            ),
                            "ok",
                        );
                    }
                    Err(error) => {
                        st.board.clear();
                        st.board_ids.clear();
                        st.board_line = "状态板没载入。".to_owned();
                        say(
                            st,
                            format!("{error}（先确认项目已导入监视器；网页版可导入镜头）"),
                            "fail",
                        );
                    }
                }
            });
            publish(app);
        },
    );
}

fn load_shot(weak: slint::Weak<App>, index: usize) {
    let id = with_pro(|st| st.board_ids.get(index).cloned().unwrap_or_default());
    if id.is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    let work_id = id.clone();
    host::spawn(
        move || concat_host::pro::monitor_shot(&work_id),
        move |_studio, app, _models, result| {
            with_pro(|st| match result {
                Ok(value) => {
                    let shot = value.get("shot").cloned().unwrap_or(Value::Null);
                    st.shot_title = format!(
                        "{} · {}",
                        text_of(&shot, &["shot_id"]),
                        text_of(&shot, &["description", "title"])
                    );
                    st.shot_meta = format!(
                        "第 {} 集 · {} 镜 · 景别 {} · 角色 {} · take {} · {}",
                        num_of(&shot, &["episode_no"]) as i64,
                        num_of(&shot, &["shot_no"]) as i64,
                        text_of(&shot, &["shot_type"]),
                        shot.get("characters")
                            .and_then(Value::as_array)
                            .map(|rows| {
                                rows.iter()
                                    .map(|row| {
                                        if let Some(name) = row.as_str() {
                                            name.to_owned()
                                        } else {
                                            text_of(row, &["name", "role"])
                                        }
                                    })
                                    .filter(|name| !name.is_empty())
                                    .collect::<Vec<_>>()
                                    .join(" / ")
                            })
                            .unwrap_or_default(),
                        num_of(&shot, &["take_count"]) as i64,
                        text_of(&shot, &["status_label"]),
                    );
                    st.takes.clear();
                    st.take_ids.clear();
                    if let Some(takes) = shot.get("takes").and_then(Value::as_array) {
                        for take in takes {
                            let take_id = text_of(take, &["take_id"]);
                            st.take_ids.push(take_id.clone());
                            let status = text_of(take, &["status"]);
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
                                    "{} · {}s · {} · {}",
                                    text_of(take, &["model"]),
                                    num_of(take, &["duration_ms"]) / 1000.0,
                                    money(num_of(take, &["cost_cny"])),
                                    take_id
                                ),
                                value: String::new(),
                                level: if status == "accepted" { "on" } else { "" }.to_owned(),
                            });
                        }
                    }
                    st.notes.clear();
                    st.note_ids.clear();
                    if let Some(notes) = shot.get("notes").and_then(Value::as_array) {
                        for note in notes {
                            st.note_ids.push(text_of(note, &["note_id"]));
                            st.notes.push(Row {
                                label: format!(
                                    "{} {}",
                                    text_of(note, &["severity"]),
                                    if num_of(note, &["t_ms"]) > 0.0 {
                                        format!("@{}s", num_of(note, &["t_ms"]) / 1000.0)
                                    } else {
                                        String::new()
                                    }
                                ),
                                detail: format!(
                                    "{}（{}）",
                                    text_of(note, &["content"]),
                                    text_of(note, &["created_at"])
                                ),
                                value: String::new(),
                                level: if note.get("resolved").and_then(Value::as_bool) == Some(true) {
                                    "on"
                                } else {
                                    ""
                                }
                                .to_owned(),
                            });
                        }
                    }
                    st.shot_line = id.clone();
                    st.shot = Some(shot);
                    st.take_line = format!(
                        "当前镜头 {}：{} 条 take / {} 条批注。接受 / 退回 / 待定都写平台审计日志。",
                        id,
                        st.takes.len(),
                        st.notes.len()
                    );
                }
                Err(error) => say(st, error, "fail"),
            });
            publish(app);
        },
    );
}

fn shot_action(weak: slint::Weak<App>, what: &str, index: i32, text: &str) {
    let config = config_dir();
    let shot_id = with_pro(|st| st.shot_line.clone());
    if shot_id.is_empty() && what != "retake-reason" {
        with_pro(|st| say(st, "先在状态板上点一镜，再对这个镜头操作。", "warn"));
        publish_now();
        return;
    }
    match what {
        "take-url" => {
            with_pro(|st| st.take_url = text.to_owned());
            return;
        }
        "note-text" => {
            with_pro(|st| st.note_text = text.to_owned());
            return;
        }
        "retake-prompt" => {
            with_pro(|st| st.retake_prompt = text.to_owned());
            return;
        }
        "retake-reason" => {
            with_pro(|st| {
                let slot = index.clamp(0, 4) as usize;
                st.retake_reasons[slot] = !st.retake_reasons[slot];
            });
            publish_now();
            return;
        }
        _ => {}
    }
    let take_id = with_pro(|st| st.take_ids.get(index.max(0) as usize).cloned().unwrap_or_default());
    let note_id = with_pro(|st| st.note_ids.get(index.max(0) as usize).cloned().unwrap_or_default());
    let Some(app) = weak.upgrade() else { return };
    let shot = shot_id.clone();
    match what {
        "take-add" => {
            let url = with_pro(|st| st.take_url.trim().to_owned());
            if url.is_empty() {
                with_pro(|st| say(st, "先填 take 的视频地址（video_url）。", "warn"));
                publish_now();
                return;
            }
            with_pro(|st| {
                st.busy = true;
                st.take_line = "正在登记 take…".to_owned();
            });
            publish(&app);
            host::spawn(
                move || {
                    concat_host::pro::monitor_take(
                        &shot,
                        &json!({ "video_url": url, "model": "手动登记" }),
                    )
                },
                |_studio, app, _m, result| {
                    with_pro(|st| {
                        st.busy = false;
                        match result {
                            Ok(_) => {
                                st.take_line = "take 已登记，列表刷新中…".to_owned();
                                say(st, "take 已登记（真实 /take）。", "ok");
                            }
                            Err(error) => say(st, error, "fail"),
                        }
                    });
                    publish(app);
                },
            );
        }
        "take-decision" => {
            let decision = text.to_owned();
            with_pro(|st| {
                st.busy = true;
                st.take_line = format!("正在{}…", if decision == "accept" { "接受" } else if decision == "reject" { "退回" } else { "标记待定" });
            });
            publish(&app);
            host::spawn(
                move || concat_host::pro::monitor_decision(&shot, &take_id, &decision),
                |_studio, app, _m, result| {
                    with_pro(|st| {
                        st.busy = false;
                        match result {
                            Ok(_) => {
                                st.take_line = "take 决定已写入平台审计日志。".to_owned();
                                say(st, "take 状态已更新。", "ok");
                            }
                            Err(error) => say(st, error, "fail"),
                        }
                    });
                    publish(app);
                },
            );
        }
        "note-add" => {
            let content = with_pro(|st| st.note_text.trim().to_owned());
            if content.is_empty() {
                with_pro(|st| say(st, "先写批注内容。", "warn"));
                publish_now();
                return;
            }
            with_pro(|st| {
                st.busy = true;
                st.note_line = "正在加批注…".to_owned();
            });
            publish(&app);
            let shot = shot_id.clone();
            host::spawn(
                move || {
                    concat_host::pro::monitor_note(
                        &shot,
                        &json!({ "content": content, "kind": "text", "severity": "medium" }),
                    )
                },
                |_studio, app, _m, result| {
                    with_pro(|st| {
                        st.busy = false;
                        match result {
                            Ok(_) => {
                                st.note_text.clear();
                                st.note_line = "批注已加。".to_owned();
                                say(st, "批注已写入这一镜。", "ok");
                            }
                            Err(error) => say(st, error, "fail"),
                        }
                    });
                    publish(app);
                },
            );
        }
        "note-resolve" => {
            let note = note_id;
            with_pro(|st| st.note_line = "正在标记已解决…".to_owned());
            publish(&app);
            host::spawn(
                move || concat_host::pro::monitor_note_resolve(&note),
                |_studio, app, _m, result| {
                    with_pro(|st| match result {
                        Ok(_) => st.note_line = "批注已解决。".to_owned(),
                        Err(error) => say(st, error, "fail"),
                    });
                    publish(app);
                },
            );
        }
        "retake" => {
            let (reasons, prompt, tier) = with_pro(|st| {
                const NAMES: [&str; 5] = ["画面质量", "连续性", "口型 / 台词", "时长节奏", "其他"];
                let reasons: Vec<&str> = NAMES
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| st.retake_reasons[*index])
                    .map(|(_, name)| *name)
                    .collect();
                (
                    reasons,
                    st.retake_prompt.trim().to_owned(),
                    ["standard", "pro", "master"][st.retake_tier.clamp(0, 2) as usize],
                )
            });
            if reasons.is_empty() {
                with_pro(|st| say(st, "至少勾一个重拍原因。", "warn"));
                publish_now();
                return;
            }
            let tiers = tier.to_owned();
            with_pro(|st| {
                st.busy = true;
                st.retake_line = "正在登记重拍请求…".to_owned();
            });
            publish(&app);
            let shot = shot_id.clone();
            host::spawn(
                move || {
                    concat_host::pro::monitor_retake(
                        &shot,
                        &json!({ "reason": reasons, "extra_prompt": prompt, "tier": tiers }),
                    )
                },
                {
                    let config = config.clone();
                    move |_studio, app, _m, result| {
                        let mut record = None;
                        with_pro(|st| {
                            st.busy = false;
                            match result {
                                Ok(value) => {
                                    let tier = text_of(&value, &["target_tier"]);
                                    let model = text_of(&value, &["suggested_model"]);
                                    let cost = num_of(&value, &["est_cost_cny"]);
                                    st.retake_line = format!(
                                        "已登记重拍：目标档位 {} · 模型 {} · 预计 {}（由导演编排器消费，不自动触发真实生成）。",
                                        tier.if_empty("默认"),
                                        model.clone().if_empty("未指定"),
                                        money(cost)
                                    );
                                    say(st, "重拍请求已登记到平台队列。", "ok");
                                    record = Some(concat_host::pro::LocalTask {
                                        id: format!(
                                            "retake-{}-{}",
                                            st.shot_line,
                                            now_secs()
                                        ),
                                        module: "导演监视器".to_owned(),
                                        title: format!("一键重拍 {}", st.shot_line),
                                        kind: "none".to_owned(),
                                        remote: String::new(),
                                        status: "已登记".to_owned(),
                                        progress: 0.0,
                                        cost,
                                        detail: model,
                                        file: String::new(),
                                        url: String::new(),
                                        created: now_secs(),
                                        updated: now_secs(),
                                    });
                                }
                                Err(error) => {
                                    st.retake_line = "重拍登记失败。".to_owned();
                                    say(st, error, "fail");
                                }
                            }
                        });
                        if let Some(task) = record {
                            push_task(&config, task, app.as_weak());
                        }
                        publish(app);
                    }
                },
            );
        }
        _ => {}
    }
}

fn load_continuity(weak: slint::Weak<App>, run: bool) {
    let project = with_pro(|st| st.project.clone());
    let episode = with_pro(|st| st.episode.clone());
    if project.trim().is_empty() {
        with_pro(|st| say(st, "先填 project_id。", "warn"));
        publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    with_pro(|st| {
        st.busy = true;
        st.cont_line = if run {
            "正在跑连续性检查（平台逐镜比对）…".to_owned()
        } else {
            "正在取连续性结果…".to_owned()
        };
    });
    publish(&app);
    host::spawn(
        move || concat_host::pro::monitor_continuity(&project, run, &episode),
        |_studio, app, _m, result| {
            with_pro(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let summary = value.get("summary").cloned().unwrap_or(Value::Null);
                        st.cont_line = format!(
                            "连续性：通过 {} / 注意 {} / 不通过 {} · 共 {} 项{}",
                            num_of(&summary, &["pass"]) as i64,
                            num_of(&summary, &["warn"]) as i64,
                            num_of(&summary, &["fail"]) as i64,
                            num_of(&value, &["total"]) as i64,
                            if value.get("cached").and_then(Value::as_bool) == Some(true) {
                                " · 缓存结果"
                            } else {
                                ""
                            }
                        );
                        st.cont_rows.clear();
                        if let Some(groups) = value.get("groups").and_then(Value::as_object) {
                            for (kind, items) in groups {
                                if let Some(items) = items.as_array() {
                                    for item in items.iter().take(6) {
                                        st.cont_rows.push(Row {
                                            label: kind.clone(),
                                            detail: text_of(item, &["note", "check_type"]),
                                            value: text_of(item, &["status"]),
                                            level: match text_of(item, &["status"]).as_str() {
                                                "pass" => "on",
                                                "warn" => "warn",
                                                "fail" => "fail",
                                                _ => "",
                                            }
                                            .to_owned(),
                                        });
                                    }
                                }
                            }
                        }
                        say(st, "连续性检查完成。", "ok");
                    }
                    Err(error) => say(st, error, "fail"),
                }
            });
            publish(app);
        },
    );
}

fn load_retakes(weak: slint::Weak<App>) {
    let project = with_pro(|st| st.project.clone());
    if project.trim().is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    host::spawn(
        move || concat_host::pro::monitor_retakes(&project),
        |_studio, app, _m, result| {
            with_pro(|st| match result {
                Ok(value) => {
                    st.retake_queue.clear();
                    if let Some(rows) = value.get("retakes").and_then(Value::as_array) {
                        for row in rows {
                            st.retake_queue.push(Row {
                                label: text_of(row, &["shot_id"]),
                                detail: format!(
                                    "{} · {} · {}",
                                    row.get("reason")
                                        .and_then(Value::as_array)
                                        .map(|rows| rows
                                            .iter()
                                            .filter_map(Value::as_str)
                                            .collect::<Vec<_>>()
                                            .join(" / "))
                                        .unwrap_or_default(),
                                    text_of(row, &["target_tier"]),
                                    text_of(row, &["suggested_model"]),
                                ),
                                value: money(num_of(row, &["est_cost_cny"])),
                                level: "warn".to_owned(),
                            });
                        }
                    }
                    st.queue_line = format!(
                        "重拍队列 {} 条（{}）",
                        st.retake_queue.len(),
                        text_of(&value, &["queue_note"])
                    );
                    say(st, "重拍队列已刷新。", "ok");
                }
                Err(error) => say(st, error, "warn"),
            });
            publish(app);
        },
    );
}

fn load_monitor_progress(weak: slint::Weak<App>) {
    let project = with_pro(|st| st.project.clone());
    if project.trim().is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    host::spawn(
        move || concat_host::pro::monitor_progress(&project),
        |_studio, app, _m, result| {
            with_pro(|st| match result {
                Ok(value) => {
                    let totals = value.get("totals").cloned().unwrap_or(Value::Null);
                    let budget = value.get("budget").cloned().unwrap_or(Value::Null);
                    st.budget_line = format!(
                        "预算 {} / {}（{}） · 已通过 {} 镜",
                        money(num_of(&budget, &["spent_cny"])),
                        money(num_of(&budget, &["cap_cny", "budget_cap_cny"])),
                        pct(num_of(&budget, &["pct", "budget_pct"])),
                        num_of(&totals, &["approved"]) as i64
                    );
                    st.counts = format!(
                        "未生成 {} · 生成中 {} · 待审 {} · 已通过 {} · 退回 {} · 重拍 {} · 锁定 {}",
                        num_of(&totals, &["not_generated"]) as i64,
                        num_of(&totals, &["generating"]) as i64,
                        num_of(&totals, &["pending_review"]) as i64,
                        num_of(&totals, &["approved"]) as i64,
                        num_of(&totals, &["rejected"]) as i64,
                        num_of(&totals, &["reshoot"]) as i64,
                        num_of(&totals, &["locked"]) as i64,
                    );
                    let eta = value.get("eta").cloned().unwrap_or(Value::Null);
                    say(
                        st,
                        format!(
                            "进度已刷新：{} · 预计 {}",
                            st.counts,
                            text_of(&eta, &["text", "note"]).if_empty("—")
                        ),
                        "ok",
                    );
                }
                Err(error) => say(st, error, "warn"),
            });
            publish(app);
        },
    );
}

fn load_audit(weak: slint::Weak<App>) {
    let project = with_pro(|st| st.project.clone());
    if project.trim().is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    host::spawn(
        move || concat_host::pro::monitor_audit(&project, 20),
        |_studio, app, _m, result| {
            with_pro(|st| match result {
                Ok(value) => {
                    st.audit_rows.clear();
                    if let Some(rows) = value.get("audit").and_then(Value::as_array) {
                        for row in rows.iter().take(12) {
                            st.audit_rows.push(Row {
                                label: text_of(row, &["action"]),
                                detail: format!(
                                    "{} · {}",
                                    text_of(row, &["created_at"]),
                                    text_of(row, &["target"])
                                ),
                                value: text_of(row, &["actor"]),
                                level: "info".to_owned(),
                            });
                        }
                    }
                    say(st, "审计日志已刷新（接受 / 退回 / 批注 / 重拍都会写）。", "ok");
                }
                Err(error) => say(st, error, "warn"),
            });
            publish(app);
        },
    );
}

// ── ③ 发行中心 ───────────────────────────────────────────────────────

fn load_distribution(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    with_pro(|st| {
        st.busy = true;
        st.dist_line = "正在取短剧列表与发行记录…".to_owned();
    });
    publish(&app);
    host::spawn(
        || {
            let dramas = concat_host::pro::dramas(100)?;
            let dists = concat_host::pro::distributions(100)?;
            Ok::<_, String>((dramas, dists))
        },
        |_studio, app, _m, result| {
            with_pro(|st| {
                st.busy = false;
                match result {
                    Ok((dramas, dists)) => {
                        st.dramas = dramas
                            .get("items")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        st.dists = dists
                            .get("items")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        if st.drama_index as usize >= st.dramas.len() {
                            st.drama_index = 0;
                        }
                        if st.dist_pick as usize >= st.dists.len() {
                            st.dist_pick = -1;
                        }
                        st.dist_line = format!(
                            "短剧 {} 部 · 发行记录 {} 条（真实 /aidrama/list + /distributions）。",
                            st.dramas.len(),
                            st.dists.len()
                        );
                        say(
                            st,
                            format!(
                                "发行中心接口在线：短剧 {} 部、发行记录 {} 条。",
                                st.dramas.len(),
                                st.dists.len()
                            ),
                            "ok",
                        );
                    }
                    Err(error) => {
                        st.dist_line = "短剧 / 发行记录没取回来。".to_owned();
                        say(st, error, "fail");
                    }
                }
            });
            publish(app);
        },
    );
}

fn make_distribution(weak: slint::Weak<App>) {
    let (drama, platform, lang) = with_pro(|st| {
        let drama = st
            .dramas
            .get(st.drama_index.max(0) as usize)
            .cloned()
            .unwrap_or(Value::Null);
        let platform = ["douyin", "kuaishou", "xiaohongshu", "tiktok", "youtube", "generic"]
            [st.platform.clamp(0, 5) as usize];
        let lang = ["zh", "en", "ja", "es", "ar"][st.lang.clamp(0, 4) as usize];
        (drama, platform.to_owned(), lang.to_owned())
    });
    let drama_id = text_of(&drama, &["drama_id", "id"]);
    if drama_id.is_empty() {
        with_pro(|st| say(st, "先刷新短剧列表并选一部。", "warn"));
        publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    with_pro(|st| {
        st.busy = true;
        st.dist_line = format!("正在打包发行包（{platform} / {lang}）… 平台要压 zip，通常几十秒到几分钟。");
        st.dist_url.clear();
        st.dist_files.clear();
    });
    publish(&app);
    let config = config_dir();
    host::spawn(
        move || -> Result<Value, String> {
            concat_host::pro::distribute(&json!({
                "drama_id": drama_id,
                "platform": platform,
                "lang": lang,
                "ratio": text_of(&drama, &["ratio"]).if_empty("9:16"),
                "auto_translate": true,
            }))
        },
        {
            let config = config.clone();
            move |_studio, app, _m, result: Result<Value, String>| {
                let mut record = None;
                with_pro(|st| {
                    st.busy = false;
                    match result {
                        Ok(value) => {
                            let dist = value
                                .get("distribution")
                                .or_else(|| value.get("item"))
                                .cloned()
                                .unwrap_or(Value::Null);
                            let files = value
                                .get("files")
                                .or_else(|| dist.get("files"))
                                .and_then(Value::as_array)
                                .cloned()
                                .unwrap_or_default();
                            st.dist_files = files
                                .iter()
                                .map(|file| Row {
                                    label: text_of(file, &["name"]),
                                    detail: format!(
                                        "{} · {}",
                                        text_of(file, &["type"]),
                                        num_of(file, &["size"]) as i64
                                    ),
                                    value: String::new(),
                                    level: "".to_owned(),
                                })
                                .collect();
                            st.dist_url = text_of(&dist, &["zip_url", "download_url"]);
                            st.dist_line = format!(
                                "发行包已生成：{} · {} 个文件 · {}",
                                text_of(&dist, &["filename"]),
                                files.len(),
                                text_of(&dist, &["status"])
                            );
                            say(
                                st,
                                format!("发行包已生成（{} 个文件）。", files.len()),
                                "ok",
                            );
                            record = Some(concat_host::pro::LocalTask {
                                id: format!("dist-{}", text_of(&dist, &["id"])),
                                module: "发行中心".to_owned(),
                                title: format!(
                                    "发行包 {} · {}",
                                    text_of(&dist, &["drama_title", "title"]),
                                    text_of(&dist, &["platform"])
                                ),
                                kind: "none".to_owned(),
                                remote: String::new(),
                                status: "完成".to_owned(),
                                progress: 100.0,
                                cost: 0.0,
                                detail: format!("{} 个文件", files.len()),
                                file: String::new(),
                                url: st.dist_url.clone(),
                                created: now_secs(),
                                updated: now_secs(),
                            });
                        }
                        Err(error) => {
                            st.dist_line = "发行包没生成出来。".to_owned();
                            say(st, error, "fail");
                        }
                    }
                });
                if let Some(task) = record {
                    push_task(&config, task, app.as_weak());
                }
                publish(app);
            }
        },
    );
}

fn fill_stats(weak: slint::Weak<App>) {
    let (dist_id, body) = with_pro(|st| {
        let dist = st
            .dists
            .get(st.dist_pick.max(0) as usize)
            .cloned()
            .unwrap_or(Value::Null);
        (
            text_of(&dist, &["id", "distribution_id"]),
            json!({
                "views": st.dist_views.trim().parse::<i64>().unwrap_or(0),
                "likes": st.dist_likes.trim().parse::<i64>().unwrap_or(0),
                "conversions": st.dist_conversions.trim().parse::<i64>().unwrap_or(0),
                "publish_url": st.dist_publish_url.trim(),
            }),
        )
    });
    if dist_id.is_empty() {
        with_pro(|st| say(st, "先在发行记录里选一条再回填。", "warn"));
        publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    with_pro(|st| st.dist_stats_line = "正在回填…".to_owned());
    publish(&app);
    host::spawn(
        move || concat_host::pro::distribution_stats(&dist_id, &body),
        |_studio, app, _m, result| {
            with_pro(|st| match result {
                Ok(value) => {
                    let item = value
                        .get("item")
                        .or_else(|| value.get("distribution"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    st.dist_stats_line = format!(
                        "已回填：播放 {} / 点赞 {} / 转化 {}",
                        num_of(&item, &["views"]) as i64,
                        num_of(&item, &["likes"]) as i64,
                        num_of(&item, &["conversions"]) as i64
                    );
                    say(st, "数据已回填到平台台账。", "ok");
                }
                Err(error) => say(st, error, "fail"),
            });
            publish(app);
        },
    );
}

// ── ④ 授权护照 ───────────────────────────────────────────────────────

fn load_passport(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    host::spawn(
        || {
            let list = concat_host::pro::passport_list(20)?;
            let stats = concat_host::pro::passport_stats().unwrap_or(Value::Null);
            let signing = concat_host::pro::passport_signing().unwrap_or(Value::Null);
            Ok::<_, String>((list, stats, signing))
        },
        |_studio, app, _m, result| {
            with_pro(|st| match result {
                Ok((list, stats, signing)) => {
                    st.pass_recent.clear();
                    if let Some(items) = list.get("items").and_then(Value::as_array) {
                        for item in items.iter().take(10) {
                            st.pass_recent.push(Row {
                                label: text_of(item, &["passport_id"]),
                                detail: format!(
                                    "{} · {} · 台账 #{}",
                                    if text_of(item, &["passport_type"]) == "project" {
                                        "项目护照"
                                    } else {
                                        "素材护照"
                                    },
                                    text_of(item, &["subject_name"]),
                                    num_of(item, &["ledger_seq"]) as i64
                                ),
                                value: text_of(item, &["created_at"]),
                                level: "info".to_owned(),
                            });
                        }
                    }
                    st.pass_line = format!(
                        "平台护照台账：{} 张（素材 {} / 项目 {}）· 签名 {} · 指纹 {}。{}",
                        num_of(&stats, &["total_passports"]) as i64,
                        stats
                            .get("by_type")
                            .and_then(Value::as_array)
                            .and_then(|rows| rows.first())
                            .map(|row| num_of(row, &["c"]) as i64)
                            .unwrap_or(0),
                        stats
                            .get("by_type")
                            .and_then(Value::as_array)
                            .and_then(|rows| rows.get(1))
                            .map(|row| num_of(row, &["c"]) as i64)
                            .unwrap_or(0),
                        text_of(&signing, &["algorithm"]),
                        text_of(&signing, &["fingerprint"]),
                        "生成 / 校验都用真实 /api/passport/*，桌面不改服务端。"
                    );
                    say(st, "授权护照接口在线：台账、统计、签名公钥已取回。", "ok");
                }
                Err(error) => say(st, error, "fail"),
            });
            publish(app);
        },
    );
}

fn gen_asset_passport(weak: slint::Weak<App>) {
    let config = config_dir();
    let body = with_pro(|st| {
        let id = st.pass_asset_id.trim().to_owned();
        let url = st.pass_asset_url.trim().to_owned();
        if id.is_empty() && url.is_empty() {
            say(st, "填素材库 ID，或粘一个外部素材 URL（二选一）。", "warn");
            return None;
        }
        st.busy = true;
        st.pass_line = "正在生成素材护照（算哈希 + 写台账 + 签名）…".to_owned();
        let mut body = json!({ "project_id": st.pass_project.trim(), "note": "桌面 0.3.7 生成" });
        if !id.is_empty() {
            body["asset_id"] = json!(id);
        } else {
            body["asset_url"] = json!(url);
        }
        Some(body)
    });
    let Some(body) = body else {
        publish_now();
        return;
    };
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    host::spawn(
        move || concat_host::pro::passport_asset(&body),
        move |_studio, app, _m, result| {
            let mut new_passport_task: Option<(&str, String, String)> = None;
            with_pro(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        apply_passport(st, &value, "素材");
                        say(st, "素材护照已生成（哈希链 + 签名）。", "ok");
                        new_passport_task = Some(("素材", st.pass_id.clone(), st.pass_zip_url.clone()));
                    }
                    Err(error) => {
                        st.pass_line = "素材护照没生成出来。".to_owned();
                        say(st, error, "fail");
                    }
                }
            });
            if let Some((kind, passport_id, zip)) = new_passport_task {
                let task = concat_host::pro::LocalTask {
                    id: format!("passport-{passport_id}"),
                    module: "授权护照".to_owned(),
                    title: format!("{kind}护照 {passport_id}"),
                    kind: "none".to_owned(),
                    remote: String::new(),
                    status: "完成".to_owned(),
                    progress: 100.0,
                    cost: 0.0,
                    detail: "哈希链 + Ed25519 签名".to_owned(),
                    file: String::new(),
                    url: zip,
                    created: now_secs(),
                    updated: now_secs(),
                };
                push_task(&config, task, app.as_weak());
            }
            publish(app);
        },
    );
}

fn gen_project_passport(weak: slint::Weak<App>) {
    let config = config_dir();
    let body = with_pro(|st| {
        let pack = st.pass_pack.trim().to_owned();
        if pack.is_empty() {
            say(st, "填工程包 ID（可在「项目包」或网页导演任务里拿到）。", "warn");
            return None;
        }
        st.busy = true;
        st.pass_line = "正在生成项目护照（逐素材子护照 + 批次）…".to_owned();
        let mut body = json!({ "pack_id": pack });
        if !st.pass_title.trim().is_empty() {
            body["title"] = json!(st.pass_title.trim());
        }
        if !st.pass_client.trim().is_empty() {
            body["client"] = json!(st.pass_client.trim());
        }
        Some(body)
    });
    let Some(body) = body else {
        publish_now();
        return;
    };
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    host::spawn(
        move || concat_host::pro::passport_project(&body),
        move |_studio, app, _m, result| {
            let mut new_passport_task: Option<(&str, String, String)> = None;
            with_pro(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        apply_passport(st, &value, "项目");
                        say(st, "项目护照已生成（批量 + 哈希链）。", "ok");
                        new_passport_task = Some(("项目", st.pass_id.clone(), st.pass_zip_url.clone()));
                    }
                    Err(error) => {
                        st.pass_line = "项目护照没生成出来。".to_owned();
                        say(st, error, "fail");
                    }
                }
            });
            if let Some((kind, passport_id, zip)) = new_passport_task {
                let task = concat_host::pro::LocalTask {
                    id: format!("passport-{passport_id}"),
                    module: "授权护照".to_owned(),
                    title: format!("{kind}护照 {passport_id}"),
                    kind: "none".to_owned(),
                    remote: String::new(),
                    status: "完成".to_owned(),
                    progress: 100.0,
                    cost: 0.0,
                    detail: "哈希链 + Ed25519 签名".to_owned(),
                    file: String::new(),
                    url: zip,
                    created: now_secs(),
                    updated: now_secs(),
                };
                push_task(&config, task, app.as_weak());
            }
            publish(app);
        },
    );
}

fn apply_passport(st: &mut ProState, value: &Value, kind: &str) {
    let passport = value.get("passport").cloned().unwrap_or(Value::Null);
    st.pass_id = text_of(value, &["passport_id"]);
    st.pass_rows.clear();
    st.pass_rows.push(Row {
        label: "护照编号".to_owned(),
        detail: st.pass_id.clone(),
        value: String::new(),
        level: "ok".to_owned(),
    });
    if kind == "素材" {
        let asset = passport.get("asset").cloned().unwrap_or(Value::Null);
        let fingerprint = passport.get("fingerprint").cloned().unwrap_or(Value::Null);
        let license = passport.get("license").cloned().unwrap_or(Value::Null);
        let chain = passport.get("chain_ref").cloned().unwrap_or(Value::Null);
        st.pass_rows.push(Row {
            label: "素材".to_owned(),
            detail: format!(
                "{} · {} · {} · {}",
                text_of(&asset, &["title"]),
                text_of(&asset, &["source"]),
                text_of(&asset, &["author"]),
                text_of(&asset, &["size_human"])
            ),
            value: text_of(&asset, &["type"]),
            level: "info".to_owned(),
        });
        st.pass_rows.push(Row {
            label: "内容 SHA-256".to_owned(),
            detail: text_of(&fingerprint, &["sha256"]),
            value: text_of(&fingerprint, &["mime"]),
            level: "ok".to_owned(),
        });
        st.pass_rows.push(Row {
            label: "授权".to_owned(),
            detail: format!(
                "{}（{}）· {}",
                text_of(&license, &["name"]),
                text_of(&license, &["code"]),
                text_of(&license, &["attribution_text"])
            ),
            value: if license.get("commercial_ok").and_then(Value::as_bool) == Some(true) {
                "可商用".to_owned()
            } else {
                "商用性以原许可为准".to_owned()
            },
            level: "ok".to_owned(),
        });
        st.pass_rows.push(Row {
            label: "台账 / 签名".to_owned(),
            detail: format!(
                "seq #{} · {} · 链哈希 {}…",
                num_of(&chain, &["ledger_seq"]) as i64,
                text_of(&chain, &["signature_alg"]),
                text_of(&chain, &["chain_hash"]).chars().take(24).collect::<String>()
            ),
            value: text_of(&chain, &["signing_fp"]),
            level: "info".to_owned(),
        });
        st.pass_zip_url = text_of(value.get("package").unwrap_or(&Value::Null), &["download_url"]);
        st.pass_line = format!(
            "素材护照已生成：{} · {} · 签发 {}。",
            st.pass_id,
            text_of(&license, &["name"]),
            text_of(&passport, &["created_at"])
        );
    } else {
        let batch = passport.get("batch").cloned().unwrap_or(Value::Null);
        let project = passport.get("project").cloned().unwrap_or(Value::Null);
        let package = value.get("package").cloned().unwrap_or(Value::Null);
        st.pass_rows.push(Row {
            label: "项目".to_owned(),
            detail: format!(
                "{} · {} · 子护照 {} 张",
                text_of(&project, &["title"]),
                text_of(&project, &["client"]),
                num_of(&batch, &["total"]) as i64
            ),
            value: text_of(&project, &["project_id"]),
            level: "info".to_owned(),
        });
        if let Some(items) = passport.get("items").and_then(Value::as_array) {
            for item in items.iter().take(12) {
                let license = item.get("license").cloned().unwrap_or(Value::Null);
                st.pass_rows.push(Row {
                    label: text_of(item, &["role"]),
                    detail: format!(
                        "{} · {} · {}",
                        text_of(item, &["name"]),
                        text_of(&license, &["name"]),
                        text_of(item, &["sha256"]).chars().take(16).collect::<String>()
                    ),
                    value: text_of(&license, &["status"]),
                    level: "ok".to_owned(),
                });
            }
        }
        st.pass_zip_url = text_of(&package, &["download_url"]);
        st.pass_line = format!(
            "项目护照已生成：{} · {} 张子护照 · zip {}。",
            st.pass_id,
            num_of(&batch, &["total"]) as i64,
            text_of(&package, &["zip_size_human"])
        );
    }
    st.pass_last = Some(value.clone());
}

fn verify_passport(weak: slint::Weak<App>) {
    let (id, sha) = with_pro(|st| (st.pass_verify_id.trim().to_owned(), st.pass_sha.trim().to_owned()));
    if id.is_empty() {
        with_pro(|st| say(st, "填要校验的护照编号（pp-a-… / pp-p-…）。", "warn"));
        publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    with_pro(|st| {
        st.pass_verify_line = "正在重算哈希链与签名…".to_owned();
        st.pass_verify_rows.clear();
    });
    publish(&app);
    host::spawn(
        move || concat_host::pro::passport_verify(&id, &sha),
        |_studio, app, _m, result| {
            with_pro(|st| match result {
                Ok(value) => {
                    let valid = value.get("valid").and_then(Value::as_bool).unwrap_or(false);
                    st.pass_verify_line = format!(
                        "{} · {}",
                        if valid { "校验通过：护照可信" } else { "校验失败：护照不可信" },
                        text_of(&value, &["summary"])
                    );
                    st.pass_verify_rows.clear();
                    if let Some(checks) = value.get("checks").and_then(Value::as_object) {
                        for (key, check) in checks {
                            let ok = check.get("ok").and_then(Value::as_bool).unwrap_or(false);
                            st.pass_verify_rows.push(Row {
                                label: text_of(check, &["label"]).if_empty(key),
                                detail: text_of(check, &["detail"]),
                                value: if ok { "PASS".to_owned() } else { "FAIL".to_owned() },
                                level: if ok { "ok" } else { "fail" }.to_owned(),
                            });
                        }
                    }
                    if let Some(reasons) = value.get("reasons").and_then(Value::as_array) {
                        for reason in reasons {
                            if let Some(text) = reason.as_str() {
                                st.pass_verify_rows.push(Row {
                                    label: "失败原因".to_owned(),
                                    detail: text.to_owned(),
                                    value: String::new(),
                                    level: "fail".to_owned(),
                                });
                            }
                        }
                    }
                    say(
                        st,
                        if valid { "护照校验通过。" } else { "护照校验失败：不要当授权凭证使用。" },
                        if valid { "ok" } else { "fail" },
                    );
                }
                Err(error) => say(st, error, "fail"),
            });
            publish(app);
        },
    );
}

// ── ⑤ 合规沙盘 ───────────────────────────────────────────────────────

fn load_sandbox(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    host::spawn(
        || {
            let version = concat_host::pro::sandbox_version()?;
            let history = concat_host::pro::sandbox_history(10).unwrap_or(Value::Null);
            Ok::<_, String>((version, history))
        },
        |_studio, app, _m, result| {
            with_pro(|st| match result {
                Ok((version, history)) => {
                    st.sb_platforms.clear();
                    if let Some(platforms) = version.get("platforms").and_then(Value::as_object) {
                        for (key, platform) in platforms {
                            st.sb_platforms.push(Row {
                                label: text_of(platform, &["name"]).if_empty(key),
                                detail: format!(
                                    "严度 {} · {}",
                                    num_of(platform, &["strictness"]),
                                    platform
                                        .get("focus")
                                        .and_then(Value::as_array)
                                        .map(|rows| rows
                                            .iter()
                                            .filter_map(Value::as_str)
                                            .collect::<Vec<_>>()
                                            .join("、"))
                                        .unwrap_or_default()
                                ),
                                value: String::new(),
                                level: "info".to_owned(),
                            });
                        }
                    }
                    st.sb_history.clear();
                    if let Some(items) = history.get("items").and_then(Value::as_array) {
                        for item in items.iter().take(8) {
                            st.sb_history.push(Row {
                                label: format!(
                                    "{} 分 · {}",
                                    num_of(item, &["risk_score"]) as i64,
                                    text_of(item, &["grade"])
                                ),
                                detail: format!(
                                    "{} · {} · 命中 {} 项{}",
                                    text_of(item, &["platform"]),
                                    text_of(item, &["ctype"]),
                                    num_of(item, &["findings_count"]) as i64,
                                    if text_of(item, &["parent_check_id"]).is_empty() {
                                        String::new()
                                    } else {
                                        " · 复评".to_owned()
                                    }
                                ),
                                value: text_of(item, &["created_at"]),
                                level: if num_of(item, &["risk_score"]) >= 75.0 {
                                    "fail"
                                } else if num_of(item, &["risk_score"]) >= 50.0 {
                                    "warn"
                                } else {
                                    "ok"
                                }
                                .to_owned(),
                            });
                        }
                    }
                    st.sb_line = format!(
                        "规则库 {} · {} 条 · {} 类 · 25 份法规 / 平台来源。",
                        text_of(&version, &["version"]),
                        num_of(&version, &["rule_count"]) as i64,
                        version
                            .get("categories")
                            .and_then(Value::as_array)
                            .map(|rows| rows.len())
                            .unwrap_or(0)
                    );
                    if st.sb_history_line.is_empty() {
                        st.sb_history_line = "历史：本次打开还没有彩排记录。".to_owned();
                    }
                    say(st, "合规沙盘接口在线：规则库版本与平台严度已取回。", "ok");
                }
                Err(error) => say(st, error, "fail"),
            });
            publish(app);
        },
    );
}

fn run_sandbox(weak: slint::Weak<App>, recheck: bool) {
    let (content, ctype, platform, ai, parent) = with_pro(|st| {
        let ctype = ["copy", "script", "video", "image", "live"][st.sb_type.clamp(0, 4) as usize];
        let platform = ["douyin", "kuaishou", "xiaohongshu", "generic"]
            [st.sb_platform.clamp(0, 3) as usize];
        (
            st.sb_content.clone(),
            ctype.to_owned(),
            platform.to_owned(),
            st.sb_ai,
            if recheck { st.sb_check_id.clone() } else { String::new() },
        )
    });
    if content.trim().is_empty() {
        with_pro(|st| say(st, "先把要检查的文案 / 脚本粘进来。", "warn"));
        publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    with_pro(|st| {
        st.busy = true;
        st.sb_line = "正在跑规则引擎（必要时叠加 AI 语义辅助）…".to_owned();
    });
    publish(&app);
    host::spawn(
        move || {
            let mut body = json!({
                "type": ctype,
                "platform": platform,
                "content": content,
                "has_ai_assets": ai,
            });
            if !parent.is_empty() {
                body["parent_check_id"] = json!(parent);
            }
            concat_host::pro::sandbox_check(&body)
        },
        {
            let config = config_dir();
            move |_studio, app, _m, result| {
                let mut task = None;
                with_pro(|st| {
                    st.busy = false;
                    match result {
                        Ok(value) => {
                            st.sb_check_id = text_of(&value, &["check_id"]);
                            st.sb_score = num_of(&value, &["risk_score"]) as i32;
                            let grade = value.get("grade").cloned().unwrap_or(Value::Null);
                            st.sb_grade = text_of(&grade, &["level"]);
                            st.sb_summary = text_of(&value, &["summary"]);
                            let stats = value.get("stats").cloned().unwrap_or(Value::Null);
                            let by_level = stats.get("by_level").cloned().unwrap_or(Value::Null);
                            st.sb_stats = format!(
                                "严重 {} · 高 {} · 中 {} · 低 {} · 命中分类：{}",
                                num_of(&by_level, &["critical"]) as i64,
                                num_of(&by_level, &["high"]) as i64,
                                num_of(&by_level, &["medium"]) as i64,
                                num_of(&by_level, &["low"]) as i64,
                                stats
                                    .get("by_category")
                                    .and_then(Value::as_array)
                                    .map(|rows| rows
                                        .iter()
                                        .map(|row| format!(
                                            "{}（{}）",
                                            text_of(row, &["category"]),
                                            num_of(row, &["count"]) as i64
                                        ))
                                        .collect::<Vec<_>>()
                                        .join("、"))
                                    .unwrap_or_default()
                            );
                            st.sb_findings.clear();
                            if let Some(findings) = value.get("findings").and_then(Value::as_array) {
                                for finding in findings {
                                    let matched = finding.get("matched").cloned().unwrap_or(Value::Null);
                                    st.sb_findings.push(Finding {
                                        level: text_of(finding, &["level"]),
                                        title: text_of(finding, &["title"]),
                                        category: text_of(finding, &["category"]),
                                        position: format!(
                                            "第{}行 {}",
                                            num_of(&matched, &["line"]) as i64,
                                            text_of(&matched, &["section"])
                                        ),
                                        quote: format!("「{}」", text_of(&matched, &["text"])),
                                        basis: text_of(finding, &["basis"]),
                                        why: text_of(finding, &["why"]),
                                        suggest: text_of(finding, &["suggest"]),
                                    });
                                }
                            }
                            let rewrite = value.get("suggested_rewrite").cloned().unwrap_or(Value::Null);
                            st.sb_rewrite = text_of(&rewrite, &["text"]);
                            let ai_label = value.get("ai_label").cloned().unwrap_or(Value::Null);
                            st.sb_ai_line = format!(
                                "AI 标识：{} {}",
                                if ai_label.get("missing").and_then(Value::as_bool) == Some(true) {
                                    "缺少显著标识"
                                } else {
                                    "未发现标识问题"
                                },
                                text_of(&ai_label, &["reminder"])
                            );
                            let engine = value.get("engine").cloned().unwrap_or(Value::Null);
                            st.sb_line = format!(
                                "{} · 规则库 {} · 引擎 {}",
                                text_of(&value, &["summary"]),
                                text_of(&value, &["rules_version"]),
                                text_of(&engine, &["name"]).if_empty("rule")
                            );
                            st.sb_history_line = format!(
                                "本次 {} · 风险分 {} · 命中 {} 条；复评会带 parent_check_id 给出整改对比。",
                                st.sb_check_id,
                                st.sb_score,
                                st.sb_findings.len()
                            );
                            say(
                                st,
                                format!(
                                    "{}完成：风险分 {}（{}）。",
                                    if st.sb_check_id.is_empty() { "彩排" } else { "彩排" },
                                    st.sb_score,
                                    st.sb_grade
                                ),
                                if st.sb_score >= 50 { "warn" } else { "ok" },
                            );
                            task = Some(concat_host::pro::LocalTask {
                                id: format!("sandbox-{}", st.sb_check_id),
                                module: "合规沙盘".to_owned(),
                                title: format!("合规彩排 {}", st.sb_check_id),
                                kind: "none".to_owned(),
                                remote: String::new(),
                                status: "完成".to_owned(),
                                progress: 100.0,
                                cost: 0.0,
                                detail: format!("风险分 {} · {}", st.sb_score, st.sb_grade),
                                file: String::new(),
                                url: String::new(),
                                created: now_secs(),
                                updated: now_secs(),
                            });
                        }
                        Err(error) => {
                            st.sb_line = "彩排没跑完。".to_owned();
                            say(st, error, "fail");
                        }
                    }
                });
                if let Some(task) = task {
                    push_task(&config, task, app.as_weak());
                }
                publish(app);
            }
        },
    );
}

// ── ⑥ 插件市场 ───────────────────────────────────────────────────────

fn load_plugins(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    host::spawn(
        || {
            let market = concat_host::pro::plugin_market(100)?;
            let installed = concat_host::pro::plugin_installed()?;
            Ok::<_, String>((market, installed))
        },
        |_studio, app, _m, result| {
            with_pro(|st| match result {
                Ok((market, installed)) => {
                    st.plug_market = market
                        .get("items")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    st.plug_installed = installed
                        .get("plugins")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    st.plug_line = format!(
                        "市场 {} 个 · 已安装 {} 个（真实 /plugins/market + /installed）",
                        st.plug_market.len(),
                        st.plug_installed.len()
                    );
                    say(st, "插件市场接口在线：市场与已安装列表已取回。", "ok");
                }
                Err(error) => say(st, error, "fail"),
            });
            publish(app);
        },
    );
}

fn pick_market_plugin(weak: slint::Weak<App>, index: usize) {
    let item = with_pro(|st| st.plug_market.get(index).cloned().unwrap_or(Value::Null));
    let installed = item.get("installed").and_then(Value::as_bool).unwrap_or(false);
    let plugin_id = text_of(&item, &["plugin_id"]);
    with_pro(|st| {
        st.plug_selected = text_of(&item, &["market_id", "plugin_id", "id"]);
        st.plug_installed_flag = installed;
        st.plug_detail_line = text_of(&item, &["name"]);
        st.plug_detail_meta = format!(
            "{} · v{} · {} · {} · {}",
            text_of(&item, &["author"]),
            text_of(&item, &["version"]),
            text_of(&item, &["category"]),
            text_of(&item, &["type_label"]),
            if item.get("official").and_then(Value::as_bool) == Some(true) {
                "官方"
            } else {
                "第三方"
            }
        );
        st.plug_detail_desc = text_of(&item, &["description"]);
        st.plug_schema = text_of(&item, &["quality_label"]).if_empty("待认证");
        st.plug_runnable = text_of(&item, &["type"]) != "panel"
            && text_of(&item, &["type"]) != "assetpack";
        st.plug_config_json = "{}".to_owned();
        st.plug_run_json = "{}".to_owned();
        st.plug_output.clear();
    });
    if !installed || plugin_id.is_empty() {
        publish_now();
        return;
    }
    pick_installed_plugin(weak, &plugin_id);
}

fn pick_installed_plugin(weak: slint::Weak<App>, plugin_id: &str) {
    let id = plugin_id.to_owned();
    let Some(app) = weak.upgrade() else { return };
    with_pro(|st| st.plug_selected = id.clone());
    host::spawn(
        move || {
            let detail = concat_host::pro::plugin_detail(&id)?;
            let config = concat_host::pro::plugin_config(&id)?;
            Ok::<_, String>((detail, config))
        },
        |_studio, app, _m, result| {
            with_pro(|st| match result {
                Ok((detail, config)) => {
                    let plugin = detail.get("plugin").cloned().unwrap_or(Value::Null);
                    st.plug_detail_line = text_of(&plugin, &["name"]);
                    st.plug_detail_meta = format!(
                        "{} · v{} · {} · 权限：{}",
                        text_of(&plugin, &["author"]),
                        text_of(&plugin, &["version"]),
                        text_of(&plugin, &["type_label"]),
                        plugin
                            .get("permissions")
                            .and_then(Value::as_array)
                            .map(|rows| rows
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" / "))
                            .unwrap_or_default()
                    );
                    st.plug_detail_desc = text_of(&plugin, &["description"]);
                    st.plug_installed_flag = true;
                    st.plug_enabled = text_of(&plugin, &["status"]) == "enabled";
                    st.plug_runnable = plugin.get("runnable").and_then(Value::as_bool).unwrap_or(true);
                    st.plug_config_json =
                        serde_json::to_string_pretty(config.get("config").unwrap_or(&json!({})))
                            .unwrap_or_else(|_| "{}".to_owned());
                    st.plug_schema = config
                        .get("config_schema")
                        .map(|schema| serde_json::to_string_pretty(schema).unwrap_or_default())
                        .unwrap_or_else(|| "该插件未声明 config_schema，可直接编辑 JSON。".to_owned());
                    say(st, format!("插件详情已取回：{}。", st.plug_detail_line), "ok");
                }
                Err(error) => say(st, error, "fail"),
            });
            publish(app);
        },
    );
}

fn plugin_action(weak: slint::Weak<App>, what: &str, index: usize, text: &str) {
    match what {
        "query" => {
            with_pro(|st| st.plug_query = text.to_owned());
            publish_now();
            return;
        }
        "config-json" => {
            with_pro(|st| st.plug_config_json = text.to_owned());
            return;
        }
        "run-json" => {
            with_pro(|st| st.plug_run_json = text.to_owned());
            return;
        }
        "pick-market" => {
            pick_market_plugin(weak, index);
            return;
        }
        "pick-installed" => {
            pick_installed_plugin(weak, text);
            return;
        }
        _ => {}
    }
    let (market_id, plugin_id) = with_pro(|st| {
        (
            st.plug_market
                .get(index)
                .map(|item| text_of(item, &["market_id", "id"]))
                .unwrap_or_default(),
            st.plug_selected.clone(),
        )
    });
    let Some(app) = weak.upgrade() else { return };
    let plugin = if what == "install" { market_id } else { plugin_id };
    if plugin.is_empty() {
        with_pro(|st| say(st, "先在市场 / 已安装列表里选一个插件。", "warn"));
        publish_now();
        return;
    }
    match what {
        "install" => {
            with_pro(|st| {
                st.busy = true;
                st.plug_line = format!("正在安装 {plugin}…");
            });
            publish(&app);
            host::spawn(
                move || concat_host::pro::plugin_install(&plugin),
                |_studio, app, _m, result| {
                    with_pro(|st| {
                        st.busy = false;
                        match result {
                            Ok(_) => {
                                st.plug_line = "插件已安装（服务端记账）。".to_owned();
                                say(st, "插件已安装。", "ok");
                            }
                            Err(error) => say(st, error, "fail"),
                        }
                    });
                    publish(app);
                },
            );
        }
        "enable" | "disable" => {
            let on = what == "enable";
            with_pro(|st| st.plug_line = if on { "正在启用…" } else { "正在停用…" }.to_owned());
            publish(&app);
            host::spawn(
                move || concat_host::pro::plugin_set_enabled(&plugin, on),
                move |_studio, app, _m, result| {
                    with_pro(|st| match result {
                        Ok(_) => {
                            st.plug_enabled = on;
                            st.plug_line = if on { "插件已启用。" } else { "插件已停用。" }.to_owned();
                            say(st, st.plug_line.clone(), "ok");
                        }
                        Err(error) => say(st, error, "fail"),
                    });
                    publish(app);
                },
            );
        }
        "config-load" => {
            let id = plugin.clone();
            host::spawn(
                move || concat_host::pro::plugin_config(&id),
                |_studio, app, _m, result| {
                    with_pro(|st| match result {
                        Ok(config) => {
                            st.plug_config_json = serde_json::to_string_pretty(
                                config.get("config").unwrap_or(&json!({})),
                            )
                            .unwrap_or_else(|_| "{}".to_owned());
                            st.plug_schema = config
                                .get("config_schema")
                                .map(|schema| serde_json::to_string_pretty(schema).unwrap_or_default())
                                .unwrap_or_default();
                            say(st, "配置已读取（保存会按 config_schema 校验）。", "ok");
                        }
                        Err(error) => say(st, error, "fail"),
                    });
                    publish(app);
                },
            );
        }
        "config-save" => {
            let raw = with_pro(|st| st.plug_config_json.clone());
            let Ok(config) = serde_json::from_str::<Value>(&raw) else {
                with_pro(|st| say(st, "配置 JSON 不合法：检查括号与引号。", "fail"));
                publish_now();
                return;
            };
            let id = plugin.clone();
            with_pro(|st| st.plug_line = "正在保存配置…".to_owned());
            publish(&app);
            host::spawn(
                move || concat_host::pro::plugin_save_config(&id, config),
                |_studio, app, _m, result| {
                    with_pro(|st| match result {
                        Ok(_) => {
                            st.plug_line = "配置已保存，下次运行生效。".to_owned();
                            say(st, "插件配置已保存。", "ok");
                        }
                        Err(error) => say(st, error, "fail"),
                    });
                    publish(app);
                },
            );
        }
        "run" => {
            let raw = with_pro(|st| st.plug_run_json.clone());
            let Ok(input) = serde_json::from_str::<Value>(&raw) else {
                with_pro(|st| say(st, "输入的 JSON 不合法。", "fail"));
                publish_now();
                return;
            };
            let id = plugin.clone();
            with_pro(|st| {
                st.busy = true;
                st.plug_output = "正在平台沙箱里运行…".to_owned();
            });
            publish(&app);
            host::spawn(
                move || concat_host::pro::plugin_run(&id, input),
                |_studio, app, _m, result| {
                    with_pro(|st| {
                        st.busy = false;
                        match result {
                            Ok(value) => {
                                st.plug_output = format!(
                                    "run {} · {} · 耗时 {}ms · 成本 {} · 输出：{}",
                                    text_of(&value, &["run_id"]),
                                    if value.get("ok").and_then(Value::as_bool) == Some(true) {
                                        "成功"
                                    } else {
                                        "失败"
                                    },
                                    num_of(&value, &["duration_ms"]) as i64,
                                    money(num_of(&value, &["cost_cny"])),
                                    serde_json::to_string(value.get("output").unwrap_or(&Value::Null))
                                        .unwrap_or_default()
                                        .chars()
                                        .take(400)
                                        .collect::<String>()
                                );
                                say(st, "插件运行完成，结果见输出。", "ok");
                            }
                            Err(error) => {
                                st.plug_output = format!("运行失败：{error}");
                                say(st, error, "fail");
                            }
                        }
                    });
                    publish(app);
                },
            );
        }
        "runs" => {
            let id = plugin.clone();
            host::spawn(
                move || concat_host::pro::plugin_runs(&id, 10),
                |_studio, app, _m, result| {
                    with_pro(|st| match result {
                        Ok(value) => {
                            let runs = value
                                .get("runs")
                                .and_then(Value::as_array)
                                .cloned()
                                .unwrap_or_default();
                            st.plug_runs_line = format!(
                                "运行历史 {} 条：{}",
                                runs.len(),
                                runs.iter()
                                    .take(5)
                                    .map(|run| format!(
                                        "{} {} {}ms",
                                        text_of(run, &["run_id"]).chars().take(12).collect::<String>(),
                                        text_of(run, &["status"]),
                                        num_of(run, &["duration_ms"]) as i64
                                    ))
                                    .collect::<Vec<_>>()
                                    .join(" · ")
                            );
                            say(st, "运行历史已刷新。", "ok");
                        }
                        Err(error) => say(st, error, "fail"),
                    });
                    publish(app);
                },
            );
        }
        _ => {}
    }
}

// ── ⑦ 素材库（共用媒体面板的 AssetsPane） ───────────────────────────

fn assets_line(st: &mut ProState) {
    st.asset_line = "素材库：搜索 / 行业 / 类型 / 用途筛选与下载、插入时间线在下面这块面板里（数据来自 /api/assets/*）。".to_owned();
}

// ── ⑧ 模板市场 ───────────────────────────────────────────────────────

fn template_query(st: &ProState) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(key) = st.tpl_industry_keys.get(st.tpl_industry.max(0) as usize) {
        if !key.is_empty() {
            parts.push(format!("industry={}", concat_host::pro::enc(key)));
        }
    }
    if let Some(key) = st.tpl_scene_keys.get(st.tpl_scene.max(0) as usize) {
        if !key.is_empty() {
            parts.push(format!("scene={}", concat_host::pro::enc(key)));
        }
    }
    parts.join("&")
}

fn load_templates(weak: slint::Weak<App>) {
    let query = with_pro(|st| template_query(st));
    let Some(app) = weak.upgrade() else { return };
    with_pro(|st| {
        st.tpl_line = "正在取模板市场…".to_owned();
        st.busy = true;
    });
    publish(&app);
    host::spawn(
        move || {
            let meta = concat_host::pro::template_meta()?;
            let list = concat_host::pro::template_list(&query)?;
            Ok::<_, String>((meta, list))
        },
        |_studio, app, _m, result| {
            with_pro(|st| {
                st.busy = false;
                match result {
                    Ok((meta, list)) => {
                        if let Some(industries) = meta.get("industries").and_then(Value::as_array) {
                            st.tpl_industries = vec!["全部行业".to_owned()];
                            st.tpl_industry_keys = vec![String::new()];
                            for item in industries {
                                let key = text_of(item, &["key"]);
                                let name = text_of(item, &["name"]);
                                st.tpl_industries.push(format!(
                                    "{} ({})",
                                    name,
                                    num_of(item, &["count"]) as i64
                                ));
                                st.tpl_industry_keys.push(key);
                            }
                            if st.tpl_industry as usize >= st.tpl_industries.len() {
                                st.tpl_industry = 0;
                            }
                        }
                        if let Some(scenes) = meta.get("scenes").and_then(Value::as_array) {
                            st.tpl_scenes = vec!["全部场景".to_owned()];
                            st.tpl_scene_keys = vec![String::new()];
                            for item in scenes {
                                let key = text_of(item, &["key"]);
                                let name = text_of(item, &["name"]);
                                st.tpl_scenes.push(format!(
                                    "{} ({})",
                                    name,
                                    num_of(item, &["count"]) as i64
                                ));
                                st.tpl_scene_keys.push(key);
                            }
                            if st.tpl_scene as usize >= st.tpl_scenes.len() {
                                st.tpl_scene = 0;
                            }
                        }
                        st.tpl_list = list
                            .get("items")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let stats = meta.get("stats").cloned().unwrap_or(Value::Null);
                        st.tpl_line = format!(
                            "模板 {} 个 · 行业 {} · 场景 {} · 累计套用 {} 次（真实 /templates-market/list + /meta）。",
                            num_of(&stats, &["templates"]) as i64,
                            num_of(&stats, &["industries"]) as i64,
                            num_of(&stats, &["scenes"]) as i64,
                            num_of(&stats, &["uses"]) as i64
                        );
                        say(st, "模板市场接口在线：列表与筛选项已取回。", "ok");
                    }
                    Err(error) => {
                        st.tpl_line = "模板市场没取回来。".to_owned();
                        say(st, error, "fail");
                    }
                }
            });
            publish(app);
        },
    );
}

fn pick_template(weak: slint::Weak<App>, index: usize) {
    let id = with_pro(|st| {
        st.tpl_list
            .get(index)
            .map(|item| text_of(item, &["id"]))
            .unwrap_or_default()
    });
    if id.is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    with_pro(|st| st.tpl_selected = id.clone());
    host::spawn(
        move || concat_host::pro::template_detail(&id),
        |_studio, app, _m, result| {
            with_pro(|st| match result {
                Ok(value) => {
                    let item = value.get("item").cloned().unwrap_or(Value::Null);
                    st.tpl_detail_line = format!(
                        "{} · {}",
                        text_of(&item, &["name"]),
                        text_of(&item, &["industry_name"])
                    );
                    st.tpl_detail_desc = format!(
                        "{}　场景：{} · 画幅 {} · 时长 {} · 风格 {} · 配音 {} · BGM {}。{}",
                        text_of(&item, &["desc"]),
                        text_of(&item, &["scene_name"]),
                        text_of(&item, &["ratio"]),
                        text_of(&item, &["duration_label"]),
                        text_of(&item, &["style"]),
                        text_of(&item, &["voice"]),
                        text_of(&item, &["bgm"]),
                        text_of(&item, &["commercial_note"])
                    );
                    st.tpl_fav = false;
                    st.tpl_detail = Some(item);
                    say(st, "模板详情已取回。", "ok");
                }
                Err(error) => say(st, error, "fail"),
            });
            publish(app);
        },
    );
}

fn template_use(weak: slint::Weak<App>) {
    let (id, body) = with_pro(|st| {
        let id = st.tpl_selected.clone();
        if id.is_empty() {
            say(st, "先选一个模板。", "warn");
            return (String::new(), Value::Null);
        }
        st.busy = true;
        st.tpl_gen_line = "正在提交渲染任务…".to_owned();
        st.tpl_gen_progress = 0.0;
        st.tpl_video_url.clear();
        st.tpl_download_url.clear();
        (
            id,
            json!({
                "template_id": st.tpl_selected,
                "duration": if st.tpl_duration == 0 { 15 } else { 30 },
                "store_name": st.tpl_store.trim(),
                "product": st.tpl_product.trim(),
                "selling_points": st.tpl_points.trim(),
                "copy": st.tpl_copy.trim(),
                "quality": if st.tpl_quality == 0 { "720p" } else { "1080p" },
            }),
        )
    });
    if id.is_empty() {
        publish_now();
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    publish(&app);
    host::spawn(
        move || concat_host::pro::template_use(&body),
        |_studio, app, _m, result| {
            with_pro(|st| {
                st.busy = false;
                match result {
                    Ok(value) => {
                        let tasks = value
                            .get("tasks")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let task_id = tasks
                            .first()
                            .map(|task| text_of(task, &["task_id"]))
                            .unwrap_or_else(|| text_of(&value, &["task_id"]));
                        if task_id.is_empty() {
                            st.tpl_gen_line = "平台没有返回任务 id。".to_owned();
                            say(st, "模板任务没提交成功。", "fail");
                        } else {
                            st.tpl_task = Some(task_id.clone());
                            st.tpl_gen_line = format!("已提交模板渲染任务 {task_id}，等平台串行队列。");
                            say(st, "模板任务已提交，进度每秒刷新。", "ok");
                        }
                    }
                    Err(error) => {
                        st.tpl_gen_line = "模板任务没提交成功。".to_owned();
                        say(st, error, "fail");
                    }
                }
            });
            publish(app);
        },
    );
}

// ── ⑨ 任务中心动作 / 下载 ───────────────────────────────────────────

fn download_to(weak: slint::Weak<App>, url: String, sub: &str, name: &str) {
    if url.trim().is_empty() {
        return;
    }
    let Some(app) = weak.upgrade() else { return };
    // 落在「视频 / 星籁」下，和导出的成片放一起。
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
            concat_host::pro::download(&url, &dest_for_job, &cancel)
        },
        move |studio, app, _m, result| match result {
            Ok(size) => {
                studio.notify(
                    &format!(
                        "已下载到 {}（{:.1} MB）",
                        dest.display(),
                        size as f64 / 1024.0 / 1024.0
                    ),
                    false,
                );
                if let Err(error) = platform::reveal(&dest.to_string_lossy()) {
                    log::warn!("pro: 打开下载文件失败：{error}");
                }
            }
            Err(error) => studio.notify(&error, true),
        },
    );
    let _ = app;
}

fn task_action(weak: slint::Weak<App>, index: usize, what: &str) {
    let (task, config) = with_pro(|st| {
        (
            st.tasks.get(index).cloned(),
            config_dir_from_studio(),
        )
    });
    let Some(task) = task else { return };
    match what {
        "open" => {
            if task.file.is_empty() || !Path::new(&task.file).is_file() {
                with_pro(|st| say(st, "这条任务没有本机文件；有网页地址的用「打开成片地址」。", "warn"));
                publish_now();
                return;
            }
            if let Err(error) = platform::reveal(&task.file) {
                with_pro(|st| say(st, format!("打开文件失败：{error}"), "fail"));
                publish_now();
            }
        }
        "cancel" => {
            let mut tasks = concat_host::pro::load_tasks(&config);
            for row in tasks.iter_mut() {
                if row.id == task.id {
                    row.status = "已取消".to_owned();
                    row.detail = "已在桌面停止本地跟踪（服务端任务不能从桌面取消）".to_owned();
                    row.updated = now_secs();
                }
            }
            let _ = concat_host::pro::save_tasks(&config, &tasks);
            with_pro(|st| {
                st.tasks = tasks;
                say(st, "已停止本地跟踪（服务端任务仍按平台队列跑，不能从桌面取消）。", "warn");
            });
            publish_now();
        }
        "remove" => {
            let mut tasks = concat_host::pro::load_tasks(&config);
            tasks.retain(|row| row.id != task.id);
            let _ = concat_host::pro::save_tasks(&config, &tasks);
            with_pro(|st| st.tasks = tasks);
            publish_now();
        }
        _ => {}
    }
}

fn config_dir_from_studio() -> PathBuf {
    config_dir()
}

fn open_web(tab: i32) {
    let url = concat_host::pro::web_url(web_of(tab));
    if let Err(error) = platform::open_link(&url) {
        log::warn!("pro: 打开网页版失败：{error}（{url}）");
    }
}

// ── 动作总入口 ───────────────────────────────────────────────────────

/// 面板上的一个动作：页签、字段、按钮。`index` 是列表下标，`text` 是输入框内容。
pub fn action(weak: slint::Weak<App>, action: String, index: i32, text: String) {
    let Some(app) = weak.upgrade() else { return };
    // 页签 / 关闭 / 网页版：任何页都一样。
    match action.as_str() {
        "tab" => {
            let tab = index.clamp(0, TAB_TASKS);
            let changed = with_pro(|st| {
                if st.tab == tab {
                    return false;
                }
                st.tab = tab;
                st.epoch += 1;
                st.busy = false;
                st.status_line.clear();
                st.status_level = "info".to_owned();
                st.hint = hint_of(tab);
                if tab == TAB_ASSETS {
                    st.asset_line.clear();
                    assets_line(st);
                }
                true
            });
            if !changed {
                return;
            }
            publish(&app);
            if tab == TAB_ASSETS {
                Shell::with(|shell, _| {
                    shell.studio.borrow_mut().handle(crate::panes::Msg::Assets(
                        crate::panes::assets::AssetsMsg::Shown(true),
                    ));
                });
                assets_line_now();
                return;
            }
            // 打开哪一页就取哪一页的数（不重取别的页）。
            let loaded = with_pro(|st| match tab {
                TAB_ORCH => !st.models.is_empty(),
                TAB_MONITOR => !st.mon_types.is_empty(),
                TAB_DIST => !st.dramas.is_empty(),
                TAB_PASS => !st.pass_recent.is_empty(),
                TAB_SANDBOX => !st.sb_platforms.is_empty(),
                TAB_PLUGIN => !st.plug_market.is_empty(),
                TAB_TEMPLATE => !st.tpl_list.is_empty(),
                _ => st.tasks_loaded,
            });
            if loaded {
                return;
            }
            match tab {
                TAB_ORCH => load_models(weak.clone()),
                TAB_MONITOR => load_monitor_types(weak.clone()),
                TAB_DIST => load_distribution(weak.clone()),
                TAB_PASS => load_passport(weak.clone()),
                TAB_SANDBOX => load_sandbox(weak.clone()),
                TAB_PLUGIN => load_plugins(weak.clone()),
                TAB_TEMPLATE => load_templates(weak.clone()),
                TAB_TASKS => refresh_tasks(weak.clone(), true),
                _ => {}
            }
            return;
        }
        "close" => {
            close(weak);
            return;
        }
        "web" => {
            let tab = with_pro(|st| st.tab);
            open_web(tab);
            return;
        }
        // 0.4.0：监视器面板上的导演台入口（独立窗口 / 同屏浮窗）。
        "director-open" => {
            crate::director::open_window(weak.clone());
            return;
        }
        "director-pin" => {
            crate::director::toggle_pin(weak.clone());
            return;
        }
        _ => {}
    }

    let tab = with_pro(|st| st.tab);
    // 先处理纯输入字段（不发起网络）。
    match action.as_str() {
        "orch-budget" => {
            with_pro(|st| st.budget = text);
            return;
        }
        "orch-title" => {
            with_pro(|st| st.title = text);
            return;
        }
        "orch-shot-scene" | "orch-shot-line" | "orch-shot-dur" => {
            with_pro(|st| {
                if let Some(shot) = st.text_shots.get_mut(index.max(0) as usize) {
                    match action.as_str() {
                        "orch-shot-scene" => shot.scene = text,
                        "orch-shot-line" => shot.line = text,
                        _ => shot.dur = text,
                    }
                }
            });
            return;
        }
        "mon-project" => {
            with_pro(|st| st.project = text);
            return;
        }
        "mon-episode" => {
            with_pro(|st| st.episode = text);
            return;
        }
        "mon-take-url" | "mon-note-text" | "mon-retake-prompt" | "mon-retake-reason" => {
            shot_action(weak.clone(), action.as_str(), index, &text);
            return;
        }
        "pass-asset-id" | "pass-asset-url" | "pass-project" | "pass-pack" | "pass-title"
        | "pass-client" | "pass-verify-id" | "pass-sha" => {
            with_pro(|st| match action.as_str() {
                "pass-asset-id" => st.pass_asset_id = text,
                "pass-asset-url" => st.pass_asset_url = text,
                "pass-project" => st.pass_project = text,
                "pass-pack" => st.pass_pack = text,
                "pass-title" => st.pass_title = text,
                "pass-client" => st.pass_client = text,
                "pass-verify-id" => st.pass_verify_id = text,
                _ => st.pass_sha = text,
            });
            return;
        }
        "sb-content" | "sb-rewrite" => {
            with_pro(|st| {
                if action == "sb-content" {
                    st.sb_content = text;
                } else {
                    st.sb_rewrite = text;
                }
            });
            return;
        }
        "plug-query" | "plug-config-json" | "plug-run-json" => {
            let what = action.trim_start_matches("plug-").to_owned();
            plugin_action(weak.clone(), what.as_str(), index.max(0) as usize, &text);
            return;
        }
        "tpl-store" | "tpl-product" | "tpl-points" | "tpl-copy" => {
            with_pro(|st| match action.as_str() {
                "tpl-store" => st.tpl_store = text,
                "tpl-product" => st.tpl_product = text,
                "tpl-points" => st.tpl_points = text,
                _ => st.tpl_copy = text,
            });
            return;
        }
        "dist-views" | "dist-likes" | "dist-conversions" | "dist-publish-url" => {
            with_pro(|st| match action.as_str() {
                "dist-views" => st.dist_views = text,
                "dist-likes" => st.dist_likes = text,
                "dist-conversions" => st.dist_conversions = text,
                _ => st.dist_publish_url = text,
            });
            return;
        }
        _ => {}
    }

    // 再处理按钮。
    match tab {
        TAB_ORCH => match action.as_str() {
            "orch-tier" => {
                with_pro(|st| st.tier = index.clamp(0, 2));
                publish_now();
            }
            "orch-models" => load_models(weak.clone()),
            "orch-plan" => run_plan(weak.clone()),
            "orch-run" => {
                let config = config_dir();
                run_director(weak.clone(), config);
            }
            "orch-add-shot" => {
                with_pro(|st| {
                    let no = st.text_shots.len() as i32 + 1;
                    st.text_shots.push(ShotEdit {
                        label: format!("S{no:02}"),
                        no,
                        scene: String::new(),
                        line: String::new(),
                        dur: "5".to_owned(),
                    });
                });
                publish_now();
            }
            "orch-del-shot" => {
                with_pro(|st| {
                    if st.text_shots.len() > 1 && (index.max(0) as usize) < st.text_shots.len() {
                        st.text_shots.remove(index.max(0) as usize);
                        let mut no = 1;
                        for shot in st.text_shots.iter_mut() {
                            shot.no = no;
                            shot.label = format!("S{no:02}");
                            no += 1;
                        }
                    }
                });
                publish_now();
            }
            "orch-task" => {
                if let Some(task) = with_pro(|st| st.run_task.clone()) {
                    let config = config_dir();
                    spawn_task_poll(weak.clone(), task, false);
                    let _ = config;
                } else {
                    with_pro(|st| say(st, "当前没有进行中的导演任务。", "info"));
                    publish_now();
                }
            }
            "orch-download-film" => {
                let url = with_pro(|st| st.film_url.clone());
                download_to(weak.clone(), url, "导演成片", "导演成片.mp4");
            }
            "orch-download-pack" => {
                let url = with_pro(|st| st.pack_url.clone());
                download_to(weak.clone(), url, "导演工程包", "导演工程包.zip");
            }
            "orch-open-film" => {
                let url = with_pro(|st| st.film_url.clone());
                if let Err(error) = platform::open_link(&url) {
                    with_pro(|st| say(st, format!("打开成片地址失败：{error}"), "fail"));
                    publish_now();
                }
            }
            _ => {}
        },
        TAB_MONITOR => match action.as_str() {
            "mon-load" => load_board(weak.clone()),
            "mon-types" => load_monitor_types(weak.clone()),
            "mon-type-pick" => load_type_detail(weak.clone(), index.max(0) as usize),
            "mon-shot-pick" => load_shot(weak.clone(), index.max(0) as usize),
            "mon-progress" => load_monitor_progress(weak.clone()),
            "mon-cont" => load_continuity(weak.clone(), false),
            "mon-cont-run" => load_continuity(weak.clone(), true),
            "mon-retakes" => load_retakes(weak.clone()),
            "mon-audit" => load_audit(weak.clone()),
            "mon-retake-tier" => {
                with_pro(|st| st.retake_tier = index.clamp(0, 2));
                publish_now();
            }
            "mon-take-add" | "mon-take-decision" | "mon-note-add" | "mon-note-resolve"
            | "mon-retake" | "mon-retake-reason" | "mon-take-url" | "mon-note-text"
            | "mon-retake-prompt" => {
                shot_action(weak.clone(), action.as_str(), index, &text);
            }
            _ => {}
        },
        TAB_DIST => match action.as_str() {
            "dist-load" => load_distribution(weak.clone()),
            "dist-drama" => {
                with_pro(|st| st.drama_index = index.max(0));
                publish_now();
            }
            "dist-platform" => {
                with_pro(|st| st.platform = index.clamp(0, 5));
                publish_now();
            }
            "dist-lang" => {
                with_pro(|st| st.lang = index.clamp(0, 4));
                publish_now();
            }
            "dist-make" => make_distribution(weak.clone()),
            "dist-pick" => {
                with_pro(|st| {
                    st.dist_pick = index.max(0);
                    if let Some(dist) = st.dists.get(index.max(0) as usize) {
                        st.dist_url = text_of(dist, &["zip_url", "download_url"]);
                        st.dist_views = (num_of(dist, &["views"]) as i64).to_string();
                        st.dist_likes = (num_of(dist, &["likes"]) as i64).to_string();
                        st.dist_conversions = (num_of(dist, &["conversions"]) as i64).to_string();
                        st.dist_publish_url = text_of(dist, &["publish_url"]);
                        st.dist_files.clear();
                        if let Some(files) = dist.get("files").and_then(Value::as_array) {
                            for file in files.iter().take(24) {
                                st.dist_files.push(Row {
                                    label: text_of(file, &["name"]),
                                    detail: format!(
                                        "{} · {}",
                                        text_of(file, &["type"]),
                                        num_of(file, &["size"]) as i64
                                    ),
                                    value: String::new(),
                                    level: "".to_owned(),
                                });
                            }
                        }
                        st.dist_pick = index.max(0);
                        st.dist_line = format!(
                            "已选发行记录：{} · {} · {} 个文件",
                            text_of(dist, &["drama_title", "title"]),
                            text_of(dist, &["platform"]),
                            num_of(dist, &["file_count"]) as i64
                        );
                    }
                });
                publish_now();
            }
            "dist-download" => {
                let url = with_pro(|st| st.dist_url.clone());
                download_to(weak.clone(), url, "发行包", "发行包.zip");
            }
            "dist-open-url" => {
                let url = with_pro(|st| st.dist_url.clone());
                let _ = platform::open_link(&url);
            }
            "dist-stats" => fill_stats(weak.clone()),
            _ => {}
        },
        TAB_PASS => match action.as_str() {
            "pass-mode" => {
                with_pro(|st| st.pass_mode = index.clamp(0, 2));
                publish_now();
            }
            "pass-load" => load_passport(weak.clone()),
            "pass-gen-asset" => gen_asset_passport(weak.clone()),
            "pass-gen-project" => gen_project_passport(weak.clone()),
            "pass-verify" => verify_passport(weak.clone()),
            "pass-download-zip" => {
                let url = with_pro(|st| st.pass_zip_url.clone());
                download_to(weak.clone(), url, "护照", "护照包.zip");
            }
            "pass-open-json" => {
                let path = with_pro(|st| st.pass_json_path.clone());
                if !path.is_empty() {
                    let _ = platform::reveal(&path);
                }
            }
            "pass-open-verify" => {
                let id = with_pro(|st| {
                    if st.pass_verify_id.trim().is_empty() {
                        st.pass_id.clone()
                    } else {
                        st.pass_verify_id.trim().to_owned()
                    }
                });
                let url = format!("{}/ai-nav/api/passport/verify/{}", concat_host::pro::SITE, id);
                let _ = platform::open_link(&url);
            }
            _ => {}
        },
        TAB_SANDBOX => match action.as_str() {
            "sb-type" => {
                with_pro(|st| st.sb_type = index.clamp(0, 4));
                publish_now();
            }
            "sb-platform" => {
                with_pro(|st| st.sb_platform = index.clamp(0, 3));
                publish_now();
            }
            "sb-ai" => {
                with_pro(|st| st.sb_ai = !st.sb_ai);
                publish_now();
            }
            "sb-check" => run_sandbox(weak.clone(), false),
            "sb-recheck" => run_sandbox(weak.clone(), true),
            "sb-history" => load_sandbox(weak.clone()),
            _ => {}
        },
        TAB_PLUGIN => match action.as_str() {
            "plug-load" => load_plugins(weak.clone()),
            "plug-pick-market" => plugin_action(weak.clone(), "pick-market", index.max(0) as usize, &text),
            "plug-pick-installed" => plugin_action(weak.clone(), "pick-installed", 0, &text),
            "plug-install" | "plug-enable" | "plug-disable" | "plug-config-load"
            | "plug-config-save" | "plug-run" | "plug-runs" => {
                plugin_action(weak.clone(), action.trim_start_matches("plug-"), index.max(0) as usize, &text)
            }
            _ => {}
        },
        TAB_TEMPLATE => match action.as_str() {
            "tpl-load" => load_templates(weak.clone()),
            "tpl-industry" => {
                with_pro(|st| st.tpl_industry = index.max(0));
                load_templates(weak.clone());
            }
            "tpl-scene" => {
                with_pro(|st| st.tpl_scene = index.max(0));
                load_templates(weak.clone());
            }
            "tpl-pick" => pick_template(weak.clone(), index.max(0) as usize),
            "tpl-duration" => {
                with_pro(|st| st.tpl_duration = index.clamp(0, 1));
                publish_now();
            }
            "tpl-quality" => {
                with_pro(|st| st.tpl_quality = index.clamp(0, 1));
                publish_now();
            }
            "tpl-use" => template_use(weak.clone()),
            "tpl-poll" => {
                if let Some(task) = with_pro(|st| st.tpl_task.clone()) {
                    spawn_task_poll(weak.clone(), task, true);
                }
            }
            "tpl-download" => {
                let url = with_pro(|st| {
                    if st.tpl_download_url.is_empty() {
                        st.tpl_video_url.clone()
                    } else {
                        st.tpl_download_url.clone()
                    }
                });
                download_to(weak.clone(), url, "模板成片", "模板成片.mp4");
            }
            "tpl-open-video" => {
                let url = with_pro(|st| st.tpl_video_url.clone());
                let _ = platform::open_link(&url);
            }
            "tpl-fav" => {
                let (id, on) = with_pro(|st| (st.tpl_selected.clone(), !st.tpl_fav));
                with_pro(|st| st.tpl_fav = on);
                publish_now();
                host::spawn(
                    move || concat_host::pro::template_favorite(&id, on),
                    |_studio, _app, _m, _result| {},
                );
            }
            _ => {}
        },
        TAB_TASKS => match action.as_str() {
            "task-refresh" => refresh_tasks(weak.clone(), true),
            "task-open" => task_action(weak.clone(), index.max(0) as usize, "open"),
            "task-cancel" => task_action(weak.clone(), index.max(0) as usize, "cancel"),
            "task-remove" => task_action(weak.clone(), index.max(0) as usize, "remove"),
            _ => {}
        },
        _ => {}
    }
}

fn assets_line_now() {
    with_pro(|st| {
        st.asset_line =
            "素材库：搜索 / 行业 / 类型 / 用途 / 授权角标 / 下载 / 插入时间线，全部走已上线的 /api/assets/*。"
                .to_owned();
        st.hint = hint_of(TAB_ASSETS);
    });
    publish_now();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_formats_small_and_large() {
        assert_eq!(money(0.0), "¥0");
        assert_eq!(money(0.85), "¥0.85");
        assert_eq!(money(18.562), "¥18.56");
    }

    #[test]
    fn pct_clamps() {
        assert_eq!(pct(-1.0), "0%");
        assert_eq!(pct(150.0), "100%");
        assert_eq!(pct(42.4), "42%");
    }

    #[test]
    fn default_script_has_three_shots() {
        let st = ProState::default();
        assert_eq!(st.text_shots.len(), 3);
        assert_eq!(st.text_shots[1].line, "这张照片……哪来的？");
    }
}
