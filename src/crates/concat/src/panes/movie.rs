// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 电影 / 剧集制作向导（0.3.5）：15 步「傻瓜式」制作流程的窗口这一半。
//!
//! 流程照真实剧组的制作 SOP 排：类型 → 一句话 → 剧本 → 角色 → 场景/服化道 →
//! 分镜 → 预演 → 档位/预算 → 生成 → 审片 → 调色 → 声音 → 字幕/演职人员表 →
//! 成片 → 交付。简单模式只填一句话，一键全流程；专业模式每一步都能改。
//!
//! 两条生成链（都在 [`concat_host::movie`] 里，这里只做编排与落屏）：
//! - **导演编排器**：按镜头路由模型，逐镜进度、断点续跑；「改 1 个角色 / 1 个
//!   分镜只重跑受影响镜头」用它——DAG 的脏镜头集合算好后传 `shot_nos`。
//! - **电影成片管线**：2.39:1 遮幅 / LUT / 声音设计 / 片头尾+演职人员表 / QC /
//!   工程包，用 `/movie/plan`（规划免费）+ `/movie/render`。
//!
//! 面板不在启动路径上：打开时才取 9 类类型表一次；任务轮询只在有任务时跑。
//! 失败一律中文一句话；平台做不到的（暂停运行中任务、单镜重拍电影管线）如实说。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_host::movie::{self, MovieType};
use concat_project::Command;
use serde_json::{json, Value};

use crate::host;
use crate::panes::Msg;
use crate::studio::Studio;
use crate::ui::MovieData;

/// 15 步：面板左栏的名字。
const STEP_LABELS: [&str; 15] = [
    "类型与规格",
    "一句话 premise",
    "剧本定稿",
    "角色设定",
    "场景 / 服化道",
    "分镜表",
    "预演",
    "模型档位与预算",
    "生成（逐镜）",
    "审片（take 板）",
    "调色",
    "声音",
    "字幕 / 演职人员表",
    "成片预览",
    "交付",
];

/// 真实剧组的叫法 + 一句说明（教育性：让客户觉得专业）。
const STEP_NAMES: [(&str, &str); 15] = [
    ("立项 / 定规格", "先定片种与交付规格：画幅、帧率、调色倾向、镜头语言配比与响度都以片种为默认值。"),
    ("一句话故事 / pitch", "制片人给导演的一句话卖点；平台按片种把它扩成 15–30 秒的电影化分镜。"),
    ("剧本定稿 / 分场", "编剧与导演把每一镜的画面与台词定稿；这是后面所有工序的图纸。"),
    ("选角 / 定妆", "角色卡的外貌写进每一镜的提示词，定妆照作为参考图保证同一张脸。"),
    ("堪景 / 美术 & 服化道", "场景与整体质感统一；场景卡由分镜自动归类，也可逐张改。"),
    ("故事板 / shot list", "一镜一行：景别、运镜、时长、台词；现场照它拍，后期照它剪。"),
    ("动态预演 / previz", "先用低清快速版本确认节奏与镜头关系，再出正式成片。"),
    ("预算会 / 排产", "按档位、镜头数、对白镜与口型估成本；超预算先调预算或降档。"),
    ("拍摄 / 制作进行", "逐镜生成，每一镜都有状态；2C2G 单机串行，可离开页面等它跑。"),
    ("看样片 / take 会", "通过 / 退回 / 批注；退回的镜头只重拍这一镜，别的镜头不动。"),
    ("调色 / DI", "LUT、曲线、对比饱和与颗粒；正式调色在桌面调色页逐镜精调、示波器监看。"),
    ("声音设计 / 混录", "对白配音、环境底噪、音效、BGM 与 ducking，最后按交付标准做响度。"),
    ("字幕 / 上片尾", "烧录字幕 + SRT 文件；片尾演职人员表与「AI 生成」标识按合规要求写。"),
    ("看片 / 终混确认", "横版 2.39:1 与竖版 9:16 各一条；预览、下载、丢进时间轴逐帧看。"),
    ("交付 / DIT 交接", "成片、工程包、QC、EDL/OTIO、字幕 SRT 一起交；工程包可直接导回桌面改。"),
];

/// LUT 的机器名（与电影模式预设 `lut_suggestions` 一致）。
const LUTS: [(&str, &str); 7] = [
    ("青橙电影", "teal_orange_film"),
    ("柯达 2383", "kodak_2383"),
    ("冷夜", "cool_night"),
    ("暖剧", "warm_drama"),
    ("漂白旁路", "bleach_bypass"),
    ("干净品牌", "neutral"),
    ("不调色", "neutral"),
];

/// 档位机器名。
const TIERS: [&str; 3] = ["standard", "pro", "master"];
/// 档位目录价（元/镜，用于预算预估；实际以平台账单为准）。
const TIER_PER_SHOT: [f64; 3] = [0.85, 0.85, 3.05];

/// 一句话示例。
const SAMPLES: [&str; 3] = [
    "都市情感：加班到凌晨的女主，在天台遇到同样没回家的邻居，两个人聊起了留下还是离开。",
    "悬疑：深夜值班的保安在监控里看见一个不存在的人，走廊的灯一盏盏熄灭。",
    "品牌广告：一双跑鞋从清晨的街道跑进山里，最后停在日出前的山顶。",
];

/// 一镜的分镜数据（面板可改）。
#[derive(Clone, Debug, Default)]
struct Shot {
    no: u32,
    id: String,
    size: String,
    camera: String,
    duration: f64,
    scene: String,
    action: String,
    line_who: String,
    line_text: String,
    characters: Vec<String>,
    lighting: String,
    sound: String,
    color: String,
    prompt_image: String,
    prompt_video: String,
}

impl Shot {
    fn from_plan(item: &Value, index: usize) -> Shot {
        let line = item.get("line");
        let who = line
            .and_then(|l| l.get("who"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let text = line
            .and_then(|l| l.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Shot {
            no: item
                .get("no")
                .and_then(Value::as_u64)
                .map(|n| n as u32)
                .unwrap_or((index + 1) as u32),
            id: item
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("S{}", index + 1)),
            size: item
                .get("size")
                .or_else(|| item.get("shot_type"))
                .and_then(Value::as_str)
                .unwrap_or("中景")
                .to_owned(),
            camera: item
                .get("camera_move")
                .and_then(Value::as_str)
                .unwrap_or("固定")
                .to_owned(),
            duration: item
                .get("duration")
                .and_then(Value::as_f64)
                .unwrap_or(5.0),
            scene: item
                .get("scene")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            action: item
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            line_who: who,
            line_text: text,
            characters: item
                .get("characters")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            lighting: item
                .get("lighting")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            sound: item
                .get("sound_design")
                .map(|s| {
                    let ambience = s.get("ambience").and_then(Value::as_str).unwrap_or("");
                    let cue = s.get("music_cue").and_then(Value::as_str).unwrap_or("");
                    format!("环境 {ambience} · 配乐 {cue}")
                })
                .unwrap_or_default(),
            color: item
                .get("color_intent")
                .and_then(|c| c.get("intent"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            prompt_image: item
                .get("prompt")
                .and_then(|p| p.get("image"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            prompt_video: item
                .get("prompt")
                .and_then(|p| p.get("video"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }
    }

    /// 一镜写给导演编排器的 scene 文本（含表演与光线）。
    fn director_scene(&self) -> String {
        let mut parts = vec![self.scene.clone()];
        if !self.action.trim().is_empty() {
            parts.push(self.action.trim().to_owned());
        }
        if !self.lighting.trim().is_empty() {
            parts.push(format!("光线：{}", self.lighting.trim()));
        }
        parts.join("；")
    }

    /// 分镜表的一行。
    fn row(&self) -> String {
        let line = if self.line_text.trim().is_empty() {
            String::new()
        } else {
            format!(" · 台词「{}」", self.line_text.trim())
        };
        format!(
            "S{} · {} · {} · {:.1}s{}",
            self.no, self.size, self.camera, self.duration, line
        )
    }

    /// 给剧情预览的一行。
    fn card(&self) -> String {
        format!(
            "▍S{} {}｜{}\n{}\n{}",
            self.no,
            self.size,
            self.camera,
            if self.scene.trim().is_empty() {
                "（未写画面）"
            } else {
                self.scene.trim()
            },
            if self.line_text.trim().is_empty() {
                "（无对白）".to_owned()
            } else {
                format!("「{}」— {}", self.line_text.trim(), self.line_who)
            }
        )
    }

    /// 给导演编排器的镜头路由类型。
    fn director_type(&self) -> &'static str {
        match self.size.as_str() {
            "特写" | "大特写" => "人物特写",
            "近景" | "中近景" => "对话",
            "全景" | "远景" | "大远景" | "空镜" => "空镜",
            _ => "场景",
        }
    }
}

/// 角色卡。
#[derive(Clone, Debug, Default)]
struct Character {
    id: String,
    name: String,
    look: String,
    voice: String,
    age: i32,
}

impl Character {
    fn row(&self) -> String {
        let mut parts = vec![self.name.clone()];
        if !self.look.trim().is_empty() {
            parts.push(self.look.trim().to_owned());
        }
        if !self.voice.trim().is_empty() {
            parts.push(format!("音色 {}", self.voice.trim()));
        }
        parts.join(" · ")
    }

    fn from_plan(item: &Value, index: usize) -> Character {
        Character {
            id: item
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("C{}", index + 1)),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("未命名角色")
                .to_owned(),
            look: item
                .get("look")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            voice: item
                .get("voice")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            age: item.get("age").and_then(Value::as_i64).unwrap_or(0) as i32,
        }
    }
}

/// 场景卡：由分镜的 scene 归类，记录哪些镜头用到它。
#[derive(Clone, Debug, Default)]
struct SceneCard {
    text: String,
    shots: Vec<usize>,
}

/// 一次生成任务（两条链共用一套落屏字段）。
#[derive(Clone, Debug, Default)]
struct TaskState {
    kind: String,
    task_id: String,
    status: String,
    stage: String,
    progress: f32,
    shots: Vec<Value>,
    film_url: String,
    vertical_url: String,
    pack_url: String,
    pack_id: String,
    pack_download_url: String,
    pack_size: u64,
    qc_line: String,
    cost_line: String,
    interop_line: String,
    warnings: Vec<String>,
    done: bool,
}

/// 面板消息。
#[derive(Debug)]
pub enum MovieMsg {
    /// 打开（step 0..14）。
    Open(i32),
    Close,
    /// 界面上的一次动作。
    Action {
        action: String,
        index: i32,
        text: String,
    },
    /// 9 类类型表回来了。
    Types(Box<Result<Vec<MovieType>, String>>),
    /// 分镜方案回来了。
    Planned(Box<Result<Value, String>>),
    /// 任务提交回执（director/run 或 movie/render）。
    Submitted { kind: String, result: Box<Result<Value, String>> },
    /// 轮询到一次任务状态。
    Poll { kind: String, result: Box<Result<Value, String>> },
    /// 一个下载好了（成片 / 工程包）。
    Downloaded { kind: String, result: Box<Result<PathBuf, String>> },
    /// 工程包导入完了。
    Imported(Box<Result<concat_host::projectpack::ImportedPack, String>>),
    /// 后台一句告知。
    Toast(String),
}

/// 电影 / 剧集向导的面板状态。
pub struct MoviePane {
    pub open: bool,
    step: i32,
    simple: bool,
    busy: bool,
    progress: f32,
    stage: String,
    message: String,
    last_alert: String,
    signed_in: bool,
    admin: bool,
    account_line: String,
    // 类型
    types: Vec<MovieType>,
    type_index: i32,
    loading_types: bool,
    // 一句话 / 剧本
    premise: String,
    title: String,
    logline: String,
    plan: Option<Value>,
    plan_id: String,
    auto_run: bool,
    // 分镜与角色
    shots: Vec<Shot>,
    shot_index: usize,
    characters: Vec<Character>,
    char_index: usize,
    scenes: Vec<SceneCard>,
    scene_index: usize,
    wardrobe: String,
    // 档位 / 预算 / 引擎
    tier: i32,
    budget: String,
    engine: i32,
    estimate_director: String,
    estimate_movie: String,
    // 生成任务
    task: Option<TaskState>,
    cancel: Option<Arc<AtomicBool>>,
    // 审片
    take_index: usize,
    review: Vec<String>,
    notes: Vec<String>,
    take_note: String,
    dirty_shots: Vec<u32>,
    // 调色 / 声音 / 字幕
    lut: i32,
    contrast: String,
    saturation: String,
    grain: i32,
    music: bool,
    lipsync: bool,
    loudness: i32,
    subtitles: bool,
    ai_mark: bool,
    cast: String,
    crew: String,
    // 成片 / 交付
    film_local: Option<PathBuf>,
    pack_local: Option<PathBuf>,
}

impl Default for MoviePane {
    fn default() -> Self {
        Self {
            open: false,
            step: 0,
            simple: true,
            busy: false,
            progress: 0.0,
            stage: String::new(),
            message: String::new(),
            last_alert: String::new(),
            signed_in: false,
            admin: false,
            account_line: "未登录：云端能力需先登录星籁账号".to_owned(),
            types: Vec::new(),
            type_index: 0,
            loading_types: false,
            premise: String::new(),
            title: String::new(),
            logline: String::new(),
            plan: None,
            plan_id: String::new(),
            auto_run: false,
            shots: Vec::new(),
            shot_index: 0,
            characters: Vec::new(),
            char_index: 0,
            scenes: Vec::new(),
            scene_index: 0,
            wardrobe: "电影级写实，宽银幕，胶片颗粒，浅景深；服装与道具按角色身份统一".to_owned(),
            tier: 0,
            budget: "25".to_owned(),
            engine: 0,
            estimate_director: "先写一句话并生成分镜，这里会显示预估".to_owned(),
            estimate_movie: String::new(),
            task: None,
            cancel: None,
            take_index: 0,
            review: Vec::new(),
            notes: Vec::new(),
            take_note: String::new(),
            dirty_shots: Vec::new(),
            lut: 0,
            contrast: "1.05".to_owned(),
            saturation: "1.0".to_owned(),
            grain: 4,
            music: true,
            lipsync: true,
            loudness: 0,
            subtitles: true,
            ai_mark: true,
            cast: String::new(),
            crew: "导演 / 总编排 = 星籁电影向导（AI 生成 + 人工确认）\n编剧 = AI 剧本编排（DeepSeek）\n调色 / 剪辑 / 声音设计 = 星籁电影模式工程管线".to_owned(),
            film_local: None,
            pack_local: None,
        }
    }
}

impl MoviePane {
    /// 打开（或切到某一步）。
    pub fn open(&mut self, step: i32, _studio: &mut Studio) {
        self.open = true;
        self.step = step.clamp(0, 14);
        self.message.clear();
        self.refresh_account();
        if self.types.is_empty() && !self.loading_types {
            self.load_types();
        }
    }

    fn refresh_account(&mut self) {
        match concat_host::account::load() {
            Some(account) if !account.token.trim().is_empty() => {
                self.signed_in = true;
                self.admin = account.is_admin;
                self.account_line = format!(
                    "已登录：{} · {}",
                    account.display_name(),
                    account.plan_label()
                );
            }
            _ => {
                self.signed_in = false;
                self.admin = false;
                self.account_line = "未登录：云端能力需先登录星籁账号".to_owned();
            }
        }
    }

    /// 处理一条消息。
    pub fn update(&mut self, msg: MovieMsg, studio: &mut Studio) {
        match msg {
            MovieMsg::Open(step) => self.open(step, studio),
            MovieMsg::Close => {
                self.open = false;
                self.message.clear();
            }
            MovieMsg::Action {
                action,
                index,
                text,
            } => self.action(&action, index, text, studio),
            MovieMsg::Types(result) => {
                self.loading_types = false;
                match *result {
                    Ok(types) => {
                        self.types = types;
                        self.type_index = self.type_index.clamp(0, (self.types.len() as i32 - 1).max(0));
                    }
                    Err(error) => {
                        self.message = error;
                        if self.last_alert != self.message {
                            self.last_alert.clone_from(&self.message);
                            studio.notify(&self.message, true);
                        }
                    }
                }
            }
            MovieMsg::Planned(result) => self.planned(*result, studio),
            MovieMsg::Submitted { kind, result } => {
                self.busy = false;
                match *result {
                    Ok(value) => {
                        let mut task = TaskState {
                            kind: kind.clone(),
                            ..Default::default()
                        };
                        task.task_id = value
                            .get("task_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        task.status = value
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("queued")
                            .to_owned();
                        task.stage = value
                            .get("stage")
                            .and_then(Value::as_str)
                            .or_else(|| value.get("message").and_then(Value::as_str))
                            .unwrap_or("已提交")
                            .to_owned();
                        task.progress = 0.03;
                        self.task = Some(task);
                        self.review.clear();
                        self.notes.clear();
                        self.take_index = 0;
                        self.dirty_shots.clear();
                        self.stage = "已提交，等待平台开始…".to_owned();
                        self.progress = 0.03;
                        self.message = format!(
                            "任务已提交（{}）：2C2G 单机串行，30 秒短片约 8–20 分钟；自动轮询进度，也可以「停止等待」后回来看。",
                            if kind == "movie" { "电影成片管线" } else { "导演编排器" }
                        );
                        self.poll_task(studio);
                    }
                    Err(error) => {
                        self.stage.clear();
                        self.message = error;
                        self.alert(studio);
                    }
                }
            }
            MovieMsg::Poll { kind, result } => {
                self.handle_poll(kind, *result, studio);
            }
            MovieMsg::Downloaded { kind, result } => {
                match *result {
                    Ok(path) => {
                        if kind == "film" {
                            self.film_local = Some(path.clone());
                            self.insert_film(&path, studio);
                        } else {
                            self.pack_local = Some(path.clone());
                            self.message = format!("工程包已下载：{}", path.display());
                        }
                    }
                    Err(error) => {
                        self.message = error;
                        self.alert(studio);
                    }
                }
                self.busy = false;
            }
            MovieMsg::Imported(result) => match *result {
                Ok(imported) => {
                    self.message = imported.summary();
                    let path = imported.project_path.clone();
                    match studio.open_project_at(&path) {
                        Ok(()) => {
                            self.message = format!("{}；已在编辑器打开。", self.message);
                            self.open = false;
                            studio.notify("AI 工程包已导入并打开（时间线可播）", false);
                        }
                        Err(error) => {
                            self.message = format!("工程包已解压到 {path}，但打开失败：{error}");
                            self.alert(studio);
                        }
                    }
                }
                Err(error) => {
                    self.message = error;
                    self.alert(studio);
                }
            },
            MovieMsg::Toast(text) => {
                self.message = text;
            }
        }
    }

    fn alert(&mut self, studio: &mut Studio) {
        if self.last_alert != self.message {
            self.last_alert.clone_from(&self.message);
            studio.notify(&self.message, true);
        }
    }

    fn type_of(&self) -> Option<&MovieType> {
        self.types.get(self.type_index.max(0) as usize)
    }

    fn type_id(&self) -> String {
        self.type_of()
            .map(|t| t.id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "film".to_owned())
    }

    fn type_label(&self) -> String {
        self.type_of()
            .map(|t| t.label.clone())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| "电影".to_owned())
    }

    fn tier_id(&self) -> &'static str {
        TIERS[self.tier.max(0) as usize % TIERS.len()]
    }

    /// 导演编排器用的画幅：它只有 16:9 / 9:16 / 1:1 等，2.39:1 归到 16:9。
    fn director_aspect(&self) -> &'static str {
        match self.type_of().map(|t| t.aspect.as_str()).unwrap_or("16:9") {
            "9:16" => "9:16",
            _ => "16:9",
        }
    }

    fn selected_shot(&self) -> Option<&Shot> {
        self.shots.get(self.shot_index)
    }

    fn selected_shot_mut(&mut self) -> Option<&mut Shot> {
        self.shots.get_mut(self.shot_index)
    }

    fn selected_char(&self) -> Option<&Character> {
        self.characters.get(self.char_index)
    }

    fn selected_char_mut(&mut self) -> Option<&mut Character> {
        self.characters.get_mut(self.char_index)
    }

    /// 场景卡：按分镜的 scene 文本归类（保留首次出现顺序与镜头下标）。
    fn rebuild_scenes(&mut self) {
        let mut cards: Vec<SceneCard> = Vec::new();
        for (at, shot) in self.shots.iter().enumerate() {
            let text = shot.scene.trim().to_owned();
            if text.is_empty() {
                continue;
            }
            if let Some(card) = cards.iter_mut().find(|card| card.text == text) {
                card.shots.push(at);
            } else {
                cards.push(SceneCard {
                    text,
                    shots: vec![at],
                });
            }
        }
        self.scene_index = self.scene_index.min(cards.len().saturating_sub(1));
        self.scenes = cards;
    }

    /// 脏镜头：改角色 → 用到该角色的镜头；改一镜 → 这一镜。
    fn mark_all_shots_dirty(&mut self) {
        self.dirty_shots = self.shots.iter().map(|shot| shot.no).collect();
    }

    /// 一句「下游受影响」的说明。
    fn dirty_line(&self) -> String {
        if self.plan.is_none() {
            return "还没有分镜：先生成方案，「只重跑下游」从分镜定稿之后才开始计算。".to_owned();
        }
        if self.dirty_shots.is_empty() {
            return "当前没有改脏的镜头：改角色 / 分镜 / 场景后，这里会列出只重跑哪些镜。".to_owned();
        }
        let names = self
            .dirty_shots
            .iter()
            .map(|no| format!("S{no}"))
            .collect::<Vec<_>>()
            .join("、");
        format!(
            "DAG：下游受影响 {} 镜（{}）。重拍时只把这些镜头传给平台（shot_nos），其余镜头与成片结构不动。",
            self.dirty_shots.len(),
            names
        )
    }

    /// 取 9 类类型表（打开面板才取一次，不在启动路径上）。
    fn load_types(&mut self) {
        self.loading_types = true;
        host::spawn(
            move || movie::types(),
            |studio, _, _, result| {
                studio.handle(Msg::Movie(MovieMsg::Types(Box::new(result))));
            },
        );
    }

    /// 分镜方案回来了。
    fn planned(&mut self, result: Result<Value, String>, studio: &mut Studio) {
        self.busy = false;
        self.progress = 0.0;
        self.stage.clear();
        match result {
            Ok(value) => {
                self.plan_id = value
                    .get("plan_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                self.title = value
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("未命名影片")
                    .to_owned();
                self.logline = value
                    .get("logline")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let shots = value
                    .get("shots")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut drafts: Vec<Shot> = shots
                    .iter()
                    .enumerate()
                    .map(|(at, item)| Shot::from_plan(item, at))
                    .collect();
                let mut trimmed = false;
                // 简单模式按 15 秒目标取前 3 镜（专业模式里改回来）。
                if self.simple && drafts.len() > 3 {
                    drafts.truncate(3);
                    for (at, shot) in drafts.iter_mut().enumerate() {
                        shot.no = (at + 1) as u32;
                        shot.id = format!("S{}", at + 1);
                    }
                    trimmed = true;
                }
                let characters = value
                    .get("characters")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                self.characters = characters
                    .iter()
                    .enumerate()
                    .map(|(at, item)| Character::from_plan(item, at))
                    .collect();
                if self.characters.is_empty() {
                    self.characters = drafts
                        .iter()
                        .flat_map(|shot| shot.characters.clone())
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .enumerate()
                        .map(|(at, id)| Character {
                            id: id.clone(),
                            name: id,
                            ..Default::default()
                        })
                        .collect();
                }
                self.shots = drafts;
                self.shot_index = 0;
                self.char_index = 0;
                self.rebuild_scenes();
                self.scene_index = 0;
                self.shots_len_sync();
                self.fill_credits(&value);
                self.recompute_estimates(&value);
                let total_dur: f64 = self.shots.iter().map(|shot| shot.duration).sum();
                self.plan = Some(value);
                self.message = format!(
                    "分镜方案就绪：{} 镜 · 约 {:.0} 秒{}。往下逐项改，或直接去「生成」。",
                    self.shots.len(),
                    total_dur,
                    if trimmed {
                        "（简单模式按 15 秒目标取了前 3 镜；专业模式可加回）"
                    } else {
                        ""
                    }
                );
                if self.auto_run {
                    self.auto_run = false;
                    self.step = 8;
                    self.run(studio);
                } else {
                    studio.notify("分镜方案已生成（规划免费，生成才计费）", false);
                }
            }
            Err(error) => {
                self.auto_run = false;
                self.message = error;
                self.alert(studio);
            }
        }
    }

    fn shots_len_sync(&mut self) {
        if self.shots.is_empty() {
            self.shot_index = 0;
        } else {
            self.shot_index = self.shot_index.min(self.shots.len() - 1);
        }
        if self.characters.is_empty() {
            self.char_index = 0;
        } else {
            self.char_index = self.char_index.min(self.characters.len() - 1);
        }
    }

    /// 演职人员表默认值。
    fn fill_credits(&mut self, plan: &Value) {
        if !self.cast.trim().is_empty() {
            return;
        }
        let mut lines = Vec::new();
        if let Some(rows) = plan
            .get("credits")
            .and_then(|c| c.get("cast"))
            .and_then(Value::as_array)
        {
            for row in rows {
                let role = row.get("role").and_then(Value::as_str).unwrap_or("主演");
                let name = row.get("name").and_then(Value::as_str).unwrap_or("");
                if !name.is_empty() {
                    lines.push(format!("{role} = {name}"));
                }
            }
        }
        if lines.is_empty() {
            for character in &self.characters {
                lines.push(format!("主演 = {}（AI 生成角色）", character.name));
            }
        }
        self.cast = lines.join("\n");
    }

    /// 预估成本：两条链各给一句。
    fn recompute_estimates(&mut self, plan: &Value) {
        let per_shot = TIER_PER_SHOT[self.tier.max(0) as usize % TIER_PER_SHOT.len()];
        let dialogue = self
            .shots
            .iter()
            .filter(|shot| !shot.line_text.trim().is_empty())
            .count();
        let director = self.shots.len() as f64 * per_shot
            + if self.tier >= 2 {
                dialogue as f64 * 0.4
            } else {
                0.0
            };
        self.estimate_director = format!(
            "导演编排器：约 ¥{director:.2}（{} 镜 × ¥{per_shot:.2}{}）· 预估 {:.0} 积分",
            self.shots.len(),
            if self.tier >= 2 {
                " + 对白口型"
            } else {
                ""
            },
            director.ceil()
        );
        let movie_est = plan
            .get("cost_estimate")
            .and_then(|c| c.get("total_cny"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        self.estimate_movie = if movie_est > 0.0 {
            format!(
                "电影成片管线：约 ¥{movie_est:.2}（规划给出的 6 镜全套估算；裁成 {} 镜后按平台实际账单）· 预估 {:.0} 积分",
                self.shots.len(),
                movie_est.ceil()
            )
        } else {
            "电影成片管线：方案就绪后给估算".to_owned()
        };
    }

    /// 提交生成。
    fn run(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        if !self.signed_in {
            self.message = "请先登录星籁账号：登录后回到这里再生成".to_owned();
            self.alert(studio);
            return;
        }
        if self.shots.is_empty() {
            self.message = "还没有分镜：先在「一句话 premise」生成分镜方案".to_owned();
            self.alert(studio);
            return;
        }
        if self.premise.trim().is_empty() {
            self.message = "先写一句 premise".to_owned();
            self.alert(studio);
            return;
        }
        let budget: f64 = self.budget.trim().parse().unwrap_or(25.0);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.busy = true;
        self.progress = 0.01;
        self.stage = "提交任务…".to_owned();
        self.review.clear();
        self.notes.clear();
        self.take_index = 0;
        self.dirty_shots.clear();
        if self.engine == 0 {
            let body = self.director_payload(budget);
            self.message = "已提交导演编排器：逐镜生成，可断点续跑 / 单镜重拍。".to_owned();
            host::spawn(
                move || movie::director_run(&body),
                |studio, _, _, result| {
                    studio.handle(Msg::Movie(MovieMsg::Submitted {
                        kind: "director".to_owned(),
                        result: Box::new(result),
                    }));
                },
            );
        } else {
            let body = self.movie_plan_payload(budget);
            self.message = "已提交电影成片管线：先出 2.39:1 分镜方案，再渲染成片+工程包。".to_owned();
            host::spawn(
                move || -> Result<Value, String> {
                    let plan = movie::plan(&body)?;
                    let plan_id = plan
                        .get("plan_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let mut render = json!({
                        "plan_id": plan_id,
                        "budget_cny": budget,
                        "vertical": true,
                        "ai_watermark": true,
                    });
                    let _ = &mut render;
                    movie::render(&render)
                },
                |studio, _, _, result| {
                    studio.handle(Msg::Movie(MovieMsg::Submitted {
                        kind: "movie".to_owned(),
                        result: Box::new(result),
                    }));
                },
            );
        }
        let _ = cancel;
    }

    /// 导演编排器入参：把可改的分镜/角色/场景翻成导演契约。
    fn director_payload(&self, budget: f64) -> Value {
        let style = format!(
            "{}{}",
            self.type_label(),
            if self.wardrobe.trim().is_empty() {
                String::new()
            } else {
                format!("；{}", self.wardrobe.trim())
            }
        );
        let mut shots = Vec::new();
        for shot in &self.shots {
            let mut scene = shot.director_scene();
            // 角色外貌并进 scene 提示词，改角色才真的影响画面。
            for id in &shot.characters {
                if let Some(character) = self.characters.iter().find(|c| &c.id == id) {
                    scene.push_str(&format!("；角色 {}：{}", character.name, character.look));
                }
            }
            shots.push(json!({
                "scene": scene,
                "line": shot.line_text.trim(),
                "dur": shot.duration,
                "type": shot.director_type(),
            }));
        }
        json!({
            "tier": self.tier_id(),
            "budget_cny": budget,
            "aspect": self.director_aspect(),
            "batch_size": 6,
            "script": {
                "title": self.title,
                "style": style,
                "shots": shots,
            },
        })
    }

    /// 电影成片管线规划入参（分镜已定稿，直接传 script，不再让人等一次大模型）。
    fn movie_plan_payload(&self, budget: f64) -> Value {
        let shots: Vec<Value> = self
            .shots
            .iter()
            .map(|shot| {
                json!({
                    "id": shot.id,
                    "dur": shot.duration,
                    "shot_type": shot.size,
                    "camera": shot.camera,
                    "scene": shot.director_scene(),
                    "action": shot.action,
                    "characters": shot.characters,
                    "lipsync": !shot.line_text.trim().is_empty(),
                    "line": if shot.line_text.trim().is_empty() {
                        Value::Null
                    } else {
                        json!({"who": shot.line_who, "text": shot.line_text})
                    },
                    "image_prompt": shot.prompt_image,
                    "video_prompt": shot.prompt_video,
                })
            })
            .collect();
        json!({
            "type": self.type_id(),
            "tier": self.tier_id(),
            "budget_cny": budget,
            "script": {
                "title": self.title,
                "logline": self.logline,
                "characters": self.characters.iter().map(|c| json!({
                    "id": c.id, "name": c.name, "age": c.age, "look": c.look, "voice": c.voice
                })).collect::<Vec<_>>(),
                "shots": shots,
            },
        })
    }

    /// 轮询任务：每 3 秒一次，直到终态或停止等待。
    fn poll_task(&mut self, _studio: &mut Studio) {
        let Some(task) = self.task.as_ref() else {
            return;
        };
        let task_id = task.task_id.clone();
        let kind = task.kind.clone();
        if task_id.is_empty() {
            return;
        }
        let cancel = self
            .cancel
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        host::spawn(
            move || -> Result<Value, String> {
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("已停止等待（平台任务可能仍在跑，回「生成」点查进度可接上）".to_owned());
                    }
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    let value = if kind == "movie" {
                        movie::task(&task_id)?
                    } else {
                        movie::director_task(&task_id)?
                    };
                    let status = value
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("running")
                        .to_owned();
                    let terminal = matches!(
                        status.as_str(),
                        "done" | "failed" | "partial" | "aborted_budget" | "interrupted" | "cancelled"
                    );
                    let kind_now = kind.clone();
                    host::on_ui(move |studio, _, _| {
                        studio.handle(Msg::Movie(MovieMsg::Poll {
                            kind: kind_now,
                            result: Box::new(Ok(value)),
                        }));
                    });
                    if terminal {
                        return Ok(serde_json::json!({"terminal": true}));
                    }
                }
            },
            |studio, _, _, result| {
                if let Err(error) = result {
                    studio.handle(Msg::Movie(MovieMsg::Toast(error)));
                }
            },
        );
    }

    fn handle_poll(&mut self, kind: String, result: Result<Value, String>, studio: &mut Studio) {
        if self.cancel.as_ref().is_some_and(|flag| flag.load(Ordering::Relaxed)) && result.is_err() {
            self.busy = false;
            self.stage = "已停止等待".to_owned();
            self.message = result.unwrap_err();
            return;
        }
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                self.busy = false;
                self.message = error;
                self.alert(studio);
                return;
            }
        };
        let mut status_now = String::new();
        let mut stage_now = String::new();
        let mut progress_now = 0.0f32;
        let mut terminal = false;
        let mut shots_now: Vec<Value> = Vec::new();
        {
            let task = self.task.get_or_insert_with(TaskState::default);
            task.kind = kind.clone();
            if let Some(status) = value.get("status").and_then(Value::as_str) {
                task.status = status.to_owned();
            }
            task.stage = value
                .get("stage")
                .and_then(Value::as_str)
                .or_else(|| value.get("stage_label").and_then(Value::as_str))
                .or_else(|| value.get("message").and_then(Value::as_str))
                .unwrap_or("生成中")
                .to_owned();
            if let Some(progress) = value.get("progress").and_then(Value::as_f64) {
                task.progress = (progress as f32 / 100.0).clamp(0.0, 1.0);
            }
            let shots = if kind == "movie" {
                value
                    .get("result")
                    .and_then(|r| r.get("shots"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            } else {
                value
                    .get("shots")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            };
            if !shots.is_empty() {
                task.shots = shots;
            }
            terminal = matches!(
                task.status.as_str(),
                "done" | "failed" | "partial" | "aborted_budget" | "interrupted" | "cancelled"
            );
            status_now = task.status.clone();
            stage_now = task.stage.clone();
            progress_now = task.progress;
            shots_now = task.shots.clone();
        }
        if terminal {
            self.busy = false;
            self.cancel = None;
            self.stage = movie::status_text(&status_now, &stage_now);
            self.progress = if status_now == "done" { 1.0 } else { 0.99 };
            if kind == "movie" {
                self.collect_movie_result(&value);
            } else {
                self.collect_director_result(&value);
            }
            self.init_review();
            let ok_count = shots_now
                .iter()
                .filter(|s| s.get("status").and_then(Value::as_str) == Some("ok"))
                .count();
            let warnings = self
                .task
                .as_ref()
                .map(|task| task.warnings.clone())
                .unwrap_or_default();
            self.message = match status_now.as_str() {
                "done" => "生成完成：成片与工程包已就绪。去「审片」逐镜看，或直接去「成片预览」。".to_owned(),
                "partial" => format!(
                    "部分完成：可用镜头 {ok_count}/{}。看下方 warnings 里哪几镜失败，可在「审片」单镜重拍。",
                    self.shots.len().max(shots_now.len())
                ),
                "aborted_budget" => "预算中止：已出镜头保留，调高预算后可续跑。".to_owned(),
                "failed" => format!(
                    "生成失败：{}。若是平台模型额度/通道问题（如余额不足、厂商欠费），充值或恢复后回这一步点「重拍 / 重跑脏镜头」即可续跑；也可以切到「电影成片管线」再试一次。",
                    warnings
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "看平台返回的错误".to_owned())
                ),
                other => format!("任务状态：{other}"),
            };
            if (status_now == "done" || status_now == "partial") && self.step == 8 {
                self.step = 9;
            }
            studio.notify(&self.message, status_now == "failed");
        } else {
            self.progress = progress_now.max(self.progress);
            self.stage = stage_now;
        }
    }

    /// 导演任务结果落到面板。
    fn collect_director_result(&mut self, value: &Value) {
        let Some(task) = self.task.as_mut() else {
            return;
        };
        task.film_url = value
            .get("video_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(pack) = value.get("pack").filter(|p| p.is_object()) {
            task.pack_url = pack
                .get("pack_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            task.pack_id = pack
                .get("pack_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            task.pack_download_url = pack
                .get("pack_download_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            task.pack_size = pack
                .get("pack_size")
                .or_else(|| pack.get("size"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
        } else {
            task.pack_url = value
                .get("pack_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
        }
        let cost = value
            .get("actual_cost_cny")
            .or_else(|| value.get("cost").and_then(|c| c.get("total_cny")))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let budget = value
            .get("budget_cny")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        task.cost_line = format!("实际成本 ¥{cost:.2} / 预算 ¥{budget:.2}（1 积分 = ¥1）");
        let ok = task
            .shots
            .iter()
            .filter(|s| s.get("status").and_then(Value::as_str) == Some("ok"))
            .count();
        let passed = task
            .shots
            .iter()
            .filter(|s| {
                s.get("qc")
                    .and_then(|q| q.get("passed"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .count();
        task.qc_line = format!(
            "逐镜 QC：{passed}/{ok} 镜通过（导演编排器质检：时长/分辨率/黑帧/音轨/文件）"
        );
        task.interop_line = format!(
            "交接：工程包 manifest {}；成片内嵌字幕 SRT 见工程包；EDL/OTIO 在电影成片管线产物里。",
            if task.pack_id.is_empty() {
                "（无）".to_owned()
            } else {
                task.pack_id.clone()
            }
        );
        task.warnings = value
            .get("warnings")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        task.done = true;
    }

    /// 电影成片管线结果落到面板。
    fn collect_movie_result(&mut self, value: &Value) {
        let result = value.get("result").cloned().unwrap_or_default();
        let Some(task) = self.task.as_mut() else {
            return;
        };
        task.film_url = result
            .get("film_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        task.vertical_url = result
            .get("vertical_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(pack) = result.get("pack").filter(|p| p.is_object()) {
            task.pack_url = pack
                .get("url")
                .or_else(|| pack.get("pack_url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            task.pack_id = pack
                .get("pack_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            task.pack_download_url = pack
                .get("download_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            task.pack_size = pack
                .get("size")
                .and_then(Value::as_u64)
                .unwrap_or(0);
        }
        let cost = result
            .get("cost")
            .and_then(|c| c.get("total_cny"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let budget = result
            .get("cost")
            .and_then(|c| c.get("budget_cny"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        task.cost_line = format!("实际成本 ¥{cost:.2} / 预算 ¥{budget:.2}");
        let qc = result.get("qc").cloned().unwrap_or_default();
        task.qc_line = format!(
            "QC {} 分 · {} · {}",
            qc.get("score").and_then(Value::as_f64).unwrap_or(0.0),
            if qc.get("passed").and_then(Value::as_bool).unwrap_or(false) {
                "全部通过"
            } else {
                "有未通过项"
            },
            qc.get("report_url").and_then(Value::as_str).unwrap_or("")
        );
        let files = result
            .get("post_interop")
            .and_then(|p| p.get("files"))
            .cloned()
            .unwrap_or_default();
        let pickup = |key: &str| {
            files
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_default()
        };
        task.interop_line = format!(
            "交接文件：EDL {} · OTIO {} · QC 报告 {} · LUT {}",
            pickup("edl"),
            pickup("otio"),
            pickup("qc_report"),
            pickup("grade_cube")
        );
        task.warnings = result
            .get("warnings")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        task.done = true;
    }

    fn init_review(&mut self) {
        let ok_shots: Vec<u32> = self
            .task
            .as_ref()
            .map(|task| {
                task.shots
                    .iter()
                    .filter(|shot| shot.get("status").and_then(Value::as_str) == Some("ok"))
                    .filter_map(|shot| {
                        shot.get("no").and_then(Value::as_u64).map(|no| no as u32)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let count = self.shots.len().max(
            self.task
                .as_ref()
                .map(|task| task.shots.len())
                .unwrap_or(0),
        );
        self.review = vec!["待审".to_owned(); count];
        self.notes = vec![String::new(); count];
        for at in 0..count {
            let no = self
                .shots
                .get(at)
                .map(|shot| shot.no)
                .unwrap_or((at + 1) as u32);
            if !ok_shots.contains(&no) {
                self.review[at] = "未生成".to_owned();
            }
        }
        self.take_index = self.take_index.min(count.saturating_sub(1));
    }

    fn insert_film(&mut self, path: &PathBuf, studio: &mut Studio) {
        if studio.session.is_none() {
            self.message =
                "成片已下载，但当前没有打开的工程：先在「文件 → 打开 / 新建」里开一个工程，再点一次「下载并放进时间轴」。"
                    .to_owned();
            self.alert(studio);
            return;
        }
        let Some(summary) = concat_host::media::probe(&path.to_string_lossy()).ok() else {
            self.message = format!("成片读不了：{}", path.display());
            self.alert(studio);
            return;
        };
        let mut item = summary.to_new_media();
        item.name = if self.title.trim().is_empty() {
            "AI 电影成片".to_owned()
        } else {
            format!("AI 电影 · {}", self.title.trim())
        };
        let created = studio.apply(Command::AddMedia { item });
        let media_id = created.or_else(|| {
            studio
                .project()
                .media
                .iter()
                .find(|media| media.path == summary.path)
                .map(|media| media.id.clone())
        });
        if let Some(media_id) = media_id {
            let start = f64::from(studio.playhead.max(0.0));
            if studio
                .apply(Command::AddClipAtFirstFree { media_id, start })
                .is_some()
            {
                self.message = format!(
                    "成片已放进时间轴（{} 起），可以逐帧看、继续剪。",
                    format!("{start:.1}s")
                );
                studio.notify("成片已放进时间轴", false);
                return;
            }
        }
        self.message = "成片放进时间轴失败：请先打开一个工程".to_owned();
        self.alert(studio);
    }

    /// 下载成片 / 工程包（后台）。
    fn download(&mut self, kind: &str, url: String, studio: &mut Studio) {
        if url.trim().is_empty() {
            self.message = format!("还没有{}：先生成任务", if kind == "film" { "成片" } else { "工程包" });
            self.alert(studio);
            return;
        }
        if self.busy {
            return;
        }
        let dir = concat_host::ai_studio::output_dir(&studio.host.dirs);
        let ext = if kind == "film" { "mp4" } else { "zip" };
        let stem = if self.title.trim().is_empty() {
            "movie".to_owned()
        } else {
            self.title
                .trim()
                .chars()
                .take(20)
                .collect::<String>()
                .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
        };
        let dest = dir.join(format!("{stem}-{}-.{ext}", movie::short_id(&uuid_like())));
        let _ = std::fs::create_dir_all(&dir);
        self.busy = true;
        self.message = format!("正在下载{}…", if kind == "film" { "成片" } else { "工程包" });
        let kind_owned = kind.to_owned();
        let cancel = Arc::new(AtomicBool::new(false));
        host::spawn(
            move || movie::download(&url, &dest, &cancel).map(|_| dest),
            move |studio, _, _, result| {
                studio.handle(Msg::Movie(MovieMsg::Downloaded {
                    kind: kind_owned,
                    result: Box::new(result),
                }));
            },
        );
    }

    /// 导入工程包（下载后解压 → 打开工程）。
    fn import_pack(&mut self, studio: &mut Studio) {
        let url = self
            .task
            .as_ref()
            .map(|task| {
                if task.pack_download_url.trim().is_empty() {
                    task.pack_url.clone()
                } else {
                    task.pack_download_url.clone()
                }
            })
            .unwrap_or_default();
        if url.trim().is_empty() {
            self.message = "还没有工程包：先跑完一次生成".to_owned();
            self.alert(studio);
            return;
        }
        if self.busy {
            return;
        }
        let dir = concat_host::ai_studio::output_dir(&studio.host.dirs).join("movie-packs");
        let _ = std::fs::create_dir_all(&dir);
        let dest = dir.join(format!("pack-{}.zip", uuid_like()));
        let dest_for_import = dest.clone();
        self.busy = true;
        self.message = "正在下载并导入 AI 工程包…".to_owned();
        let cancel = Arc::new(AtomicBool::new(false));
        host::spawn(
            move || -> Result<concat_host::projectpack::ImportedPack, String> {
                movie::download(&url, &dest, &cancel)?;
                concat_host::projectpack::import(&dest_for_import, None, "")
            },
            |studio, _, _, result| {
                studio.handle(Msg::Movie(MovieMsg::Imported(Box::new(result))));
            },
        );
    }

    /// 单镜重拍 / 全部脏镜头重拍（导演编排器 shot_nos；电影管线不做单镜）。
    fn retake(&mut self, only_single: bool, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Some(task) = self.task.as_ref() else {
            self.message = "还没有任务：先跑一次生成".to_owned();
            self.alert(studio);
            return;
        };
        if task.kind != "director" {
            self.message =
                "电影成片管线不支持单镜重拍（会整片重渲染）：改用导演编排器，或回「生成」重跑整片。"
                    .to_owned();
            self.alert(studio);
            return;
        }
        // 失败的镜头也要能重拍：平台/额度恢复后，专业模式就是从失败处续跑。
        if !matches!(
            task.status.as_str(),
            "done" | "partial" | "failed" | "aborted_budget" | "interrupted"
        ) {
            self.message = "任务还没跑完：等它结束后再重拍".to_owned();
            self.alert(studio);
            return;
        }
        let nos: Vec<u32> = if only_single {
            vec![self
                .shots
                .get(self.take_index)
                .map(|shot| shot.no)
                .unwrap_or((self.take_index + 1) as u32)]
        } else if self.dirty_shots.is_empty() {
            self.shots.iter().map(|shot| shot.no).collect()
        } else {
            self.dirty_shots.clone()
        };
        if nos.is_empty() {
            self.message = "没有脏镜头可重拍".to_owned();
            self.alert(studio);
            return;
        }
        let task_id = task.task_id.clone();
        let budget: Option<f64> = self.budget.trim().parse::<f64>().ok();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.busy = true;
        self.stage = format!("重拍 {nos:?}…");
        self.message = format!(
            "已提交单镜重拍：只重跑 {} 镜（导演编排器断点续跑），其余镜头复用。",
            nos.len()
        );
        self.review
            .iter_mut()
            .enumerate()
            .for_each(|(at, status)| {
                let no = self.shots.get(at).map(|shot| shot.no).unwrap_or(0);
                if nos.contains(&no) {
                    *status = "重拍中".to_owned();
                }
            });
        host::spawn(
            move || movie::director_resume(&task_id, &nos, budget),
            |studio, _, _, result| {
                studio.handle(Msg::Movie(MovieMsg::Submitted {
                    kind: "director".to_owned(),
                    result: Box::new(result),
                }));
            },
        );
        let _ = cancel;
    }

    fn action(&mut self, action: &str, index: i32, text: String, studio: &mut Studio) {
        match action {
            "close" => {
                self.open = false;
                self.message.clear();
            }
            "mode" => {
                self.simple = index == 0;
                self.message = if self.simple {
                    "简单模式：只填一句话，一键全流程；点任意一步仍可改，改完只重跑下游。".to_owned()
                } else {
                    "专业模式：15 步逐项可改；DAG 只重跑受影响镜头。".to_owned()
                };
            }
            "step" => {
                self.step = index.clamp(0, 14);
                self.message.clear();
            }
            "refresh" => {
                self.types.clear();
                self.load_types();
            }
            "type" => {
                self.type_index = index.clamp(0, (self.types.len() as i32 - 1).max(0));
                if !self.shots.is_empty() {
                    self.mark_all_shots_dirty();
                }
            }
            "premise" => {
                self.premise = text;
            }
            "premise-sample" => {
                if let Some(sample) = SAMPLES.get(index.max(0) as usize) {
                    self.premise = (*sample).to_owned();
                }
            }
            "title" => self.title = text,
            "logline" => self.logline = text,
            "generate-plan" => self.generate_plan(false, studio),
            "generate-all" => self.generate_plan(true, studio),
            "shot" => {
                self.shot_index = index.max(0) as usize;
                self.shots_len_sync();
            }
            "shot-add" => {
                let no = self.shots.len() as u32 + 1;
                self.shots.push(Shot {
                    no,
                    id: format!("S{no}"),
                    size: "中景".to_owned(),
                    camera: "固定".to_owned(),
                    duration: 5.0,
                    ..Default::default()
                });
                self.shot_index = self.shots.len() - 1;
                self.rebuild_scenes();
                self.mark_all_shots_dirty();
            }
            "shot-remove" => {
                if self.shots.len() > 1 && self.shot_index < self.shots.len() {
                    self.shots.remove(self.shot_index);
                    for (at, shot) in self.shots.iter_mut().enumerate() {
                        shot.no = (at + 1) as u32;
                        shot.id = format!("S{}", at + 1);
                    }
                    self.shots_len_sync();
                    self.rebuild_scenes();
                    self.mark_all_shots_dirty();
                }
            }
            "shot-up" => {
                if self.shot_index > 0 && self.shot_index < self.shots.len() {
                    self.shots.swap(self.shot_index, self.shot_index - 1);
                    self.shot_index -= 1;
                    for (at, shot) in self.shots.iter_mut().enumerate() {
                        shot.no = (at + 1) as u32;
                        shot.id = format!("S{}", at + 1);
                    }
                    self.rebuild_scenes();
                    self.mark_all_shots_dirty();
                }
            }
            "shot-down" => {
                if self.shot_index + 1 < self.shots.len() {
                    self.shots.swap(self.shot_index, self.shot_index + 1);
                    self.shot_index += 1;
                    for (at, shot) in self.shots.iter_mut().enumerate() {
                        shot.no = (at + 1) as u32;
                        shot.id = format!("S{}", at + 1);
                    }
                    self.rebuild_scenes();
                    self.mark_all_shots_dirty();
                }
            }
            "shot-scene" => {
                if let Some(shot) = self.selected_shot_mut() {
                    shot.scene = text;
                }
                self.shots_len_sync();
                self.rebuild_scenes();
                self.mark_current_shot_dirty();
            }
            "shot-action" => {
                if let Some(shot) = self.selected_shot_mut() {
                    shot.action = text;
                }
                self.mark_current_shot_dirty();
            }
            "shot-line" => {
                if let Some(shot) = self.selected_shot_mut() {
                    shot.line_text = text;
                }
                self.mark_current_shot_dirty();
            }
            "shot-size" => {
                if let Some(shot) = self.selected_shot_mut() {
                    shot.size = text;
                }
                self.mark_current_shot_dirty();
            }
            "shot-camera" => {
                if let Some(shot) = self.selected_shot_mut() {
                    shot.camera = text;
                }
                self.mark_current_shot_dirty();
            }
            "shot-dur" => {
                if let Ok(dur) = text.trim().parse::<f64>() {
                    if let Some(shot) = self.selected_shot_mut() {
                        shot.duration = dur.clamp(1.0, 10.0);
                    }
                }
                self.mark_current_shot_dirty();
            }
            "char" => {
                self.char_index = index.max(0) as usize;
                self.shots_len_sync();
            }
            "char-add" => {
                self.characters.push(Character {
                    id: format!("C{}", self.characters.len() + 1),
                    name: format!("角色 {}", self.characters.len() + 1),
                    ..Default::default()
                });
                self.char_index = self.characters.len() - 1;
            }
            "char-remove" => {
                if self.char_index < self.characters.len() && self.characters.len() > 1 {
                    let removed = self.characters.remove(self.char_index);
                    self.char_index = self.char_index.min(self.characters.len() - 1);
                    // 删角色：用到它的镜头变脏。
                    let dirty: Vec<u32> = self
                        .shots
                        .iter()
                        .filter(|shot| shot.characters.iter().any(|id| id == &removed.id))
                        .map(|shot| shot.no)
                        .collect();
                    self.add_dirty(dirty);
                }
            }
            "char-name" | "char-look" | "char-voice" => {
                let mut changes: Vec<u32> = Vec::new();
                if let Some(character) = self.selected_char_mut() {
                    match action {
                        "char-name" => character.name = text,
                        "char-look" => character.look = text,
                        _ => character.voice = text,
                    }
                    let id = character.id.clone();
                    changes = self
                        .shots
                        .iter()
                        .filter(|shot| shot.characters.iter().any(|c| c == &id))
                        .map(|shot| shot.no)
                        .collect();
                }
                self.add_dirty(changes);
            }
            "scene" => {
                self.scene_index = index.max(0) as usize;
            }
            "scene-text" => {
                let mut dirty = Vec::new();
                if let Some(card) = self.scenes.get(self.scene_index).cloned() {
                    for at in &card.shots {
                        if let Some(shot) = self.shots.get_mut(*at) {
                            shot.scene = text.clone();
                            dirty.push(shot.no);
                        }
                    }
                }
                self.rebuild_scenes();
                self.add_dirty(dirty);
            }
            "wardrobe" => {
                self.wardrobe = text;
                // 服化道全片共用：所有镜头变脏。
                if !self.shots.is_empty() {
                    self.mark_all_shots_dirty();
                }
            }
            "tier" => {
                self.tier = index.clamp(0, 2);
                if let Some(plan) = self.plan.clone() {
                    self.recompute_estimates(&plan);
                }
            }
            "budget" => self.budget = text,
            "engine" => {
                self.engine = index.clamp(0, 1);
            }
            "run" => self.run(studio),
            "preview-run" => {
                self.tier = 0;
                self.run(studio);
            }
            "stop-wait" => {
                if let Some(cancel) = self.cancel.as_ref() {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.busy = false;
                self.stage = "已停止等待".to_owned();
                self.message =
                    "已停止等待：平台任务可能仍在跑完。回「生成」点「查进度」可接上结果。"
                        .to_owned();
                studio.notify("已停止等待（任务仍在服务端跑）", false);
            }
            "refresh-task" => {
                if let Some(task) = self.task.as_ref() {
                    let task = task.clone();
                    if !task.task_id.is_empty() {
                        self.busy = true;
                        self.stage = "查询任务…".to_owned();
                        let task_id = task.task_id.clone();
                        let kind = task.kind.clone();
                        let kind_for_callback = kind.clone();
                        host::spawn(
                            move || {
                                if kind == "movie" {
                                    movie::task(&task_id)
                                } else {
                                    movie::director_task(&task_id)
                                }
                            },
                            move |studio, _, _, result| {
                                studio.handle(Msg::Movie(MovieMsg::Poll {
                                    kind: kind_for_callback,
                                    result: Box::new(result),
                                }));
                            },
                        );
                    }
                }
            }
            "take" => {
                self.take_index = index.max(0) as usize;
                self.shots_len_sync();
                self.take_index = self.take_index.min(self.review.len().saturating_sub(1));
            }
            "take-pass" => {
                if let Some(status) = self.review.get_mut(self.take_index) {
                    *status = "通过".to_owned();
                }
            }
            "take-reject" => {
                if let Some(status) = self.review.get_mut(self.take_index) {
                    *status = "退回".to_owned();
                }
                self.add_dirty(vec![self
                    .shots
                    .get(self.take_index)
                    .map(|shot| shot.no)
                    .unwrap_or((self.take_index + 1) as u32)]);
            }
            "take-note" => {
                self.take_note = text.clone();
                if self.take_index < self.notes.len() {
                    self.notes[self.take_index] = text;
                }
            }
            "retake-shot" => self.retake(true, studio),
            "retake-dirty" => self.retake(false, studio),
            "lut" => self.lut = index.clamp(0, (LUTS.len() - 1) as i32),
            "contrast" => self.contrast = text,
            "saturation" => self.saturation = text,
            "grain" => {
                self.grain = text.trim().parse().unwrap_or(4);
            }
            "music" => self.music = index == 0,
            "lipsync" => self.lipsync = index == 0,
            "loudness" => self.loudness = index.clamp(0, 1),
            "subtitles" => self.subtitles = index == 0,
            "ai-mark" => self.ai_mark = index == 0,
            "cast" => self.cast = text,
            "crew" => self.crew = text,
            "preview-film" => {
                if let Some(task) = self.task.as_ref() {
                    if !task.film_url.is_empty() {
                        let _ = opener::open(&task.film_url);
                    } else {
                        self.message = "还没有成片地址：先生成".to_owned();
                        self.alert(studio);
                    }
                }
            }
            "preview-vertical" => {
                if let Some(task) = self.task.as_ref() {
                    if !task.vertical_url.is_empty() {
                        let _ = opener::open(&task.vertical_url);
                    }
                }
            }
            "import-film" => {
                if let Some(path) = self.film_local.clone() {
                    self.insert_film(&path, studio);
                    return;
                }
                let url = self
                    .task
                    .as_ref()
                    .map(|task| task.film_url.clone())
                    .unwrap_or_default();
                self.download("film", url, studio);
            }
            "download-pack" => {
                let url = self
                    .task
                    .as_ref()
                    .map(|task| {
                        if task.pack_download_url.trim().is_empty() {
                            task.pack_url.clone()
                        } else {
                            task.pack_download_url.clone()
                        }
                    })
                    .unwrap_or_default();
                self.download("pack", url, studio);
            }
            "import-pack" => self.import_pack(studio),
            "goto-grade" => {
                self.message =
                    "调色页在编辑器右列属性面板（选中片段 → 调色 / 示波器）；电影成片管线的 LUT 已在服务端套好，导入工程包后可逐镜精调。"
                        .to_owned();
                studio.notify("导入工程包后，在编辑器右列「调色」里逐镜精调", false);
            }
            "goto-audio" => {
                self.message = "AI 音频导演在编辑器右列「音频」/ AI 面板里：配音、环境声、音效、BGM 与 ducking 可逐句改。".to_owned();
                studio.notify("音频页：右列属性面板 → 音频 / AI 编辑", false);
            }
            "web" => {
                let _ = opener::open(format!("{}/ai-nav/monitor/", movie::SITE));
            }
            _ => {}
        }
        self.last_alert.clear();
    }

    fn mark_current_shot_dirty(&mut self) {
        let no = self
            .shots
            .get(self.shot_index)
            .map(|shot| shot.no)
            .unwrap_or(0);
        if no > 0 {
            self.add_dirty(vec![no]);
        }
    }

    fn add_dirty(&mut self, mut shots: Vec<u32>) {
        shots.sort_unstable();
        shots.dedup();
        for no in shots {
            if !self.dirty_shots.contains(&no) {
                self.dirty_shots.push(no);
            }
        }
        self.dirty_shots.sort_unstable();
    }

    /// 一句话 premise → 分镜方案。
    fn generate_plan(&mut self, auto_run: bool, studio: &mut Studio) {
        if self.busy {
            return;
        }
        if self.premise.trim().chars().count() < 4 {
            self.message = "先写一句 premise（至少几个字）".to_owned();
            self.alert(studio);
            return;
        }
        if !self.signed_in {
            self.message = "请先登录星籁账号：登录后回到这里再生成分镜".to_owned();
            self.alert(studio);
            return;
        }
        if auto_run {
            self.simple = true;
        }
        self.auto_run = auto_run;
        self.busy = true;
        self.progress = 0.0;
        self.stage = "平台正在写分镜方案…".to_owned();
        self.message = "正在生成分镜方案（规划免费；这一步可能要 10–40 秒）…".to_owned();
        let body = json!({
            "type": self.type_id(),
            "tier": self.tier_id(),
            "budget_cny": self.budget.trim().parse::<f64>().unwrap_or(25.0),
            "topic": format!(
                "{}。请写成 3 个镜头、总长 15 秒左右的电影短片；{}",
                self.premise.trim(),
                if self.wardrobe.trim().is_empty() {
                    String::new()
                } else {
                    format!("整体风格：{}", self.wardrobe.trim())
                }
            ),
        });
        host::spawn(
            move || movie::plan(&body),
            |studio, _, _, result| {
                studio.handle(Msg::Movie(MovieMsg::Planned(Box::new(result))));
            },
        );
    }

    /// 发布给 Slint 的数据。
    pub fn data(&self) -> MovieData {
        let type_of = self.type_of();
        let shot = self.selected_shot();
        let character = self.selected_char();
        let scene = self.scenes.get(self.scene_index);
        let task = self.task.as_ref();
        let dirties = self
            .dirty_shots
            .iter()
            .map(|no| format!("S{no}"))
            .collect::<Vec<_>>()
            .join("、");
        let step = self.step.clamp(0, 14) as usize;
        let total_dur: f64 = self.shots.iter().map(|shot| shot.duration).sum();
        let preview = if self.shots.is_empty() {
            "还没有分镜：先在「一句话 premise」生成方案。".to_owned()
        } else {
            let mut text = format!(
                "故事板 {} 镜 · 约 {:.0} 秒 · {} 个角色：\n",
                self.shots.len(),
                total_dur,
                self.characters.len()
            );
            for shot in self.shots.iter().take(6) {
                text.push_str(&shot.card());
                text.push_str("\n\n");
            }
            text
        };
        let estimate = if self.engine == 0 {
            self.estimate_director.clone()
        } else {
            self.estimate_movie.clone()
        };
        let credits_line = if self.estimate_director.is_empty() {
            "预估积分：生成分镜后显示".to_owned()
        } else {
            format!(
                "1 积分 = ¥1；{}",
                if self.admin {
                    "内部账号免扣积分（成本照记）"
                } else {
                    "按平台实际结算"
                }
            )
        };
        let take_state = {
            let no = self
                .shots
                .get(self.take_index)
                .map(|shot| shot.no)
                .unwrap_or((self.take_index + 1) as u32);
            let status = self
                .review
                .get(self.take_index)
                .cloned()
                .unwrap_or_else(|| "未生成".to_owned());
            let note = self
                .notes
                .get(self.take_index)
                .filter(|note| !note.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| "（无批注）".to_owned());
            format!(
                "S{no} · 审片：{status} · 批注：{note} · 全片审片进度：通过 {}/{}",
                self.review.iter().filter(|s| s.as_str() == "通过").count(),
                self.review.len()
            )
        };
        let task_rows: Vec<slint::SharedString> = task
            .map(|task| {
                task.shots
                    .iter()
                    .map(|shot| {
                        let no = shot.get("no").and_then(Value::as_u64).unwrap_or(0);
                        let status = shot
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("等待");
                        let status_cn = match status {
                            "ok" => "已生成".to_owned(),
                            "failed" => "失败".to_owned(),
                            "running" => "生成中".to_owned(),
                            other => other.to_owned(),
                        };
                        let model = shot
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let cost = shot
                            .get("cost_cny")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0);
                        let url = shot
                            .get("video_url")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        format!("S{no} · {status_cn} · {model} · ¥{cost:.2}{}", if url.is_empty() {
                            String::new()
                        } else {
                            " · 已有视频".to_owned()
                        })
                        .into()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let take_rows: Vec<slint::SharedString> = self
            .shots
            .iter()
            .enumerate()
            .map(|(at, shot)| {
                let status = self.review.get(at).cloned().unwrap_or_else(|| "待审".to_owned());
                let line = if shot.line_text.trim().is_empty() {
                    String::new()
                } else {
                    format!(" · 「{}」", shot.line_text.chars().take(16).collect::<String>())
                };
                format!(
                    "S{} · {} · {:.1}s{}{line} · {status}",
                    shot.no,
                    shot.size,
                    shot.duration,
                    if self.dirty_shots.contains(&shot.no) {
                        " · 脏"
                    } else {
                        ""
                    }
                )
                .into()
            })
            .collect();
        let delivery_rows: Vec<slint::SharedString> = {
            let mut rows: Vec<String> = Vec::new();
            if let Some(task) = task {
                rows.push(if task.film_url.is_empty() {
                    "成片：还在生成（完成后这里给 URL）".to_owned()
                } else {
                    format!("成片：{}", task.film_url)
                });
                if !task.vertical_url.is_empty() {
                    rows.push(format!("竖版：{}", task.vertical_url));
                }
                rows.push(if task.pack_url.is_empty() {
                    "工程包：暂无".to_owned()
                } else {
                    format!(
                        "工程包：{}（{}）",
                        task.pack_id,
                        if task.pack_size > 0 {
                            format!("{:.1} MB", task.pack_size as f64 / 1048576.0)
                        } else {
                            "大小未知".to_owned()
                        }
                    )
                });
                rows.push(format!("成本：{}", task.cost_line));
                if !task.qc_line.is_empty() {
                    rows.push(format!("QC：{}", task.qc_line));
                }
                if !task.interop_line.is_empty() {
                    rows.push(task.interop_line.clone());
                }
                for warning in task.warnings.iter().take(4) {
                    rows.push(format!("注意：{warning}"));
                }
            } else {
                rows.push("还没有交付物：先生成一次任务。".to_owned());
            }
            rows.push(format!(
                "AI 标识：{}（成片画面角标 + 片尾演职人员表，合规要求）",
                if self.ai_mark { "开" } else { "仅片尾" }
            ));
            rows.push(format!("字幕：{}", if self.subtitles { "烧录 + SRT" } else { "只要 SRT" }));
            rows.into_iter().map(Into::into).collect()
        };
        let shot_dur = shot.map(|shot| shot.duration).unwrap_or(5.0);
        MovieData {
            open: self.open,
            step: self.step,
            simple: self.simple,
            busy: self.busy,
            progress: if task.is_some_and(|task| task.done) {
                self.progress.max(1.0)
            } else {
                self.progress
            },
            stage: self.stage.as_str().into(),
            message: self.message.as_str().into(),
            account_line: self.account_line.as_str().into(),
            signed_in: self.signed_in,
            admin: self.admin,
            web: format!("{}/ai-nav/monitor/", movie::SITE).into(),
            limits: movie::LIMITS.into(),
            step_name: STEP_NAMES[step].0.into(),
            step_note: STEP_NAMES[step].1.into(),
            type_index: self.type_index,
            type_count: self.types.len() as i32,
            type_desc: type_of
                .map(|t| {
                    format!(
                        "{} · {} · {}fps · {} LUFS。{}",
                        t.label, t.group, t.fps_default, t.loudness_default, t.desc
                    )
                })
                .unwrap_or_else(|| "类型表加载中（需要平台连通；失败会给出中文原因）".to_owned())
                .into(),
            aspect_line: type_of
                .map(|t| {
                    format!(
                        "默认画幅 {} · {}fps · 响度 {} LUFS · {}",
                        t.aspect, t.fps_default, t.loudness_default, t.label
                    )
                })
                .unwrap_or_default()
                .into(),
            premise: self.premise.as_str().into(),
            title: self.title.as_str().into(),
            logline: self.logline.as_str().into(),
            has_plan: self.plan.is_some(),
            shot_index: self.shot_index as i32,
            shot_count: self.shots.len() as i32,
            shot_size: shot.map(|shot| shot.size.clone()).unwrap_or_default().into(),
            shot_camera: shot.map(|shot| shot.camera.clone()).unwrap_or_default().into(),
            shot_scene: shot.map(|shot| shot.scene.clone()).unwrap_or_default().into(),
            shot_action: shot.map(|shot| shot.action.clone()).unwrap_or_default().into(),
            shot_line: shot.map(|shot| shot.line_text.clone()).unwrap_or_default().into(),
            shot_dur: shot_dur as f32,
            shot_dur_text: format!("{shot_dur:.1}").into(),
            char_index: self.char_index as i32,
            char_count: self.characters.len() as i32,
            char_name: character.map(|c| c.name.clone()).unwrap_or_default().into(),
            char_look: character.map(|c| c.look.clone()).unwrap_or_default().into(),
            char_voice: character.map(|c| c.voice.clone()).unwrap_or_default().into(),
            scene_index: self.scene_index as i32,
            scene_count: self.scenes.len() as i32,
            scene_text: scene.map(|s| s.text.clone()).unwrap_or_default().into(),
            wardrobe: self.wardrobe.as_str().into(),
            preview_line: preview.into(),
            tier: self.tier,
            budget: self.budget.as_str().into(),
            estimate: estimate.into(),
            credits: credits_line.into(),
            engine: self.engine,
            engine_line: if self.engine == 0 {
                format!(
                    "导演编排器：生成快、约 ¥{:.2}/镜；支持断点续跑与单镜重拍（专业模式 DAG 用这条）。画幅 {}。",
                    TIER_PER_SHOT[self.tier.max(0) as usize % TIER_PER_SHOT.len()],
                    self.director_aspect()
                )
            } else {
                "电影成片管线：2.39:1 遮幅、LUT/颗粒、环境+音效+BGM、片头尾+演职人员表、QC 与工程包；更慢更贵，但交付更完整。".to_owned()
            }
            .into(),
            has_task: task.is_some(),
            task_stage: task
                .map(|task| movie::status_text(&task.status, &task.stage))
                .unwrap_or_else(|| "还没有任务".to_owned())
                .into(),
            task_status: task.map(|task| task.status.clone()).unwrap_or_default().into(),
            task_progress: task.map(|task| task.progress).unwrap_or(0.0),
            task_line: match task {
                Some(task) => format!(
                    "任务 {} · {} · 进度 {:.0}% · 成本 {}",
                    movie::short_id(&task.task_id),
                    movie::status_text(&task.status, &task.stage),
                    task.progress * 100.0,
                    task.cost_line
                ),
                None => format!(
                    "还没提交：确认档位与预算后点「开始生成」（{}）",
                    if self.engine == 0 { "导演编排器" } else { "电影成片管线" }
                ),
            }
            .into(),
            can_cancel: self.busy,
            take_index: self.take_index as i32,
            take_count: self.review.len() as i32,
            take_state: take_state.into(),
            take_note: self.take_note.as_str().into(),
            dirty: self.dirty_line().into(),
            lut: self.lut,
            contrast: self.contrast.as_str().into(),
            saturation: self.saturation.as_str().into(),
            grain: self.grain,
            grain_text: format!("{}", self.grain).into(),
            music: self.music,
            lipsync: self.lipsync,
            loudness: self.loudness,
            subtitles: self.subtitles,
            ai_mark: self.ai_mark,
            cast: self.cast.as_str().into(),
            crew: self.crew.as_str().into(),
            film_url: task.map(|t| t.film_url.clone()).unwrap_or_default().into(),
            vertical_url: task.map(|t| t.vertical_url.clone()).unwrap_or_default().into(),
            qc_line: task.map(|t| t.qc_line.clone()).unwrap_or_default().into(),
            cost_line: task.map(|t| t.cost_line.clone()).unwrap_or_default().into(),
            has_film: task.is_some_and(|t| !t.film_url.is_empty()),
            has_qc: task.is_some_and(|t| !t.qc_line.is_empty()),
            pack_line: task
                .map(|task| {
                    if task.pack_url.is_empty() {
                        "工程包：暂无（导演编排器在任务完成后打包；电影管线在 QC 后打包）".to_owned()
                    } else {
                        format!(
                            "工程包：{} · {} · {}",
                            task.pack_id,
                            if task.pack_size > 0 {
                                format!("{:.1} MB", task.pack_size as f64 / 1048576.0)
                            } else {
                                "大小未知".to_owned()
                            },
                            task.pack_url
                        )
                    }
                })
                .unwrap_or_else(|| "工程包：生成完成后可用".to_owned())
                .into(),
            pack_url: task.map(|t| t.pack_url.clone()).unwrap_or_default().into(),
            pack_id: task.map(|t| t.pack_id.clone()).unwrap_or_default().into(),
            interop_line: task
                .map(|t| t.interop_line.clone())
                .unwrap_or_default()
                .into(),
            has_pack: task.is_some_and(|t| !t.pack_url.is_empty()),
            can_back: self.step > 0,
            can_next: self.step < 14,
            current_name: format!(
                "{}{}",
                STEP_LABELS[step],
                if dirties.is_empty() {
                    String::new()
                } else {
                    format!("（{} 个脏镜头）", self.dirty_shots.len())
                }
            )
            .into(),
        }
    }

    /// 类型表（面板的 9 个按钮）。
    pub fn type_labels(&self) -> Vec<slint::SharedString> {
        self.types.iter().map(|t| t.label.as_str().into()).collect()
    }

    /// 分镜行。
    pub fn shot_rows(&self) -> Vec<slint::SharedString> {
        self.shots.iter().map(|shot| shot.row().into()).collect()
    }

    /// 角色行。
    pub fn char_rows(&self) -> Vec<slint::SharedString> {
        self.characters
            .iter()
            .map(|character| character.row().into())
            .collect()
    }

    /// 场景行。
    pub fn scene_rows(&self) -> Vec<slint::SharedString> {
        self.scenes
            .iter()
            .enumerate()
            .map(|(at, card)| {
                format!(
                    "{}{}（{} 镜）",
                    if at == self.scene_index { "▶ " } else { "" },
                    card.text.chars().take(40).collect::<String>(),
                    card.shots.len()
                )
                .into()
            })
            .collect()
    }

    /// 15 步完成标记。
    pub fn done_mask(&self) -> Vec<i32> {
        let mut done = vec![0i32; 15];
        if self.plan.is_some() {
            done[0] = 1;
            done[1] = 1;
            done[2] = 1;
            done[3] = 1;
            done[4] = 1;
            done[5] = 1;
            done[6] = 1;
            done[7] = 1;
        }
        if self.task.is_some() {
            done[8] = 1;
        }
        if self.task.as_ref().is_some_and(|task| {
            matches!(task.status.as_str(), "done" | "partial" | "failed" | "aborted_budget")
        }) {
            done[9] = 1;
            done[10] = 1;
            done[11] = 1;
            done[12] = 1;
            if self.task.as_ref().is_some_and(|task| !task.film_url.is_empty()) {
                done[13] = 1;
            }
            if self.task.as_ref().is_some_and(|task| !task.pack_url.is_empty()) {
                done[14] = 1;
            }
        }
        done
    }

    /// 生成时逐镜状态行。
    pub fn task_rows(&self) -> Vec<slint::SharedString> {
        self.task
            .as_ref()
            .map(|task| {
                task.shots
                    .iter()
                    .map(|shot| {
                        let no = shot.get("no").and_then(Value::as_u64).unwrap_or(0);
                        let status = shot
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("等待");
                        let status_cn = match status {
                            "ok" => "已生成".to_owned(),
                            "failed" => "失败".to_owned(),
                            "running" => "生成中".to_owned(),
                            other => other.to_owned(),
                        };
                        let model = shot.get("model").and_then(Value::as_str).unwrap_or("");
                        let cost = shot
                            .get("cost_cny")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0);
                        let url = shot
                            .get("video_url")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        format!(
                            "S{no} · {status_cn} · {model} · ¥{cost:.2}{}",
                            if url.is_empty() {
                                String::new()
                            } else {
                                " · 已有视频".to_owned()
                            }
                        )
                        .into()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 审片 take 行。
    pub fn take_rows(&self) -> Vec<slint::SharedString> {
        self.shots
            .iter()
            .enumerate()
            .map(|(at, shot)| {
                let status = self
                    .review
                    .get(at)
                    .cloned()
                    .unwrap_or_else(|| "待审".to_owned());
                let line = if shot.line_text.trim().is_empty() {
                    String::new()
                } else {
                    format!(
                        " · 「{}」",
                        shot.line_text.chars().take(16).collect::<String>()
                    )
                };
                format!(
                    "S{} · {} · {:.1}s{}{line} · {status}",
                    shot.no,
                    shot.size,
                    shot.duration,
                    if self.dirty_shots.contains(&shot.no) {
                        " · 脏"
                    } else {
                        ""
                    }
                )
                .into()
            })
            .collect()
    }

    /// 交付清单行。
    pub fn delivery_rows(&self) -> Vec<slint::SharedString> {
        let mut rows: Vec<String> = Vec::new();
        if let Some(task) = self.task.as_ref() {
            rows.push(if task.film_url.is_empty() {
                "成片：还在生成（完成后这里给 URL）".to_owned()
            } else {
                format!("成片：{}", task.film_url)
            });
            if !task.vertical_url.is_empty() {
                rows.push(format!("竖版：{}", task.vertical_url));
            }
            rows.push(if task.pack_url.is_empty() {
                "工程包：暂无".to_owned()
            } else {
                format!(
                    "工程包：{}（{}）",
                    task.pack_id,
                    if task.pack_size > 0 {
                        format!("{:.1} MB", task.pack_size as f64 / 1048576.0)
                    } else {
                        "大小未知".to_owned()
                    }
                )
            });
            rows.push(format!("成本：{}", task.cost_line));
            if !task.qc_line.is_empty() {
                rows.push(format!("QC：{}", task.qc_line));
            }
            if !task.interop_line.is_empty() {
                rows.push(task.interop_line.clone());
            }
            for warning in task.warnings.iter().take(4) {
                rows.push(format!("注意：{warning}"));
            }
        } else {
            rows.push("还没有交付物：先生成一次任务。".to_owned());
        }
        rows.push(format!(
            "AI 标识：{}（成片画面角标 + 片尾演职人员表，合规要求）",
            if self.ai_mark { "开" } else { "仅片尾" }
        ));
        rows.push(format!(
            "字幕：{}",
            if self.subtitles {
                "烧录 + SRT"
            } else {
                "只要 SRT"
            }
        ));
        rows.into_iter().map(Into::into).collect()
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{stamp:x}")
}
