// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! AI 工作室：把星籁网页平台的三个 AI 剪辑能力搬进桌面窗口。
//!
//! - **一键成片**：一句需求 / 一段文案 → 平台分镜 → 配音 + 画面 → 成片，
//!   自动下载、导入素材库并放到播放头。
//! - **动态解说片**：主题 → 平台脚本（可改标题）→ 4 套风格逐帧渲染 →
//!   成片下载、插入。脚本编辑这一步只做「只读预览 + 改标题」，够用且不
//!   假装是全功能编辑器。
//! - **爆款复刻**：本地参考视频 → 上传 → 平台拆解镜头/文案结构/模板 →
//!   自动配站内商用素材 → 复刻成片。上传或拆解不可用时，面板给网页版
//!   入口并如实说明。
//!
//! 所有网络调用在 [`concat_host::ai_studio`]（与登录同一条直连、不打包
//! Key）；这里只做窗口这一半：状态、进度、错误的中文说法，以及「下载完
//! 自动插入时间轴」。面板默认不在启动路径上：只有从菜单打开时才取平台
//! 清单，启动耗时不受影响。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use concat_host::ai_studio;
use concat_host::media;
use concat_project::Command;

use crate::host;
use crate::panes::Msg;
use crate::studio::Studio;
use crate::ui::AiStudioData;

/// 一键成片的音色（平台 `/autoedit/voices` 的 5 个预设，接口取不到时也用这份）。
const VOICES: &[(&str, &str)] = &[
    ("女声·亲切自然", "温柔亲切，适合门店服务"),
    ("男声·稳重专业", "沉稳可信，适合检测/汽车"),
    ("女声·活泼种草", "轻快种草，适合美业/餐饮"),
    ("男声·青年", "年轻自然，适合知识科普"),
    ("男声·成熟", "成熟稳重，适合房产/金融"),
];

/// 一键成片的时长档（秒）。
const AE_DURATIONS: &[u32] = &[10, 15, 30, 60];

/// 解说片的时长档。
const EX_DURATIONS: &[u32] = &[30, 40, 60];

/// 解说片的 4 套风格（`/explainer/styles` 的 id 固定）。
const EX_STYLES: &[(&str, &str)] = &[
    ("科技蓝", "techblue"),
    ("中国龙 · 赤金", "longjin"),
    ("极简白", "minimal"),
    ("青绿", "teal"),
];

/// 解说片的音色（`/explainer/styles` 的 4 个 edge-tts 音色）。
const EX_VOICES: &[(&str, &str)] = &[
    ("男声 · 青年", "zh-CN-YunxiNeural"),
    ("男声 · 沉稳", "zh-CN-YunjianNeural"),
    ("女声 · 温柔", "zh-CN-XiaoxiaoNeural"),
    ("女声 · 活泼", "zh-CN-XiaoyiNeural"),
];

/// 解说片的配音占位说明。
const EXPLAINER_NOTE: &str =
    "解说片由平台渲染（POC 阶段平台侧不扣积分）；每帧带「AI 生成」角标，成片更慢时请耐心等，可取消。";

/// 爆款复刻的说明。
const REPLICATE_NOTE: &str =
    "只拆解你上传的参考视频，成片用站内商用素材（Pixabay 授权）重新生成，不搬运原片画面。参考视频最长 3 分钟。";

/// 面板消息。
#[derive(Debug)]
pub enum AiStudioMsg {
    /// 打开面板（tab：0 一键成片、1 动态解说、2 爆款复刻）。
    Open(i32),
    /// 关掉面板。
    Close,
    /// 平台任务 id（取消解说片时要用）。
    TaskId(String),
    /// 界面上的一次动作。
    Action {
        /// 动作名。
        action: String,
        /// 下拉/按钮下标。
        index: i32,
        /// 文本输入。
        text: String,
    },
    /// 后台进度。
    Progress {
        /// 0..1。
        fraction: f32,
        /// 正在做什么。
        stage: String,
    },
    /// 平台清单（音色、数字人形象、解说风格、积分预估）回来了。
    Catalog(Box<Result<Catalog, String>>),
    /// 解说片脚本生成好了。
    Script(Box<Result<ai_studio::Script, String>>),
    /// 参考视频拆解好了。
    Analysis(Box<Result<ai_studio::Analysis, String>>),
    /// 成片完成。
    Done {
        /// 0 一键成片、1 动态解说、2 爆款复刻。
        feature: i32,
        /// 成片或失败原因。
        result: Box<Result<ai_studio::VideoOutput, String>>,
    },
}

/// 一次取回的平台清单。
#[derive(Debug, Default)]
pub struct Catalog {
    /// 一键成片：音色 + 云端数字人形象。
    autoedit: ai_studio::AutoeditCatalog,
    /// 解说片：风格 / 音色 / 分辨率。
    explainer: ai_studio::ExplainerCatalog,
    /// 积分预估（管理员免扣在这里体现）。
    estimate: Option<ai_studio::Estimate>,
    /// 取不到的东西，一句话。
    note: String,
}

/// AI 工作室的面板状态。
pub struct AiStudioPane {
    /// 面板开着。
    pub open: bool,
    /// 0 一键成片、1 动态解说、2 爆款复刻。
    pub tab: i32,
    /// 清单取回来了。
    loaded: bool,
    /// 正在取清单。
    loading: bool,
    /// 一键成片：需求/文案。
    ae_text: String,
    /// 比例：0 9:16、1 16:9。
    ae_aspect: i32,
    /// 时长档下标（[`AE_DURATIONS`]）。
    ae_duration: i32,
    /// 音色下标（[`VOICES`]）。
    ae_voice: i32,
    /// 出镜：0 无、1 云端数字人。
    ae_presenter: i32,
    /// 云端数字人形象下标。
    ae_person: i32,
    /// 解说片：主题。
    ex_topic: String,
    /// 解说片：可改的标题。
    ex_title: String,
    /// 解说片：时长档下标。
    ex_duration: i32,
    /// 解说片：比例 0 16:9、1 9:16。
    ex_aspect: i32,
    /// 解说片：风格下标。
    ex_style: i32,
    /// 解说片：分辨率 0 1080p、1 720p。
    ex_resolution: i32,
    /// 解说片：音色下标。
    ex_voice: i32,
    /// 解说片：是否配音。
    ex_tts: bool,
    /// 解说片：0 表单、1 脚本预览。
    ex_step: i32,
    /// 解说片：生成的脚本。
    script: Option<ai_studio::Script>,
    /// 爆款复刻：选中的本地参考视频。
    rp_file: Option<PathBuf>,
    /// 爆款复刻：成片时长版本下标。
    rp_version: i32,
    /// 爆款复刻：拆解结果。
    analysis: Option<ai_studio::Analysis>,
    /// 爆款复刻：已配好的素材（与槽位一一对应）。
    materials: Vec<ai_studio::LibraryItem>,
    /// 平台清单。
    catalog: Catalog,
    /// 正在忙（长任务）。
    busy: bool,
    /// 进度 0..1。
    progress: f32,
    /// 正在做什么。
    stage: String,
    /// 常驻说明 / 结果。
    message: String,
    /// 上一次成片的一句话结果。
    last: String,
    /// 最后一次成片。
    last_output: Option<ai_studio::VideoOutput>,
    /// 取消标志。
    cancel: Option<Arc<AtomicBool>>,
    /// 正在等平台的任务 id（取消时给解说片发 /explainer/cancel）。
    active_task: Option<String>,
    /// 平台说这是管理员免扣（来自 /credits/estimate）。
    admin_exempt: bool,
    /// 积分预估一行。
    estimate_line: String,
    /// 最近一次提示，避免同一句反复弹。
    last_alert: String,
}

impl Default for AiStudioPane {
    fn default() -> Self {
        Self {
            open: false,
            tab: 0,
            loaded: false,
            loading: false,
            ae_text: String::new(),
            ae_aspect: 0,
            ae_duration: 1,
            ae_voice: 0,
            ae_presenter: 0,
            ae_person: 0,
            ex_topic: String::new(),
            ex_title: String::new(),
            ex_duration: 0,
            ex_aspect: 0,
            ex_style: 0,
            ex_resolution: 1,
            ex_voice: 0,
            ex_tts: true,
            ex_step: 0,
            script: None,
            rp_file: None,
            rp_version: 0,
            analysis: None,
            materials: Vec::new(),
            catalog: Catalog::default(),
            busy: false,
            progress: 0.0,
            stage: String::new(),
            message: String::new(),
            last: String::new(),
            last_output: None,
            cancel: None,
            active_task: None,
            admin_exempt: false,
            estimate_line: String::new(),
            last_alert: String::new(),
        }
    }
}

impl AiStudioPane {
    /// 打开（或切到某一页）。
    pub fn open(&mut self, tab: i32, studio: &mut Studio) {
        self.open = true;
        self.tab = tab.clamp(0, 2);
        self.message.clear();
        if !self.loaded && !self.loading {
            self.load_catalog(studio);
        }
        if let Some(account) = concat_host::account::load() {
            self.admin_exempt = account.is_admin;
        }
    }

    /// 处理一条消息。
    pub fn update(&mut self, msg: AiStudioMsg, studio: &mut Studio) {
        match msg {
            AiStudioMsg::Open(tab) => self.open(tab, studio),
            AiStudioMsg::Close => {
                self.open = false;
                self.message.clear();
            }
            AiStudioMsg::TaskId(id) => {
                self.active_task = Some(id);
            }
            AiStudioMsg::Action {
                action,
                index,
                text,
            } => self.action(&action, index, text, studio),
            AiStudioMsg::Progress { fraction, stage } => {
                self.progress = fraction.clamp(0.0, 1.0);
                self.stage.clone_from(&stage);
            }
            AiStudioMsg::Catalog(result) => {
                self.loading = false;
                self.loaded = true;
                match *result {
                    Ok(catalog) => {
                        if let Some(estimate) = catalog.estimate.as_ref() {
                            self.admin_exempt = estimate.admin_exempt;
                            self.estimate_line = estimate.display();
                        }
                        if !catalog.note.trim().is_empty() && self.message.is_empty() {
                            self.message = catalog.note.clone();
                        }
                        self.catalog = catalog;
                    }
                    Err(error) => {
                        self.loaded = true;
                        self.message = format!("平台清单取不到：{error}");
                    }
                }
            }
            AiStudioMsg::Script(result) => {
                self.busy = false;
                self.cancel = None;
                self.stage.clear();
                self.progress = 0.0;
                match *result {
                    Ok(script) => {
                        self.ex_title = script.title.clone();
                        self.message = format!(
                            "脚本已生成：{} 页 · 预计 {:.0} 秒{}。标题可以改，改完点「渲染成片」。",
                            script.pages.len(),
                            script.est_duration,
                            if script.engine == "deepseek" { "（DeepSeek）" } else { "（平台本地模板兜底）" }
                        );
                        self.script = Some(script);
                        self.ex_step = 1;
                        studio.notify("解说脚本已生成，可改标题后渲染", false);
                    }
                    Err(error) => {
                        self.message = format!("脚本生成失败：{error}");
                        self.alert(studio);
                    }
                }
            }
            AiStudioMsg::Analysis(result) => {
                self.busy = false;
                self.cancel = None;
                self.stage.clear();
                self.progress = 0.0;
                match *result {
                    Ok(analysis) => {
                        self.message = format!(
                            "{}。{}",
                            analysis.summary(),
                            if analysis.shots.is_empty() {
                                "没拆到镜头：换一条节奏更清楚的视频，或到网页版手动填模板"
                            } else {
                                "点「一键匹配素材并生成」：自动配站内商用素材并提交复刻"
                            }
                        );
                        self.rp_version = 0;
                        self.materials.clear();
                        self.analysis = Some(analysis);
                        studio.notify("参考视频拆解完成", false);
                    }
                    Err(error) => {
                        self.message = format!("拆解失败：{error}");
                        self.alert(studio);
                    }
                }
            }
            AiStudioMsg::Done { feature, result } => self.finished(studio, feature, *result),
        }
    }

    /// 关闭后的清理：任务继续，只是不看了。
    pub fn close(&mut self) {
        self.open = false;
    }

    fn action(&mut self, action: &str, index: i32, text: String, studio: &mut Studio) {
        match action {
            "tab" => {
                self.tab = index.clamp(0, 2);
                self.message.clear();
                if !self.loaded && !self.loading {
                    self.load_catalog(studio);
                }
            }
            "close" => self.open = false,
            "refresh" => {
                if !self.busy {
                    self.load_catalog(studio);
                }
            }
            "login" => {
                self.message = "请先登录星籁账号；登录后回到这里点「生成」".to_owned();
                studio.account_sheet = true;
                studio.notify("请先登录星籁账号", true);
            }
            "open-folder" => {
                let dir = ai_studio::output_dir(&studio.host.dirs);
                let _ = std::fs::create_dir_all(&dir);
                if let Err(error) = opener::open(&dir) {
                    self.message = format!("打开成片目录失败：{error}");
                }
            }
            "cancel" => self.cancel_run(studio),
            "insert" => self.insert_last(studio),
            // ── 一键成片 ──
            "ae-text" => self.ae_text = text.chars().take(3000).collect(),
            "ae-aspect" => self.ae_aspect = index.clamp(0, 1),
            "ae-duration" => self.ae_duration = index.clamp(0, (AE_DURATIONS.len() - 1) as i32),
            "ae-voice" => self.ae_voice = index.clamp(0, (VOICES.len() - 1) as i32),
            "ae-presenter" => {
                self.ae_presenter = index.clamp(0, 1);
                if self.ae_presenter == 1 && self.catalog.autoedit.persons.is_empty() {
                    self.message =
                        "平台没有返回可用的云端数字人形象：可以先选「无出镜」，或到网页工作台配置形象"
                            .to_owned();
                }
            }
            "ae-person" => {
                self.ae_person = index.max(0);
            }
            "ae-generate" => self.autoedit_generate(studio),
            // ── 动态解说 ──
            "ex-topic" => self.ex_topic = text.chars().take(80).collect(),
            "ex-title" => {
                self.ex_title = text.chars().take(40).collect();
                if let Some(script) = self.script.as_mut() {
                    script.rename(&self.ex_title);
                }
            }
            "ex-duration" => self.ex_duration = index.clamp(0, (EX_DURATIONS.len() - 1) as i32),
            "ex-aspect" => self.ex_aspect = index.clamp(0, 1),
            "ex-style" => self.ex_style = index.clamp(0, (EX_STYLES.len() - 1) as i32),
            "ex-resolution" => self.ex_resolution = index.clamp(0, 1),
            "ex-voice" => self.ex_voice = index.clamp(0, (EX_VOICES.len() - 1) as i32),
            "ex-tts" => self.ex_tts = !self.ex_tts,
            "ex-script" => self.explainer_script(studio),
            "ex-back" => {
                self.ex_step = 0;
                self.message.clear();
            }
            "ex-render" => self.explainer_render(studio),
            // ── 爆款复刻 ──
            "rp-pick" => self.pick_reference(studio),
            "rp-analyze" => self.replicate_analyze(studio),
            "rp-version" => {
                self.rp_version = index.max(0);
                self.materials.clear();
            }
            "rp-build" => self.replicate_build(studio),
            "rp-web" => {
                let _ = opener::open(ai_studio::REPLICATE_WEB);
            }
            "web-autoedit" => {
                let _ = opener::open(ai_studio::AUTOEDIT_WEB);
            }
            "web-explainer" => {
                let _ = opener::open(ai_studio::EXPLAINER_WEB);
            }
            _ => {}
        }
    }

    /// 把同一句失败提示只弹一次通知。
    fn alert(&mut self, studio: &mut Studio) {
        if self.last_alert != self.message {
            self.last_alert.clone_from(&self.message);
            studio.notify(&self.message, true);
        }
    }

    /// 取平台清单（音色 / 数字人形象 / 解说风格 / 积分预估）。
    fn load_catalog(&mut self, studio: &mut Studio) {
        if self.loading {
            return;
        }
        self.loading = true;
        let _ = studio;
        let token = concat_host::account::load().map(|account| account.token);
        host::spawn(
            move || -> Result<Catalog, String> {
                let mut note = String::new();
                let autoedit = match ai_studio::autoedit_catalog(token.as_deref()) {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        note = format!("一键成片的平台清单取不到：{error}");
                        ai_studio::AutoeditCatalog::default()
                    }
                };
                let explainer = match ai_studio::explainer_catalog(token.as_deref()) {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        if !note.is_empty() {
                            note.push('；');
                        }
                        note.push_str(&format!("解说片的平台清单取不到：{error}"));
                        ai_studio::ExplainerCatalog::default()
                    }
                };
                let estimate = ai_studio::estimate(token.as_deref(), "none", 15.0).ok();
                Ok(Catalog {
                    autoedit,
                    explainer,
                    estimate,
                    note,
                })
            },
            |studio, _, _, result| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Catalog(Box::new(result))));
            },
        );
    }

    /// 一次长任务的收尾：进度、取消标志、通知。
    fn begin(&mut self, stage: &str) -> Option<Arc<AtomicBool>> {
        if self.busy {
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.busy = true;
        self.progress = 0.0;
        self.stage = stage.to_owned();
        self.message = stage.to_owned();
        self.last_alert.clear();
        Some(cancel)
    }

    fn cancel_run(&mut self, studio: &mut Studio) {
        if let Some(cancel) = self.cancel.as_ref() {
            cancel.store(true, Ordering::Relaxed);
            self.stage = "取消中…".to_owned();
            self.message = "已取消：平台上的任务可能仍在后台跑完（成片不会自动下载）".to_owned();
            // 解说片平台支持取消；另外两个只能本地停止等待。
            if let Some(task) = self.active_task.take() {
                if let Ok(token) = ai_studio::token() {
                    host::spawn_detached(move || {
                        let _ = ai_studio::explainer_cancel(&token, &task);
                    });
                }
            }
            studio.notify("已取消等待", true);
        }
    }

    /// 一键成片：分镜 → 渲染 → 下载。
    fn autoedit_generate(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Ok(token) = ai_studio::token() else {
            self.message = "请先登录星籁账号：登录后回到这里再生成".to_owned();
            self.alert(studio);
            return;
        };
        if self.ae_text.trim().chars().count() < 8 {
            self.message = "先写一句需求或粘贴一段文案（至少 8 个字）".to_owned();
            self.alert(studio);
            return;
        }
        let aspect = if self.ae_aspect == 1 { "16:9" } else { "9:16" };
        let seconds = f64::from(AE_DURATIONS[self.ae_duration.max(0) as usize % AE_DURATIONS.len()]);
        let voice_label = VOICES[self.ae_voice.max(0) as usize % VOICES.len()].0;
        let presenter = match self.ae_presenter {
            1 => {
                let person = self
                    .catalog
                    .autoedit
                    .persons
                    .get(self.ae_person.max(0) as usize)
                    .cloned();
                match person {
                    Some(person) => ai_studio::Presenter::Cloud {
                        person_id: person.id.clone(),
                        name: person.label(),
                    },
                    None => ai_studio::Presenter::None,
                }
            }
            _ => ai_studio::Presenter::None,
        };
        if self.ae_presenter == 1 && matches!(presenter, ai_studio::Presenter::None) {
            self.message = "还没有可用的云端数字人形象：请刷新清单，或先选「无出镜」".to_owned();
            self.alert(studio);
            return;
        }
        let Some(cancel) = self.begin("生成中…") else {
            return;
        };
        self.message = format!(
            "已提交：{} · {} 秒 · {} · {}",
            aspect,
            seconds,
            voice_label,
            presenter.label()
        );
        let text = self.ae_text.trim().to_owned();
        let token = token.clone();
        let dirs = studio.host.dirs.clone();
        let presenter_mode = presenter.mode().to_owned();
        let mut last = None::<Instant>;
        let mut report = move |fraction: f32, stage: String| {
            let due = last.is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(400));
            if !due {
                return;
            }
            last = Some(Instant::now());
            host::on_ui(move |studio, _, _| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Progress { fraction, stage }));
            });
        };
        host::spawn(
            move || -> Result<ai_studio::VideoOutput, String> {
                // 生成前先拿一次平台的积分预估（管理员免扣也在这里体现）。
                if let Ok(estimate) = ai_studio::estimate(Some(&token), &presenter_mode, seconds) {
                    if !estimate.display().is_empty() {
                        report(0.01, estimate.display());
                    }
                }
                report(0.04, "AI 分镜中（文案 → 镜头计划）…".to_owned());
                let project = ai_studio::autoedit_understand(
                    &token,
                    &text,
                    seconds,
                    aspect,
                    voice_label,
                )?;
                report(
                    0.12,
                    format!(
                        "分镜完成：{} 个镜头（{}）；提交渲染…",
                        project.shots.max(1),
                        if project.source == "deepseek" { "AI 分镜" } else { "规则分镜" }
                    ),
                );
                ai_studio::autoedit_render(
                    &token,
                    &dirs,
                    &project.project_id,
                    voice_label,
                    &presenter,
                    seconds,
                    &mut report,
                    &cancel,
                )
            },
            |studio, _, _, result| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Done {
                    feature: 0,
                    result: Box::new(result),
                }));
            },
        );
    }

    /// 解说片：主题 → 脚本。
    fn explainer_script(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Ok(token) = ai_studio::token() else {
            self.message = "请先登录星籁账号：登录后回到这里再生成脚本".to_owned();
            self.alert(studio);
            return;
        };
        if self.ex_topic.trim().is_empty() {
            self.message = "先写一个主题，例如「AI 如何重塑餐饮」".to_owned();
            self.alert(studio);
            return;
        }
        let Some(cancel) = self.begin("生成脚本中…") else {
            return;
        };
        let topic = self.ex_topic.trim().to_owned();
        let seconds = EX_DURATIONS[self.ex_duration.max(0) as usize % EX_DURATIONS.len()];
        let aspect = if self.ex_aspect == 1 { "9:16" } else { "16:9" };
        let style = EX_STYLES[self.ex_style.max(0) as usize % EX_STYLES.len()].1;
        let mut last = None::<Instant>;
        let mut report = move |fraction: f32, stage: String| {
            let due = last.is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(400));
            if !due {
                return;
            }
            last = Some(Instant::now());
            host::on_ui(move |studio, _, _| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Progress { fraction, stage }));
            });
        };
        host::spawn(
            move || -> Result<ai_studio::Script, String> {
                report(0.3, "平台生成脚本中（AI 写分页口播）…".to_owned());
                let script =
                    ai_studio::explainer_script(&token, &topic, seconds, aspect, style)?;
                report(0.95, "脚本已生成".to_owned());
                let _ = &cancel;
                Ok(script)
            },
            |studio, _, _, result| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Script(Box::new(result))));
            },
        );
    }

    /// 解说片：脚本 → 渲染 → 下载。
    fn explainer_render(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Ok(token) = ai_studio::token() else {
            self.message = "请先登录星籁账号：登录后回到这里再渲染".to_owned();
            self.alert(studio);
            return;
        };
        let Some(script) = self.script.clone() else {
            self.message = "还没有脚本：先点「生成脚本」".to_owned();
            self.alert(studio);
            return;
        };
        let Some(cancel) = self.begin("提交渲染…") else {
            return;
        };
        let style = EX_STYLES[self.ex_style.max(0) as usize % EX_STYLES.len()].1.to_owned();
        let aspect = if self.ex_aspect == 1 { "9:16" } else { "16:9" }.to_owned();
        let resolution = if self.ex_resolution == 1 { "720p" } else { "1080p" }.to_owned();
        let voice = EX_VOICES[self.ex_voice.max(0) as usize % EX_VOICES.len()].1.to_owned();
        let tts = self.ex_tts;
        let dirs = studio.host.dirs.clone();
        let raw = script.raw.clone();
        let mut last = None::<Instant>;
        let mut report = move |fraction: f32, stage: String| {
            let due = last.is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(400));
            if !due {
                return;
            }
            last = Some(Instant::now());
            host::on_ui(move |studio, _, _| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Progress { fraction, stage }));
            });
        };
        let mut on_task = |id: String| {
            host::on_ui(move |studio, _, _| {
                studio.handle(Msg::AiStudio(AiStudioMsg::TaskId(id)));
            });
        };
        host::spawn(
            move || -> Result<ai_studio::VideoOutput, String> {
                let request = ai_studio::ExplainerRequest {
                    script: &raw,
                    style,
                    aspect,
                    resolution,
                    tts,
                    voice,
                };
                ai_studio::explainer_render(
                    &token,
                    &dirs,
                    &request,
                    &mut report,
                    &mut on_task,
                    &cancel,
                )
                .map_err(|error| {
                    if error.contains("已取消") {
                        error
                    } else {
                        format!("{error}（解说片也可以到网页版重试）")
                    }
                })
            },
            |studio, _, _, result| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Done {
                    feature: 1,
                    result: Box::new(result),
                }));
            },
        );
    }

    /// 爆款复刻：选一条本地参考视频。
    fn pick_reference(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("选择参考视频（mp4/mov/webm，10–30 秒最合适，最长 3 分钟）")
            .add_filter("视频", &["mp4", "mov", "webm", "m4v"])
            .pick_file()
        else {
            return;
        };
        let size = std::fs::metadata(&path).map_or(0, |meta| meta.len());
        if size > 200 * 1024 * 1024 {
            self.message = "参考视频超过 200MB：请先裁剪到 3 分钟以内".to_owned();
            self.alert(studio);
            return;
        }
        self.message = format!(
            "已选参考视频：{}（{:.1} MB）。下一步「上传并拆解」。",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            size as f64 / (1024.0 * 1024.0)
        );
        self.rp_file = Some(path);
        self.analysis = None;
        self.materials.clear();
    }

    /// 爆款复刻：上传 + 拆解。
    fn replicate_analyze(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Ok(token) = ai_studio::token() else {
            self.message = "请先登录星籁账号：登录后回到这里再拆解".to_owned();
            self.alert(studio);
            return;
        };
        let Some(path) = self.rp_file.clone() else {
            self.message = "先选一条参考视频".to_owned();
            self.alert(studio);
            return;
        };
        let Some(cancel) = self.begin("上传参考视频…") else {
            return;
        };
        let mut last = None::<Instant>;
        let mut report = move |fraction: f32, stage: String| {
            let due = last.is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(400));
            if !due {
                return;
            }
            last = Some(Instant::now());
            host::on_ui(move |studio, _, _| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Progress { fraction, stage }));
            });
        };
        host::spawn(
            move || -> Result<ai_studio::Analysis, String> {
                let url = ai_studio::upload(&token, &path, &mut report, &cancel)?;
                report(0.18, "上传完成，开始 AI 拆解…".to_owned());
                ai_studio::analyze(&token, &url, &mut report, &cancel)
            },
            |studio, _, _, result| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Analysis(Box::new(result))));
            },
        );
    }

    /// 爆款复刻：自动配素材 + 提交出片。
    fn replicate_build(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Ok(token) = ai_studio::token() else {
            self.message = "请先登录星籁账号：登录后回到这里再出片".to_owned();
            self.alert(studio);
            return;
        };
        let Some(analysis) = self.analysis.clone() else {
            self.message = "先拆解参考视频".to_owned();
            self.alert(studio);
            return;
        };
        let versions = analysis.versions();
        let index = (self.rp_version.max(0) as usize).min(versions.len().saturating_sub(1));
        let seconds = versions.get(index).copied().unwrap_or(15);
        let slots = analysis.slots(seconds);
        if slots.is_empty() {
            self.message = "模板里没有可用的镜头槽位：请重新拆解参考视频".to_owned();
            self.alert(studio);
            return;
        }
        let Some(cancel) = self.begin("匹配素材…") else {
            return;
        };
        let dirs = studio.host.dirs.clone();
        self.message = format!(
            "开始出片：{} 秒版本 · {} 个镜头 · 自动匹配站内商用素材",
            seconds,
            slots.len()
        );
        let title = format!(
            "爆款复刻 · {}",
            analysis
                .hook
                .trim()
                .chars()
                .take(18)
                .collect::<String>()
                .trim()
                .to_owned()
        );
        let mut last = None::<Instant>;
        let mut report = move |fraction: f32, stage: String| {
            let due = last.is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(400));
            if !due {
                return;
            }
            last = Some(Instant::now());
            host::on_ui(move |studio, _, _| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Progress { fraction, stage }));
            });
        };
        host::spawn(
            move || -> Result<ai_studio::VideoOutput, String> {
                let materials =
                    ai_studio::match_materials(&token, &slots, &mut report, &cancel)?;
                report(0.32, "素材配齐，提交复刻渲染…".to_owned());
                ai_studio::replicate_build(
                    &token,
                    &dirs,
                    &analysis,
                    seconds,
                    &slots,
                    &materials,
                    &title,
                    &mut report,
                    &cancel,
                )
            },
            |studio, _, _, result| {
                studio.handle(Msg::AiStudio(AiStudioMsg::Done {
                    feature: 2,
                    result: Box::new(result),
                }));
            },
        );
    }

    /// 成片完成：下载好之后导入媒体库并放到播放头。
    fn finished(
        &mut self,
        studio: &mut Studio,
        feature: i32,
        result: Result<ai_studio::VideoOutput, String>,
    ) {
        self.busy = false;
        self.cancel = None;
        self.active_task = None;
        self.stage.clear();
        self.progress = 0.0;
        match result {
            Ok(output) => {
                if output.admin_exempt {
                    self.admin_exempt = true;
                }
                let inserted = self.insert(studio, &output);
                let feature_name = match feature {
                    0 => "一键成片",
                    1 => "动态解说片",
                    _ => "爆款复刻",
                };
                self.last = output.summary(feature_name, inserted);
                self.message = self.last.clone();
                ai_studio::log_usage(&studio.host.dirs, feature_name, &output, "ai-studio");
                self.last_output = Some(output);
                studio.notify(
                    if inserted.is_some() {
                        "成片已下载并插入时间轴"
                    } else {
                        "成片已下载（没有打开的工程，未插入）"
                    },
                    false,
                );
            }
            Err(error) => {
                self.message = if error.contains("已取消") {
                    "已取消（平台上的任务可能仍在后台跑完）".to_owned()
                } else {
                    format!("{}失败：{error}", Self::feature_name(feature))
                };
                self.last = self.message.clone();
                self.alert(studio);
            }
        }
    }

    /// 成片进素材库并放到时间轴（播放头）；返回落点秒数。
    fn insert(&mut self, studio: &mut Studio, output: &ai_studio::VideoOutput) -> Option<f64> {
        let summary = media::probe(&output.path.to_string_lossy()).ok()?;
        let name = output
            .path
            .file_stem()
            .map(|stem| format!("AI 工作室 · {}", stem.to_string_lossy()))
            .unwrap_or_else(|| "AI 工作室成片".to_owned());
        let mut item = summary.to_new_media();
        item.name = name;
        let created = studio.apply(Command::AddMedia { item });
        let media_id = created.or_else(|| {
            studio
                .project()
                .media
                .iter()
                .find(|media| media.path == summary.path)
                .map(|media| media.id.clone())
        })?;
        let start = f64::from(studio.playhead.max(0.0));
        studio
            .apply(Command::AddClipAtFirstFree { media_id, start })
            .map(|_| start)
    }

    fn insert_last(&mut self, studio: &mut Studio) {
        let Some(output) = self.last_output.clone() else {
            self.message = "还没有生成的成片：先生成一条".to_owned();
            return;
        };
        match self.insert(studio, &output) {
            Some(start) => {
                self.message = format!("已插入到播放头 {start:.2}s");
                studio.notify("已插入时间轴", false);
            }
            None => {
                self.message = "插入失败：请先打开一个工程".to_owned();
                studio.notify("插入失败：请先打开一个工程", true);
            }
        }
    }

    fn feature_name(feature: i32) -> &'static str {
        match feature {
            0 => "一键成片",
            1 => "动态解说片",
            _ => "爆款复刻",
        }
    }

    /// 时长（秒）当前的选择。
    fn ae_seconds(&self) -> u32 {
        AE_DURATIONS[self.ae_duration.max(0) as usize % AE_DURATIONS.len()]
    }

    fn ex_seconds(&self) -> u32 {
        EX_DURATIONS[self.ex_duration.max(0) as usize % EX_DURATIONS.len()]
    }

    /// 一键成片的预计积分一行：平台费率口径 + 管理员免扣。
    fn ae_cost_line(&self) -> String {
        let seconds = f64::from(self.ae_seconds());
        let render = 2.0;
        let presenter = if self.ae_presenter == 1 {
            0.5 * seconds
        } else {
            0.0
        };
        let total = render + presenter;
        if self.admin_exempt {
            format!("预计 {total:.1} 积分 · 内部账号（管理员）免扣积分，仅记录成本")
        } else {
            format!("预计消耗 {total:.1} 积分（成片渲染 2.0 + 数字人 {presenter:.1}）；平台实际扣费为准")
        }
    }

    /// 解说片的预计积分一行。
    fn ex_cost_line(&self) -> String {
        if self.admin_exempt {
            "解说片为平台 POC：当前不扣积分（管理员账号同样免扣）".to_owned()
        } else {
            "解说片为平台 POC：当前渲染不扣积分；平台后续接入计费时以平台返回为准".to_owned()
        }
    }

    /// 爆款复刻的预计积分一行。
    fn rp_cost_line(&self) -> String {
        if self.admin_exempt {
            "预计 2.0 积分（成片渲染）· 内部账号（管理员）免扣积分，仅记录成本".to_owned()
        } else {
            "预计约 2.0 积分（成片渲染）；平台以实际扣费为准".to_owned()
        }
    }


    /// 发布给 Slint 的数据。
    pub fn data(&self, signed_in: bool, account_line: &str) -> AiStudioData {
        let analysis = self.analysis.as_ref();
        let versions = analysis.map(ai_studio::Analysis::versions).unwrap_or_default();
        let slots = analysis
            .map(|analysis| {
                let index = (self.rp_version.max(0) as usize).min(versions.len().saturating_sub(1));
                analysis.slots(versions.get(index).copied().unwrap_or(15))
            })
            .unwrap_or_default();
        let copy_lines = match analysis {
            Some(analysis) => {
                let mut lines = Vec::new();
                if !analysis.hook.trim().is_empty() {
                    lines.push(format!("开场钩子：{}", analysis.hook.trim()));
                }
                if !analysis.body.is_empty() {
                    lines.push(format!("正文：{}", analysis.body.join(" / ")));
                }
                if !analysis.cta.trim().is_empty() {
                    lines.push(format!("结尾引导：{}", analysis.cta.trim()));
                }
                lines.push(format!("字幕：{}", analysis.subtitle));
                lines.join("\n")
            }
            None => String::new(),
        };
        let script = self.script.as_ref();
        let person_count = self.catalog.autoedit.persons.len();
        AiStudioData {
            open: self.open,
            tab: self.tab,
            busy: self.busy,
            progress: self.progress,
            stage: self.stage.as_str().into(),
            message: self.message.as_str().into(),
            account_line: account_line.into(),
            signed_in,
            admin_exempt: self.admin_exempt,
            estimate_line: self.estimate_line.as_str().into(),
            ae_text: self.ae_text.as_str().into(),
            ae_aspect: self.ae_aspect,
            ae_duration: self.ae_duration,
            ae_voice: self.ae_voice,
            ae_presenter: self.ae_presenter,
            ae_person: self.ae_person,
            has_persons: person_count > 0,
            ae_cost: self.ae_cost_line().into(),
            ae_hint: self.catalog.autoedit.note.as_str().into(),
            ex_topic: self.ex_topic.as_str().into(),
            ex_title: script.map(|script| script.title.clone()).unwrap_or_default().into(),
            ex_duration: self.ex_duration,
            ex_aspect: self.ex_aspect,
            ex_style: self.ex_style,
            ex_resolution: self.ex_resolution,
            ex_voice: self.ex_voice,
            ex_tts: self.ex_tts,
            ex_step: self.ex_step,
            has_script: script.is_some(),
            ex_est: script
                .map(|script| {
                    format!(
                        "脚本 {} 页 · 预计 {:.0} 秒 · 引擎 {}",
                        script.pages.len(),
                        script.est_duration,
                        if script.engine == "deepseek" { "DeepSeek" } else { "平台模板兜底" }
                    )
                })
                .unwrap_or_default()
                .into(),
            ex_note: script.map(|script| script.note.clone()).unwrap_or_default().into(),
            ex_cost: self.ex_cost_line().into(),
            ex_web: ai_studio::EXPLAINER_WEB.into(),
            rp_file: self
                .rp_file
                .as_ref()
                .map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .unwrap_or_default()
                .into(),
            has_file: self.rp_file.is_some(),
            has_analysis: analysis.is_some(),
            rp_summary: analysis.map(ai_studio::Analysis::summary).unwrap_or_default().into(),
            rp_copy: copy_lines.into(),
            rp_version: self.rp_version,
            rp_slots: slots.len() as i32,
            rp_has_materials: !self.materials.is_empty(),
            rp_materials: if self.materials.is_empty() {
                String::new()
            } else {
                format!("已匹配 {} 个站内商用素材（Pixabay 授权）", self.materials.len())
            }
            .into(),
            rp_cost: self.rp_cost_line().into(),
            rp_hint: REPLICATE_NOTE.into(),
            rp_web: ai_studio::REPLICATE_WEB.into(),
            has_output: self.last_output.is_some(),
            last: self.last.as_str().into(),
            output_name: self
                .last_output
                .as_ref()
                .map(|output| output.path.display().to_string())
                .unwrap_or_default()
                .into(),
            output_credits: self
                .last_output
                .as_ref()
                .map(|output| output.credits.clone())
                .unwrap_or_default()
                .into(),
        }
    }

    /// 云端数字人形象下拉的每一行。
    pub fn person_rows(&self) -> Vec<slint::SharedString> {
        if self.catalog.autoedit.persons.is_empty() {
            return vec![slint::SharedString::from("（平台没有返回形象）")];
        }
        self.catalog
            .autoedit
            .persons
            .iter()
            .map(|person| slint::SharedString::from(person.label()))
            .collect()
    }

    /// 解说脚本的逐页预览。
    pub fn page_rows(&self) -> Vec<slint::SharedString> {
        self.script
            .as_ref()
            .map(|script| {
                script
                    .pages
                    .iter()
                    .map(|line| slint::SharedString::from(line.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 参考视频拆出来的镜头。
    pub fn shot_rows(&self) -> Vec<slint::SharedString> {
        self.analysis
            .as_ref()
            .map(|analysis| {
                analysis
                    .shots
                    .iter()
                    .map(|shot| slint::SharedString::from(shot.label()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 复刻模板的槽位。
    pub fn slot_rows(&self) -> Vec<slint::SharedString> {
        let Some(analysis) = self.analysis.as_ref() else {
            return Vec::new();
        };
        let versions = analysis.versions();
        let index = (self.rp_version.max(0) as usize).min(versions.len().saturating_sub(1));
        analysis
            .slots(versions.get(index).copied().unwrap_or(15))
            .iter()
            .map(|slot| slint::SharedString::from(slot.label()))
            .collect()
    }

    /// 复刻成片时长版本下拉的行。
    pub fn version_rows(&self) -> Vec<slint::SharedString> {
        self.analysis
            .as_ref()
            .map(|analysis| {
                analysis
                    .versions()
                    .iter()
                    .map(|seconds| slint::SharedString::from(format!("{seconds} 秒")))
                    .collect()
            })
            .unwrap_or_default()
    }
}
