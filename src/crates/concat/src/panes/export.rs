// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer Abdullah and Concat contributors

//! The export sheet: the form, the running render, and the result.
//!
//! The first pane cut out of the window's controller, and the shape every
//! later one follows. The pane owns its state; every way it can change -
//! a field edited, a button pressed, a worker reporting - is one
//! [`ExportMsg`]; [`ExportPane::update`] is the only code that changes the
//! state; and [`ExportPane::data`] is the one place its Slint rows are
//! built. The controller it needs for context - the session, the output
//! size, the job slots - is handed to `update` as the studio, which the
//! pane reads and asks things of but never reaches into for its own
//! fields.

use concat_export::{ClipKind, ExportClip};
use concat_host::export::{self, ExportSpec};
use concat_media::ColorRange;

use crate::format::{bytes, eta};
use crate::host::{on_ui, spawn};
use crate::i18n::{self, t, tf};
use crate::panes::Msg;
use crate::platform;
use crate::studio::{AUDIO_BPS, EXPORT_CRF, EXPORT_SHORT_SIDES, RATES, Studio, home_folder};
use crate::ui::{ExportData, ExportPhase};
use slint::{ModelRc, SharedString, VecModel};

/// Everything that can happen to the export sheet.
#[derive(Clone, Debug)]
pub enum ExportMsg {
    /// The sheet is asked for: the menu, the tray button or ⌘E.
    Open,
    /// The sheet is dismissed.
    Close,
    NameEdited(String),
    RateChanged(i32),
    CodecChanged(i32),
    TenBitChanged(bool),
    /// For an HDR timeline: written HDR (true), or tone-mapped to SDR.
    HdrChanged(bool),
    /// 三档质量：0 快速（预览级）、1 标准、2 高质量。它只改编码器的
    /// 速度/画质取舍，不改文件格式。
    QualityChanged(i32),
    /// 导出面板上「启用 AI 标识」的按钮：把设置打开，不默认。
    EnableAiBadge,
    /// The target bitrate field, in Mbps: the bitrate becomes a custom one.
    BitrateChanged(f32),
    /// Back to the form after a finished or failed render.
    Again,
    /// Pick the destination folder.
    Browse,
    /// The folder the picker answered with.
    FolderChosen(String),
    /// Show the finished file in the file manager.
    Reveal,
    Start,
    /// Start over a file already there: the confirmation sheet's answer.
    Overwrite,
    Cancel,
    /// The render's worker reporting where it is.
    Progress {
        /// Of the whole, `0..=1`.
        fraction: f32,
        /// What it is doing, in the person's language.
        stage: String,
    },
    /// The render's worker is done: the file written, or why not.
    Finished(Result<String, String>),
    /// The phone has moved the file to where it shows it - the path under
    /// the phone's storage - or could not, and the file is where it was.
    Published(Result<String, String>),
    /// 一键多版本矩阵的开关：开时「导出」按钮一次导出 N 个变体。
    MatrixToggled(bool),
    /// 矩阵的版本数量加减：`delta` 是 +1 或 -1，数量收在 2..=5。
    MatrixCountChanged(i32),
    /// 矩阵里一个版本的进度：第 `index` 个（0 起）、共 `count` 个，
    /// `fraction` 是这个版本自己的进度（0..=1），`stage` 是中文阶段名。
    MatrixProgress {
        /// 第几个版本，0 起。
        index: usize,
        /// 一共几个版本。
        count: usize,
        /// 版本的短名，如「钩子版」。
        tag: String,
        /// 这个版本自己的进度。
        fraction: f32,
        /// 阶段名，已翻译。
        stage: String,
    },
    /// 矩阵跑完：按顺序写出的文件，或第一个失败的原因。
    MatrixFinished(Result<Vec<String>, String>),
}

/// The export sheet's state.
pub struct ExportPane {
    pub open: bool,
    pub name: String,
    pub folder: String,
    pub resolution: usize,
    pub rate: usize,
    /// The quality tier picked: 0 quick (preview level), 1 standard,
    /// 2 high.
    pub quality: usize,
    /// The bitrate is the typed one, not the recommended one for the
    /// frame and the rate.
    pub custom: bool,
    /// Index into `VideoCodec::ALL`.
    pub codec: usize,
    pub ten_bit: bool,
    /// An HDR timeline written tone-mapped to SDR rather than as it is.
    pub sdr: bool,
    /// The typed bitrate in kbps, the one written when `custom`.
    pub bitrate: u32,
    pub phase: ExportPhase,
    pub progress: f32,
    pub stage: String,
    pub message: String,
    /// Where the finished file is, for Reveal.
    pub written: String,
    /// When the render started, for a real ETA.
    started_at: Option<std::time::Instant>,
    /// How long the last export took of its own footage: wall seconds a
    /// media second, so the next one can be estimated before it starts.
    last_speed: Option<f64>,
    /// The timeline's length when the render started, for that rate.
    started_media: f64,
    /// 一键多版本矩阵：开时一次导出 N 个变体；关时是普通的单文件导出。
    pub matrix: bool,
    /// 矩阵要几个版本，收在 2..=5。
    pub matrix_count: usize,
    /// 矩阵写出的成片，按版本顺序；普通导出时为空。结果区用它列出文件。
    matrix_outputs: Vec<String>,
}

impl Default for ExportPane {
    fn default() -> Self {
        Self {
            open: false,
            name: "未命名".into(),
            // An iPhone's sandbox has no Movies the app may make, as it has
            // no Desktop (#299); Documents is the folder Files shows, and a
            // finished export goes on to Photos from there.
            folder: home_folder(if cfg!(target_os = "ios") {
                "Documents/Exports"
            } else {
                "Movies"
            }),
            resolution: 2,
            rate: 0,
            quality: 1,
            custom: false,
            codec: 0,
            ten_bit: false,
            sdr: false,
            bitrate: 8000,
            phase: ExportPhase::Idle,
            progress: 0.0,
            stage: String::new(),
            message: String::new(),
            written: String::new(),
            started_at: None,
            last_speed: None,
            started_media: 0.0,
            matrix: false,
            matrix_count: 3,
            matrix_outputs: Vec::new(),
        }
    }
}

impl ExportPane {
    /// Applies one message. The studio is the rest of the window, for
    /// what the sheet needs to know and to start; the pane's own state is
    /// `self`, and while this runs the studio's copy of it is a blank the
    /// pane must not read.
    pub fn update(&mut self, msg: ExportMsg, studio: &mut Studio) {
        match msg {
            ExportMsg::Open => {
                self.open = true;
                self.phase = ExportPhase::Idle;
                self.message.clear();
                // The timeline's own size: the one rung that can be picked;
                // and its own rate, first on the rate list.
                self.resolution = Self::own_rung(studio);
                self.rate = 0;
                self.follow_canvas(studio);
            }
            ExportMsg::Close => self.open = false,
            ExportMsg::NameEdited(name) => self.name = name,
            ExportMsg::RateChanged(index) => {
                self.rate = (index.max(0) as usize).min(Self::rates(studio).len() - 1);
            }
            // A codec this device cannot encode is not taken: the picker
            // says so under it, and the choice before stands.
            ExportMsg::CodecChanged(index) => {
                let index = (index.max(0) as usize).min(concat_media::VideoCodec::ALL.len() - 1);
                if writable(concat_media::VideoCodec::ALL[index]) {
                    self.codec = index;
                }
            }
            ExportMsg::TenBitChanged(on) => self.ten_bit = on,
            ExportMsg::HdrChanged(on) => self.sdr = !on,
            ExportMsg::QualityChanged(index) => {
                self.quality = (index.max(0) as usize).min(QUALITY_PRESETS.len() - 1);
                log::info!("export: quality tier {}", self.quality);
            }
            ExportMsg::EnableAiBadge => {
                log::info!("export: AI 标识 opened from the export sheet");
                studio.prefs.ai_badge = Some(true);
                studio.prefs.save(&studio.host.dirs);
                studio.notify(&t("export.aiBadgeOn"), false);
            }
            ExportMsg::BitrateChanged(mbps) => {
                if mbps.is_finite() {
                    self.bitrate = ((mbps * 1000.0).round() as u32).clamp(500, 400_000);
                    self.custom = true;
                }
            }
            ExportMsg::Again => {
                self.phase = ExportPhase::Idle;
                self.progress = 0.0;
                self.matrix_outputs.clear();
            }
            // The picker runs with the studio unborrowed and answers as a
            // message of its own; see `host::after_dialog`.
            ExportMsg::Browse => {
                let title = i18n::t("export.exportTo");
                let current = self.folder.clone();
                crate::host::after_dialog(
                    move || platform::pick_folder(&title, &current),
                    |studio, _, _, folder| {
                        studio.handle(Msg::Export(ExportMsg::FolderChosen(
                            folder.to_string_lossy().into_owned(),
                        )));
                    },
                );
            }
            ExportMsg::FolderChosen(folder) => self.folder = folder,
            ExportMsg::Reveal => {
                if !self.written.is_empty()
                    && let Err(error) = platform::reveal(&self.written)
                {
                    studio.notify(&i18n::tf("export.couldNotShowFile", &[&error]), true);
                }
            }
            ExportMsg::Start => {
                if self.matrix {
                    self.start_matrix(studio);
                } else {
                    self.start(studio);
                }
            }
            ExportMsg::Overwrite => {
                if self.matrix {
                    self.begin_matrix(studio, true);
                } else {
                    self.begin(studio, true);
                }
            }
            ExportMsg::Cancel => {
                studio.host.exporter.cancel();
                self.phase = ExportPhase::Idle;
                self.progress = 0.0;
            }
            ExportMsg::Progress { fraction, stage } => {
                if self.phase == ExportPhase::Running {
                    // Gone back: the export started again from its first
                    // frame on the slower encoder, and the time left is
                    // reckoned from there.
                    if fraction < self.progress {
                        self.started_at = Some(std::time::Instant::now());
                    }
                    self.progress = fraction.clamp(0.0, 1.0);
                    self.stage = stage;
                }
            }
            ExportMsg::Finished(Ok(written)) => {
                if let Some(started) = self.started_at {
                    let wall = started.elapsed().as_secs_f64().max(0.1);
                    self.last_speed = Some(self.started_media.max(0.1) / wall);
                }
                self.phase = ExportPhase::Done;
                self.progress = 1.0;
                self.written = written.clone();
                // A phone moves the file to where its gallery shows it, and
                // the sheet says "finished" once it is there.
                let handed = platform::publish_export(written.into(), |result| {
                    on_ui(move |studio, _, _| {
                        studio.handle(Msg::Export(ExportMsg::Published(result)));
                    });
                });
                if !handed {
                    studio.notify(&t("export.exportFinished"), false);
                }
            }
            ExportMsg::Published(Ok(path)) => {
                self.written = path;
                studio.notify(&t("export.exportFinished"), false);
            }
            ExportMsg::Published(Err(error)) => {
                let folder = platform::published_folder().unwrap_or_default();
                studio.notify(&tf("export.couldNotPublish", &[&folder, &error]), true);
            }
            ExportMsg::MatrixToggled(on) => {
                if self.phase == ExportPhase::Running {
                    return;
                }
                self.matrix = on;
                log::info!("export: 一键多版本矩阵 {}", if on { "开" } else { "关" });
            }
            ExportMsg::MatrixCountChanged(delta) => {
                if self.phase == ExportPhase::Running {
                    return;
                }
                let next = self.matrix_count as i32 + delta;
                self.matrix_count = next.clamp(2, MATRIX_VARIANTS.len() as i32) as usize;
            }
            ExportMsg::MatrixProgress {
                index,
                count,
                tag,
                fraction,
                stage,
            } => {
                if self.phase == ExportPhase::Running && count > 0 {
                    // 一个版本的进度映射到整批进度里；同一版本内阶段切换
                    // 会让引擎的帧数从头算，所以取已经到过的最大值，进度
                    // 条不会倒着走。
                    let wrapped = (index as f32 + fraction.clamp(0.0, 1.0)) / count as f32;
                    self.progress = wrapped.clamp(self.progress, 1.0);
                    self.stage = tf(
                        "export.matrixProgress",
                        &[
                            &format!("{}", index + 1),
                            &format!("{count}"),
                            &tag,
                            &stage,
                        ],
                    );
                }
            }
            ExportMsg::MatrixFinished(Ok(written)) => {
                if let Some(started) = self.started_at {
                    let wall = started.elapsed().as_secs_f64().max(0.1);
                    self.last_speed = Some(self.started_media.max(0.1) / wall);
                }
                self.phase = ExportPhase::Done;
                self.progress = 1.0;
                self.written = written.last().cloned().unwrap_or_default();
                self.matrix_outputs = written;
                studio.notify(&t("export.matrixFinished"), false);
            }
            ExportMsg::MatrixFinished(Err(error)) => {
                if self.phase == ExportPhase::Idle {
                    // 取消：面板已经退回空闲了。
                    return;
                }
                self.phase = ExportPhase::Failed;
                self.message = error.clone();
                studio.notify(&tf("export.exportFailed", &[&error]), true);
            }
            ExportMsg::Finished(Err(error)) => {
                if self.phase == ExportPhase::Idle {
                    // Cancelled: the sheet already went back to idle.
                    return;
                }
                self.phase = ExportPhase::Failed;
                self.message = error.clone();
                studio.notify(&tf("export.exportFailed", &[&error]), true);
            }
        }
    }

    /// The resolution ladder for this project: the standard short sides,
    /// with the project's own among them where it is not already - a
    /// custom frame has its own rung - from largest to smallest.
    fn ladder(studio: &Studio) -> Vec<u32> {
        let mut rungs: Vec<u32> = EXPORT_SHORT_SIDES.to_vec();
        let own = Self::own_short(studio);
        if !rungs.contains(&own) {
            rungs.push(own);
        }
        rungs.sort_unstable_by(|a, b| b.cmp(a));
        rungs
    }

    /// The rates the sheet offers: the timeline's own first - what it is
    /// exported at unless the person picks another - then every other
    /// rate a project can be made at. A 25 fps cut used to export at 30
    /// unless someone noticed (audit 2026-10-04, #8).
    fn rates(studio: &Studio) -> Vec<(i64, i64)> {
        let video = studio.project().active().video;
        Self::rates_around((video.rate_num, video.rate_den.max(1)))
    }

    /// [`ExportPane::rates`] for a timeline at `own`.
    fn rates_around(own: (i64, i64)) -> Vec<(i64, i64)> {
        let same = |(num, den): (i64, i64)| num * own.1 == own.0 * den;
        std::iter::once(own)
            .chain(
                RATES
                    .iter()
                    .map(|&(_, num, den)| (num, den))
                    .filter(|&rate| !same(rate)),
            )
            .collect()
    }

    /// The rate the export renders at.
    fn rate(&self, studio: &Studio) -> (i64, i64) {
        let rates = Self::rates(studio);
        rates[self.rate.min(rates.len() - 1)]
    }

    /// A rate as the sheet names it: "23.976 fps", "25 fps".
    fn rate_name((num, den): (i64, i64)) -> String {
        let name = match RATES.iter().find(|&&(_, n, d)| n * den == num * d) {
            Some((name, _, _)) => (*name).to_owned(),
            None => {
                let fps = format!("{:.3}", crate::studio::fps_of(num, den));
                fps.trim_end_matches('0').trim_end_matches('.').to_owned()
            }
        };
        format!("{name} fps")
    }

    /// The project's short side, in pixels.
    fn own_short(studio: &Studio) -> u32 {
        let (width, height) = studio.output_size();
        width.min(height).max(2)
    }

    /// The rung of the ladder that is the project's own size: the one
    /// the sheet lets be picked, since an edit exports at the size it was
    /// cut at. https://github.com/jub0t/Concat/issues/109
    fn own_rung(studio: &Studio) -> usize {
        let own = Self::own_short(studio);
        Self::ladder(studio)
            .iter()
            .position(|rung| *rung == own)
            .unwrap_or(0)
    }

    /// `short` scaled along the project's aspect and rounded to even
    /// dimensions, which is what the encoder's chroma subsampling needs.
    fn frame_for(studio: &Studio, short: u32) -> (u32, u32) {
        let (project_w, project_h) = studio.output_size();
        let (project_w, project_h) = (project_w.max(1) as f64, project_h.max(1) as f64);
        let even = |side: f64| ((side / 2.0).round() as u32 * 2).max(2);
        if project_w >= project_h {
            (even(short as f64 * project_w / project_h), short)
        } else {
            (short, even(short as f64 * project_h / project_w))
        }
    }

    /// The frame the export renders at: the sheet's rung of the ladder,
    /// along the project's aspect.
    pub fn size(&self, studio: &Studio) -> (u32, u32) {
        let ladder = Self::ladder(studio);
        let short = ladder[self.resolution.min(ladder.len() - 1)];
        Self::frame_for(studio, short)
    }

    /// 矩阵要几个版本：收在 2..=5，默认 3。
    fn matrix_count(&self) -> usize {
        self.matrix_count.clamp(2, MATRIX_VARIANTS.len())
    }

    /// 导出面板上「一键多版本」的计划：每行一个版本，平台、版本名、
    /// 真实秒数、开头剪掉多少、字幕样式。写出来是给人在点导出之前
    /// 先看清楚的，不是日志。
    fn matrix_plan(&self, studio: &Studio) -> String {
        if !self.matrix {
            return String::new();
        }
        let whole = f64::from(studio.duration()).max(0.0);
        MATRIX_VARIANTS[..self.matrix_count()]
            .iter()
            .enumerate()
            .map(|(index, variant)| {
                let (skip, seconds) = variant_seconds(variant, whole);
                tf(
                    "export.matrixPlanLine",
                    &[
                        &format!("{}", index + 1),
                        &t(variant.platform),
                        &t(variant.tag),
                        &format!("{seconds:.1}"),
                        &format!("{skip:.1}"),
                        &matrix_look_name(variant.look),
                    ],
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 矩阵导完之后的结果行：一句「已导出 N 个版本」，下面按顺序列出
    /// 文件名；不是矩阵、还没导完就是空串。
    fn matrix_done(&self) -> String {
        if !self.matrix || self.phase != ExportPhase::Done || self.matrix_outputs.is_empty() {
            return String::new();
        }
        let mut lines = vec![tf(
            "export.matrixDone",
            &[&format!("{}", self.matrix_outputs.len())],
        )];
        for path in &self.matrix_outputs {
            lines.push(format!("· {}", crate::studio::file_name_of(path)));
        }
        lines.join("\n")
    }

    /// The bitrate the file is written at, in kbps: the typed one, or the
    /// one YouTube asks uploads at for the frame - 5 Mbps at 720p, 8 at
    /// 1080p, 16 at 1440p and 40 at 4K, half again above 30 fps - which
    /// every other platform takes too, less for HEVC and AV1, which spend
    /// fewer bits on the same picture, and a quarter more at ten bits.
    /// Rounded to half a megabit, as a person would say it. The
    /// recommended one follows the frame, the rate and the codec until a
    /// bitrate is typed.
    pub fn target_kbps(&self, studio: &Studio) -> u32 {
        if self.custom {
            return self.bitrate;
        }
        let (width, height) = self.size(studio);
        let (num, den) = self.rate(studio);
        let fps = num as f32 / den.max(1) as f32;
        let base = match width.min(height) {
            0..=480 => 2_500.0,
            481..=720 => 5_000.0,
            721..=1080 => 8_000.0,
            1081..=1440 => 16_000.0,
            _ => 40_000.0,
        };
        let motion = if fps > 30.5 { 1.5 } else { 1.0 };
        let deep = if self.deep(studio) { 1.25 } else { 1.0 };
        let kbps = base * motion * self.codec(studio).size_factor() * deep;
        ((kbps / 500.0).round() * 500.0).max(500.0) as u32
    }

    /// The file's size, in bytes: the bitrate is an average the encoder
    /// keeps to, so this is what is written, give or take.
    pub fn size_bytes(&self, studio: &Studio) -> f32 {
        let video = self.target_kbps(studio) as f32 * 1000.0;
        (video + AUDIO_BPS) * studio.duration().max(1.0) / 8.0
    }

    /// The codec a sheet opens on follows the canvas (#456): a timeline
    /// that lets something show through opens on ProRes 4444, which every
    /// editor takes with its alpha (VP9 where ProRes cannot be written),
    /// and an opaque one leaves an alpha codec picked for an overlay
    /// before for H.264, rather than writing it hundreds of megabits it
    /// has no use for. Within the sheet the pick is the person's.
    fn follow_canvas(&mut self, studio: &Studio) {
        use concat_media::VideoCodec;
        let chosen = VideoCodec::ALL[self.codec.min(VideoCodec::ALL.len() - 1)];
        let see_through = studio.timeline().video.canvas[3] < 255;
        let wanted = if see_through && !chosen.keeps_alpha() {
            [VideoCodec::ProRes4444, VideoCodec::Vp9Alpha]
                .into_iter()
                .find(|codec| writable(*codec))
        } else if !see_through && chosen.keeps_alpha() {
            Some(VideoCodec::H264)
        } else {
            None
        };
        if let Some(index) =
            wanted.and_then(|codec| VideoCodec::ALL.iter().position(|c| *c == codec))
        {
            self.codec = index;
        }
    }

    /// The codec the sheet has chosen - HEVC for an HDR file where H.264
    /// was, since H.264 does not carry HDR.
    pub fn codec(&self, studio: &Studio) -> concat_media::VideoCodec {
        let codec =
            concat_media::VideoCodec::ALL[self.codec.min(concat_media::VideoCodec::ALL.len() - 1)];
        if self.hdr(studio) && codec == concat_media::VideoCodec::H264 {
            concat_media::VideoCodec::Hevc
        } else {
            codec
        }
    }

    /// Whether the file is written HDR: the timeline is, the sheet was
    /// not told to tone-map it to SDR, and the codec an HDR file would be
    /// written in can be written at ten bits here - on a phone it cannot,
    /// and the file is tone-mapped rather than failing.
    pub fn hdr(&self, studio: &Studio) -> bool {
        let chosen =
            concat_media::VideoCodec::ALL[self.codec.min(concat_media::VideoCodec::ALL.len() - 1)];
        let deep = match chosen {
            concat_media::VideoCodec::H264 => concat_media::VideoCodec::Hevc,
            codec => codec,
        };
        // An overlay that keeps its alpha is written SDR: the export
        // tone-maps an HDR timeline for it.
        !chosen.keeps_alpha()
            && studio.timeline().video.color_space.is_hdr()
            && !self.sdr
            && deep.ten_bit_available()
            && writable(deep)
    }

    /// Whether the file is written at ten bits: HDR always, SDR when the
    /// switch asks and the codec can be written that deep here.
    pub fn deep(&self, studio: &Studio) -> bool {
        self.hdr(studio) || (self.ten_bit && self.codec(studio).ten_bit_available())
    }

    /// Starts the render on a worker, once the name is a file's and no
    /// file is already there; its reports come back as messages.
    fn start(&mut self, studio: &mut Studio) {
        self.begin(studio, false);
    }

    /// 一键多版本矩阵：从同一个工程一次导出 N 个变体。
    ///
    /// 每个变体都是工程的一份副本：换开头（剪掉 `skip` 秒）、截时长到
    /// 15/30/60 秒上限、换字幕样式（黄字黑边 / 原样 / 白字底板）。文件名
    /// 按「平台 + 版本名 + 真实秒数」自动生成，例如
    /// `宣传片-抖音-钩子版-15s.mp4`。用户的工程、撤销栈、工程文件都不动。
    fn start_matrix(&mut self, studio: &mut Studio) {
        self.begin_matrix(studio, false);
    }

    /// [`ExportPane::start_matrix`]，`overwrite` 同 [`ExportPane::begin`]：
    /// 同名文件已有的时候，等确认页回来再覆盖。
    fn begin_matrix(&mut self, studio: &mut Studio, overwrite: bool) {
        if studio.session.is_none() {
            return;
        }
        if studio.timeline().clips.is_empty() {
            self.phase = ExportPhase::Failed;
            self.message = t("export.nothingTimelineExport");
            return;
        }
        // 名字照旧要是一个文件的名字；矩阵只在它后面接平台与版本。
        let Some(name) = file_name(&self.name) else {
            self.phase = ExportPhase::Failed;
            self.message = t("export.nameNeedsToBeAFileName");
            return;
        };
        let whole = f64::from(studio.duration());
        if whole < 1.0 {
            self.phase = ExportPhase::Failed;
            self.message = t("export.matrixTooShort");
            return;
        }
        let container = self.codec(studio).container();
        let folder = self.folder.trim_end_matches('/').to_owned();
        let count = self.matrix_count();
        let variants: Vec<Variant> = MATRIX_VARIANTS[..count].to_vec();
        let outputs: Vec<String> = variants
            .iter()
            .map(|variant| {
                let (_, seconds) = variant_seconds(variant, whole);
                matrix_output(&folder, &name, variant, seconds, container)
            })
            .collect();
        // 同名文件已有：和普通导出一样，先把确认页交给用户。
        if !overwrite
            && let Some(existing) = outputs
                .iter()
                .find(|path| std::path::Path::new(path.as_str()).exists())
        {
            studio.confirm = Some(crate::studio::Confirm::ReplaceExport {
                path: existing.clone(),
            });
            return;
        }
        let job = match studio.host.exporter.begin() {
            Ok(job) => job,
            Err(error) => {
                self.phase = ExportPhase::Failed;
                self.message = error;
                return;
            }
        };
        // 导出线程要的东西：一份工程副本、它的文件夹、帧与帧率、字幕
        // 绘制器与预览监视器。都取完就把 session 的借用放下。
        let (mut project, project_dir) = {
            let Some(session) = studio.session.as_ref() else {
                return;
            };
            (
                session.project().clone(),
                std::path::PathBuf::from(session.path()),
            )
        };
        // AI 角标与水印照旧只进导出副本；每个变体都从同一份干净副本变出来。
        if studio.prefs.ai_badge.unwrap_or(false) {
            add_ai_badge(&mut project, whole);
        }
        let watermark = watermark_overlay(studio);
        let (width, height) = self.size(studio);
        let (num, den) = self.rate(studio);
        let titles = std::sync::Arc::clone(&studio.host.titles);
        let monitor = studio.host.monitor.clone();
        let spec = ExportSpec {
            // 每版写出前才填；这里只要一份公共设置。
            output: String::new(),
            crf: QUALITY_CRF[self.quality],
            preset: QUALITY_PRESETS[self.quality].into(),
            codec: self.codec(studio),
            ten_bit: self.deep(studio),
            rate_mode: concat_media::RateMode::Average,
            bitrate_kbps: self.target_kbps(studio),
            color_range: ColorRange::Limited,
            hdr: self.hdr(studio),
        };
        let total_media: f64 = variants
            .iter()
            .map(|variant| variant_seconds(variant, whole).1)
            .sum();
        studio.pause();
        self.started_media = total_media.max(0.1);
        self.phase = ExportPhase::Running;
        self.progress = 0.0;
        self.stage = t("export.preparing");
        self.message.clear();
        self.written.clear();
        self.matrix_outputs.clear();
        self.started_at = Some(std::time::Instant::now());
        log::info!("export: 一键多版本矩阵 · {count} 版 · 工程 {whole:.2} 秒");

        spawn(
            move || {
                let job = job;
                let cancel = job.cancel_flag();
                let stopped = || cancel.load(std::sync::atomic::Ordering::Relaxed);
                // 预览用的解码器先关掉，给导出自己的解码器与编码器让路；
                // 与单文件导出同一套做法。
                let closed = concat_host::scheduler()
                    .pool()
                    .close_idle(std::time::Duration::ZERO);
                if closed > 0 {
                    log::debug!("export: closed {closed} idle preview decoder(s) before matrix");
                }
                let mut written = Vec::with_capacity(count);
                for (index, variant) in variants.iter().enumerate() {
                    if stopped() {
                        return Err("export cancelled".to_owned());
                    }
                    let (_, seconds) = variant_seconds(variant, whole);
                    let output = matrix_output(&folder, &name, variant, seconds, container);
                    let tag = t(variant.tag);
                    // 标题/字幕按本变体自己的样式重新画；同一个工程副本
                    // 会同时被剪开头、截时长，所以绘制与扁平化都在这之后。
                    let mut edition = project.clone();
                    shape_variant(&mut edition, variant, whole);
                    let Some(titles) = titles.clips_unless(&edition, width, height, cancel) else {
                        return Err("export cancelled".to_owned());
                    };
                    let titles = titles.into_iter().map(|title| title.clip).collect();
                    let mut spec = spec.clone();
                    spec.output = output.clone();
                    let mut request =
                        export::request_for(&edition, &project_dir, &spec, titles);
                    request.width = width;
                    request.height = height;
                    request.rate_num = num;
                    request.rate_den = den;
                    if let Some(mut watermark) = watermark.clone() {
                        watermark.track = request
                            .clips
                            .iter()
                            .map(|clip| clip.track)
                            .max()
                            .unwrap_or(0)
                            + 1;
                        request.clips.push(watermark);
                    }
                    {
                        let tag = tag.clone();
                        let stage = t("export.matrixPreparing");
                        on_ui(move |studio, _, _| {
                            studio.handle(Msg::Export(ExportMsg::MatrixProgress {
                                index,
                                count,
                                tag,
                                fraction: 0.0,
                                stage,
                            }));
                        });
                    }
                    let compositor = monitor.off_thread();
                    let tag_progress = tag.clone();
                    let result = export::run_on(&request, compositor, cancel, move |progress| {
                        let fraction = if progress.total > 0 {
                            progress.frame as f32 / progress.total as f32
                        } else {
                            0.0
                        };
                        let stage = stage_word(progress.stage);
                        let tag = tag_progress.clone();
                        on_ui(move |studio, _, _| {
                            studio.handle(Msg::Export(ExportMsg::MatrixProgress {
                                index,
                                count,
                                tag,
                                fraction,
                                stage,
                            }));
                        });
                    });
                    match result {
                        Ok(path) => {
                            log::info!(
                                "export: 矩阵第 {}/{count} 版写出 {}",
                                index + 1,
                                crate::studio::file_name_of(&path)
                            );
                            written.push(path);
                        }
                        Err(error) => {
                            return Err(tf(
                                "export.matrixFailedVariant",
                                &[&tag, &error],
                            ));
                        }
                    }
                }
                log::info!("export: 一键多版本矩阵完成 · {} 个文件", written.len());
                Ok(written)
            },
            |studio, _, _, result| studio.handle(Msg::Export(ExportMsg::MatrixFinished(result))),
        );
    }

    /// [`ExportPane::start`], with `overwrite` saying the person has
    /// answered the sheet that asked about the file already there.
    fn begin(&mut self, studio: &mut Studio, overwrite: bool) {
        if studio.session.is_none() {
            return;
        }
        if studio.timeline().clips.is_empty() {
            self.phase = ExportPhase::Failed;
            self.message = t("export.nothingTimelineExport");
            return;
        }
        // The name is a file's, or the sheet says what is wrong with it: an
        // empty one wrote a hidden `.mp4`, and a slash made folders (audit
        // 2026-10-09, finding 7).
        let Some(name) = file_name(&self.name) else {
            self.phase = ExportPhase::Failed;
            self.message = t("export.nameNeedsToBeAFileName");
            return;
        };
        let output = format!(
            "{}/{name}.{}",
            self.folder.trim_end_matches('/'),
            self.codec(studio).container()
        );
        // A file already there is replaced once the person has said so:
        // the confirmation sheet asks, and its answer comes back as
        // `Overwrite`. Nothing is written until then; the engine renders
        // beside the file and renames over it at the end.
        if !overwrite && std::path::Path::new(&output).exists() {
            studio.confirm = Some(crate::studio::Confirm::ReplaceExport { path: output });
            return;
        }
        let job = match studio.host.exporter.begin() {
            Ok(job) => job,
            Err(error) => {
                self.phase = ExportPhase::Failed;
                self.message = error;
                return;
            }
        };
        let Some(session) = studio.session.as_ref() else {
            return;
        };
        let spec = ExportSpec {
            output: output.clone(),
            // Unused at an average bitrate; the quality the encoder falls
            // back on were it ever not given one.
            crf: QUALITY_CRF[self.quality],
            preset: QUALITY_PRESETS[self.quality].into(),
            codec: self.codec(studio),
            ten_bit: self.deep(studio),
            rate_mode: concat_media::RateMode::Average,
            bitrate_kbps: self.target_kbps(studio),
            // Video range, what every player and every platform expects.
            color_range: ColorRange::Limited,
            hdr: self.hdr(studio),
        };
        // What the export's thread needs, taken here and nothing more: a
        // copy of the edit, its folder, the frame and the rate, and the
        // handles on the painter and the monitor. Everything that takes
        // time is done on that thread - the titles painted, which waits
        // on the fonts a preview worker holds for the whole of its batch;
        // the edit flattened; the preview's decoders closed; the GPU
        // chosen - and the window goes on answering: hundreds of captions
        // are seconds of painting on a phone, and Android closes an app
        // whose window does not answer a touch for five.
        let mut project = session.project().clone();
        // 右下角的「AI 生成」角标：只写进导出的这份工程副本，
        // 不碰用户的工程、也不进撤销栈。
        if studio.prefs.ai_badge.unwrap_or(false) {
            add_ai_badge(&mut project, f64::from(studio.duration()));
        }
        // 自定义水印：同样的做派，只是一张图片片段。存在这里是因为
        // 导出线程拿不到设置；它自己算好尺寸、位置和透明度。
        let watermark = watermark_overlay(studio);
        let project_dir = std::path::PathBuf::from(session.path());
        let (width, height) = self.size(studio);
        let (num, den) = self.rate(studio);
        let titles = std::sync::Arc::clone(&studio.host.titles);
        let monitor = studio.host.monitor.clone();

        studio.pause();
        self.started_media = f64::from(studio.duration()).max(0.1);
        self.phase = ExportPhase::Running;
        self.progress = 0.0;
        self.stage = t("export.preparing");
        self.message.clear();
        self.written.clear();
        self.started_at = Some(std::time::Instant::now());

        spawn(
            move || {
                let job = job;
                let cancel = job.cancel_flag();
                let stopped = || cancel.load(std::sync::atomic::Ordering::Relaxed);
                // At the export's own frame, not the project's: a 4K export
                // of an HD edit was its captions painted at HD and scaled
                // up, soft. The API's export paints them so too.
                let Some(titles) = titles.clips_unless(&project, width, height, cancel) else {
                    return Err("export cancelled".to_owned());
                };
                let titles = titles.into_iter().map(|title| title.clip).collect();
                let mut request = export::request_for(&project, &project_dir, &spec, titles);
                request.width = width;
                request.height = height;
                request.rate_num = num;
                request.rate_den = den;
                // 自定义水印叠在一切之上：它的轨道比现有最上面的轨道还高
                // 一层，所以盖住字幕与「AI 生成」角标，位置由设置算好。
                if let Some(mut watermark) = watermark {
                    watermark.track = request
                        .clips
                        .iter()
                        .map(|clip| clip.track)
                        .max()
                        .unwrap_or(0)
                        + 1;
                    request.clips.push(watermark);
                }
                // The monitor's readers that sit idle are closed now rather
                // than ten seconds on by the sweep: on a phone up to three of
                // them hold MediaCodec decoder sessions, which the chip has
                // few of and its encoder and the export's own decoders need.
                // A reader comes back on the next frame the monitor asks of
                // it; the cached frames stay.
                let closed = concat_host::scheduler()
                    .pool()
                    .close_idle(std::time::Duration::ZERO);
                if closed > 0 {
                    log::debug!("export: closed {closed} idle preview decoder(s) before starting");
                }
                if stopped() {
                    return Err("export cancelled".to_owned());
                }
                // The export draws on the window's device - a sibling of the
                // monitor's compositor - not on a second device opened
                // through the drivers for the export alone, which on Windows
                // laptops with NVIDIA chips took the whole app down at the
                // first frame with nothing in the log (issue #202). Not on
                // Vulkan, whose queue the window submits to from its own
                // thread (#324); see `Monitor::off_thread`. None there or
                // without a GPU, and the render then opens what it can.
                let compositor = monitor.off_thread();
                export::run_on(&request, compositor, cancel, |progress| {
                    let fraction = if progress.total > 0 {
                        progress.frame as f32 / progress.total as f32
                    } else {
                        0.0
                    };
                    let stage = match progress.stage {
                        "rendering" => t("export.renderingVideo"),
                        concat_export::RENDERING_IN_SOFTWARE => t("export.renderingInSoftware"),
                        "mixing audio" => t("export.mixingAudio"),
                        "muxing" => t("export.finalisingFile"),
                        other => other.to_owned(),
                    };
                    on_ui(move |studio, _, _| {
                        studio.handle(Msg::Export(ExportMsg::Progress { fraction, stage }));
                    });
                })
            },
            |studio, _, _, result| studio.handle(Msg::Export(ExportMsg::Finished(result))),
        );
    }

    /// The sheet as Slint shows it.
    pub fn data(&self, studio: &Studio) -> ExportData {
        let (width, height) = self.size(studio);
        let (num, den) = self.rate(studio);
        let rate = num as f32 / den as f32;
        let clips = studio.timeline().clips.len();
        let titles = studio
            .timeline()
            .clips
            .iter()
            .filter(|clip| clip.kind == concat_project::model::ClipKind::Text)
            .count();
        let container = self.codec(studio).container();
        ExportData {
            open: self.open,
            name: self.name.as_str().into(),
            // Where the file ends up: on a phone, the folder its gallery
            // shows, not the app's own that the export writes into.
            // The name as it will be written: trimmed, as `begin` trims it.
            path: match platform::published_folder() {
                Some(folder) => format!("{folder}/{}.{container}", self.name.trim()),
                None => format!(
                    "{}/{}.{container}",
                    self.folder.trim_end_matches('/'),
                    self.name.trim()
                ),
            }
            .into(),
            format: format!("{width} × {height} · {rate:.2} fps").into(),
            duration: {
                let whole = studio.duration().max(0.0) as i32;
                format!("{}:{:02}", whole / 60, whole % 60).into()
            },
            contents: if titles > 0 {
                tf("export.contentsClipsTitles", &[&clips, &titles])
            } else {
                tf("export.contentsClips", &[&clips])
            }
            .into(),
            resolution: self.resolution as i32,
            resolutions: {
                let sizes: Vec<SharedString> = Self::ladder(studio)
                    .iter()
                    .map(|rung| {
                        let (width, height) = Self::frame_for(studio, *rung);
                        format!(
                            "{width} × {height}{}",
                            platform_label(width, height)
                        )
                        .into()
                    })
                    .collect();
                ModelRc::new(VecModel::from(sizes))
            },
            rate: self.rate as i32,
            rates: {
                let names: Vec<SharedString> = Self::rates(studio)
                    .into_iter()
                    .map(|rate| Self::rate_name(rate).into())
                    .collect();
                ModelRc::new(VecModel::from(names))
            },
            rate_details: {
                let mut details: Vec<SharedString> =
                    vec![SharedString::new(); Self::rates(studio).len()];
                details[0] = t("export.timelineSize").into();
                ModelRc::new(VecModel::from(details))
            },
            codec: concat_media::VideoCodec::ALL
                .iter()
                .position(|codec| *codec == self.codec(studio))
                .unwrap_or(0) as i32,
            h264_encodable: writable(concat_media::VideoCodec::H264),
            hevc_encodable: writable(concat_media::VideoCodec::Hevc),
            av1_encodable: writable(concat_media::VideoCodec::Av1),
            prores_encodable: writable(concat_media::VideoCodec::ProRes4444),
            vp9_alpha_encodable: writable(concat_media::VideoCodec::Vp9Alpha),
            see_through: studio.timeline().video.canvas[3] < 255,
            keeps_alpha: self.codec(studio).keeps_alpha(),
            rate_controlled: self.codec(studio).rate_controlled(),
            ten_bit: self.ten_bit,
            hdr_timeline: studio.timeline().video.color_space.is_hdr(),
            hdr: self.hdr(studio),
            deep_available: concat_media::VideoCodec::ALL
                .iter()
                .any(|codec| codec.ten_bit_available()),
            hdr_label: match studio.timeline().video.color_space {
                concat_project::model::ColorSpace::Pq => "HDR (PQ)",
                _ => "HDR (HLG)",
            }
            .into(),
            target: self.target_kbps(studio) as f32 / 1000.0,
            encoding: {
                // "HEVC 10-bit · hardware": the standard, the depth when it
                // is the deeper one, and whether the platform's own encoder
                // will be doing it.
                let codec = self.codec(studio);
                let mut words = vec![codec.label().to_owned()];
                if self.deep(studio) {
                    words.push("10-bit".to_owned());
                }
                if self.hdr(studio) {
                    words.push(
                        match studio.timeline().video.color_space {
                            concat_project::model::ColorSpace::Pq => "PQ",
                            _ => "HLG",
                        }
                        .to_owned(),
                    );
                }
                if codec
                    .hardware_encoded_known(true, self.deep(studio))
                    .unwrap_or(false)
                {
                    words.push(format!("· {}", t("export.hardware")));
                }
                words.join(" ")
            }
            .into(),
            size_chosen: bytes(self.size_bytes(studio)).into(),
            phase: self.phase,
            progress: self.progress,
            stage: self.stage.as_str().into(),
            eta: if self.phase == ExportPhase::Running && self.progress > 0.02 {
                self.started_at
                    .map(|started| {
                        let elapsed = started.elapsed().as_secs_f32();
                        eta(elapsed / self.progress * (1.0 - self.progress)).into()
                    })
                    .unwrap_or_default()
            } else {
                slint::SharedString::new()
            },
            message: self.message.as_str().into(),
            ai_hint: ai_hint(studio).into(),
            quality: self.quality as i32,
            quality_options: ModelRc::new(VecModel::from(
                (0..QUALITY_PRESETS.len())
                    .map(|tier| SharedString::from(quality_name(tier)))
                    .collect::<Vec<_>>(),
            )),
            progress_detail: progress_detail(self, studio).into(),
            estimate: estimate(self, studio).into(),
            done_size: bytes(self.size_bytes(studio)).into(),
            // 一键多版本矩阵：开关、版本数、导出前的计划、导完的清单。
            matrix: self.matrix,
            matrix_count: self.matrix_count() as i32,
            matrix_plan: self.matrix_plan(studio).into(),
            matrix_done: self.matrix_done().into(),
            empty: clips == 0,
        }
    }
}


/// 一键多版本矩阵的一个版本配方：同一个工程变出什么。
///
/// 变体之间只改导出用的工程副本：开头剪掉 `skip` 秒（换一个开头）、
/// 总长截到 `cap` 秒（15/30/60 秒版）、字幕换成 `look` 的样式。
#[derive(Clone, Copy, Debug)]
struct Variant {
    /// 版本名的 i18n key。
    tag: &'static str,
    /// 投放平台的 i18n key，也进文件名。
    platform: &'static str,
    /// 从头剪掉多少秒。
    skip: f64,
    /// 这一版最长多少秒。
    cap: f64,
    /// 字幕/标题的样式。
    look: Look,
}

/// 矩阵里一个版本的字幕样式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Look {
    /// 黄字黑边、加粗：短视频平台的钩子款。
    Hook,
    /// 保持作者做好的样式。
    Original,
    /// 白字 + 半透明黑板：口播、信息流的长句更好读。
    Plate,
}

/// 矩阵的五套配方。默认三版（钩子 / 标准 / 完整），数量可以加到五。
const MATRIX_VARIANTS: [Variant; 5] = [
    Variant {
        tag: "export.matrixHook",
        platform: "export.matrixDouyin",
        skip: 1.5,
        cap: 15.0,
        look: Look::Hook,
    },
    Variant {
        tag: "export.matrixStandard",
        platform: "export.matrixChannels",
        skip: 0.5,
        cap: 30.0,
        look: Look::Original,
    },
    Variant {
        tag: "export.matrixFull",
        platform: "export.matrixBilibili",
        skip: 0.0,
        cap: 60.0,
        look: Look::Plate,
    },
    Variant {
        tag: "export.matrixFast",
        platform: "export.matrixKuaishou",
        skip: 3.0,
        cap: 12.0,
        look: Look::Hook,
    },
    Variant {
        tag: "export.matrixLight",
        platform: "export.matrixXiaohongshu",
        skip: 0.2,
        cap: 20.0,
        look: Look::Plate,
    },
];

/// 一个变体在这个工程上真实的开头与时长：开头剪掉的秒数，和成片秒数。
///
/// 工程太短时 `skip` 自动收住，保证每版至少还剩两秒；比 `cap` 短的工程
/// 不会被拉长。
fn variant_seconds(variant: &Variant, whole: f64) -> (f64, f64) {
    let whole = whole.max(0.1);
    let skip = variant.skip.min((whole - 2.0).max(0.0));
    let seconds = (whole - skip).clamp(0.1, variant.cap.max(0.1));
    (skip, seconds)
}

/// 矩阵里某个版本的成片路径：平台、版本名、真实秒数都在文件名里，导出
/// 完直接按平台上传，不用再改名。
fn matrix_output(folder: &str, name: &str, variant: &Variant, seconds: f64, container: &str) -> String {
    format!(
        "{folder}/{name}-{}-{}-{:.0}s.{container}",
        t(variant.platform),
        t(variant.tag),
        seconds.max(0.1)
    )
}

/// 把一份工程副本改成矩阵里的一个版本：换开头、截时长、换字幕样式。
/// 只碰这一份副本；用户工程、撤销栈、工程文件都不动。
fn shape_variant(project: &mut concat_project::model::Project, variant: &Variant, whole: f64) {
    let (skip, seconds) = variant_seconds(variant, whole);
    retime(project, skip, seconds);
    restyle(project, variant.look);
}

/// 剪掉开头 `skip` 秒、把结束点收到 `seconds`。
///
/// 视频、音频、文字一视同仁：跨过剪切点的片段改入点与长度，整段落在
/// 丢掉的头里的删掉，越过结尾的截短。关键帧是相对片段自身长度的百分比，
/// 片段剪短后仍按比例落在画面上，所以这里不改它。
fn retime(project: &mut concat_project::model::Project, skip: f64, seconds: f64) {
    const EPS: f64 = 1e-6;
    let timeline = project.active_mut();
    let mut kept: Vec<std::sync::Arc<concat_project::model::Clip>> =
        Vec::with_capacity(timeline.clips.len());
    for clip in timeline.clips.drain(..) {
        let mut clip = (*clip).clone();
        if clip.start + clip.duration <= skip + EPS {
            continue;
        }
        if clip.start < skip {
            let cut = skip - clip.start;
            clip.source_start += cut * clip.speed.max(0.01);
            clip.duration -= cut;
            clip.start = 0.0;
            clip.fade_in = clip.fade_in.min(clip.duration);
        } else {
            clip.start -= skip;
        }
        if clip.start >= seconds - EPS {
            continue;
        }
        if clip.start + clip.duration > seconds {
            clip.duration = seconds - clip.start;
            clip.fade_out = clip.fade_out.min(clip.duration);
        }
        // 引擎的下限是六十分之一秒；比这还短的尾巴就不留了。
        if clip.duration < 1.0 / 60.0 {
            continue;
        }
        kept.push(std::sync::Arc::new(clip));
    }
    timeline.clips = kept;
}

/// 给所有文字片段（标题与字幕）换字幕样式；钩子款是黄字黑边并加粗，
/// 底板款是白字 + 半透明黑板，原样款什么都不做。
fn restyle(project: &mut concat_project::model::Project, look: Look) {
    if look == Look::Original {
        return;
    }
    let timeline = project.active_mut();
    for clip in &mut timeline.clips {
        let clip = std::sync::Arc::make_mut(clip);
        let Some(text) = clip.text.as_mut() else {
            continue;
        };
        match look {
            Look::Hook => {
                text.color = "#ffe100".to_owned();
                text.stroke_width = 0.006;
                text.stroke_color = "#000000".to_owned();
                text.shadow = true;
                text.background.clear();
                text.font_size = (text.font_size * 1.12).clamp(0.02, 0.30);
                text.font_weight = text.font_weight.max(800.0);
            }
            Look::Plate => {
                text.color = "#ffffff".to_owned();
                text.stroke_width = 0.0;
                text.shadow = false;
                text.background = "#000000b3".to_owned();
                text.background_radius = 0.012;
                text.background_padding_x = 0.02;
                text.background_padding_y = 0.012;
            }
            Look::Original => {}
        }
    }
}

/// 一个阶段的界面说法，矩阵与单文件导出共用。
fn stage_word(stage: &str) -> String {
    match stage {
        "rendering" => t("export.renderingVideo"),
        concat_export::RENDERING_IN_SOFTWARE => t("export.renderingInSoftware"),
        "mixing audio" => t("export.mixingAudio"),
        "muxing" => t("export.finalisingFile"),
        other => other.to_owned(),
    }
}

/// 矩阵版本字幕样式的界面说法。
fn matrix_look_name(look: Look) -> String {
    t(match look {
        Look::Hook => "export.matrixLookHook",
        Look::Original => "export.matrixLookOriginal",
        Look::Plate => "export.matrixLookPlate",
    })
}

/// 三档质量的编码器 preset（x264 的名字，其它编码器按各自规则映射；见
/// `concat_media` 的 AMF/NVENC/QSV 设置）与码率控制之外的 CRF。快速档
/// 是预览级：芯片的 speed 档 + x264 ultrafast；标准档是平台常用；
/// 高质量档给存档。
const QUALITY_PRESETS: [&str; 3] = ["ultrafast", "veryfast", "medium"];
const QUALITY_CRF: [u8; 3] = [26, EXPORT_CRF, 16];

/// 三档质量的名字，走界面语言。
fn quality_name(tier: usize) -> String {
    match tier {
        0 => t("export.qualityQuick"),
        2 => t("export.qualityHigh"),
        _ => t("export.qualityStandard"),
    }
}

/// 工程里是否用了 AI 做的东西：数字人的成片、AI 配音、AI 生成的图。
/// 这些功能的产物都落在工程目录的 `ai_avatar/`、`ai_voice/`、
/// `ai_image/` 下，导入素材的名字也带着这些词；只看路径与文件名，不
/// 打开任何文件。导出面板据此建议打开 AI 标识，而不是替人打开。
fn ai_material(studio: &Studio) -> bool {
    let root = studio
        .session
        .as_ref()
        .map(|session| session.path().replace('\\', "/"))
        .unwrap_or_default();
    studio.project().media.iter().any(|item| {
        let path = item.path.replace('\\', "/");
        let name = item.name.to_lowercase();
        ["ai_avatar", "ai_voice", "ai_image", "ai-background", "aigen"]
            .iter()
            .any(|folder| path.contains(&format!("/{folder}/")))
            || (root.is_empty() || path.starts_with(&root))
                && [
                    "local-avatar",
                    "avatar-voice",
                    "ai-avatar",
                    "ai_image",
                    "ai-image",
                ]
                .iter()
                .any(|word| name.contains(word))
    })
}

/// 导出面板上的一行建议：工程有 AI 素材、而 AI 标识没开时说一句，
/// 已经开了或没有 AI 素材就什么都不说。
fn ai_hint(studio: &Studio) -> String {
    if studio.prefs.ai_badge.unwrap_or(false) || !ai_material(studio) {
        return String::new();
    }
    t("export.aiHint")
}

/// 导出时那一行细节：编到第几帧、实际帧率、还剩几秒；空闲时是空串。
fn progress_detail(pane: &ExportPane, studio: &Studio) -> String {
    if pane.phase != ExportPhase::Running || pane.progress <= 0.0 {
        return String::new();
    }
    let (num, den) = pane.rate(studio);
    let fps = num as f64 / den.max(1) as f64;
    let total = (f64::from(studio.duration()).max(0.0) * fps).round().max(1.0);
    let elapsed = pane
        .started_at
        .map_or(0.0, |started| started.elapsed().as_secs_f64());
    let done = f64::from(pane.progress) * total;
    let speed = if elapsed > 0.0 { done / elapsed } else { 0.0 };
    let left = if speed > 0.0 {
        (total - done).max(0.0) / speed
    } else {
        0.0
    };
    tf(
        "export.progressDetail",
        &[
            &format!("{}", done.round() as i64),
            &format!("{}", total as i64),
            &format!("{speed:.0}"),
            &format!("{left:.0}"),
        ],
    )
}

/// 开工前的预计耗时：上一回每秒成片花了多少墙钟秒，乘这次的时长。
/// 本机还没导出过就按 1080p 硬编的常见速度先估一个，写着「预计」，
/// 真开始后有实测帧率与剩余时间。
fn estimate(pane: &ExportPane, studio: &Studio) -> String {
    if pane.phase != ExportPhase::Idle {
        return String::new();
    }
    let length = f64::from(studio.duration());
    if length <= 0.0 {
        return String::new();
    }
    // 墙钟秒 / 成片秒：本机 RX 6600 的 AMF 与 x264 veryfast 都落在
    // 0.3–0.6 之间，首次估 0.45。
    let speed = pane.last_speed.unwrap_or(0.45).clamp(0.05, 20.0);
    tf(
        "export.estimateTime",
        &[&format!("{:.0}", (length * speed).max(1.0))],
    )
}

/// Whether `codec` can be written here, as far as is known without
/// opening an encoder on the window's thread, which on a phone is seconds
/// of a window that does not answer. Until the start-up probe has found
/// out (`concat_media::encode::probe_encoders`), H.264 - which every
/// platform writes, a phone through its own software encoder where its
/// chip turns the export away - and whatever needs no encoder opened to
/// know.
fn writable(codec: concat_media::VideoCodec) -> bool {
    codec
        .encodable_known()
        .unwrap_or(codec == concat_media::VideoCodec::H264)
}

/// The typed name as the file's, or `None` for one that cannot be: empty
/// once trimmed, a dot or two, or holding a character no file system
/// here takes. The set is Windows' - a slash and a backslash, the colon,
/// the quote and the wildcards, the angle brackets and the bar - on every
/// platform, so a project's export is named the same wherever it is
/// opened; a control character is refused with them.
fn file_name(typed: &str) -> Option<String> {
    let name = typed.trim();
    if name.is_empty() || name.chars().all(|c| c == '.') {
        return None;
    }
    let refused = |c: char| c.is_control() || r#"/\:*?"<>|"#.contains(c);
    (!name.chars().any(refused)).then(|| name.to_owned())
}

/// 往导出的工程副本里放一枚只有文字的标题片段：右下角的
/// 「AI 生成」角标。导出的副本不写回工程，也不进撤销栈。
///
/// 文字的定位和标题一样走 clip 的 offset：正的 y 往下、正的 x 往右，
/// 单位是画面宽高的比例；字号是画面高度的比例。
fn add_ai_badge(project: &mut concat_project::model::Project, duration: f64) {
    use concat_project::model::{Clip, ClipKind, TextStyle, Track};

    let track_id = "ai-badge-track".to_owned();
    let timeline = project.active_mut();
    timeline.tracks.push(Track {
        id: track_id.clone(),
        ..Track::default()
    });
    let style = TextStyle {
        content: "AI 生成".to_owned(),
        font_family: "Hanken Grotesk".to_owned(),
        font_size: 0.026,
        font_weight: 600.0,
        color: "#ffffff".to_owned(),
        opacity: 0.88,
        background: "#00000066".to_owned(),
        background_radius: 0.008,
        background_padding_x: 0.012,
        background_padding_y: 0.006,
        shadow: true,
        ..TextStyle::default()
    };
    let mut clip = Clip::blank(
        "ai-badge-clip",
        track_id,
        ClipKind::Text,
        "AI 生成",
        0.0,
        duration.max(1.0),
    );
    clip.text = Some(style.tidy());
    clip.offset_x = 0.34;
    clip.offset_y = 0.40;
    timeline.clips.push(std::sync::Arc::new(clip));
}

/// 导出面板的尺寸后来一截平台标注：这段画幅主要发给谁看。和新建工程
/// 的尺寸下拉是同一套说法，选导出尺寸时就知道成片给哪个平台。
fn platform_label(width: u32, height: u32) -> &'static str {
    if height == 0 {
        return "";
    }
    let aspect = f64::from(width) / f64::from(height);
    let near = |target: f64| (aspect - target).abs() < target * 0.02;
    if near(16.0 / 9.0) {
        " · B 站/YouTube 横屏"
    } else if near(9.0 / 16.0) {
        " · 抖音/视频号/快手"
    } else if near(4.0 / 5.0) {
        " · 小红书"
    } else if near(1.0) {
        " · 朋友圈"
    } else if near(4.0 / 3.0) {
        " · 课件/PPT"
    } else if near(21.0 / 9.0) {
        " · 带鱼屏"
    } else {
        ""
    }
}

/// 自定义水印：把设置里那张 PNG 作为一条图片片段算进导出清单。位置按
/// 画面比例算，所以同一份设置在 1080p 与 4K 成片上落在同样的地方；尺寸
/// 是「图宽占画面宽度的百分比」，含等比缩放，长图短图都不会变形。
fn watermark_overlay(studio: &Studio) -> Option<ExportClip> {
    let watermark = studio.prefs.watermark.clone()?.tidy();
    if !watermark.is_on() {
        return None;
    }
    let bytes = std::fs::read(&watermark.path).ok()?;
    let (image_w, image_h) = png_size(&bytes)?;
    let (frame_w, frame_h) = studio.output_size();
    let (scale, offset_x, offset_y) = watermark_geometry(
        (image_w, image_h),
        (frame_w, frame_h),
        watermark.size,
        watermark.position,
    );
    let mut clip = ExportClip::blank(
        ClipKind::Image,
        0.0,
        f64::from(studio.duration()).max(0.1),
        0,
    );
    clip.path = watermark.path.clone();
    // 画面尺寸给了引擎，等比缩放的底子才准。
    clip.media_width = Some(u32::from(image_w.max(1)));
    clip.media_height = Some(u32::from(image_h.max(1)));
    clip.scale = scale;
    clip.offset_x = offset_x;
    clip.offset_y = offset_y;
    clip.opacity = f64::from(watermark.opacity) / 100.0;
    clip.muted = true;
    clip.volume = 0.0;
    clip.has_audio = Some(false);
    log::info!(
        "export: watermark {} · position {} · {:.0}% · {:.2}x",
        crate::studio::file_name_of(&watermark.path),
        watermark.position,
        watermark.opacity,
        scale
    );
    Some(clip)
}

/// 水印在一帧画面里的几何：缩放倍数，以及中心相对画面中心的偏移（画面
/// 宽、高的比例）。尺寸是图宽占画面宽度的百分比，等比缩放；位置留 2%
/// 的边，四角贴边不压线。都是纯数，所以能单测。
fn watermark_geometry(
    image: (u32, u32),
    frame: (u32, u32),
    size: u8,
    position: u8,
) -> (f64, f64, f64) {
    const MARGIN: f64 = 0.02;
    let (frame_w, frame_h) = (f64::from(frame.0.max(1)), f64::from(frame.1.max(1)));
    let image_aspect = f64::from(image.0.max(1)) / f64::from(image.1.max(1));
    let frame_aspect = frame_w / frame_h;
    // 等比缩进画面（contain）：与引擎的 fit 同一套算法，算出来的尺
    // 寸才和实际画上去的一致。
    let (fit_w, fit_h) = if image_aspect >= frame_aspect {
        (frame_w, frame_w / image_aspect)
    } else {
        (frame_h * image_aspect, frame_h)
    };
    let scale = (f64::from(size.clamp(5, 30)) / 100.0 * frame_w) / fit_w;
    let shown_w = fit_w * scale / frame_w;
    let shown_h = fit_h * scale / frame_h;
    let (offset_x, offset_y) = match position {
        0 => (
            -0.5 + shown_w / 2.0 + MARGIN,
            -0.5 + shown_h / 2.0 + MARGIN,
        ),
        1 => (
            0.5 - shown_w / 2.0 - MARGIN,
            -0.5 + shown_h / 2.0 + MARGIN,
        ),
        2 => (
            -0.5 + shown_w / 2.0 + MARGIN,
            0.5 - shown_h / 2.0 - MARGIN,
        ),
        4 => (0.0, 0.0),
        _ => (
            0.5 - shown_w / 2.0 - MARGIN,
            0.5 - shown_h / 2.0 - MARGIN,
        ),
    };
    (scale, offset_x, offset_y)
}

/// 一张 PNG 的宽高与高，从文件头的 IHDR 里读：不解码整张图，导出前
/// 就知道它多大；不是 PNG 就是 None（水印在设置里只收 PNG）。
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(PNG) || bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let wide = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let high = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    (wide > 0 && high > 0).then_some((wide, high))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG's size from the IHDR in its header; None when it is not a PNG.
    #[test]
    fn a_pngs_size_comes_out_of_its_header() {
        // The smallest thing with a PNG signature and an IHDR chunk: the
        // rest may be anything for a size read.
        let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        bytes.extend_from_slice(&1280u32.to_be_bytes());
        bytes.extend_from_slice(&720u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(png_size(&bytes), Some((1280, 720)));
        assert_eq!(png_size(b"not a png"), None);
        assert_eq!(png_size(b""), None);
    }

    /// The watermark's geometry: a tenth of the frame wide, a corner keeps
    /// a margin, and a portrait picture scaled to the same width lands
    /// with the same width - contain, not stretch.
    #[test]
    fn the_watermark_keeps_its_aspect_and_its_corner() {
        let frame = (1920, 1080);
        let (scale, x, y) = watermark_geometry((100, 100), frame, 10, 0);
        // Fit by height, so the scale is a tenth of 1080 over the fitted
        // square: the picture is 108px wide, 10% of the frame's height and
        // 5.6% of its width.
        assert!((scale - 0.1).abs() < 1e-9, "scale {scale}");
        assert!((x - (-0.5 + 0.0281 + 0.02)).abs() < 1e-3, "x {x}");
        assert!((y - (-0.5 + 0.05 + 0.02)).abs() < 1e-3, "y {y}");
        // A wide logo fitted by width: a tenth of the width, its own height.
        let (_, wide_x, wide_y) = watermark_geometry((400, 100), frame, 10, 3);
        assert!((wide_x - (0.5 - 0.05 - 0.02)).abs() < 1e-9, "x {wide_x}");
        assert!(
            (wide_y - (0.5 - (1.0 / 4.0 * 108.0 / 1080.0) / 2.0 - 0.02)).abs() < 1e-6,
            "y {wide_y}"
        );
        // Centre is the centre, whatever the size.
        let (_, x, y) = watermark_geometry((400, 100), frame, 25, 4);
        assert_eq!((x, y), (0.0, 0.0));
    }

    /// A name is a file's or the export does not start: trimmed, never
    /// empty, never a path, and none of the characters Windows refuses.
    #[test]
    fn the_file_name_is_a_file_name() {
        assert_eq!(file_name("  My cut "), Some("My cut".to_owned()));
        assert_eq!(file_name("cut.final"), Some("cut.final".to_owned()));
        assert_eq!(file_name("Ünïcödé 日本"), Some("Ünïcödé 日本".to_owned()));
        for bad in [
            "", "   ", ".", "..", "a/b", r"a\b", "c:d", "a?b", "<a>", "a|b", "a\"b", "a*", "a\tb",
        ] {
            assert_eq!(file_name(bad), None, "{bad:?}");
        }
    }

    /// The timeline's rate leads the list and is not offered twice; the
    /// NTSC fractions keep their camera names.
    #[test]
    fn the_timeline_rate_comes_first() {
        let rates = ExportPane::rates_around((25, 1));
        assert_eq!(rates[0], (25, 1));
        assert_eq!(rates.iter().filter(|rate| **rate == (25, 1)).count(), 1);
        assert_eq!(rates.len(), RATES.len());
        let rates = ExportPane::rates_around((50, 2));
        assert_eq!(rates.len(), RATES.len(), "25 written as 50/2 is still 25");
        let odd = ExportPane::rates_around((15, 1));
        assert_eq!((odd[0], odd.len()), ((15, 1), RATES.len() + 1));
        assert_eq!(ExportPane::rate_name((24000, 1001)), "23.976 fps");
        assert_eq!(ExportPane::rate_name((25, 1)), "25 fps");
        assert_eq!(ExportPane::rate_name((15, 1)), "15 fps");
        assert_eq!(ExportPane::rate_name((12500, 1000)), "12.5 fps");
    }
}
