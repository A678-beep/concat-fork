// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! AI 面板：智能算力选择、本地配音（Piper）、照片数字人（二期插件占位）、
//! 本地插件管理，以及默认关闭的云端兜底。
//!
//! 「智能」是默认：本机检测出独显与内存够用就走本地（素材不出本机），
//! 核显/小显存/小内存则默认推荐云端。运行方式可以随时在面板顶部或设置里
//! 改成「仅本地 / 仅云端」。云端需要登录、消耗套餐额度，而且每次生成前都
//! 要用户点一下确认——**默认不上传任何素材**。
//!
//! 生成出来的文件一律落在工程目录：本地配音 `ai_voice/*.wav`，云端配音
//! `ai_voice/*.mp3`，数字人 `ai_avatar/*.mp4`，然后进素材库、上时间轴（当前
//! 播放头位置）。本地数字人是二期插件，这一版只做入口与说明。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_host::hardware::{Hardware, Tier};
use concat_host::media::{self, MediaSummary};
use concat_host::{cloud_ai, piper, plugins};
use concat_project::Command;
use concat_project::model::{MediaKind, MediaOrigin};

use crate::host::{self, spawn, spawn_in_project};
use crate::panes::Msg;
use crate::prefs::Preferences;
use crate::studio::Studio;
use crate::ui::{AiData, AiPluginItem, AiVoiceItem};

/// 云端每次生成的提醒，界面上常驻。
const CLOUD_COST: &str = "云端：需登录星籁账号，按套餐额度扣费（配音约 ¥0.004/51 字，数字人按次扣）；素材会上传到云端，每次生成前都会确认。";

/// 配音文字上限，与界面一致。
const MAX_TEXT: usize = 2000;

/// AI 运行方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AiMode {
    /// 智能：按硬件自动选本地或云端（默认）。
    #[default]
    Smart,
    /// 仅本地：只用本机插件，绝不联网。
    Local,
    /// 仅云端：走星籁平台接口，需登录。
    Cloud,
}

impl AiMode {
    /// 存进设置的键。
    pub fn key(self) -> &'static str {
        match self {
            AiMode::Smart => "smart",
            AiMode::Local => "local",
            AiMode::Cloud => "cloud",
        }
    }

    /// 界面上的分段控件下标。
    pub fn index(self) -> i32 {
        match self {
            AiMode::Smart => 0,
            AiMode::Local => 1,
            AiMode::Cloud => 2,
        }
    }

    /// 从下标认。
    pub fn from_index(index: i32) -> AiMode {
        match index {
            1 => AiMode::Local,
            2 => AiMode::Cloud,
            _ => AiMode::Smart,
        }
    }

    /// 从设置里存的键认；缺省或认不出是智能。
    pub fn from_prefs(value: Option<&str>) -> AiMode {
        match value.map(str::trim) {
            Some("local") => AiMode::Local,
            Some("cloud") => AiMode::Cloud,
            _ => AiMode::Smart,
        }
    }

    /// 分段控件上的字。
    pub fn label(self) -> &'static str {
        match self {
            AiMode::Smart => "智能",
            AiMode::Local => "仅本地",
            AiMode::Cloud => "仅云端",
        }
    }
}

/// 面板收到的消息：界面上的每一个动作，和后台线程的每一条回报。
#[derive(Debug)]
pub enum AiMsg {
    /// 菜单打开面板并直接落在某一页：0 配音、1 数字人、2 插件。
    OpenTab(i32),
    /// 关闭面板。
    Close,
    /// 换页。
    Tab(i32),
    /// 改要念的文字。
    Text(String),
    /// 换音色。
    Voice(i32),
    /// 改语速。
    Rate(f32),
    /// 生成（配音页或数字人页，看当前页）。
    Generate,
    /// 试听刚生成的音频。
    Preview,
    /// 取消正在跑的生成或下载。
    Cancel,
    /// 换运行方式。
    Mode(i32),
    /// 重新检测硬件。
    Redetect,
    /// 向导：用本地。
    WizardLocal,
    /// 向导：用云端。
    WizardCloud,
    /// 向导：先不定。
    WizardClose,
    /// 找插件包 zip 并导入。
    PluginImport,
    /// 插件导入完成。
    PluginImported(Box<Result<String, String>>),
    /// 删掉一个插件。
    PluginRemove(String),
    /// 在资源管理器里打开插件目录。
    PluginOpen(String),
    /// 从服务器下载并安装一个插件。
    PluginDownload(String),
    /// 刷新插件列表。
    PluginRefresh,
    /// 挑照片。
    PhotoPick,
    /// 清掉照片。
    PhotoClear,
    /// 云端素材确认：继续。
    CloudConfirm,
    /// 云端素材确认：取消。
    CloudCancel,
    /// 后台进度：0..1 和一句话。
    Progress { fraction: f32, stage: String },
    /// 生成完成（成功是文件与插入结果，失败是一句中文）。
    Finished(Box<Result<Done, String>>),
    /// 云端音色列表回来了。
    CloudVoices(Box<Result<concat_host::cloud_ai::CloudVoices, String>>),
}

/// 一次生成的结果。
#[derive(Clone, Debug)]
pub struct Done {
    /// 落盘的文件。
    pub path: PathBuf,
    /// 探测结果。
    pub summary: MediaSummary,
    /// 素材库里的名字。
    pub name: String,
    /// 一句话说明（时长、耗时、CPU）。
    pub detail: String,
    /// 云端合成时平台给的音频地址：数字人可以直接引用它。
    pub cloud_audio_url: Option<String>,
}

/// 选中的照片。
struct Photo {
    bytes: Vec<u8>,
    mime: String,
    name: String,
    size: u64,
    image: slint::Image,
}

/// 面板状态。
pub struct AiPane {
    /// 面板开着。
    pub open: bool,
    /// 当前页：0 配音、1 数字人、2 插件。
    pub tab: i32,
    /// 运行方式设置。
    pub mode: AiMode,
    /// 硬件检测结果。
    pub hardware: Option<Hardware>,
    /// 本机插件。
    pub plugins: Vec<plugins::Plugin>,
    /// 装好的 Piper 引擎。
    pub piper: Option<piper::Engine>,
    /// 当前可选的音色（本地或云端，跟着运行方式走）。
    voices: Vec<VoiceRow>,
    /// 云端音色列表。
    cloud_voices: Option<cloud_ai::CloudVoices>,
    /// 选中的音色下标。
    pub voice: i32,
    /// 语速。
    pub rate: f32,
    /// 要念的文字。
    pub text: String,
    /// 正在生成/下载。
    pub busy: bool,
    /// 进度 0..1；拿不到进度时是 0。
    pub progress: f32,
    /// 正在做什么。
    pub stage: String,
    /// 常驻说明/最近一次的结果。
    pub message: String,
    /// 最近一次生成的结果句。
    pub last: String,
    /// 最近一次生成的音频，试听用。
    last_audio: Option<PathBuf>,
    /// 最近一次生成音频的名字。
    audio_name: String,
    /// 云端合成/数字人用的音频地址。
    cloud_audio_url: Option<String>,
    /// 照片。
    photo: Option<Photo>,
    /// 向导要不要开。
    pub wizard_open: bool,
    /// 云端确认条开着。
    pub confirm_open: bool,
    /// 确认条上的字。
    pub confirm_text: String,
    /// 等确认的云端动作。
    confirm: Option<Confirm>,
    /// 当前登录的账号名。
    pub signed_in: bool,
    /// 账号名，给面板显示。
    pub account: String,
    /// 取消标志。
    cancel: Option<Arc<AtomicBool>>,
    /// 本地引擎上一个失败的原因，避免每次发布都重复猜。
    engine_issue: String,
}

/// 等确认的云端动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    Tts,
    Avatar,
}

/// 面板里的一行音色。
#[derive(Clone, Debug, Default)]
struct VoiceRow {
    id: String,
    name: String,
    detail: String,
    ready: bool,
    cloned: bool,
}

impl Default for AiPane {
    fn default() -> Self {
        AiPane {
            open: false,
            tab: 0,
            mode: AiMode::Smart,
            hardware: None,
            plugins: Vec::new(),
            piper: None,
            voices: Vec::new(),
            cloud_voices: None,
            voice: 0,
            rate: 1.0,
            text: String::new(),
            busy: false,
            progress: 0.0,
            stage: String::new(),
            message: String::new(),
            last: String::new(),
            last_audio: None,
            audio_name: String::new(),
            cloud_audio_url: None,
            photo: None,
            wizard_open: false,
            confirm_open: false,
            confirm_text: String::new(),
            confirm: None,
            signed_in: false,
            account: String::new(),
            cancel: None,
            engine_issue: String::new(),
        }
    }
}

impl AiPane {
    /// 开一个面板：读本机插件、检测硬件，并把首次使用的向导准备好。
    pub fn new(dirs: &concat_host::AppDirs, prefs: &Preferences) -> AiPane {
        let mut pane = AiPane {
            mode: AiMode::from_prefs(prefs.ai_mode.as_deref()),
            rate: prefs.ai_rate.unwrap_or(1.0).clamp(0.8, 1.3),
            wizard_open: !prefs.ai_wizard_seen,
            ..AiPane::default()
        };
        pane.hardware = Some(Hardware::detect());
        pane.refresh(dirs);
        // 上次用的音色还在插件里就接着用。
        if let Some(wanted) = prefs.ai_voice.as_deref()
            && let Some(index) = pane.voices.iter().position(|voice| voice.id == wanted)
        {
            pane.voice = index as i32;
        }
        pane
    }

    /// 设置页要显示的下标。
    pub fn mode_index(&self) -> i32 {
        self.mode.index()
    }

    /// 设置页改运行方式。
    pub fn set_mode(&mut self, index: i32) {
        self.mode = AiMode::from_index(index);
        self.cloud_voices = None;
        self.confirm = None;
        self.confirm_open = false;
    }

    /// 重新读插件、登录状态与音色。
    pub fn refresh(&mut self, dirs: &concat_host::AppDirs) {
        self.plugins = plugins::scan(dirs);
        self.piper = self
            .plugins
            .iter()
            .find(|plugin| plugin.engine() == "piper")
            .and_then(|plugin| match piper::Engine::load(plugin) {
                Ok(engine) => {
                    self.engine_issue.clear();
                    Some(engine)
                }
                Err(error) => {
                    self.engine_issue = error;
                    None
                }
            });
        if let Some(account) = concat_host::account::load() {
            self.signed_in = !account.token.trim().is_empty();
            self.account = account.display_name();
        } else {
            self.signed_in = false;
            self.account.clear();
        }
        // 云端音色：只有真的走云端、且登录着才去取。
        if self.effective() == AiMode::Cloud
            && self.signed_in
            && self.cloud_voices.is_none()
            && !self.busy
        {
            self.load_cloud_voices();
        }
        self.rebuild_voices();
        if !self.busy {
            self.message = self.hint();
        }
    }

    /// 去平台取音色列表（后台）。
    fn load_cloud_voices(&mut self) {
        self.message = "正在获取云端音色…".to_owned();
        spawn(
            || cloud_ai::token().and_then(|token| cloud_ai::list_voices(&token)),
            |studio, _, _, result| {
                studio.handle(Msg::Ai(AiMsg::CloudVoices(Box::new(result))));
            },
        );
    }

    /// 音色列表按运行方式重建。
    fn rebuild_voices(&mut self) {
        let mut rows: Vec<VoiceRow> = Vec::new();
        if self.effective() == AiMode::Cloud {
            if let Some(cloud) = self.cloud_voices.as_ref() {
                for voice in &cloud.voices {
                    rows.push(VoiceRow {
                        id: voice.id.clone(),
                        name: voice.label(),
                        detail: if voice.desc.trim().is_empty() {
                            voice.tag.clone()
                        } else {
                            format!("{} · {}", voice.tag, voice.desc)
                        },
                        ready: voice.ready,
                        cloned: voice.cloned,
                    });
                }
            }
        } else if let Some(engine) = self.piper.as_ref() {
            for voice in &engine.voices {
                rows.push(VoiceRow {
                    id: voice.id.clone(),
                    name: format!("{} · {}", voice.name, voice.quality),
                    detail: voice.tag.clone(),
                    ready: true,
                    cloned: false,
                });
            }
        }
        self.voices = rows;
        if self.voice < 0 || self.voice as usize >= self.voices.len() {
            self.voice = 0;
            // 默认音色：平台的 default，或者第一个可用。
            if self.effective() == AiMode::Cloud
                && let Some(cloud) = self.cloud_voices.as_ref()
                && let Some(index) = self
                    .voices
                    .iter()
                    .position(|voice| voice.id == cloud.default_id)
            {
                self.voice = index as i32;
            }
        }
    }

    /// 面板顶上的常驻小字：现在用什么、为什么。
    fn reason(&self) -> String {
        let effective = self.effective();
        let mode = match (self.mode, effective) {
            (AiMode::Smart, AiMode::Local) => "智能选择 · 本地",
            (AiMode::Smart, _) => "智能选择 · 云端",
            (AiMode::Local, _) => "仅本地",
            (AiMode::Cloud, _) => "仅云端",
        };
        let hardware = self
            .hardware
            .as_ref()
            .map(Hardware::reason)
            .unwrap_or_else(|| "本机硬件还没有检测".to_owned());
        match effective {
            AiMode::Local => format!("{mode}：{hardware}"),
            _ => format!("{mode}：{hardware}；云端需要登录，素材会上传，每次生成前确认。"),
        }
    }

    /// 这次会走哪边。
    fn effective(&self) -> AiMode {
        match self.mode {
            AiMode::Smart => match self.hardware.as_ref().map(|hardware| hardware.tier) {
                Some(Tier::CloudRec) => AiMode::Cloud,
                _ => AiMode::Local,
            },
            other => other,
        }
    }

    /// 现在这条状态该说什么。
    fn hint(&self) -> String {
        match self.effective() {
            AiMode::Local if self.piper.is_none() => {
                if self.engine_issue.is_empty() {
                    "本地配音插件还没装：到「插件」页导入 Piper 插件包（zip，约 100MB），装好即可离线配音。"
                        .to_owned()
                } else {
                    format!("本地配音插件读不出来：{}", self.engine_issue)
                }
            }
            AiMode::Cloud if !self.signed_in => {
                "云端模式需要先登录星籁账号：回到首屏登录后再打开这个面板。".to_owned()
            }
            AiMode::Cloud => "云端模式：生成前会先让你确认上传素材与扣费。".to_owned(),
            AiMode::Local => "本地模式：文字与音频都不出本机，不需要登录。".to_owned(),
            AiMode::Smart => String::new(),
        }
    }

    /// 本地数字人的状态（一期只有入口）。
    fn digital_status(&self) -> String {
        let installed = self
            .plugins
            .iter()
            .any(|plugin| plugin.engine() == "wav2lip" && plugin.complete);
        if installed {
            "Wav2Lip 插件已安装。本地数字人（二期）将在 0.2.13 接入；这一版可以用云端生成。"
                .to_owned()
        } else {
            "本地数字人是二期插件（Wav2Lip ONNX，可选 DirectML）：到「插件」页导入插件包或从服务器下载。这一版可用云端生成数字人。"
                .to_owned()
        }
    }

    /// 数字人插件装没装。
    fn digital_ready(&self) -> bool {
        self.plugins
            .iter()
            .any(|plugin| plugin.engine() == "wav2lip" && plugin.complete)
    }

    /// 发布给 Slint 的数据。
    pub fn data(&self, studio: &Studio) -> AiData {
        let _ = studio;
        let hardware = self.hardware.as_ref();
        AiData {
            open: self.open,
            tab: self.tab,
            mode: self.mode.index(),
            effective: if self.effective() == AiMode::Cloud {
                1
            } else {
                0
            },
            effective_label: if self.effective() == AiMode::Cloud {
                "云端".into()
            } else {
                "本地".into()
            },
            reason: self.reason().into(),
            hardware: hardware
                .map(Hardware::summary)
                .unwrap_or_default()
                .into(),
            tier: hardware
                .map(|hardware| hardware.tier.label())
                .unwrap_or("尚未检测")
                .into(),
            estimate: hardware
                .map(|hardware| {
                    format!("{}；{}", hardware.tts_estimate(), hardware.avatar_estimate())
                })
                .unwrap_or_default()
                .into(),
            local_ready: self.piper.is_some(),
            local_path: self
                .piper
                .as_ref()
                .map(|engine| engine.root.display().to_string())
                .unwrap_or_default()
                .into(),
            signed_in: self.signed_in,
            account: self.account.as_str().into(),
            cloud_cost: CLOUD_COST.into(),
            text: self.text.as_str().into(),
            voice: self.voice,
            rate: self.rate,
            busy: self.busy,
            progress: self.progress,
            stage: self.stage.as_str().into(),
            message: self.message.as_str().into(),
            last: self.last.as_str().into(),
            has_audio: self.last_audio.is_some(),
            audio_name: self.audio_name.as_str().into(),
            photo_name: self
                .photo
                .as_ref()
                .map(|photo| photo.name.clone())
                .unwrap_or_default()
                .into(),
            photo_size: self
                .photo
                .as_ref()
                .map(|photo| plugins::human_size(photo.size))
                .unwrap_or_default()
                .into(),
            has_photo: self.photo.is_some(),
            digital_ready: self.digital_ready(),
            digital_status: self.digital_status().into(),
            wizard_open: self.wizard_open,
            confirm_open: self.confirm_open,
            confirm_text: self.confirm_text.as_str().into(),
            mode_label: self.mode.label().into(),
        }
    }

    /// 音色下拉的行。
    pub fn voice_rows(&self) -> Vec<AiVoiceItem> {
        self.voices
            .iter()
            .map(|voice| AiVoiceItem {
                id: voice.id.as_str().into(),
                name: voice.name.as_str().into(),
                detail: voice.detail.as_str().into(),
                ready: voice.ready,
                cloned: voice.cloned,
            })
            .collect()
    }

    /// 插件页的行。
    pub fn plugin_rows(&self) -> Vec<AiPluginItem> {
        self.plugins
            .iter()
            .map(|plugin| {
                let kind = if plugin.engine() == "wav2lip" {
                    "数字人"
                } else {
                    "本地配音"
                };
                let voices = plugin.voices().len();
                let state = if plugin.complete {
                    if voices > 0 {
                        format!("{voices} 个音色 · 已就绪")
                    } else {
                        "已安装".to_owned()
                    }
                } else {
                    "缺少主程序".to_owned()
                };
                AiPluginItem {
                    id: plugin.manifest.id.as_str().into(),
                    name: plugin.manifest.name.as_str().into(),
                    kind: kind.into(),
                    version: plugin.manifest.version.as_str().into(),
                    size: plugin.size_label().into(),
                    state: state.into(),
                    path: plugin.folder.display().to_string().into(),
                    complete: plugin.complete,
                    voices: i32::try_from(voices).unwrap_or(0),
                }
            })
            .collect()
    }

    // ── 消息 ──

    /// 处理一条消息。`studio` 是窗口的其余部分。
    pub fn update(&mut self, msg: AiMsg, studio: &mut Studio) {
        match msg {
            AiMsg::OpenTab(tab) => self.open_tab(tab, studio),
            AiMsg::Close => {
                self.open = false;
                self.confirm_open = false;
                self.confirm = None;
            }
            AiMsg::Tab(tab) => self.tab = tab.clamp(0, 2),
            AiMsg::Text(text) => {
                self.text = text.chars().take(MAX_TEXT).collect();
            }
            AiMsg::Voice(index) => {
                self.voice = index;
                self.remember(studio);
            }
            AiMsg::Rate(rate) => {
                self.rate = if rate.is_finite() {
                    rate.clamp(0.8, 1.3)
                } else {
                    1.0
                };
                self.remember(studio);
            }
            AiMsg::Mode(index) => {
                self.set_mode(index);
                self.remember(studio);
                let dirs = studio.host.dirs.clone();
                self.refresh(&dirs);
            }
            AiMsg::Redetect => {
                self.hardware = Some(Hardware::detect());
                self.message = self.hint();
                studio.notify("已重新检测本机算力", false);
            }
            AiMsg::WizardLocal => {
                self.mode = AiMode::Local;
                self.wizard_open = false;
                studio.prefs.ai_wizard_seen = true;
                studio.prefs.ai_mode = Some(self.mode.key().to_owned());
                studio.prefs.save(&studio.host.dirs);
                self.open = true;
                self.tab = if self.piper.is_some() { 0 } else { 2 };
                let dirs = studio.host.dirs.clone();
                self.refresh(&dirs);
                self.message = self.hint();
            }
            AiMsg::WizardCloud => {
                self.mode = AiMode::Cloud;
                self.wizard_open = false;
                studio.prefs.ai_wizard_seen = true;
                studio.prefs.ai_mode = Some(self.mode.key().to_owned());
                studio.prefs.save(&studio.host.dirs);
                self.open = true;
                let dirs = studio.host.dirs.clone();
                self.refresh(&dirs);
                self.message = if self.signed_in {
                    "已选择云端：生成前会确认上传与扣费。".to_owned()
                } else {
                    "已选择云端：请先在首屏登录星籁账号，再回来生成。".to_owned()
                };
            }
            AiMsg::WizardClose => {
                self.wizard_open = false;
                studio.prefs.ai_wizard_seen = true;
                studio.prefs.save(&studio.host.dirs);
            }
            AiMsg::Generate => {
                if self.tab == 1 {
                    self.digital(studio);
                } else {
                    self.generate(studio);
                }
            }
            AiMsg::Preview => {
                if let Some(path) = self.last_audio.clone() {
                    let name = if self.audio_name.is_empty() {
                        "AI 配音".to_owned()
                    } else {
                        self.audio_name.clone()
                    };
                    studio.preview_file("ai-voice", &name, &path);
                    self.message = format!("试听：{name}");
                } else {
                    self.message = "还没有可试听的音频：先生成一段配音".to_owned();
                }
            }
            AiMsg::Cancel => {
                if let Some(cancel) = self.cancel.take() {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.stage = "已取消".to_owned();
                self.message = "已取消".to_owned();
                self.confirm_open = false;
                self.confirm = None;
            }
            AiMsg::CloudConfirm => {
                self.confirm_open = false;
                let Some(action) = self.confirm.take() else {
                    return;
                };
                self.run_cloud(studio, action);
            }
            AiMsg::CloudCancel => {
                self.confirm_open = false;
                self.confirm = None;
                self.message = "已取消：没有上传任何素材".to_owned();
            }
            AiMsg::Progress { fraction, stage } => {
                self.progress = fraction.clamp(0.0, 1.0);
                self.stage = stage;
            }
            AiMsg::Finished(result) => self.finished(studio, *result),
            AiMsg::CloudVoices(result) => match *result {
                Ok(cloud) => {
                    self.cloud_voices = Some(cloud);
                    self.rebuild_voices();
                    self.message = self.hint();
                }
                Err(error) => {
                    cloud_ai::forget_if_expired(&error);
                    self.message = format!("云端音色取不到：{error}");
                    self.recheck_account();
                }
            },
            AiMsg::PhotoPick => self.pick_photo(studio),
            AiMsg::PhotoClear => {
                self.photo = None;
                self.message = "照片已清掉".to_owned();
            }
            AiMsg::PluginRefresh => {
                let dirs = studio.host.dirs.clone();
                self.refresh(&dirs);
                self.message = format!("本机插件：{} 个", self.plugins.len());
            }
            AiMsg::PluginImport => self.import_plugin(studio),
            AiMsg::PluginImported(result) => match *result {
                Ok(name) => {
                    self.busy = false;
                    self.stage.clear();
                    let dirs = studio.host.dirs.clone();
                    self.refresh(&dirs);
                    self.message = format!("插件已安装：{name}；现在可以用本地配音了");
                    studio.notify("本地 AI 插件已安装", false);
                }
                Err(error) => {
                    self.busy = false;
                    self.stage.clear();
                    self.message = format!("插件导入失败：{error}");
                    studio.notify(&error, true);
                }
            },
            AiMsg::PluginRemove(id) => match plugins::remove(&studio.host.dirs, &id) {
                Ok(()) => {
                    let dirs = studio.host.dirs.clone();
                    self.refresh(&dirs);
                    self.message = format!("已删除插件「{id}」");
                    studio.notify(&format!("已删除插件「{id}」"), false);
                }
                Err(error) => {
                    self.message = error.clone();
                    studio.notify(&error, true);
                }
            },
            AiMsg::PluginOpen(id) => {
                if let Some(plugin) = self.plugins.iter().find(|plugin| plugin.manifest.id == id) {
                    let folder = plugin.folder.clone();
                    if opener::open(&folder).is_err() {
                        self.message = format!("打不开文件夹：{}", folder.display());
                    }
                }
            }
            AiMsg::PluginDownload(id) => self.download_plugin(studio, &id),
        }
    }

    /// 开面板并落到某一页。
    fn open_tab(&mut self, tab: i32, studio: &mut Studio) {
        self.open = true;
        self.tab = tab.clamp(0, 2);
        let dirs = studio.host.dirs.clone();
        self.refresh(&dirs);
        if !studio.prefs.ai_wizard_seen {
            self.wizard_open = true;
        }
        if !self.busy {
            self.message = self.hint();
        }
    }

    /// 记住这次的选择。
    fn remember(&mut self, studio: &mut Studio) {
        studio.prefs.ai_mode = Some(self.mode.key().to_owned());
        studio.prefs.ai_rate = Some(self.rate);
        studio.prefs.ai_voice = self
            .voices
            .get(self.voice.max(0) as usize)
            .map(|v| v.id.clone());
        studio.prefs.save(&studio.host.dirs);
    }

    /// 重新读登录状态；返回是否登录着。
    fn recheck_account(&mut self) -> bool {
        match concat_host::account::load() {
            Some(account) if !account.token.trim().is_empty() => {
                self.signed_in = true;
                self.account = account.display_name();
                true
            }
            _ => {
                if self.effective() == AiMode::Cloud {
                    self.signed_in = false;
                }
                self.account.clear();
                false
            }
        }
    }

    // ── 配音 ──

    /// 「生成配音」：按当前运行方式走本地或云端。
    fn generate(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        if self.text.trim().is_empty() {
            self.message = "请先输入要配音的文字".to_owned();
            return;
        }
        let Some(project) = project_dir(studio) else {
            self.message = "请先打开一个工程：生成的音频要放进工程目录".to_owned();
            return;
        };
        match self.effective() {
            AiMode::Local => self.generate_local(&project),
            _ => self.ask_cloud(studio, Confirm::Tts),
        }
    }

    /// 本地 Piper 生成。
    fn generate_local(&mut self, project: &Path) {
        let Some(engine) = self.piper.clone() else {
            self.message = self.hint();
            self.tab = 2;
            return;
        };
        let Some(voice) = self.voices.get(self.voice.max(0) as usize).cloned() else {
            self.message = "插件里没有可用音色".to_owned();
            return;
        };
        let index = engine
            .voices
            .iter()
            .position(|candidate| candidate.id == voice.id)
            .unwrap_or(0);
        let text = self.text.trim().to_owned();
        let rate = self.rate;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        let dir = project.join("ai_voice");
        let out = dir.join(format!("ai-voice-{}.wav", stamp()));
        self.begin("本地配音生成中…");
        let voice_name = engine
            .voices
            .get(index)
            .map(|voice| voice.name.clone())
            .unwrap_or_default();
        spawn_in_project(
            move || -> Result<Done, String> {
                let spoken = engine.speak(index, &text, rate, &out, &cancel)?;
                let summary = media::probe(&spoken.path.to_string_lossy())?;
                Ok(Done {
                    path: spoken.path.clone(),
                    summary,
                    name: format!("AI配音 · {voice_name}"),
                    detail: spoken.summary(),
                    cloud_audio_url: None,
                })
            },
            |studio, _, _, result| {
                studio.handle(Msg::Ai(AiMsg::Finished(Box::new(result))));
            },
        );
    }

    /// 云端要花额度、要上传：先摆确认条。
    fn ask_cloud(&mut self, studio: &mut Studio, action: Confirm) {
        self.recheck_account();
        if !self.signed_in {
            self.message = "请先登录星籁账号，再使用云端 AI（首屏登录后回来）".to_owned();
            return;
        }
        if action == Confirm::Avatar && self.cloud_audio_url.is_none() {
            self.message = "请先在「配音」页用云端生成一段配音，数字人会用它当口播".to_owned();
            self.tab = 0;
            return;
        }
        if self.text.trim().is_empty() && action == Confirm::Tts {
            self.message = "请先输入要配音的文字".to_owned();
            return;
        }
        if self.effective() == AiMode::Local {
            self.message = "当前运行方式是本地：不会上传素材；要用云端请在顶部切换".to_owned();
            return;
        }
        self.confirm = Some(action);
        self.confirm_open = true;
        let _ = studio;
        if action == Confirm::Avatar {
            self.confirm_text =
                "云端数字人会把你选的照片和口播音频上传到星籁平台（约 1–2 分钟生成），并按套餐扣费。确认继续？"
                    .to_owned();
        } else {
            let chars = self.text.trim().chars().count();
            let cost = chars as f64 * 0.8 / 10_000.0;
            self.confirm_text = format!(
                "云端配音会把这段文字（{chars} 字）上传到星籁平台，按套餐额度扣费（约 ¥{cost:.3}），并在云端合成。确认继续？"
            );
        }
    }

    /// 用户确认之后，真正开始云端生成。
    fn run_cloud(&mut self, studio: &mut Studio, action: Confirm) {
        if self.busy {
            return;
        }
        let Some(project) = project_dir(studio) else {
            self.message = "请先打开一个工程".to_owned();
            return;
        };
        let Ok(token) = cloud_ai::token() else {
            self.message = "请先登录星籁账号".to_owned();
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        match action {
            Confirm::Tts => {
                let Some(voice) = self.voices.get(self.voice.max(0) as usize).cloned() else {
                    self.message = "云端音色列表还没取到，请稍后再试".to_owned();
                    return;
                };
                if !voice.ready {
                    self.message = "这个音色还在部署中，换一个已就绪的音色".to_owned();
                    return;
                }
                let text = self.text.trim().to_owned();
                let rate = self.rate;
                let dir = project.join("ai_voice");
                let out = dir.join(format!("cloud-voice-{}.mp3", stamp()));
                self.begin("云端配音生成中（通常几秒）…");
                spawn_in_project(
                    move || -> Result<Done, String> {
                        let tts = cloud_ai::tts(&token, &text, &voice.id, rate)?;
                        let no_cancel = AtomicBool::new(false);
                        concat_host::direct::download(&tts.url, &out, &mut |_, _| {}, &no_cancel)?;
                        let summary = media::probe(&out.to_string_lossy())?;
                        let label = if tts.label.trim().is_empty() {
                            voice.name.clone()
                        } else {
                            tts.label.clone()
                        };
                        let mut detail = format!(
                            "{:.1} 秒 · {}",
                            if tts.duration > 0.0 {
                                tts.duration
                            } else {
                                summary.duration.unwrap_or(0.0)
                            },
                            tts.engine
                        );
                        if !tts.cost_hint.trim().is_empty() {
                            detail.push_str(" · ");
                            detail.push_str(tts.cost_hint.trim());
                        }
                        if tts.fallback {
                            detail.push_str(" · （已降级到备用引擎）");
                        }
                        Ok(Done {
                            path: out.clone(),
                            summary,
                            name: format!("AI配音 · {label}"),
                            detail,
                            cloud_audio_url: Some(tts.url),
                        })
                    },
                    |studio, _, _, result| {
                        studio.handle(Msg::Ai(AiMsg::Finished(Box::new(result))));
                    },
                );
            }
            Confirm::Avatar => {
                let Some(photo) = self.photo.as_ref() else {
                    self.message = "请先选择一张照片".to_owned();
                    return;
                };
                let Some(audio_url) = self.cloud_audio_url.clone() else {
                    self.message = "请先在「配音」页用云端生成一段配音".to_owned();
                    self.tab = 0;
                    return;
                };
                let bytes = photo.bytes.clone();
                let mime = photo.mime.clone();
                let dir = project.join("ai_avatar");
                let out = dir.join(format!("cloud-avatar-{}.mp4", stamp()));
                self.begin("云端数字人生成中（约 1–2 分钟）…");
                spawn_in_project(
                    move || -> Result<Done, String> {
                        let avatar = cloud_ai::avatar(&token, &bytes, &mime, &audio_url)?;
                        let no_cancel = AtomicBool::new(false);
                        concat_host::direct::download(
                            &avatar.url,
                            &out,
                            &mut |_, _| {},
                            &no_cancel,
                        )?;
                        let summary = media::probe(&out.to_string_lossy())?;
                        let mut detail = format!(
                            "{:.1} 秒 · {}×{}",
                            avatar.duration, avatar.width, avatar.height
                        );
                        if !avatar.note.trim().is_empty() {
                            detail.push_str(" · ");
                            detail.push_str(avatar.note.trim());
                        }
                        Ok(Done {
                            path: out.clone(),
                            summary,
                            name: "照片数字人 · 云端".to_owned(),
                            detail,
                            cloud_audio_url: None,
                        })
                    },
                    |studio, _, _, result| {
                        studio.handle(Msg::Ai(AiMsg::Finished(Box::new(result))));
                    },
                );
            }
        }
    }

    /// 生成开始：忙起来，清掉上次的状态。
    fn begin(&mut self, stage: &str) {
        self.busy = true;
        self.progress = 0.0;
        self.stage = stage.to_owned();
        self.message = stage.to_owned();
        self.confirm_open = false;
    }

    /// 生成结束：落盘、进素材库、上时间轴。
    fn finished(&mut self, studio: &mut Studio, result: Result<Done, String>) {
        self.busy = false;
        self.cancel = None;
        self.progress = 0.0;
        self.stage.clear();
        match result {
            Ok(done) => {
                if let Some(url) = done.cloud_audio_url.clone() {
                    self.cloud_audio_url = Some(url);
                }
                let inserted = insert(studio, &done);
                self.audio_name = done.name.clone();
                if done.summary.kind == MediaKind::Audio {
                    self.last_audio = Some(done.path.clone());
                }
                self.last = match inserted {
                    Some(start) => format!(
                        "{} · 已插入时间轴 {:.1}s · {}",
                        done.detail,
                        start,
                        done.path.display()
                    ),
                    None => format!("{} · 文件：{}", done.detail, done.path.display()),
                };
                self.message = self.last.clone();
                studio.notify(&format!("{} 已插入时间轴", done.name), false);
            }
            Err(error) => {
                if error.contains("已取消") || error.contains("取消") {
                    self.message = "已取消".to_owned();
                    return;
                }
                cloud_ai::forget_if_expired(&error);
                self.recheck_account();
                self.message = format!("生成失败：{error}");
                self.last = self.message.clone();
                studio.notify(&self.message.clone(), true);
            }
        }
    }

    // ── 数字人 ──

    /// 「生成数字人片段」：本地是二期插件（给说明与入口），云端走 /autoedit/avatar。
    fn digital(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        if matches!(self.effective(), AiMode::Local) {
            self.message = if self.digital_ready() {
                "Wav2Lip 插件已装好，但本地数字人推理在 0.2.13 接入；这一版请切到「仅云端」生成。"
                    .to_owned()
            } else {
                "本地数字人是二期插件：先到「插件」页导入 Wav2Lip 插件包；这一版可以用云端生成。"
                    .to_owned()
            };
            self.tab = 2;
            return;
        }
        if self.photo.is_none() {
            self.message = "请先选择一张照片（JPG/PNG，≤10MB）".to_owned();
            return;
        }
        if self.cloud_audio_url.is_none() {
            self.message = "请先在「配音」页用云端生成一段配音，数字人用它当口播".to_owned();
            self.tab = 0;
            return;
        }
        self.ask_cloud(studio, Confirm::Avatar);
    }

    /// 挑照片：读进内存（≤10MB），并做成预览图。
    fn pick_photo(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("选择照片（JPG/PNG，≤10MB）")
            .add_filter("照片", &["jpg", "jpeg", "png"])
            .pick_file()
        else {
            return;
        };
        let size = std::fs::metadata(&path).map_or(0, |meta| meta.len());
        if size > 10 * 1024 * 1024 {
            self.message = "照片超过 10MB，请压缩后再选".to_owned();
            studio.notify("照片超过 10MB，请压缩后再选", true);
            return;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.message = format!("照片读不出来：{error}");
                return;
            }
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "照片".to_owned());
        let extension = path
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let mime = if extension == "png" {
            "image/png"
        } else {
            "image/jpeg"
        };
        let image = slint::Image::load_from_path(&path).unwrap_or_default();
        self.photo = Some(Photo {
            bytes,
            mime: mime.to_owned(),
            name: name.clone(),
            size,
            image,
        });
        self.message = format!("已选照片：{name}（{}）", plugins::human_size(size));
        studio.notify("照片已选好", false);
    }

    /// 当前的照片预览图，发布时写到窗口的 ai-photo 属性。
    pub fn photo_image(&self) -> slint::Image {
        self.photo
            .as_ref()
            .map(|photo| photo.image.clone())
            .unwrap_or_default()
    }

    // ── 插件 ──

    /// 找一个插件包 zip 导入。
    fn import_plugin(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("选择本地 AI 插件包（zip）")
            .add_filter("插件包", &["zip"])
            .pick_file()
        else {
            return;
        };
        self.busy = true;
        self.progress = 0.0;
        self.stage = "正在解压插件包…".to_owned();
        self.message = self.stage.clone();
        let dirs = studio.host.dirs.clone();
        spawn(
            move || {
                let mut report = |fraction: f32, stage: &str| {
                    let stage = stage.to_owned();
                    host::on_ui(move |studio, _, _| {
                        studio.handle(Msg::Ai(AiMsg::Progress { fraction, stage }));
                    });
                };
                plugins::import_zip(&dirs, &path, &mut report).map(|plugin| plugin.manifest.name)
            },
            |studio, _, _, result| {
                studio.handle(Msg::Ai(AiMsg::PluginImported(Box::new(result))));
            },
        );
    }

    /// 从服务器下载一个插件包再装上。
    fn download_plugin(&mut self, studio: &mut Studio, id: &str) {
        if self.busy {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.busy = true;
        self.progress = 0.0;
        self.stage = format!(
            "正在获取插件下载信息（约 {}）…",
            plugins::human_size(plugins::download_size_hint(id))
        );
        self.message = self.stage.clone();
        let dirs = studio.host.dirs.clone();
        let id = id.to_owned();
        spawn(
            move || -> Result<String, String> {
                let mut last = -1.0f32;
                let mut report = |fraction: f32, stage: &str| {
                    if (fraction - last).abs() < 0.01 && fraction < 1.0 {
                        return;
                    }
                    last = fraction;
                    let stage = stage.to_owned();
                    host::on_ui(move |studio, _, _| {
                        studio.handle(Msg::Ai(AiMsg::Progress { fraction, stage }));
                    });
                };
                // 远端清单优先：运营换包/换地址不用改客户端；取不到就固定地址。
                let package = plugins::remote_manifest(&dirs)
                    .remove(&id)
                    .unwrap_or_default();
                let url = if package.url.trim().is_empty() {
                    plugins::download_url(&id)
                } else {
                    package.url.clone()
                };
                let zip = plugins::download(&dirs, &id, &url, &package.sha256, &mut report, &cancel)?;
                let mut unpack = |fraction: f32, stage: &str| {
                    let stage = stage.to_owned();
                    host::on_ui(move |studio, _, _| {
                        studio.handle(Msg::Ai(AiMsg::Progress { fraction, stage }));
                    });
                };
                let plugin = plugins::import_zip(&dirs, &zip, &mut unpack)?;
                Ok(plugin.manifest.name)
            },
            |studio, _, _, result| {
                studio.handle(Msg::Ai(AiMsg::PluginImported(Box::new(result))));
            },
        );
    }
}

impl Studio {
    /// 记住 AI 运行方式（设置页改的）。
    pub fn remember_ai_mode(&mut self) {
        self.prefs.ai_mode = Some(self.ai.mode.key().to_owned());
        self.prefs.save(&self.host.dirs);
    }

    /// 设置页要发布的 AI 数据：运行方式、硬件一句话、推荐理由。
    pub fn ai_settings_rows(&self) -> (i32, String, String) {
        let hardware = self
            .ai
            .hardware
            .as_ref()
            .map(Hardware::summary)
            .unwrap_or_else(|| "尚未检测".to_owned());
        let reason = self
            .ai
            .hardware
            .as_ref()
            .map(Hardware::reason)
            .unwrap_or_default();
        (self.ai.mode_index(), hardware, reason)
    }
}

/// 当前工程目录。
fn project_dir(studio: &Studio) -> Option<PathBuf> {
    studio
        .session
        .as_ref()
        .map(|session| PathBuf::from(session.path()))
}

/// 把生成的文件放进素材库并放到时间轴上（当前播放头）。
/// 返回落点秒数；没插上（没有工程、媒体加不进去）返回 None。
fn insert(studio: &mut Studio, done: &Done) -> Option<f64> {
    let mut item = done.summary.to_new_media();
    item.name = done.name.clone();
    if done.summary.kind == MediaKind::Audio {
        // 归到「生成 › 语音」，和朗读生成的声音放在一起。
        item.origin = Some(MediaOrigin::Speech);
    }
    let created = studio.apply(Command::AddMedia { item });
    let media_id = created.or_else(|| {
        studio
            .project()
            .media
            .iter()
            .find(|item| item.path == done.summary.path)
            .map(|item| item.id.clone())
    })?;
    let start = f64::from(studio.playhead.max(0.0));
    studio
        .apply(Command::AddClipAtFirstFree {
            media_id,
            start,
        })
        .map(|_| start)
}

/// 一秒级的时间戳，给生成的文件起不重名的名字。
fn stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}
