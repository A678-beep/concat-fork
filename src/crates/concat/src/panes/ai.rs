// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! AI 面板：智能算力选择、本地配音（Piper）、照片数字人（Wav2Lip 插件）、
//! 本地插件管理，以及默认关闭的云端兜底。
//!
//! 「智能」是默认：本机检测出独显与内存够用就走本地（素材不出本机），
//! 核显/小显存/小内存则默认推荐云端。运行方式可以随时在面板顶部或设置里
//! 改成「仅本地 / 仅云端」。云端需要登录、消耗套餐额度，而且每次生成前都
//! 要用户点一下确认——**默认不上传任何素材**。
//!
//! 生成出来的文件一律落在工程目录：本地配音 `ai_voice/*.wav`，云端配音
//! `ai_voice/*.mp3`，数字人 `ai_avatar/*.mp4`，然后进素材库、上时间轴（当前
//! 播放头位置）。本地数字人走 Wav2Lip 插件：照片 + 文案（先由本地音色配音）
//! → 对口型视频，全程离线；云端数字人是兜底。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_host::hardware::{Hardware, Tier};
use concat_host::media::{self, MediaSummary};
use concat_host::{ai_models, cloud_ai, piper, plugins, sadtalker, wav2lip, zipvoice};
use concat_project::Command;
use concat_project::model::{MediaKind, MediaOrigin};

use crate::host::{self, spawn, spawn_in_project};
use crate::panes::Msg;
use crate::panes::ai_video::{AiVideoPane, VideoMsg};
use crate::prefs::Preferences;
use crate::studio::Studio;
use crate::ui::{AiData, AiPluginItem, AiVideoData, AiVoiceItem};

/// 云端每次生成的提醒，界面上常驻。
const CLOUD_COST: &str = "云端：需登录星籁账号，按套餐额度扣费（配音约 ¥0.004/51 字，数字人按次扣）；素材会上传到云端，每次生成前都会确认。";

/// 本地数字人的效果预期，界面上常驻。
const AVATAR_LIMIT: &str = "Wav2Lip 只动嘴，SadTalker 会点头/眨眼但仍不是真人；清晰度都取决于照片，效果不如云端数字人（Hedra 级），但完全离线、不花钱、素材不出本机。";

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
    /// 下载+安装的结果（带 id，失败时好把那一行标成未上线）。
    PluginDownloaded {
        /// 哪个插件。
        id: String,
        /// 成功是插件名，失败是一句中文。
        result: Box<Result<String, String>>,
    },
    /// 远端插件清单回来了（模型中心据此决定哪一项能下载）。
    Packages(Box<Result<std::collections::HashMap<String, plugins::RemotePackage>, String>>),
    /// 刷新插件列表。
    PluginRefresh,
    /// 打开账号登录。
    Login,
    /// 挑照片。
    PhotoPick,
    /// 清掉照片。
    PhotoClear,
    /// 换本地数字人引擎：0 Wav2Lip、1 SadTalker。
    Engine(i32),
    /// 云端素材确认：继续。
    CloudConfirm,
    /// 云端素材确认：取消。
    CloudCancel,
    /// 后台进度：0..1 和一句话。
    Progress { fraction: f32, stage: String },
    /// 生成完成（成功是文件与插入结果，失败是一句中文）。
    Finished(Box<Result<Done, String>>),
    /// 生视频页（AI 面板第 4 页）的消息。
    Video(VideoMsg),
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
    /// 装好的本地语音克隆引擎（ZipVoice）。
    pub clone: Option<zipvoice::Engine>,
    /// 装好的本地数字人引擎（Wav2Lip）。
    pub digital: Option<wav2lip::Engine>,
    /// 装好的本地数字人引擎（SadTalker，效果好但只在 CPU 上跑，很慢）。
    pub sadtalker: Option<sadtalker::Engine>,
    /// 数字人插件读不出来时的原因（装上了但缺文件/缺 ffmpeg）。
    digital_issue: String,
    /// SadTalker 插件读不出来的原因。
    sadtalker_issue: String,
    /// 数字人页选中的本地引擎：0 Wav2Lip、1 SadTalker。
    pub digital_engine: i32,
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
    /// 套餐名，给面板显示。
    pub account_plan: String,
    /// 第 4 页：AI 生视频。
    pub video: AiVideoPane,
    /// 远端插件清单；取到前是 None（模型中心先把「即将上线」写灰）。
    packages: Option<std::collections::HashMap<String, plugins::RemotePackage>>,
    /// 清单正在后台取。
    packages_loading: bool,
    /// 最近一次弹过的错误句：同一句不重复弹。
    last_alert: String,
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
            clone: None,
            digital: None,
            sadtalker: None,
            digital_issue: String::new(),
            sadtalker_issue: String::new(),
            digital_engine: 0,
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
            account_plan: String::new(),
            video: AiVideoPane::default(),
            packages: None,
            packages_loading: false,
            last_alert: String::new(),
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
            digital_engine: prefs.ai_avatar_engine.unwrap_or(0).clamp(0, 1),
            wizard_open: !prefs.ai_wizard_seen,
            ..AiPane::default()
        };
        pane.hardware = Some(Hardware::detect());
        pane.video.hardware = pane.hardware.clone();
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
        // A fresh read means an import, a removal or the panel opening: the
        // cached "is Piper installed" answer the speech pane keeps must go.
        plugins::forget_installed();
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
        self.clone = self
            .plugins
            .iter()
            .find(|plugin| plugin.engine() == "zipvoice")
            .and_then(|plugin| match zipvoice::Engine::load(plugin) {
                Ok(engine) => Some(engine),
                Err(error) => {
                    log::warn!("ai-plugins: 语音克隆引擎读不出来：{error}");
                    None
                }
            });
        self.digital = None;
        self.digital_issue.clear();
        self.sadtalker = None;
        self.sadtalker_issue.clear();
        if let Some(plugin) = self
            .plugins
            .iter()
            .find(|plugin| plugin.engine() == "wav2lip")
        {
            match wav2lip::Engine::load(plugin) {
                Ok(engine) => self.digital = Some(engine),
                Err(error) => {
                    log::warn!("ai-plugins: 数字人引擎读不出来：{error}");
                    self.digital_issue = error;
                }
            }
        }
        if let Some(plugin) = self
            .plugins
            .iter()
            .find(|plugin| plugin.engine() == "sadtalker")
        {
            match sadtalker::Engine::load(plugin) {
                Ok(engine) => self.sadtalker = Some(engine),
                Err(error) => {
                    log::warn!("ai-plugins: SadTalker 引擎读不出来：{error}");
                    self.sadtalker_issue = error;
                }
            }
        }
        if let Some(account) = concat_host::account::load() {
            self.signed_in = !account.token.trim().is_empty();
            self.account = account.display_name();
            self.account_plan = account.plan_label();
        } else {
            self.signed_in = false;
            self.account.clear();
            self.account_plan.clear();
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

    /// 后台取一次远端插件清单，模型中心据此把能下载的点亮。
    ///
    /// 只取一次：清单不常变，用户在插件页点「刷新」会清掉重取。
    /// The sizes `plugins::sizes` walked, onto the rows: the panel shows
    /// them in place of the dashes `scan` leaves when it has no cache yet.
    pub fn apply_sizes(&mut self, sizes: std::collections::HashMap<String, u64>) {
        if sizes.is_empty() {
            return;
        }
        for plugin in &mut self.plugins {
            if let Some(bytes) = sizes.get(&plugin.manifest.id) {
                plugin.bytes = *bytes;
            }
        }
    }

    fn load_packages(&mut self, studio: &mut Studio) {
        if self.packages_loading {
            return;
        }
        self.packages_loading = true;
        let dirs = studio.host.dirs.clone();
        spawn(
            move || plugins::remote_manifest(&dirs),
            |studio, _, _, packages| {
                studio.handle(Msg::Ai(AiMsg::Packages(Box::new(Ok(packages)))));
            },
        );
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
        } else {
            // 本地：Piper 的固定音色 + 语音克隆插件的参考音色，都在一张表里。
            if let Some(engine) = self.piper.as_ref() {
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
            if let Some(engine) = self.clone.as_ref() {
                for voice in &engine.voices {
                    rows.push(VoiceRow {
                        id: voice.id.clone(),
                        name: format!("{} · 本地克隆", voice.name),
                        detail: if voice.tag.trim().is_empty() {
                            "参考录音音色 · 完全离线".to_owned()
                        } else {
                            format!("{} · 完全离线", voice.tag)
                        },
                        ready: true,
                        cloned: true,
                    });
                }
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
            AiMode::Local if self.piper.is_none() && self.clone.is_none() => {
                if self.engine_issue.is_empty() {
                    "本地还没有可用的语音引擎：到「插件」页的模型中心下载「ZipVoice 零样本克隆」（约 160MB，支持克隆音色）或「Piper 本地配音」（约 100MB）。"
                        .to_owned()
                } else {
                    format!("本地配音插件读不出来：{}", self.engine_issue)
                }
            }
            AiMode::Cloud if !self.signed_in => {
                "云端模式需要先登录星籁账号：点下面的「去登录」，或回到首屏点右上角「登录 / 注册」。"
                    .to_owned()
            }
            AiMode::Cloud => "云端模式：生成前会先让你确认上传素材与扣费。".to_owned(),
            AiMode::Local if self.piper.is_some() && self.clone.is_some() => {
                "本地模式：Piper 与语音克隆都可用，文字与音频都不出本机，不需要登录。".to_owned()
            }
            AiMode::Local if self.clone.is_some() => {
                "本地模式：语音克隆引擎可用（参考录音克隆音色），文字与音频不出本机，不需要登录。"
                    .to_owned()
            }
            AiMode::Local => "本地模式：文字与音频都不出本机，不需要登录。".to_owned(),
            AiMode::Smart => String::new(),
        }
    }

    /// 本地数字人的状态：选中的引擎装没装、能不能跑、下一步该做什么。
    fn digital_status(&self) -> String {
        if self.digital_engine == 1 {
            if let Some(engine) = self.sadtalker.as_ref() {
                match engine.ready() {
                    Ok(()) => "SadTalker 已就绪：照片 + 文案 → 会点头、眨眼的说话视频；只在 CPU 上跑，比较慢（本机 5 秒视频约二十多分钟），生成后自动插入时间轴。急用建议切 Wav2Lip（快）。".to_owned(),
                    Err(error) => format!("SadTalker 插件装上了，但这条链路还缺东西：{error}"),
                }
            } else if !self.sadtalker_issue.trim().is_empty() {
                format!(
                    "SadTalker 插件已安装但读不出来：{}；请在「模型」页删掉再重下一次",
                    self.sadtalker_issue.trim()
                )
            } else {
                "还没有装 SadTalker 插件：到「模型」页下载「SadTalker 数字人」（约 2.3GB，纯 CPU、很慢）。也可以把上面的引擎切到 Wav2Lip（快）先用。".to_owned()
            }
        } else if let Some(engine) = self.digital.as_ref() {
            match engine.ready() {
                Ok(()) => "Wav2Lip 已就绪：照片 + 文案（用本地音色先配音）→ 对口型视频，全程离线、不上传。生成后自动插入时间轴。".to_owned(),
                Err(error) => format!("数字人插件装上了，但这条链路还缺东西：{error}"),
            }
        } else if !self.digital_issue.trim().is_empty() {
            format!(
                "数字人插件已安装但读不出来：{}；请在「模型」页删掉再重下一次",
                self.digital_issue.trim()
            )
        } else {
            "还没有装本地数字人插件：到「模型」页下载「Wav2Lip 口型同步」（约 247MB，一次装好、之后完全离线、不花钱）。".to_owned()
        }
    }

    /// Wav2Lip 装好且能跑。
    fn wav2lip_ready(&self) -> bool {
        self.digital
            .as_ref()
            .is_some_and(|engine| engine.ready().is_ok())
    }

    /// SadTalker 装好且能跑。
    fn sadtalker_ready(&self) -> bool {
        self.sadtalker
            .as_ref()
            .is_some_and(|engine| engine.ready().is_ok())
    }

    /// 选中的引擎装好且能跑。
    fn digital_ready(&self) -> bool {
        if self.digital_engine == 1 {
            self.sadtalker_ready()
        } else {
            self.wav2lip_ready()
        }
    }

    /// 本机数字人预计耗时，一句话（按选中的引擎说）。
    fn avatar_estimate(&self) -> String {
        let Some(hardware) = self.hardware.as_ref() else {
            return "本机硬件还没有检测".to_owned();
        };
        if self.digital_engine == 1 {
            hardware.sadtalker_estimate()
        } else {
            hardware.avatar_estimate()
        }
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
            local_ready: self.piper.is_some() || self.clone.is_some(),
            local_path: self
                .piper
                .as_ref()
                .map(|engine| engine.root.display().to_string())
                .or_else(|| {
                    self.clone
                        .as_ref()
                        .map(|engine| engine.root.display().to_string())
                })
                .unwrap_or_default()
                .into(),
            signed_in: self.signed_in,
            account: self.account.as_str().into(),
            account_plan: self.account_plan.as_str().into(),
            account_line: if self.signed_in {
                let name = if self.account.trim().is_empty() {
                    "星籁用户".to_owned()
                } else {
                    self.account.clone()
                };
                let plan = if self.account_plan.trim().is_empty() {
                    "未注明套餐".to_owned()
                } else {
                    self.account_plan.clone()
                };
                format!("{name} · {plan}（云端可用）")
            } else {
                "未登录（云端需登录）".to_owned()
            }
            .into(),
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
            wav2lip_ready: self.wav2lip_ready(),
            sadtalker_ready: self.sadtalker_ready(),
            digital_engine: self.digital_engine,
            digital_status: self.digital_status().into(),
            avatar_estimate: self.avatar_estimate().into(),
            avatar_limit: AVATAR_LIMIT.into(),
            wizard_open: self.wizard_open,
            confirm_open: self.confirm_open,
            confirm_text: self.confirm_text.as_str().into(),
            mode_label: self.mode.label().into(),
        }
    }

    /// 第 4 页（AI 生视频）的数据。
    pub fn video_data(&self) -> AiVideoData {
        let account_line = if self.signed_in {
            let name = if self.account.trim().is_empty() {
                "星籁用户".to_owned()
            } else {
                self.account.clone()
            };
            let plan = if self.account_plan.trim().is_empty() {
                "未注明套餐".to_owned()
            } else {
                self.account_plan.clone()
            };
            format!("{name} · {plan}（已登录，生成走平台额度）")
        } else {
            "未登录（生成需先登录）".to_owned()
        };
        self.video
            .data(self.signed_in, &account_line, self.hardware.as_ref())
    }

    /// 生视频页的模型下拉行。
    pub fn video_models(&self) -> Vec<slint::SharedString> {
        self.video.model_rows()
    }

    /// 生视频页的本地模型行（预留）。
    pub fn video_local_rows(&self) -> Vec<slint::SharedString> {
        self.video.local_rows()
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

    /// 模型中心的行：目录里的每个模型一行，外加用户自己导入的插件。
    ///
    /// 「能不能下载」由远端清单（[`Self::packages`]）与
    /// [`plugins::package_status`] 决定；清单没说、官方也没上线的，写「即将
    /// 上线」并把按钮灰掉——点了不会发请求。
    pub fn model_rows(&self) -> Vec<AiPluginItem> {
        let mut rows = Vec::new();
        for spec in &ai_models::MODELS {
            let installed = self
                .plugins
                .iter()
                .find(|plugin| plugin.manifest.id == spec.id);
            let package = plugins::package_status(self.packages.as_ref(), spec.id);
            let (fit_ok, fit_label) = match self.hardware.as_ref() {
                Some(hardware) => ai_models::fit_reason(spec, hardware),
                None => (true, "尚未检测".to_owned()),
            };
            let cloud = spec.id == "ai-background";
            let builtin = spec.id == "cutout";
            let voices = installed.map(|plugin| plugin.voices().len()).unwrap_or(0);
            let state = if cloud {
                if self.signed_in {
                    "云端 · 已登录可用".to_owned()
                } else {
                    "云端 · 需登录".to_owned()
                }
            } else if builtin {
                "已内置 · 首次使用自动下载".to_owned()
            } else if let Some(plugin) = installed {
                if plugin.complete {
                    if voices > 0 {
                        format!("已安装 · {voices} 个音色")
                    } else {
                        "已安装".to_owned()
                    }
                } else {
                    "已安装（缺主程序）".to_owned()
                }
            } else if package.online() {
                format!("可下载 · {}", plugins::human_size(package.size))
            } else {
                "即将上线".to_owned()
            };
            let downloadable = !cloud && !builtin && installed.is_none() && package.online() && fit_ok;
            rows.push(AiPluginItem {
                id: spec.id.into(),
                name: spec.name.into(),
                category: spec.kind.into(),
                summary: spec.summary.into(),
                effect: ai_models::stars(spec.effect).into(),
                size: if cloud {
                    "云端".into()
                } else if let Some(plugin) = installed {
                    plugin.size_label().into()
                } else {
                    plugins::human_size(spec.size_bytes).into()
                },
                requirement: requirement_label(spec).into(),
                speed: spec.speed.into(),
                fit: fit_label.into(),
                fit_ok,
                status: state.into(),
                note: spec.note.into(),
                path: installed
                    .map(|plugin| plugin.folder.display().to_string())
                    .unwrap_or_default()
                    .into(),
                installed: installed.is_some(),
                downloadable,
                complete: installed.is_some_and(|plugin| plugin.complete),
                voices: i32::try_from(voices).unwrap_or(0),
                version: installed
                    .map(|plugin| plugin.manifest.version.clone())
                    .unwrap_or_default()
                    .into(),
                cloud,
            });
        }
        // 用户自己导入、目录里没有的插件：也列出来，能打开目录、能删。
        for plugin in &self.plugins {
            if ai_models::spec(&plugin.manifest.id).is_some() {
                continue;
            }
            let voices = plugin.voices().len();
            rows.push(AiPluginItem {
                id: plugin.manifest.id.as_str().into(),
                name: plugin.manifest.name.as_str().into(),
                category: "本地插件".into(),
                summary: plugin.manifest.description.as_str().into(),
                effect: ai_models::stars(3).into(),
                size: plugin.size_label().into(),
                requirement: "—".into(),
                speed: "—".into(),
                fit: "已安装".into(),
                fit_ok: true,
                status: if plugin.complete {
                    format!("已安装 · {voices} 个音色")
                } else {
                    "已安装（缺主程序）".to_owned()
                }
                .into(),
                note: "导入的插件".into(),
                path: plugin.folder.display().to_string().into(),
                installed: true,
                downloadable: false,
                complete: plugin.complete,
                voices: i32::try_from(voices).unwrap_or(0),
                version: plugin.manifest.version.as_str().into(),
                cloud: false,
            });
        }
        rows
    }

    // ── 消息 ──

    /// 处理一条消息。`studio` 是窗口的其余部分。
    pub fn update(&mut self, msg: AiMsg, studio: &mut Studio) {
        match msg {
            AiMsg::Video(msg) => {
                if let VideoMsg::Action { action, .. } = &msg
                    && action.as_str() == "login"
                {
                    studio.request_login("星籁账号登录");
                }
                self.video.update(msg, studio);
            }
            AiMsg::OpenTab(tab) => self.open_tab(tab, studio),
            AiMsg::Close => {
                self.open = false;
                self.confirm_open = false;
                self.confirm = None;
            }
            AiMsg::Tab(tab) => {
                self.tab = tab.clamp(0, 3);
                if self.tab == 3 && !self.busy {
                    self.video.ensure_models(studio);
                }
                if self.tab == 2 && !self.busy {
                    self.load_packages(studio);
                }
            }
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
                self.tab = if self.piper.is_some() || self.clone.is_some() {
                    0
                } else {
                    2
                };
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
                    self.recheck_account(studio);
                    self.message = format!("云端音色取不到：{error}");
                }
            },
            AiMsg::PhotoPick => self.pick_photo(studio),
            AiMsg::PhotoClear => {
                self.photo = None;
                self.message = "照片已清掉".to_owned();
            }
            AiMsg::Engine(index) => {
                self.digital_engine = index.clamp(0, 1);
                studio.prefs.ai_avatar_engine = Some(self.digital_engine);
                studio.prefs.save(&studio.host.dirs);
                if !self.busy {
                    self.message = self.digital_status();
                }
            }
            AiMsg::PluginRefresh => {
                let dirs = studio.host.dirs.clone();
                self.refresh(&dirs);
                self.packages = None;
                self.load_packages(studio);
                self.message = format!("本机插件：{} 个；正在刷新模型中心…", self.plugins.len());
            }
            AiMsg::PluginImport => self.import_plugin(studio),
            AiMsg::PluginImported(result) => match *result {
                Ok(name) => {
                    self.busy = false;
                    self.stage.clear();
                    let dirs = studio.host.dirs.clone();
                    self.refresh(&dirs);
                    // 安装成功后把上一次的失败提示收掉：0.2.12 里失败的红条
                    // 会在成功之后还挡着，因为「完成」提示不覆盖失败提示。
                    studio.dismiss_notice();
                    self.last_alert.clear();
                    self.last.clear();
                    self.message = format!("插件已安装：{name}；现在可以用本地配音了");
                    studio.notify(&format!("插件已安装：{name}"), false);
                }
                Err(error) => {
                    self.busy = false;
                    self.stage.clear();
                    self.message = format!("插件导入失败：{error}");
                    self.alert(studio, &format!("插件导入失败：{error}"));
                }
            },
            AiMsg::PluginDownloaded { id, result } => match *result {
                Ok(name) => {
                    self.busy = false;
                    self.stage.clear();
                    self.progress = 0.0;
                    let dirs = studio.host.dirs.clone();
                    // 装好了，清单里那一行要显示「已安装」。
                    self.packages = None;
                    self.refresh(&dirs);
                    self.last_alert.clear();
                    self.last.clear();
                    self.message = format!("{name} 已下载并安装；回到「配音」页即可选用");
                    studio.dismiss_notice();
                    studio.notify(&format!("{name} 已安装"), false);
                }
                Err(error) => {
                    self.busy = false;
                    self.stage.clear();
                    self.progress = 0.0;
                    let message = plugins::readable_download_error(&id, &error);
                    // 404 / 未上线：这一行立刻灰成「即将上线」，不再给下载按钮。
                    if message.contains("暂未上线") {
                        self.mark_coming_soon(&id);
                    }
                    self.message = message.clone();
                    self.alert(studio, &message);
                }
            },
            AiMsg::Packages(result) => {
                self.packages_loading = false;
                match *result {
                    Ok(packages) => {
                        self.packages = Some(packages);
                    }
                    Err(error) => {
                        log::warn!("ai-plugins: 插件清单取不到：{error}");
                        // 清单取不到不是用户能修的事：不弹错误，按官方兜底表
                        // 显示（Piper 仍可下，没上线的仍灰着）。
                        self.packages = Some(std::collections::HashMap::new());
                    }
                }
            }
            AiMsg::Login => {
                studio.request_login("星籁账号登录");
            }
            AiMsg::PluginRemove(id) => match plugins::remove(&studio.host.dirs, &id) {
                Ok(()) => {
                    let dirs = studio.host.dirs.clone();
                    self.refresh(&dirs);
                    self.message = format!("已删除插件「{id}」");
                    studio.notify(&format!("已删除插件「{id}」"), false);
                }
                Err(error) => {
                    self.message = error.clone();
                    self.alert(studio, &error);
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
        self.tab = tab.clamp(0, 3);
        let dirs = studio.host.dirs.clone();
        self.refresh(&dirs);
        // The panel shows the sizes the last walk left on file; walk them
        // again on a worker so an install done outside the app is counted.
        studio.measure_plugin_sizes();
        if self.tab == 2 && !self.busy {
            self.load_packages(studio);
        }
        if self.tab == 3 {
            self.video.ensure_models(studio);
        }
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

    /// 重新读登录状态，并把全局的账号状态（首屏右上角、AI 面板底部）一起
    /// 刷新；返回是否登录着。token 过期时这里会把它清掉并提示重新登录。
    fn recheck_account(&mut self, studio: &mut Studio) -> bool {
        let was = self.signed_in;
        let account = concat_host::account::load();
        self.apply_account(account.as_ref());
        studio.account = account;
        if was && !self.signed_in {
            self.message = "星籁账号登录已失效：请重新登录后再用云端 AI".to_owned();
            self.alert(studio, "登录已失效：请重新登录星籁账号");
        }
        self.signed_in
    }

    /// 把一份账号落进面板状态。
    fn apply_account(&mut self, account: Option<&concat_host::account::Account>) {
        match account {
            Some(account) if !account.token.trim().is_empty() => {
                self.signed_in = true;
                self.account = account.display_name();
                self.account_plan = account.plan_label();
            }
            _ => {
                self.signed_in = false;
                self.account.clear();
                self.account_plan.clear();
            }
        }
    }

    /// 弹一条失败提示，但同一句不重复弹（同一个错误连点几次只响一次）。
    fn alert(&mut self, studio: &mut Studio, message: &str) {
        self.last_alert = message.to_owned();
        studio.notify(message, true);
    }

    /// 把一个包在本地标成「未上线」：清单没更新、但下载已经 404 了。
    fn mark_coming_soon(&mut self, id: &str) {
        let package = plugins::coming_soon(id);
        self.packages
            .get_or_insert_with(std::collections::HashMap::new)
            .insert(id.to_owned(), package);
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

    /// 本地生成：Piper 的固定音色，或 ZipVoice 克隆音色，按选中的音色走。
    fn generate_local(&mut self, project: &Path) {
        let Some(voice) = self.voices.get(self.voice.max(0) as usize).cloned() else {
            self.message = "本地还没有音色：先去「插件」页的模型中心装一个引擎".to_owned();
            self.tab = 2;
            return;
        };
        let piper_engine = self.piper.clone();
        let clone_engine = self.clone.clone();
        if piper_engine.is_none() && clone_engine.is_none() {
            self.message = self.hint();
            self.tab = 2;
            return;
        }
        let text = self.text.trim().to_owned();
        let rate = self.rate;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        let dir = project.join("ai_voice");
        let out = dir.join(format!("ai-voice-{}.wav", stamp()));
        let voice_id = voice.id.clone();
        let voice_name = voice.name.clone();
        let clone_voice = clone_engine.as_ref().and_then(|engine| {
            engine
                .voices
                .iter()
                .position(|candidate| candidate.id == voice_id)
                .map(|index| (engine.clone(), index))
        });
        let piper_voice = piper_engine.as_ref().and_then(|engine| {
            engine
                .voices
                .iter()
                .position(|candidate| candidate.id == voice_id)
                .map(|index| (engine.clone(), index))
        });
        self.begin(if clone_voice.is_some() {
            "本地语音克隆生成中（CPU，约 1–2× 实时）…"
        } else {
            "本地配音生成中…"
        });
        spawn_in_project(
            move || -> Result<Done, String> {
                let spoken = if let Some((engine, index)) = clone_voice {
                    engine.speak(index, &text, rate, &out, &cancel)?
                } else if let Some((engine, index)) = piper_voice {
                    engine.speak(index, &text, rate, &out, &cancel)?
                } else {
                    return Err("选中的音色不在已安装的引擎里：请重新选一个".to_owned());
                };
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
        self.recheck_account(studio);
        if !self.signed_in {
            // 未登录不是错误：直接引导登录，不弹红色提示。
            self.message =
                "云端 AI 需要登录星籁账号：已在弹窗里打开登录，登录后会自动刷新".to_owned();
            studio.request_login("云端 AI 需要登录星籁账号");
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
                self.recheck_account(studio);
                self.message = format!("生成失败：{error}");
                self.last = self.message.clone();
                self.alert(studio, &self.message.clone());
            }
        }
    }

    // ── 数字人 ──

    /// 「生成数字人片段」：本地走 Wav2Lip 插件，云端走 /autoedit/avatar。
    fn digital(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        if matches!(self.effective(), AiMode::Local) {
            self.digital_local(studio);
        } else {
            self.digital_cloud(studio);
        }
    }

    /// 云端数字人（兜底）：照片 + 云端配音的音频 → 平台生成。
    fn digital_cloud(&mut self, studio: &mut Studio) {
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

    /// 本地数字人：照片 + 文案（先用本地音色配音）或已有本地配音
    /// → 选中的引擎（Wav2Lip / SadTalker）出片 → mp4 → 插入时间轴。全程离线。
    fn digital_local(&mut self, studio: &mut Studio) {
        let Some(project) = project_dir(studio) else {
            self.message = "请先打开一个工程：数字人成片要放进工程目录".to_owned();
            return;
        };
        let Some(photo) = self.photo.as_ref() else {
            self.message = "请先选择一张正脸照片（JPG/PNG，≤10MB）".to_owned();
            return;
        };
        let engine = if self.digital_engine == 1 {
            self.sadtalker.clone().map(DigitalEngine::SadTalker)
        } else {
            self.digital.clone().map(DigitalEngine::Wav2Lip)
        };
        let Some(engine) = engine else {
            self.message = self.digital_status();
            self.tab = 2;
            return;
        };
        if let Err(error) = engine.ready() {
            self.message = format!("本地数字人还不能用：{error}");
            self.tab = 2;
            return;
        }
        let engine_label = engine.label();
        let text = self.text.trim().to_owned();
        // 有文案就先配音（用「配音」页选中的本地音色）；没文案就用最近
        // 一次生成的配音——两条路都只用本机，不上传。
        let existing = self.last_audio.clone().filter(|path| path.is_file());
        let voice = if text.is_empty() {
            if existing.is_none() {
                self.message =
                    "请先输入一段文案（会用「配音」页选中的本地音色念出来），或先在「配音」页生成一段配音"
                        .to_owned();
                self.tab = 0;
                return;
            }
            None
        } else {
            match self.pick_local_voice() {
                Some(voice) => Some(voice),
                None => {
                    self.message =
                        "本地还没有可用的配音音色：请先在「模型」页装 Piper 或 ZipVoice 插件"
                            .to_owned();
                    self.tab = 2;
                    return;
                }
            }
        };
        let bytes = photo.bytes.clone();
        let extension = if photo.mime.contains("png") { "png" } else { "jpg" };
        let rate = self.rate;
        let voice_name = voice
            .as_ref()
            .map(|voice| voice.label())
            .unwrap_or_else(|| "已有配音".to_owned());
        let dir = project.join("ai_avatar");
        let audio_dir = project.join("ai_voice");
        let stamp = stamp();
        let output = dir.join(format!("local-avatar-{stamp}.mp4"));
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.begin(if voice.is_some() {
            "本地数字人：先用本地音色配音…"
        } else {
            "本地数字人：读取已有配音…"
        });
        spawn_in_project(
            move || -> Result<Done, String> {
                let report = |fraction: f32, stage: &str| {
                    let stage = stage.to_owned();
                    host::on_ui(move |studio, _, _| {
                        studio.handle(Msg::Ai(AiMsg::Progress { fraction, stage }));
                    });
                };
                let audio = match voice {
                    Some(voice) => {
                        report(0.02, "本地配音生成中…");
                        let audio_out = audio_dir.join(format!("avatar-voice-{stamp}.wav"));
                        let spoken = voice.speak(&text, rate, &audio_out, &cancel)?;
                        report(
                            0.15,
                            &format!("配音好了（{}），开始生成视频…", spoken.voice_name),
                        );
                        audio_out
                    }
                    None => {
                        let existing =
                            existing.ok_or_else(|| "没有可用的配音：请先生成一段".to_owned())?;
                        report(0.15, "用已有配音开始生成视频…");
                        existing
                    }
                };
                let photo_path = write_temp_photo(&bytes, extension)?;
                let rendered = engine.render(
                    photo_path.clone(),
                    audio,
                    output,
                    &mut |fraction, stage| report(0.15 + 0.85 * fraction, stage),
                    &cancel,
                );
                let _ = std::fs::remove_file(&photo_path);
                let rendered = rendered?;
                let summary = media::probe(&rendered.path.to_string_lossy())?;
                Ok(Done {
                    path: rendered.path.clone(),
                    summary,
                    name: format!("照片数字人 · 本地 {engine_label}（{voice_name}）"),
                    detail: rendered.summary(),
                    cloud_audio_url: None,
                })
            },
            |studio, _, _, result| {
                studio.handle(Msg::Ai(AiMsg::Finished(Box::new(result))));
            },
        );
    }

    /// 当前选中的音色落在哪个本地引擎里：ZipVoice 优先，其次 Piper。
    fn pick_local_voice(&self) -> Option<LocalVoice> {
        let row = self.voices.get(self.voice.max(0) as usize)?;
        if let Some(engine) = self.clone.as_ref()
            && let Some(index) = engine.voices.iter().position(|voice| voice.id == row.id)
        {
            return Some(LocalVoice::Clone(engine.clone(), index));
        }
        if let Some(engine) = self.piper.as_ref()
            && let Some(index) = engine.voices.iter().position(|voice| voice.id == row.id)
        {
            return Some(LocalVoice::Piper(engine.clone(), index));
        }
        // 音色表还没刷新（例如刚装完插件）：退到第一个可用音色。
        if let Some(engine) = self.clone.as_ref() {
            return Some(LocalVoice::Clone(engine.clone(), 0));
        }
        self.piper.as_ref().map(|engine| LocalVoice::Piper(engine.clone(), 0))
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
    ///
    /// 只对清单里「真的有」的包发起请求：没上线的在界面上就灰着，走到这里
    /// 也先按清单再确认一遍，不发注定 404 的请求。
    fn download_plugin(&mut self, studio: &mut Studio, id: &str) {
        if self.busy {
            return;
        }
        let package = plugins::package_status(self.packages.as_ref(), id);
        if !package.online() {
            self.mark_coming_soon(id);
            self.message = plugins::readable_download_error(id, "HTTP 404");
            self.tab = 2;
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.busy = true;
        self.progress = 0.0;
        self.stage = format!(
            "正在下载插件包（约 {}）…",
            plugins::human_size(if package.size > 0 {
                package.size
            } else {
                plugins::download_size_hint(id)
            })
        );
        self.message = self.stage.clone();
        let dirs = studio.host.dirs.clone();
        let id = id.to_owned();
        let package = package.clone();
        let work_id = id.clone();
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
                let url = if package.url.trim().is_empty() {
                    plugins::download_url(&work_id)
                } else {
                    package.url.clone()
                };
                let zip =
                    plugins::download(&dirs, &work_id, &url, &package.sha256, &mut report, &cancel)?;
                let mut unpack = |fraction: f32, stage: &str| {
                    let stage = stage.to_owned();
                    host::on_ui(move |studio, _, _| {
                        studio.handle(Msg::Ai(AiMsg::Progress { fraction, stage }));
                    });
                };
                let plugin = plugins::import_zip(&dirs, &zip, &mut unpack)?;
                Ok(plugin.manifest.name)
            },
            move |studio, _, _, result| {
                studio.handle(Msg::Ai(AiMsg::PluginDownloaded {
                    id,
                    result: Box::new(result),
                }));
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

    /// 打开账号登录弹窗，并说一句为什么要登录。
    pub fn request_login(&mut self, why: &str) {
        self.account_sheet = true;
        self.ai.message = format!("{why}：在弹窗里登录后会自动刷新账号状态，不会上传任何素材");
    }

    /// 把上一次的失败提示收掉：成功之后不该还挂着一条红字。
    pub fn dismiss_notice(&mut self) {
        self.toast.up = false;
    }

    /// 从磁盘重读账号（登录、退出、token 过期之后都用它），并把 AI 面板
    /// 与全局账号状态一起对齐。
    pub fn reload_account(&mut self) {
        let account = concat_host::account::load();
        self.ai.apply_account(account.as_ref());
        self.account = account;
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

/// 本地数字人页选中的引擎：Wav2Lip（快）或 SadTalker（有头部动作，但很慢）。
#[derive(Clone)]
enum DigitalEngine {
    /// Wav2Lip：只用 GPU/CPU 跑 256 的口型模型，快。
    Wav2Lip(wav2lip::Engine),
    /// SadTalker：PyTorch CPU，会点头眨眼，慢。
    SadTalker(sadtalker::Engine),
}

impl DigitalEngine {
    /// 界面上写进成片名字的引擎名。
    fn label(&self) -> &'static str {
        match self {
            DigitalEngine::Wav2Lip(_) => "Wav2Lip",
            DigitalEngine::SadTalker(_) => "SadTalker",
        }
    }

    /// 引擎能不能跑。
    fn ready(&self) -> Result<(), String> {
        match self {
            DigitalEngine::Wav2Lip(engine) => engine.ready(),
            DigitalEngine::SadTalker(engine) => engine.ready(),
        }
    }

    /// 生成一段视频。
    fn render(
        &self,
        photo: PathBuf,
        audio: PathBuf,
        output: PathBuf,
        progress: &mut dyn FnMut(f32, &str),
        cancel: &AtomicBool,
    ) -> Result<wav2lip::Rendered, String> {
        match self {
            DigitalEngine::Wav2Lip(engine) => engine.render(
                &wav2lip::Request {
                    photo,
                    audio,
                    output,
                    fps: 25,
                    max_side: wav2lip::MAX_SIDE,
                },
                progress,
                cancel,
            ),
            DigitalEngine::SadTalker(engine) => engine.render(
                &sadtalker::Request {
                    photo,
                    audio,
                    output,
                },
                progress,
                cancel,
            ),
        }
    }
}

/// 本地数字人的配音方案：Piper 的固定音色或 ZipVoice 的克隆音色。
#[derive(Clone)]
enum LocalVoice {
    /// ZipVoice 克隆音色。
    Clone(zipvoice::Engine, usize),
    /// Piper 固定音色。
    Piper(piper::Engine, usize),
}

impl LocalVoice {
    /// 界面上显示的音色名。
    fn label(&self) -> String {
        match self {
            LocalVoice::Clone(engine, index) => engine
                .voices
                .get(*index)
                .map(|voice| format!("{} · 本地克隆", voice.name))
                .unwrap_or_else(|| "克隆音色".to_owned()),
            LocalVoice::Piper(engine, index) => engine
                .voices
                .get(*index)
                .map(|voice| voice.name.clone())
                .unwrap_or_else(|| "本地音色".to_owned()),
        }
    }

    /// 念一段文字，得到一条 WAV。
    fn speak(
        &self,
        text: &str,
        rate: f32,
        out: &Path,
        cancel: &AtomicBool,
    ) -> Result<piper::Spoken, String> {
        match self {
            LocalVoice::Clone(engine, index) => engine.speak(*index, text, rate, out, cancel),
            LocalVoice::Piper(engine, index) => engine.speak(*index, text, rate, out, cancel),
        }
    }
}

/// 把照片写进临时目录，给数字人引擎按路径读；成功与否由调用方删掉。
fn write_temp_photo(bytes: &[u8], extension: &str) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("xinglai-wav2lip");
    std::fs::create_dir_all(&dir).map_err(|error| format!("临时目录建不出来：{error}"))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    let path = dir.join(format!("face-{}-{nanos}.{extension}", std::process::id()));
    std::fs::write(&path, bytes).map_err(|error| format!("照片写不进临时目录：{error}"))?;
    Ok(path)
}

/// 一个模型的硬件要求，写成人话：`显存 ≥6GB · 内存 ≥12GB`。
fn requirement_label(spec: &ai_models::ModelSpec) -> String {
    if spec.id == "ai-background" {
        return "联网 + 星籁账号（可选 302.AI Key）".to_owned();
    }
    if spec.id == "cutout" {
        return "纯 CPU · 内存 ≥2GB（随功能自动下载）".to_owned();
    }
    let mut parts: Vec<String> = Vec::new();
    if spec.cuda {
        parts.push("NVIDIA 显卡（CUDA）".to_owned());
    }
    if spec.vram_mb > 0 {
        parts.push(format!("显存 ≥{}GB", gb_text(spec.vram_mb)));
    }
    if spec.ram_mb > 0 {
        parts.push(format!("内存 ≥{}GB", gb_text(spec.ram_mb)));
    }
    if spec.vram_mb == 0 {
        parts.push("纯 CPU".to_owned());
    }
    if parts.is_empty() {
        "—".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// MB 按整数 GB 说。
fn gb_text(mb: u64) -> u64 {
    if mb >= 1024 {
        (mb as f64 / 1024.0).round() as u64
    } else {
        mb
    }
}
