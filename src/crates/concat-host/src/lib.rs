// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer Abdullah and Concat contributors

//! What the editor window needs that is not the edit itself.
//!
//! The edit lives in `concat-project`, pixels in `concat-render` and
//! `concat-export`, files in `concat-media`. This crate is the layer the
//! window talks to: it opens a project folder as a [`session::Session`],
//! keeps the recents list, caches waveforms and filmstrips beside the
//! project, composites the paused monitor's true frame, plays the audible
//! clips, finds the masks behind cutouts, measures the camera shake stabilized
//! clips take out, packs templates, and holds the
//! one-at-a-time slots for long jobs.
//!
//! The window calls these functions in-process, and the doctrine is that
//! no editing decision is made here. If a function starts deciding what an
//! edit means, it belongs in `concat-project`.
//!
//! Nothing here knows about a window. Long work reports through callbacks
//! and cancels through flags, and the caller decides which thread it runs on.

pub mod account;
/// 本地 AI 模型目录：效果、大小、显存内存要求、本机适不适合。
pub mod ai_models;
/// 星籁 AI 工作室：一键成片 / 动态解说片 / 爆款复刻（平台接口 + 成片下载）。
pub mod ai_studio;
/// 星籁 AI 短剧（0.3.1）：分集剧本 / 分镜 / 逐集成片（平台接口 + 本地模板兜底）。
pub mod aidrama;
pub mod assets;
/// 剪映助手同款的常驻 AI 助手（文本）：`/xiaolai/chat` 一句话一问。
pub mod assistant;
/// 素材库悬停试听：一个只播一小段的小播放器（不影响时间线的混音）。
pub mod audio_preview;
/// 星籁 AI 生视频：平台 aivideo 接口、长视频续写拼接、本地模型预留。
pub mod aivideo;
pub mod awake;
pub mod brush;
pub mod capcut;
pub mod cards;
pub mod cloud;
/// 云端 AI（默认关闭）：配音与照片数字人，需要登录与额度。
pub mod cloud_ai;
pub mod consolidate;
#[cfg(test)]
mod consolidate_tests;
pub mod crash;
pub mod cutout;
/// 精准去电流声：找音频里的窄带纯音峰（12kHz 哨音一类）。
pub mod denoise;
/// 平台自己的 HTTPS 直连下载（OpenSSL，不走系统代理）。
pub mod direct;
pub mod dirs;
pub mod enhance;
pub mod export;
/// 本机算力检测：显卡、显存、内存、磁盘与推荐的运行档位。
pub mod hardware;
pub mod jobs;
pub mod lock;
pub mod logs;
pub mod machine;
pub mod media;
pub mod memory;
pub mod models;
pub mod net;
/// 本地配音引擎：Piper 插件的一次生成。
pub mod piper;
pub mod playback;
/// 专业后期底座（0.3.6）：平台 `/api/post/*` 与本地 EDL / OTIO / QC /
/// 交付包的兜底。
pub mod post;
/// 影视专业面板（0.3.7）：导演编排 / 监视 / 发行 / 护照 / 沙盘 / 插件 /
/// 模板 / 统一任务中心的平台接口与本机任务记录。
pub mod pro;
/// 影视专业二期（0.4.0）：片头片尾 / 实拍装配 / 特效 / 配乐 / 配音 /
/// 连贯性 / 风格匹配 / 素材质量的平台接口。
pub mod pro2;
/// 本地 AI 插件：安装、导入、删除、扫描。
pub mod plugins;
pub mod preview;
#[cfg(test)]
mod project_save_tests;
pub mod projects;
pub mod proxy;
pub mod record;
pub mod reverse;
/// 星籁电影 / 剧集制作向导（0.3.5）：平台 `/movie/*` 与 `/director/*` 的桌面端。
pub mod movie;
/// AI 工程包导入（0.3.5）：project_pack v1 的 zip → 桌面工程。
pub mod projectpack;
/// 本地数字人（Wav2Lip / SadTalker）引擎：照片 + 配音 → 对口型视频。
pub mod sadtalker;
mod route;
pub mod session;
/// 智能粗剪（0.3.2）：平台 `/smartcut/*` 的提交（后端并行开发，未就绪时
/// 如实返回「接口联调中」）。
pub mod smartcut;
pub mod sounds;
pub mod stabilize;
pub mod templates;
pub mod titles;
pub mod updates;
/// 素材库悬停预览的本地取帧/取声：随包 ffmpeg 把开头片段解成小 JPEG / PCM。
pub mod video_preview;
pub mod voices;
/// 本地数字人引擎（Wav2Lip 插件）：一次「照片 + 配音 → 对口型视频」。
pub mod wav2lip;
/// 本地语音克隆引擎：ZipVoice 插件的一次生成。
pub mod zipvoice;

pub use brush::{Brushes, RegionRequest};
pub use cutout::{AnalyseRequest, Cutouts};
pub use dirs::AppDirs;
pub use enhance::{EnhanceRequest, Enhancers};
pub use jobs::{Job, SingleFlight};
pub use projects::ProjectInfo;
pub use reverse::{ReverseRequest, Reversers};
pub use session::{EditorView, Session, SettingsView};
pub use stabilize::{StabilizeRequest, Stabilizers};
pub use titles::{TitleClip, Titles};

/// The one scheduler every decode for the screen goes through: the
/// monitor's frames and the frames ahead of the playhead first, then
/// filmstrips, the bin's artwork and proxies in turn, on a few threads
/// with one always kept clear of the background work. Owns the reader
/// pool the monitors read; see `concat_media::Prefetcher`.
pub fn scheduler() -> &'static std::sync::Arc<concat_media::Prefetcher> {
    static SCHEDULER: std::sync::OnceLock<std::sync::Arc<concat_media::Prefetcher>> =
        std::sync::OnceLock::new();
    SCHEDULER.get_or_init(|| {
        std::sync::Arc::new(concat_media::Prefetcher::with_defaults(
            // The frame cache is a share of the machine's memory, not one
            // size for a small laptop and a workstation alike.
            std::sync::Arc::new(concat_media::ReaderPool::sized_for(memory::total())),
        ))
    })
}
