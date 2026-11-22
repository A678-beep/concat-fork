// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! AI 短剧（0.3.1）：桌面内的多集短剧工作台。
//!
//! 设定（题材/集数/每集时长/画幅/风格）→ 分集剧本（可逐集改标题、钩子，逐
//! 分镜改描述/台词、增删排序）→ 分镜配图（自动配站内商用素材，可换）→ 逐集
//! 渲染（单集或整剧队列）→ 下载/打开成片目录/全部插入时间轴 → 导出剧本 JSON。
//!
//! 后端接口（`/aidrama/*`）在后端联调中时面板如实显示；剧本另有本地模板，
//! 联调前也能完整编辑与导出。所有网络在 [`concat_host::aidrama`]；这里只做
//! 窗口这一半。面板不在启动路径上：只有打开时才探一次后端状态。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_host::aidrama::{self, Character, Episode, Script};
use concat_host::media;
use concat_project::Command;

use crate::host;
use crate::panes::Msg;
use crate::studio::Studio;
use crate::ui::AiDramaData;

/// 题材：面板第一组下拉。`None` 表示用户自己写。
const THEMES: &[(&str, &str)] = &[
    ("都市逆袭", "都市"),
    ("甜宠恋爱", "甜宠"),
    ("悬疑推理", "悬疑"),
    ("家庭伦理", "家庭"),
    ("职场风云", "职场"),
    ("古风权谋", "古风"),
    ("乡村生活", "乡村"),
    ("校园青春", "校园"),
];

/// 每集时长档（秒）。
const DURATIONS: &[u32] = &[30, 45, 60];

/// 画幅。
const ASPECTS: &[(&str, &str)] = &[("9:16 竖屏", "9:16"), ("16:9 横屏", "16:9")];

/// 风格。
const STYLES: &[(&str, &str)] = &[
    ("写实", "写实"),
    ("电影感", "电影感"),
    ("国风", "国风"),
    ("动漫", "动漫"),
    ("悬疑暗调", "悬疑暗调"),
];

/// ③ 成片资产：配音音色（面板标签 + edge-tts 音色 id；id 随剧本 JSON 走，
/// 渲染时作为 `voice` 提交）。
const VOICES: &[(&str, &str)] = &[
    ("女声 · 温柔叙事", "zh-CN-XiaoxiaoNeural"),
    ("女声 · 甜宠轻快", "zh-CN-XiaoyiNeural"),
    ("男声 · 悬疑低音", "zh-CN-YunjianNeural"),
    ("男声 · 都市沉稳", "zh-CN-YunxiNeural"),
    ("旁白 · 纪录片感", "zh-CN-YunyangNeural"),
];

/// ③ 成片资产：BGM（与面板下拉一一对应）。
const BGMS: &[&str] = &[
    "悬疑压迫",
    "都市轻快",
    "甜宠温柔",
    "紧张鼓点",
    "国风古韵",
    "不用 BGM",
];

/// 面板消息。
#[derive(Debug)]
pub enum AiDramaMsg {
    /// 打开面板（step：0 设定、1 剧本、2 分镜、3 成片）。
    Open(i32),
    /// 关掉面板。
    Close,
    /// 界面上的一次动作。
    Action {
        /// 动作名。
        action: String,
        /// 下拉/按钮下标。
        index: i32,
        /// 文本输入。
        text: String,
    },
    /// 后端状态与历史短剧回来了。
    Backend {
        /// 后端是否就绪 + 一句话。
        backend: aidrama::Backend,
        /// 历史短剧的行。
        history: Result<Vec<String>, String>,
    },
    /// 剧本生成好了（本地模板或平台）。
    Script(Box<Result<Script, String>>),
    /// 分镜配素材：配好了几镜。
    Matched(Box<Result<usize, String>>),
    /// 后台进度。
    Progress {
        /// 0..1。
        fraction: f32,
        /// 正在做什么。
        stage: String,
    },
    /// 一集渲染完。
    Rendered(Box<Result<aidrama::EpisodeOutput, String>>),
    /// 整剧队列跑完了。
    Queued(Box<Result<Vec<aidrama::EpisodeOutput>, String>>),
    /// 整剧 zip 下载完成。
    Zipped(Box<Result<PathBuf, String>>),
}

/// AI 短剧的面板状态。
pub struct AiDramaPane {
    /// 面板开着。
    pub open: bool,
    /// 0 设定、1 剧本、2 分镜、3 成片。
    pub step: i32,
    /// 题材下拉（[`THEMES`]）或「自定义」。
    theme_pick: i32,
    /// 自定义题材 / 题材补充。
    theme_text: String,
    /// 集数 1–10。
    episodes: i32,
    /// 时长档（[`DURATIONS`]）。
    duration: i32,
    /// 画幅（[`ASPECTS`]）。
    aspect: i32,
    /// 风格（[`STYLES`]）。
    style: i32,
    /// 合规声明确认。
    compliance: bool,
    /// 剧本。
    script: Option<Script>,
    /// 当前集（0 起）。
    episode: usize,
    /// 当前分镜（0 起）。
    scene: usize,
    /// ③ 当前角色卡（0 起）。
    character: usize,
    /// ③ 配音音色下标（[`VOICES`]）。
    voice: i32,
    /// ③ BGM 下标（[`BGMS`]）。
    bgm: i32,
    /// 后端状态。
    backend: aidrama::Backend,
    /// 正在探后端。
    checking: bool,
    /// 正在配素材。
    matching: bool,
    /// 成片库。
    outputs: Vec<aidrama::EpisodeOutput>,
    /// 整剧队列剩余集数（界面显示）。
    queue_left: usize,
    /// 历史短剧。
    history: Vec<String>,
    /// 正在忙（生成剧本 / 渲染）。
    busy: bool,
    /// 进度 0..1。
    progress: f32,
    /// 正在做什么。
    stage: String,
    /// 常驻说明 / 结果。
    message: String,
    /// 取消标志。
    cancel: Option<Arc<AtomicBool>>,
    /// 管理员（内部账号）免扣。
    admin: bool,
    /// 最近一次提示，避免同一句反复弹。
    last_alert: String,
}

impl Default for AiDramaPane {
    fn default() -> Self {
        Self {
            open: false,
            step: 0,
            theme_pick: 0,
            theme_text: String::new(),
            episodes: 3,
            duration: 0,
            aspect: 0,
            style: 0,
            compliance: false,
            script: None,
            episode: 0,
            scene: 0,
            character: 0,
            voice: 0,
            bgm: 0,
            backend: aidrama::Backend::default(),
            checking: false,
            matching: false,
            outputs: Vec::new(),
            queue_left: 0,
            history: Vec::new(),
            busy: false,
            progress: 0.0,
            stage: String::new(),
            message: String::new(),
            cancel: None,
            admin: false,
            last_alert: String::new(),
        }
    }
}

impl AiDramaPane {
    /// 打开（或切到某一步）。
    pub fn open(&mut self, step: i32, studio: &mut Studio) {
        self.open = true;
        self.step = step.clamp(0, 4);
        self.message.clear();
        if let Some(account) = concat_host::account::load() {
            self.admin = account.is_admin;
        }
        if !self.checking && self.backend.line.is_empty() {
            self.check_backend(studio);
        }
    }

    /// 处理一条消息。
    pub fn update(&mut self, msg: AiDramaMsg, studio: &mut Studio) {
        match msg {
            AiDramaMsg::Open(step) => self.open(step, studio),
            AiDramaMsg::Close => {
                self.open = false;
                self.message.clear();
            }
            AiDramaMsg::Action {
                action,
                index,
                text,
            } => self.action(&action, index, text, studio),
            AiDramaMsg::Backend { backend, history } => {
                self.checking = false;
                self.backend = backend;
                match history {
                    Ok(rows) => self.history = rows,
                    Err(error) => {
                        if self.backend.ready {
                            self.history = Vec::new();
                            log::debug!("aidrama: history unavailable: {error}");
                        }
                    }
                }
            }
            AiDramaMsg::Script(result) => {
                self.busy = false;
                self.cancel = None;
                self.stage.clear();
                self.progress = 0.0;
                match *result {
                    Ok(script) => {
                        let local = script.engine == "local-template";
                        self.message = format!(
                            "{}：{} 集 · {} 个分镜。按五阶段往下走：② 改剧本分镜 → ③ 定角色/场景/音频 → ④ 逐镜配产物 → ⑤ 拼接成片；每一步都能回头改。",
                            if local { "本地模板示例已生成" } else { "平台剧本已生成" },
                            script.episodes.len(),
                            script.scene_count()
                        );
                        self.script = Some(script);
                        self.episode = 0;
                        self.scene = 0;
                        self.sync_assets();
                        self.step = 1;
                        studio.notify(
                            if local {
                                "本地模板剧本已生成，可编辑；平台接口就绪后可用「平台生成剧本」"
                            } else {
                                "平台剧本已生成，可逐集编辑"
                            },
                            false,
                        );
                    }
                    Err(error) => {
                        self.message = format!("剧本生成失败：{error}");
                        self.alert(studio);
                    }
                }
            }
            AiDramaMsg::Matched(result) => {
                self.matching = false;
                match *result {
                    Ok(count) => {
                        self.message = format!(
                            "已配 {count} 个分镜：素材来自站内免费商用素材库（可逐个「换一个」）"
                        );
                        studio.notify("分镜素材已匹配", false);
                    }
                    Err(error) => {
                        self.message = format!("配素材失败：{error}");
                        self.alert(studio);
                    }
                }
            }
            AiDramaMsg::Progress { fraction, stage } => {
                self.progress = fraction.clamp(0.0, 1.0);
                self.stage.clone_from(&stage);
            }
            AiDramaMsg::Rendered(result) => {
                self.busy = false;
                self.cancel = None;
                self.stage.clear();
                self.progress = 0.0;
                match *result {
                    Ok(output) => {
                        let inserted = Self::insert_outputs(studio, std::slice::from_ref(&output));
                        self.message = format!(
                            "第 {} 集成片完成：{}{}",
                            output.no,
                            output.summary(),
                            if inserted > 0 {
                                " · 已自动插入时间轴"
                            } else {
                                " · 没打开工程，先放进成片库"
                            }
                        );
                        self.outputs.push(output);
                        self.step = 4;
                        studio.notify(
                            if inserted > 0 {
                                "短剧成片已下载并插入时间轴"
                            } else {
                                "短剧成片已下载（没有打开的工程，未插入）"
                            },
                            false,
                        );
                    }
                    Err(error) => {
                        self.message = format!("成片失败：{error}");
                        self.alert(studio);
                    }
                }
            }
            AiDramaMsg::Zipped(result) => {
                self.busy = false;
                self.cancel = None;
                self.stage.clear();
                self.progress = 0.0;
                match *result {
                    Ok(path) => {
                        self.message = format!("整剧已打包下载：{}", path.display());
                        studio.notify("整剧 zip 已下载", false);
                    }
                    Err(error) => {
                        self.message = format!("整剧打包下载失败：{error}");
                        self.alert(studio);
                    }
                }
            }
            AiDramaMsg::Queued(result) => {
                self.busy = false;
                self.cancel = None;
                self.stage.clear();
                self.progress = 0.0;
                self.queue_left = 0;
                match *result {
                    Ok(outputs) => {
                        let count = outputs.len();
                        self.outputs.extend(outputs);
                        self.message = format!(
                            "整剧完成：{count} 集已下载到成片库。可以「全部插入时间轴」或到成片目录取文件。"
                        );
                        self.step = 4;
                        studio.notify("整剧成片已全部下载", false);
                    }
                    Err(error) => {
                        let done = self.outputs.len();
                        self.message = format!(
                            "整剧队列失败（已完成的 {done} 集在成片库里）：{error}"
                        );
                        self.alert(studio);
                    }
                }
            }
        }
    }

    /// 关闭后的清理：任务继续，只是不看了。
    pub fn close(&mut self) {
        self.open = false;
    }

    fn action(&mut self, action: &str, index: i32, text: String, studio: &mut Studio) {
        match action {
            "close" => self.open = false,
            "step" => {
                self.step = index.clamp(0, 4);
                if self.step == 4 && self.backend.ready && self.outputs.is_empty() {
                    self.message =
                        "成片库还是空的：在「④ 分镜产物」配好素材后点「生成当前集」或「生成全部集数」"
                            .to_owned();
                }
            }
            // ── 设定 ──
            "theme" => {
                self.theme_pick = index.max(0);
                if self.theme_pick as usize >= THEMES.len() {
                    self.theme_text = text;
                }
            }
            "theme-text" => self.theme_text = text.chars().take(80).collect(),
            "episodes" => self.episodes = index.clamp(0, 9) + 1,
            "duration" => self.duration = index.clamp(0, (DURATIONS.len() - 1) as i32),
            "aspect" => self.aspect = index.clamp(0, 1),
            "style" => self.style = index.clamp(0, (STYLES.len() - 1) as i32),
            "compliance" => self.compliance = !self.compliance,
            "script-platform" => self.script_platform(studio),
            "script-local" => self.script_local(studio),
            "refresh" => {
                if !self.checking {
                    self.check_backend(studio);
                }
            }
            // ── 剧本 ──
            "episode" => {
                if let Some(script) = self.script.as_ref() {
                    self.episode = (index.max(0) as usize).min(script.episodes.len().saturating_sub(1));
                    self.scene = 0;
                    self.sync_assets();
                }
            }
            "episode-prev" => {
                if let Some(script) = self.script.as_ref() {
                    self.episode = self.episode.saturating_sub(1).min(script.episodes.len().saturating_sub(1));
                    self.scene = 0;
                    self.sync_assets();
                }
            }
            "episode-next" => {
                if let Some(script) = self.script.as_ref() {
                    self.episode = (self.episode + 1).min(script.episodes.len().saturating_sub(1));
                    self.scene = 0;
                    self.sync_assets();
                }
            }
            "ep-title" => {
                if let Some(episode) = self.episode_mut() {
                    episode.title = text.chars().take(60).collect();
                }
            }
            "ep-hook" => {
                if let Some(episode) = self.episode_mut() {
                    episode.hook = text.chars().take(160).collect();
                }
            }
            "episode-add" => {
                let seconds = f64::from(self.seconds());
                if let Some(script) = self.script.as_mut() {
                    let no = script.episodes.len() as u32 + 1;
                    if script.episodes.len() < 10 {
                        script.episodes.push(Episode {
                            no,
                            title: format!("第 {no} 集"),
                            hook: String::new(),
                            cover_text: String::new(),
                            duration: seconds,
                            scenes: Vec::new(),
                            voice: VOICES[0].1.to_owned(),
                            bgm: BGMS[0].to_owned(),
                        });
                        self.episode = script.episodes.len() - 1;
                        self.scene = 0;
                        self.character = 0;
                        self.voice = 0;
                        self.bgm = 0;
                    }
                }
            }
            "episode-remove" => {
                if let Some(script) = self.script.as_mut() {
                    if script.episodes.len() > 1 {
                        script.episodes.remove(self.episode);
                        for (at, episode) in script.episodes.iter_mut().enumerate() {
                            episode.no = at as u32 + 1;
                        }
                        self.episode = self.episode.min(script.episodes.len() - 1);
                        self.scene = 0;
                    }
                }
                self.sync_assets();
            }
            // ── 分镜 ──
            "scene" => self.scene = index.max(0) as usize,
            "scene-desc" => {
                if let Some(scene) = self.scene_mut() {
                    scene.desc = text.chars().take(400).collect();
                }
            }
            "scene-line" => {
                if let Some(scene) = self.scene_mut() {
                    scene.line = text.chars().take(240).collect();
                }
            }
            "scene-add" => {
                let no = self.script.as_ref().map_or(0, |script| {
                    script.episodes.get(self.episode).map_or(0, |episode| episode.scenes.len())
                });
                let episode = self.episode;
                if let Some(script) = self.script.as_mut() {
                    if let Some(episode) = script.episodes.get_mut(episode) {
                        if episode.scenes.len() < 20 {
                            episode.scenes.push(aidrama::Scene {
                                shot: format!("镜 {}", no + 1),
                                desc: String::new(),
                                line: String::new(),
                                // 新加的镜头先按 3 秒占位，用户在剧本里改描述/台词后
                                // 由平台按整集时长重新分配；这里不臆造精确值。
                                duration: 3.0,
                                asset: String::new(),
                                candidates: Vec::new(),
                                pick: 0,
                            });
                            self.scene = no;
                        }
                    }
                }
            }
            "scene-remove" => {
                let episode = self.episode;
                let scene = self.scene;
                if let Some(script) = self.script.as_mut() {
                    if let Some(episode) = script.episodes.get_mut(episode) {
                        if episode.scenes.len() > 1 {
                            episode.scenes.remove(scene);
                            self.scene = scene.min(episode.scenes.len() - 1);
                        }
                    }
                }
            }
            "scene-up" => {
                let episode = self.episode;
                if let Some(episode) = self
                    .script
                    .as_mut()
                    .and_then(|script| script.episodes.get_mut(episode))
                {
                    if self.scene > 0 && self.scene < episode.scenes.len() {
                        episode.scenes.swap(self.scene, self.scene - 1);
                        self.scene -= 1;
                    }
                }
            }
            "scene-down" => {
                let episode = self.episode;
                if let Some(episode) = self
                    .script
                    .as_mut()
                    .and_then(|script| script.episodes.get_mut(episode))
                {
                    if self.scene + 1 < episode.scenes.len() {
                        episode.scenes.swap(self.scene, self.scene + 1);
                        self.scene += 1;
                    }
                }
            }
            "match-assets" => self.match_assets(studio),
            // ── ③ 成片资产：角色卡 / 配音 / BGM ──
            "char" => self.character = index.max(0) as usize,
            "char-add" => {
                if let Some(script) = self.script.as_mut() {
                    if script.characters.len() < 12 {
                        script.characters.push(Character {
                            name: format!("新角色 {}", script.characters.len() + 1),
                            role: "身份 / 人物功能".to_owned(),
                            look: "外形与气质（一句话）".to_owned(),
                        });
                    }
                }
                self.character = self
                    .script
                    .as_ref()
                    .map_or(0, |script| script.characters.len().saturating_sub(1));
            }
            "char-remove" => {
                let remove_at = self.character;
                if let Some(script) = self.script.as_mut() {
                    if script.characters.len() > 1 {
                        script.characters.remove(remove_at.min(script.characters.len() - 1));
                    }
                }
                self.character = self.script.as_ref().map_or(0, |script| {
                    self.character.min(script.characters.len().saturating_sub(1))
                });
            }
            "char-text" => {
                let at = self.character;
                let line: String = text.chars().take(160).collect();
                if let Some(script) = self.script.as_mut() {
                    if let Some(slot) = script.characters.get_mut(at) {
                        *slot = Character::from_line(&line);
                    }
                }
            }
            "voice" => {
                self.voice = index.clamp(0, (VOICES.len() - 1) as i32);
                let id = VOICES[self.voice as usize].1.to_owned();
                if let Some(episode) = self.episode_mut() {
                    episode.voice = id;
                }
            }
            "bgm" => {
                self.bgm = index.clamp(0, (BGMS.len() - 1) as i32);
                let label = BGMS[self.bgm as usize].to_owned();
                if let Some(episode) = self.episode_mut() {
                    episode.bgm = label;
                }
            }
            "download-zip" => self.download_zip(studio),
            "asset-cycle" => {
                if let Some(scene) = self.scene_mut() {
                    if !scene.candidates.is_empty() {
                        scene.pick = (scene.pick + 1) % scene.candidates.len();
                        scene.asset = scene.candidates[scene.pick].clone();
                    }
                }
            }
            // ── 成片 ──
            "render-current" => self.render_one(studio),
            "render-all" => self.render_all(studio),
            "cancel" => self.cancel_run(studio),
            "open-folder" => {
                let dir = concat_host::ai_studio::output_dir(&studio.host.dirs);
                let _ = std::fs::create_dir_all(&dir);
                if let Err(error) = opener::open(&dir) {
                    self.message = format!("打开成片目录失败：{error}");
                }
            }
            "insert-all" => self.insert_all(studio),
            "export-json" => self.export_json(studio),
            "web" => {
                let _ = opener::open(aidrama::DRAMA_WEB);
            }
            "goto-studio" => {
                self.open = false;
                studio.handle(Msg::AiStudio(crate::panes::ai_studio::AiStudioMsg::Open(0)));
            }
            _ => {}
        }
    }

    fn episode_mut(&mut self) -> Option<&mut Episode> {
        let at = self.episode;
        self.script.as_mut().and_then(|script| script.episodes.get_mut(at))
    }

    /// ③ 成片资产跟着当前集走：角色卡选中项归零，音色/BGM 从这一集的设定读回。
    fn sync_assets(&mut self) {
        self.character = 0;
        let (voice, bgm) = self
            .script
            .as_ref()
            .and_then(|script| script.episodes.get(self.episode))
            .map(|episode| {
                (
                    VOICES
                        .iter()
                        .position(|(_, id)| *id == episode.voice.as_str())
                        .map_or(0, |at| at as i32),
                    BGMS.iter()
                        .position(|label| *label == episode.bgm.as_str())
                        .map_or(0, |at| at as i32),
                )
            })
            .unwrap_or((0, 0));
        self.voice = voice;
        self.bgm = bgm;
    }

    fn scene_mut(&mut self) -> Option<&mut aidrama::Scene> {
        let episode = self.episode;
        let scene = self.scene;
        self.script
            .as_mut()
            .and_then(|script| script.episodes.get_mut(episode))
            .and_then(|episode| episode.scenes.get_mut(scene))
    }

    fn theme_label(&self) -> String {
        if (self.theme_pick as usize) < THEMES.len() {
            THEMES[self.theme_pick as usize].1.to_owned()
        } else if self.theme_text.trim().is_empty() {
            "都市逆袭".to_owned()
        } else {
            self.theme_text.trim().to_owned()
        }
    }

    fn seconds(&self) -> u32 {
        DURATIONS[self.duration.max(0) as usize % DURATIONS.len()]
    }

    fn style_id(&self) -> &'static str {
        STYLES[self.style.max(0) as usize % STYLES.len()].1
    }

    fn aspect_id(&self) -> &'static str {
        ASPECTS[self.aspect.max(0) as usize % ASPECTS.len()].1
    }

    /// 把同一句失败提示只弹一次通知。
    fn alert(&mut self, studio: &mut Studio) {
        if self.last_alert != self.message {
            self.last_alert.clone_from(&self.message);
            studio.notify(&self.message, true);
        }
    }

    /// 打开时探一次后端（历史短剧一起取）。
    fn check_backend(&mut self, _studio: &mut Studio) {
        if self.checking {
            return;
        }
        self.checking = true;
        let token = concat_host::account::load().map(|account| account.token);
        host::spawn(
            move || {
                let backend = aidrama::availability(token.as_deref());
                let history = match token.as_deref() {
                    Some(token) if backend.ready => aidrama::history(token),
                    _ => Err("后端联调中".to_owned()),
                };
                (backend, history)
            },
            |studio, _, _, (backend, history)| {
                studio.handle(Msg::AiDrama(AiDramaMsg::Backend { backend, history }));
            },
        );
    }

    /// 平台生成剧本。
    fn script_platform(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Ok(token) = aidrama::token() else {
            self.message = "请先登录星籁账号：登录后回到这里再生成".to_owned();
            self.alert(studio);
            return;
        };
        if !self.compliance {
            self.message = "先勾选下面的合规确认（本人素材 / 已授权 / 遵守规范）再生成".to_owned();
            self.alert(studio);
            return;
        }
        if !self.backend.ready {
            self.message = format!(
                "{}。剧本可以先用「本地模板生成剧本」编辑；接口就绪后不用升级桌面端。",
                self.backend.line
            );
            self.alert(studio);
            return;
        }
        let theme = self.theme_label();
        let episodes = self.episodes.max(1) as u32;
        let seconds = self.seconds();
        let style = self.style_id().to_owned();
        let aspect = self.aspect_id().to_owned();
        self.busy = true;
        self.progress = 0.0;
        self.stage = "生成分集剧本…".to_owned();
        self.message = format!(
            "已提交平台：{theme} · {episodes} 集 × {seconds} 秒 · 画幅 {} · {style}",
            self.aspect_id()
        );
        self.last_alert.clear();
        host::spawn(
            move || aidrama::script(&token, &theme, episodes, seconds, &aspect, &style),
            |studio, _, _, result| {
                studio.handle(Msg::AiDrama(AiDramaMsg::Script(Box::new(result))));
            },
        );
    }

    /// 本地模板生成剧本（不调平台，对联调前可用，面板明确标注）。
    fn script_local(&mut self, studio: &mut Studio) {
        let theme = self.theme_label();
        let style = self.style_id().to_owned();
        let script = Script::local(&theme, &theme, self.episodes.max(1) as u32, self.seconds(), &style);
        studio.handle(Msg::AiDrama(AiDramaMsg::Script(Box::new(Ok(script)))));
    }

    /// 给当前集的分镜自动配站内商用素材。
    fn match_assets(&mut self, studio: &mut Studio) {
        if self.matching || self.busy {
            return;
        }
        let Ok(token) = aidrama::token() else {
            self.message = "配素材要登录星籁账号（素材库接口按账号调用）".to_owned();
            self.alert(studio);
            return;
        };
        let Some(script) = self.script.as_ref() else {
            return;
        };
        let Some(episode) = script.episodes.get(self.episode) else {
            return;
        };
        let queries: Vec<String> = episode
            .scenes
            .iter()
            .map(|scene| {
                if scene.desc.trim().is_empty() {
                    "生活 场景".to_owned()
                } else {
                    aidrama::asset_query(&scene.desc)
                }
            })
            .collect();
        if queries.is_empty() {
            self.message = "这一集还没有分镜：先在剧本里加分镜".to_owned();
            self.alert(studio);
            return;
        }
        let episode_at = self.episode;
        self.matching = true;
        self.message = format!("正在给第 {} 集的 {} 个分镜配素材…", episode_at + 1, queries.len());
        host::spawn(
            move || -> Result<Vec<Vec<String>>, String> {
                let mut out = Vec::new();
                for query in queries {
                    let candidates = aidrama::match_assets(&token, &query, 4)?;
                    out.push(candidates);
                }
                Ok(out)
            },
            move |studio, _, _, result| match result {
                Ok(candidates) => {
                    let mut count = 0usize;
                    if let Some(script) = studio.ai_drama.script.as_mut() {
                        if let Some(episode) = script.episodes.get_mut(episode_at) {
                            for (at, list) in candidates.into_iter().enumerate() {
                                if let Some(scene) = episode.scenes.get_mut(at) {
                                    scene.candidates = list;
                                    scene.pick = 0;
                                    scene.asset = scene.candidates.first().cloned().unwrap_or_default();
                                    if !scene.asset.is_empty() {
                                        count += 1;
                                    }
                                }
                            }
                        }
                    }
                    studio.handle(Msg::AiDrama(AiDramaMsg::Matched(Box::new(Ok(count)))));
                }
                Err(error) => {
                    studio.handle(Msg::AiDrama(AiDramaMsg::Matched(Box::new(Err(error)))));
                }
            },
        );
    }

    /// 渲染当前集。
    fn render_one(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Some(script) = self.script.clone() else {
            self.message = "先生成剧本".to_owned();
            self.alert(studio);
            return;
        };
        if !self.compliance {
            self.message = "先勾选合规确认再生成成片".to_owned();
            self.alert(studio);
            return;
        }
        let Ok(token) = aidrama::token() else {
            self.message = "请先登录星籁账号".to_owned();
            self.alert(studio);
            return;
        };
        if !self.backend.ready {
            self.message = format!(
                "{}。剧本与分镜可以继续编辑、导出 JSON；渲染按钮等后端就绪即可用。",
                self.backend.line
            );
            self.alert(studio);
            return;
        }
        let Some(episode) = script.episodes.get(self.episode).cloned() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.busy = true;
        self.progress = 0.0;
        self.stage = format!("提交第 {} 集…", episode.no);
        self.message = format!(
            "已提交第 {} 集：{} 个分镜 · 平台渲染。{}",
            episode.no,
            episode.scenes.len(),
            if self.admin { "内部账号免扣积分（成本照记）" } else { "按平台实际扣费" }
        );
        self.last_alert.clear();
        let dirs = studio.host.dirs.clone();
        let mut last = None::<std::time::Instant>;
        let report = move |fraction: f32, stage: String| {
            let due = last.is_none_or(|at: std::time::Instant| at.elapsed() >= std::time::Duration::from_millis(400));
            if !due {
                return;
            }
            last = Some(std::time::Instant::now());
            host::on_ui(move |studio, _, _| {
                studio.handle(Msg::AiDrama(AiDramaMsg::Progress { fraction, stage }));
            });
        };
        let episode_no = episode.no;
        host::spawn(
            move || -> Result<aidrama::EpisodeOutput, String> {
                let mut report = report;
                let task = aidrama::render(&token, &script, &episode)?;
                aidrama::wait_episode(&dirs, &token, episode_no, &task, &mut report, &cancel)
            },
            |studio, _, _, result| {
                studio.handle(Msg::AiDrama(AiDramaMsg::Rendered(Box::new(result))));
            },
        );
    }

    /// 整剧队列：一集一集渲染，任何一集失败就停下并保留已完成的。
    fn render_all(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Some(script) = self.script.clone() else {
            self.message = "先生成剧本".to_owned();
            self.alert(studio);
            return;
        };
        if !self.compliance {
            self.message = "先勾选合规确认再生成成片".to_owned();
            self.alert(studio);
            return;
        }
        let Ok(token) = aidrama::token() else {
            self.message = "请先登录星籁账号".to_owned();
            self.alert(studio);
            return;
        };
        if !self.backend.ready {
            self.message = format!(
                "{}。整剧队列等后端就绪即可用；现在可以先把剧本改好、导出 JSON。",
                self.backend.line
            );
            self.alert(studio);
            return;
        }
        let total = script.episodes.len();
        self.queue_left = total;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.busy = true;
        self.progress = 0.0;
        self.stage = format!("整剧队列：0/{total}");
        self.message = format!(
            "整剧队列已提交：{total} 集，逐集渲染并下载；可以「取消」（当前集停止等待，平台任务可能仍在后台跑完）。"
        );
        self.last_alert.clear();
        let dirs = studio.host.dirs.clone();
        let mut last = None::<std::time::Instant>;
        let report = move |fraction: f32, stage: String| {
            let due = last.is_none_or(|at: std::time::Instant| at.elapsed() >= std::time::Duration::from_millis(500));
            if !due {
                return;
            }
            last = Some(std::time::Instant::now());
            host::on_ui(move |studio, _, _| {
                studio.handle(Msg::AiDrama(AiDramaMsg::Progress { fraction, stage }));
            });
        };
        host::spawn(
            move || -> Result<Vec<aidrama::EpisodeOutput>, String> {
                let mut report = report;
                let mut outputs = Vec::new();
                let count = script.episodes.len();
                for (at, episode) in script.episodes.iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("已取消".to_owned());
                    }
                    report(
                        at as f32 / count as f32,
                        format!("整剧队列：{}/{} · 第 {} 集提交中", at, count, episode.no),
                    );
                    let task = aidrama::render(&token, &script, episode)?;
                    let mut episode_report = |fraction: f32, stage: String| {
                        report(
                            (at as f32 + fraction.clamp(0.0, 1.0)) / count as f32,
                            format!("整剧队列 {}/{} · {stage}", at + 1, count),
                        );
                    };
                    let output = aidrama::wait_episode(
                        &dirs,
                        &token,
                        episode.no,
                        &task,
                        &mut episode_report,
                        &cancel,
                    )?;
                    outputs.push(output);
                }
                Ok(outputs)
            },
            |studio, _, _, result| {
                studio.handle(Msg::AiDrama(AiDramaMsg::Queued(Box::new(result))));
            },
        );
    }

    fn cancel_run(&mut self, studio: &mut Studio) {
        if let Some(cancel) = self.cancel.as_ref() {
            cancel.store(true, Ordering::Relaxed);
            self.stage = "取消中…".to_owned();
            self.message =
                "已取消：平台上的任务可能仍在后台跑完（成片不会自动下载），已完成的集在成片库里".to_owned();
            studio.notify("已取消等待", true);
        }
    }

    /// 成片库全部插入时间轴：从播放头开始，一集接一集。
    fn insert_all(&mut self, studio: &mut Studio) {
        if self.outputs.is_empty() {
            self.message = "成片库是空的：先生成成片".to_owned();
            self.alert(studio);
            return;
        }
        let outputs = self.outputs.clone();
        let start = f64::from(studio.playhead.max(0.0));
        let inserted = Self::insert_outputs(studio, &outputs);
        if inserted > 0 {
            self.message = format!("已插入 {inserted} 集到时间轴（从 {start:.1}s 起往前排）");
            studio.notify(&format!("已插入 {inserted} 条短剧成片"), false);
        } else {
            self.message = "插入失败：请先打开一个工程".to_owned();
            self.alert(studio);
        }
    }

    /// 把成片铺到时间轴：从播放头开始一集接一集，返回插入条数。
    fn insert_outputs(studio: &mut Studio, outputs: &[aidrama::EpisodeOutput]) -> usize {
        let mut inserted = 0usize;
        let mut start = f64::from(studio.playhead.max(0.0));
        for output in outputs {
            let Some(summary) = media::probe(&output.path.to_string_lossy()).ok() else {
                continue;
            };
            let name = output
                .path
                .file_stem()
                .map(|stem| format!("AI 短剧 · {}", stem.to_string_lossy()))
                .unwrap_or_else(|| "AI 短剧成片".to_owned());
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
            });
            let Some(media_id) = media_id else {
                continue;
            };
            if studio
                .apply(Command::AddClipAtFirstFree {
                    media_id,
                    start,
                })
                .is_some()
            {
                inserted += 1;
                start += summary.duration.unwrap_or(1.0).max(0.1);
            }
        }
        inserted
    }

    /// 导出剧本 JSON（契约形状，后端/网页可直接读）。
    fn export_json(&mut self, studio: &mut Studio) {
        let Some(script) = self.script.clone() else {
            self.message = "还没有剧本：先生成或本地模板生成一部".to_owned();
            self.alert(studio);
            return;
        };
        let folder = studio
            .session
            .as_ref()
            .map(|session| PathBuf::from(session.path()))
            .unwrap_or_else(|| PathBuf::from(crate::studio::home_folder("Documents")));
        let name = format!(
            "AI短剧-{}-{}集.json",
            script.theme.chars().take(12).collect::<String>(),
            script.episodes.len()
        );
        let title = "导出 AI 短剧剧本 JSON".to_owned();
        let family = "JSON 剧本".to_owned();
        crate::host::after_dialog(
            move || {
                let chosen = crate::platform::save_file(&title, &folder, &name, (family.as_str(), &["json"]))?;
                Some(if chosen.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("json")) {
                    chosen
                } else {
                    chosen.with_extension("json")
                })
            },
            move |studio, _, _, path| {
                match script.save_json(&path) {
                    Ok(bytes) => {
                        studio.ai_drama.message =
                            format!("剧本已导出：{} · {} 字节", path.display(), bytes);
                        studio.notify("剧本 JSON 已导出", false);
                    }
                    Err(error) => {
                        studio.ai_drama.message = format!("导出失败：{error}");
                        studio.ai_drama.last_alert.clear();
                    }
                }
            },
        );
    }

    /// 整剧打包下载（`GET /aidrama/download/{drama_id}` → zip）。
    fn download_zip(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Some(script) = self.script.clone() else {
            self.message = "还没有剧本：先生成一部".to_owned();
            self.alert(studio);
            return;
        };
        if script.drama_id.trim().is_empty() {
            self.message =
                "本地模板示例没有平台 id：先用「平台生成剧本」拿到 drama_id，再打包整剧下载".to_owned();
            self.alert(studio);
            return;
        }
        let Ok(token) = aidrama::token() else {
            self.message = "请先登录星籁账号".to_owned();
            self.alert(studio);
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.busy = true;
        self.progress = 0.0;
        self.stage = "打包整剧 zip…".to_owned();
        self.message = format!("正在从平台下载《{}》整剧包…", script.title);
        self.last_alert.clear();
        let dirs = studio.host.dirs.clone();
        let drama_id = script.drama_id.clone();
        host::spawn(
            move || aidrama::download_zip(&dirs, &token, &drama_id, &cancel),
            |studio, _, _, result| {
                studio.handle(Msg::AiDrama(AiDramaMsg::Zipped(Box::new(result))));
            },
        );
    }

    /// 发布给 Slint 的数据。
    pub fn data(&self, signed_in: bool, account_line: &str) -> AiDramaData {
        let script = self.script.as_ref();
        let episodes = script.map_or(0, |script| script.episodes.len());
        let current = script.and_then(|script| script.episodes.get(self.episode));
        let scenes = current.map_or(0, |episode| episode.scenes.len());
        let scene = current.and_then(|episode| episode.scenes.get(self.scene));
        let local = script.is_some_and(|script| script.engine == "local-template");
        let engine = script
            .map(|script| {
                if script.engine == "local-template" {
                    "本地模板示例".to_owned()
                } else if script.engine.is_empty() {
                    "平台".to_owned()
                } else {
                    script.engine.clone()
                }
            })
            .unwrap_or_default();
        let summary = script
            .map(|script| {
                format!(
                    "{} 集 · {} 个分镜 · 引擎 {engine}",
                    script.episodes.len(),
                    script.scene_count()
                )
            })
            .unwrap_or_default();
        AiDramaData {
            open: self.open,
            step: self.step,
            busy: self.busy,
            matching: self.matching,
            progress: self.progress,
            stage: self.stage.as_str().into(),
            message: self.message.as_str().into(),
            account_line: account_line.into(),
            signed_in,
            admin_exempt: self.admin,
            backend_ready: self.backend.ready,
            backend_line: self.backend.line.as_str().into(),
            theme_pick: self.theme_pick,
            theme_text: self.theme_text.as_str().into(),
            episodes: self.episodes,
            duration: self.duration,
            aspect: self.aspect,
            style: self.style,
            compliance: self.compliance,
            has_script: script.is_some(),
            script_summary: summary.into(),
            script_local: local,
            drama_title: script.map(|script| script.title.clone()).unwrap_or_default().into(),
            drama_logline: script.map(|script| script.logline.clone()).unwrap_or_default().into(),
            drama_id: script.map(|script| script.drama_id.clone()).unwrap_or_default().into(),
            limits: aidrama::LIMITS.into(),
            episode_index: self.episode as i32,
            episode_count: episodes as i32,
            episode_no: current.map_or(0, |episode| episode.no) as i32,
            episode_title: current.map(|episode| episode.title.clone()).unwrap_or_default().into(),
            episode_hook: current.map(|episode| episode.hook.clone()).unwrap_or_default().into(),
            scene_index: self.scene as i32,
            scene_count: scenes as i32,
            scene_desc: scene.map(|scene| scene.desc.clone()).unwrap_or_default().into(),
            scene_line: scene.map(|scene| scene.line.clone()).unwrap_or_default().into(),
            scene_asset: scene.map(|scene| scene.asset.clone()).unwrap_or_default().into(),
            has_asset: scene.is_some_and(|scene| !scene.asset.is_empty()),
            scene_has_candidates: scene.is_some_and(|scene| scene.candidates.len() > 1),
            // ── ③ 成片资产 ──
            char_index: self.character as i32,
            char_count: script.map_or(0, |script| script.characters.len()) as i32,
            char_text: script
                .and_then(|script| script.characters.get(self.character))
                .map(Character::line)
                .unwrap_or_default()
                .into(),
            voice: self.voice,
            bgm: self.bgm,
            // ── ④ 分镜产物：已配图 / 总镜数 ──
            shot_ready: current.map_or(0, |episode| {
                episode
                    .scenes
                    .iter()
                    .filter(|scene| !scene.asset.trim().is_empty())
                    .count()
            }) as i32,
            shot_total: scenes as i32,
            estimate: aidrama::estimate_line(
                self.episodes.max(1) as u32,
                self.seconds(),
                self.admin,
            )
            .into(),
            aspect_line: format!(
                "{} · {} 秒/集 · {} 风格",
                self.aspect_id(),
                self.seconds(),
                self.style_id()
            )
            .into(),
            output_count: self.outputs.len() as i32,
            queue_left: self.queue_left as i32,
            has_outputs: !self.outputs.is_empty(),
            compliance_text: aidrama::COMPLIANCE.into(),
            web: aidrama::DRAMA_WEB.into(),
            theme_options: slint::ModelRc::new(slint::VecModel::from({
                let mut options: Vec<slint::SharedString> =
                    THEMES.iter().map(|(name, _)| (*name).into()).collect();
                options.push("自定义题材…".into());
                options
            })),
        }
    }

    /// 每集一行，给剧本列表。
    pub fn episode_rows(&self) -> Vec<slint::SharedString> {
        let script = self.script.as_ref();
        let current = self.episode;
        script
            .map(|script| {
                script
                    .episodes
                    .iter()
                    .enumerate()
                    .map(|(at, episode)| {
                        format!(
                            "{}第 {} 集 · {} · {} 镜{}",
                            if at == current { "▶ " } else { "" },
                            episode.no,
                            if episode.title.trim().is_empty() {
                                "未命名"
                            } else {
                                episode.title.trim()
                            },
                            episode.scenes.len(),
                            if episode.hook.trim().is_empty() {
                                String::new()
                            } else {
                                format!(" · 钩子：{}", episode.hook.trim())
                            }
                        )
                        .into()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 当前集的分镜行。
    pub fn scene_rows(&self) -> Vec<slint::SharedString> {
        let Some(script) = self.script.as_ref() else {
            return Vec::new();
        };
        let Some(episode) = script.episodes.get(self.episode) else {
            return Vec::new();
        };
        episode
            .scenes
            .iter()
            .enumerate()
            .map(|(at, scene)| {
                format!(
                    "{}{} · {}{}{}",
                    if at == self.scene { "▶ " } else { "" },
                    if scene.shot.trim().is_empty() {
                        format!("镜 {}", at + 1)
                    } else {
                        scene.shot.trim().to_owned()
                    },
                    if scene.desc.trim().is_empty() {
                        "（未写画面描述）"
                    } else {
                        scene.desc.trim()
                    },
                    if scene.line.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" · 台词：{}", scene.line.trim())
                    },
                    if scene.asset.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" · 素材：{}", scene.asset.trim())
                    }
                )
                .into()
            })
            .collect()
    }

    /// 成片库的行。
    pub fn output_rows(&self) -> Vec<slint::SharedString> {
        self.outputs.iter().map(|output| output.summary().into()).collect()
    }

    /// ③ 成片资产：角色卡的行（全剧共用）。
    pub fn character_rows(&self) -> Vec<slint::SharedString> {
        self.script
            .as_ref()
            .map(|script| {
                script
                    .characters
                    .iter()
                    .enumerate()
                    .map(|(at, row)| {
                        format!(
                            "{}{}",
                            if at == self.character { "▶ " } else { "" },
                            row.line()
                        )
                        .into()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 历史短剧的行。
    pub fn history_rows(&self) -> Vec<slint::SharedString> {
        self.history.iter().map(|line| line.as_str().into()).collect()
    }
}
