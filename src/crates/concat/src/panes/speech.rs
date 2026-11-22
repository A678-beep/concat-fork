// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer Abdullah

//! The speech sheet: a title's words, or any words, read aloud.
//!
//! It opens on the selected title's words when a title is selected, else
//! on a blank script to be read at the playhead. The voice is the one
//! chosen last time. The synthesiser runs on a worker and reports as
//! [`SpeechMsg::Progress`], ending as [`SpeechMsg::Finished`] with the
//! written file, which lands in the bin and on the timeline.
//!
//! The model comes first and decides the rest: which voices are offered,
//! and which dials. Chatterbox, on the desktop, reads in a recording at its
//! own pace and knows the expression tags; it can read in a sample voice
//! instead of its stock one: a recording from the bin, chosen by its
//! waveform, from a point in it of the sheet's choosing. The phone's own
//! voice, on Android, reads at a speed of your choosing. The sheet shows
//! each model its own controls rather than the few every model shares.

use std::path::PathBuf;
use std::sync::Arc;

use concat_host::media::{self, MediaSummary};
use concat_host::voices::{self, Heard, SavedVoice};
use concat_project::Command;
use concat_project::commands::ClipPatch;
use concat_project::model::{AppliedFilter, ClipKind, MediaItem, MediaOrigin};
use concat_speech::JobStage;
use concat_host::cloud_ai;
use concat_speech::tts::{
    CHATTERBOX_CLONE, CLOUD_ID, Family, PIPER_ID, Reference, VoiceInfo, family_of, is_clone,
    piper_available, voice_family,
};
use slint::SharedString;

use crate::format::wave_path;
use crate::host::{on_ui_in_project, spawn_in_project};
use crate::i18n::{t, tf};
use crate::panes::Msg;
use crate::panes::captions::CHARS_PER_SECOND;
use crate::panes::pace::{Pace, Stage, Timings};
use crate::panes::settings::installed;
use crate::studio::Studio;
use crate::ui::{NewVoiceData, SpeechSheetData};

/// The default Chatterbox voice: Elizabeth, the first recording Concat
/// carries.
const DEFAULT_CHATTERBOX_VOICE: i32 = 2000;
/// The phone's voice: whichever its engine is set to.
const DEFAULT_SYSTEM_VOICE: i32 = 3000;
/// The speeds a voice can be asked for, as the rate multiplier the sheet's
/// slider runs over; 1 is the voice's own.
const SPEEDS: (f32, f32) = (0.5, 2.0);
/// 一次生成的文字上限。本地中文音色会按句切段后逐段合成，所以 2000 字
/// 也能一次读完；云端同理按段请求。超长时给一句友好的中文提示。
const MAX_READ_CHARS: usize = 2000;

/// 0.2.26 授权合规整改：本地中文音色（Piper 插件）暂停分发，审查完成前
/// 保留入口但标注「暂停分发」并引导改用云端中文音色，点了不静默失败。
const PIPER_SUSPENDED: bool = true;

/// A style a voice can be read in: a few of the audio effects Concat ships,
/// laid on the speech clip once it lands, where they stay as ordinary
/// effects to change or take off. Original looks of the effects alone -
/// no one's voice, no character's name. The Audio page's card carries
/// its row; `name` names it in the interface's language, through a
/// literal `t` so scripts/locales.py finds the key.
pub struct VoiceStyle {
    pub name: fn() -> String,
    pub effects: &'static [&'static str],
}

/// The Audio page's Voices › Styles shelf, in its order.
pub const STYLES: [VoiceStyle; 6] = [
    VoiceStyle {
        name: || t("voices.styleChipmunk"),
        effects: &["concat.chipmunk"],
    },
    VoiceStyle {
        name: || t("voices.styleRobot"),
        effects: &["concat.robot"],
    },
    VoiceStyle {
        name: || t("voices.styleTelephone"),
        effects: &["concat.telephone"],
    },
    VoiceStyle {
        name: || t("voices.styleMegaphone"),
        effects: &["concat.megaphone"],
    },
    VoiceStyle {
        name: || t("voices.styleRadio"),
        effects: &["concat.radio"],
    },
    VoiceStyle {
        name: || t("voices.styleGiant"),
        effects: &["concat.deep", "concat.hall"],
    },
];

/// 文字转语音弹窗里「模型」一行的一个来源：本地录音棚音色（英文）、
/// 本地中文音色（Piper，离线）、云端中文音色（星籁 AI，需登录）。
#[derive(Clone)]
struct Source {
    /// 送给合成器的模型 id：Chatterbox 的模型目录名、PIPER_ID 或 CLOUD_ID。
    model_id: String,
    family: Family,
    label: String,
    detail: String,
    ready: bool,
    megabytes: f32,
}

/// 云端音色的一行。平台 `/voice/list` 返回之前先用常见中文音色垫着，
/// 登录后换成平台真实返回的列表。
#[derive(Clone)]
struct CloudRow {
    pub id: String,
    pub name: String,
    pub detail: String,
}

/// 没有登录 / 列表还没取回来时展示的常见中文音色。只有名字与说明，点生成
/// 会先要求登录；登录后换成平台返回的真实音色。
const CLOUD_FALLBACK: &[(&str, &str)] = &[
    ("女声 · 亲切自然", "温柔亲切，适合门店服务 / 家政"),
    ("男声 · 稳重专业", "沉稳可信，适合检测 / 工程 / 汽车"),
    ("女声 · 活泼种草", "轻快种草，适合美业 / 餐饮 / 电商"),
    ("男声 · 青年", "年轻自然，适合知识科普"),
    ("男声 · 成熟", "成熟稳重，适合房产 / 金融"),
];

/// 这段文字里有汉字：用它在中文/英文之间挑默认音色。
fn has_chinese(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(ch as u32,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
    })
}

/// What the Audio page's Voices shelf opens the sheet on.
#[derive(Clone, Debug)]
pub enum VoiceChoice {
    /// A voice a model names, by its id.
    Builtin(i32),
    /// A saved clone, by its id.
    Saved(String),
    /// A style, by its row of [`STYLES`].
    Style(usize),
}

/// Seconds a model takes to read one second of speech, before a read with
/// it has been timed: about what each took on a recent CPU.
fn reading_pace(family: Family) -> f32 {
    match family {
        Family::System => 0.1,
        // 每秒约 5 个字（实测华言 82 字 15.4 秒 ≈ 5.3 字/秒）。
        Family::Chatterbox => 4.0,
        // 本地中文音色：82 字约 4.5 秒（含分段开销）。
        Family::Piper => 1.1,
        // 云端合成要上传、排队、下载。
        Family::Cloud => 0.5,
    }
}

/// Everything that can happen to the speech sheet.
#[derive(Clone, Debug)]
pub enum SpeechMsg {
    /// The tray's Speak tool, or the menu.
    Open,
    /// The Audio page's Voices shelf: the sheet with that voice or style.
    OpenWith(VoiceChoice),
    /// The name a sample is to be saved under, typed.
    SaveNameEdited(String),
    /// A picture for the voice being saved, from the system's picker.
    PicturePicked(PathBuf),
    /// Keep the sample being read in as a voice of the user's own.
    SaveVoice,
    /// The save's worker is done.
    VoiceSaved(Box<Result<SavedVoice, String>>),
    /// A saved voice deleted, by its id.
    ForgetVoice(String),
    /// A recording picked by the Voices shelf's Import voice: the New
    /// voice sheet on it.
    ImportVoice(PathBuf),
    /// The listen's worker is done: what it heard in that recording.
    VoiceHeard(PathBuf, Box<Result<Heard, String>>),
    /// The New voice sheet's name, its description, and the seconds into
    /// the recording its sample starts at.
    NewVoiceNameEdited(String),
    NewVoiceDescriptionEdited(String),
    NewVoiceStartChanged(f32),
    /// A picture for the new voice, from the system's picker.
    NewVoicePicturePicked(PathBuf),
    /// Keep the new voice.
    NewVoiceSave,
    /// That save's worker is done.
    NewVoiceSaved(Box<Result<SavedVoice, String>>),
    /// The New voice sheet put away without a save.
    NewVoiceClose,
    /// The style taken off: the speech lands plain.
    ClearStyle,
    Close,
    TextEdited(String),
    VoiceChanged(i32),
    ModelChanged(i32),
    /// The rate, `0.5..=2`.
    SpeedChanged(f32),
    /// Seconds into the sample the voice is taken from.
    ReferenceStartChanged(f32),
    /// Read in a sample from the bin rather than a named voice.
    UseSampleChanged(bool),
    /// Which sample: a row of [`SpeechPane::samples`].
    SampleChanged(i32),
    /// Read the script.
    Begin,
    Cancel,
    /// The synthesiser's worker reporting where it is, `0..=1`.
    Progress(JobStage),
    /// The synthesiser's worker is done: the file it wrote, probed, or
    /// why not.
    Finished(Box<Result<MediaSummary, String>>),
    /// 云端音色列表取回来了（或取失败）。
    CloudVoices(Box<Result<cloud_ai::CloudVoices, String>>),
}

/// The Voices shelf's New voice sheet: a recording from the system's
/// files, to be kept as a voice under a name, with a picture and a line
/// about it when given them (#518). It opens on the file's name and, once
/// the recording has been listened to, on the stretch of it most given to
/// speech; see `voices::listen`.
#[derive(Default)]
pub struct NewVoice {
    pub open: bool,
    /// The recording picked.
    source: PathBuf,
    name: String,
    description: String,
    picture: Option<PathBuf>,
    /// The recording's length, once it has been listened to; zero before.
    duration: f64,
    /// Seconds into the recording the kept sample starts at, and where
    /// listening to it put the start.
    start: f64,
    found: f64,
    /// The recording being listened to: the start is not known yet.
    listening: bool,
    /// A save running.
    saving: bool,
}

/// The speech sheet's state.
#[derive(Default)]
pub struct SpeechPane {
    pub open: bool,
    /// The title the words came from, and where the sound lands. None
    /// reads a script of its own at the playhead.
    pub clip: Option<String>,
    pub text: String,
    /// Row in the chosen model's voices; see [`SpeechPane::offered`].
    pub voice: usize,
    /// Row in the installed voice model list.
    pub model: usize,
    /// The family of the chosen model, which is which voices are offered
    /// and which of the dials below are read.
    pub family: Family,
    /// The rate, `0.5..=2`; the phone's voice.
    pub speed: f32,
    /// Reading in a sample from the bin rather than a named voice;
    /// Chatterbox, which reads in a recording.
    pub use_sample: bool,
    /// Row of [`SpeechPane::samples`]: which recording.
    pub sample: usize,
    /// Seconds into the sample the voice is taken from: the first ten
    /// seconds from there are what the model listens to, and the start of
    /// a recording is not always where the speaking is.
    pub reference_start: f64,
    pub running: bool,
    /// How far the running read looks.
    pub pace: Pace,
    /// The model and the length of the running read: what its timing is
    /// kept by.
    reading: Option<(String, f32)>,
    /// How long each model took this session, for the next read's bar.
    timings: Timings,
    pub message: String,
    /// Where the running read will land on the timeline, in seconds.
    landing: f64,
    /// Who can speak, from the voice model on disk.
    pub speakers: Vec<VoiceInfo>,
    /// The voices the user saved, newest first: rows of the voice list
    /// for a model that reads in a recording. See `concat_host::voices`.
    pub saved: Vec<SavedVoice>,
    /// The saved voice being read in, by its row of `saved`, in place of
    /// a named one.
    pub saved_voice: Option<usize>,
    /// The style the speech lands in: a row of [`STYLES`].
    pub style: Option<usize>,
    /// What the sample is to be saved as, and with which picture.
    pub save_name: String,
    pub save_picture: Option<PathBuf>,
    /// A save running.
    pub saving: bool,
    /// The Voices shelf's New voice sheet, which keeps a voice of its own
    /// beside this sheet's.
    pub new_voice: NewVoice,
    /// Each saved voice's picture, read once, by its id: the rows are
    /// published on every frame of playback.
    pictures: std::cell::RefCell<std::collections::HashMap<String, slint::Image>>,
    /// 云端音色：平台返回的列表；没登录 / 没取到时为空，用 CLOUD_FALLBACK。
    cloud_voices: Vec<CloudRow>,
    /// 云端列表已经取过一次（成功或失败）。
    cloud_loaded: bool,
    /// 云端列表正在取。
    cloud_loading: bool,
    /// 云端列表取失败的原因，显示在提示行里。
    cloud_error: String,
    /// 用户手动换过模型：换过之后不再按输入语言自动切。
    model_touched: bool,
}

impl SpeechPane {
    /// Applies one message. The studio is the rest of the window; while
    /// this runs the studio's copy of the pane is a blank it must not read.
    pub fn update(&mut self, msg: SpeechMsg, studio: &mut Studio) {
        match msg {
            // A reading in progress is shown as it is when the panel is
            // asked for again: closing it never stopped the reading.
            SpeechMsg::Open if self.running => self.open = true,
            SpeechMsg::Open => {
                let title = studio
                    .sole_selection()
                    .and_then(|id| studio.clip(&id))
                    .filter(|clip| clip.kind == ClipKind::Text)
                    .cloned();
                let text = title
                    .as_ref()
                    .and_then(|clip| clip.text.as_ref().map(|text| text.content.clone()))
                    .unwrap_or_default();
                // 默认音色按语言走：中文（或空脚本）默认本地中文音色，
                // 英文默认录音棚音色。
                let sources = self.sources(studio);
                let model = Self::language_source(&sources, &text).unwrap_or(0);
                let family = sources
                    .get(model)
                    .map(|source| source.family)
                    .unwrap_or_default();
                *self = SpeechPane {
                    open: true,
                    clip: title.as_ref().map(|clip| clip.id.clone()),
                    text,
                    voice: 0,
                    model,
                    family,
                    speed: 1.0,
                    use_sample: false,
                    sample: 0,
                    reference_start: 0.0,
                    speakers: std::mem::take(&mut self.speakers),
                    timings: std::mem::take(&mut self.timings),
                    saved: std::mem::take(&mut self.saved),
                    new_voice: std::mem::take(&mut self.new_voice),
                    pictures: std::mem::take(&mut self.pictures),
                    cloud_voices: std::mem::take(&mut self.cloud_voices),
                    cloud_loaded: self.cloud_loaded,
                    cloud_loading: self.cloud_loading,
                    cloud_error: std::mem::take(&mut self.cloud_error),
                    ..SpeechPane::default()
                };
                // The voice chosen last time, if the model reads with it;
                // and the selected clip's recording as the sample to start
                // from, since that is the voice most likely wanted.
                let wanted = studio.prefs.tts_voice;
                self.voice = self.voice_row(wanted);
                self.sample = self.default_sample(studio);
                self.ensure_cloud_voices(studio);
            }
            SpeechMsg::OpenWith(choice) => {
                if !self.running {
                    self.update(SpeechMsg::Open, studio);
                    self.choose(choice, studio);
                } else {
                    self.open = true;
                }
            }
            SpeechMsg::SaveNameEdited(name) => self.save_name = name,
            SpeechMsg::PicturePicked(path) => self.save_picture = Some(path),
            SpeechMsg::SaveVoice => self.save_voice(studio),
            SpeechMsg::VoiceSaved(result) => {
                self.saving = false;
                match *result {
                    Ok(voice) => {
                        self.reload_saved(studio);
                        // Read in it from here on: it is the voice that
                        // was being read in, now under its name.
                        self.saved_voice = self.saved.iter().position(|held| held.id == voice.id);
                        if self.saved_voice.is_some() {
                            self.use_sample = false;
                        }
                        self.save_name.clear();
                        self.save_picture = None;
                        studio.notify(&tf("voices.saved", &[&voice.name]), false);
                    }
                    Err(error) => self.message = error,
                }
            }
            SpeechMsg::ForgetVoice(id) => {
                let Some(voice) = self.saved.iter().find(|voice| voice.id == id).cloned() else {
                    return;
                };
                match voices::remove(&voice) {
                    Ok(()) => {
                        self.saved_voice = None;
                        self.reload_saved(studio);
                        studio.notify(&tf("voices.removed", &[&voice.name]), false);
                    }
                    Err(error) => studio.notify(&error, true),
                }
            }
            SpeechMsg::ImportVoice(source) => self.import_voice(source),
            SpeechMsg::VoiceHeard(source, heard) => {
                // An answer about a recording the sheet has since let go
                // of is not the sheet's.
                let sheet = &mut self.new_voice;
                if !sheet.open || sheet.source != source {
                    return;
                }
                sheet.listening = false;
                match *heard {
                    Ok(heard) => {
                        sheet.duration = heard.duration;
                        sheet.start = heard.start;
                        sheet.found = heard.start;
                    }
                    // Nothing to keep a voice from: the sheet goes, and
                    // the notice says why.
                    Err(error) => {
                        self.new_voice = NewVoice::default();
                        studio.notify(&error, true);
                    }
                }
            }
            SpeechMsg::NewVoiceNameEdited(name) => self.new_voice.name = name,
            SpeechMsg::NewVoiceDescriptionEdited(text) => self.new_voice.description = text,
            SpeechMsg::NewVoiceStartChanged(seconds) => {
                let sheet = &mut self.new_voice;
                // Held short of the end, as the speech sheet's start is,
                // so there is always a second of sound left to keep.
                sheet.start = if seconds.is_finite() {
                    f64::from(seconds).clamp(0.0, (sheet.duration - 1.0).max(0.0))
                } else {
                    sheet.found
                }
            }
            SpeechMsg::NewVoicePicturePicked(path) => self.new_voice.picture = Some(path),
            SpeechMsg::NewVoiceSave => self.save_new_voice(studio),
            SpeechMsg::NewVoiceSaved(result) => {
                self.new_voice.saving = false;
                match *result {
                    Ok(voice) => {
                        self.new_voice = NewVoice::default();
                        // The new voice lands at the head of the list, so
                        // the one this sheet reads in is found again by
                        // its id rather than kept by its row.
                        let chosen = self
                            .saved_voice
                            .and_then(|row| self.saved.get(row))
                            .map(|held| held.id.clone());
                        self.reload_saved(studio);
                        self.saved_voice =
                            chosen.and_then(|id| self.saved.iter().position(|held| held.id == id));
                        studio.notify(&tf("voices.saved", &[&voice.name]), false);
                    }
                    Err(error) => studio.notify(&error, true),
                }
            }
            SpeechMsg::NewVoiceClose => {
                if !self.new_voice.saving {
                    self.new_voice = NewVoice::default();
                }
            }
            SpeechMsg::ClearStyle => self.style = None,
            SpeechMsg::CloudVoices(result) => {
                self.cloud_loading = false;
                match *result {
                    Ok(cloud) => {
                        let rows: Vec<CloudRow> = cloud
                            .voices
                            .into_iter()
                            .filter(|voice| voice.ready && !voice.cloned)
                            .map(|voice| CloudRow {
                                id: voice.id.clone(),
                                name: if voice.name.trim().is_empty() {
                                    voice.id.clone()
                                } else {
                                    voice.name.clone()
                                },
                                detail: if voice.desc.trim().is_empty() {
                                    voice.engine.clone()
                                } else {
                                    format!("{} · {}", voice.desc.trim(), voice.engine)
                                },
                            })
                            .collect();
                        if !rows.is_empty() {
                            self.cloud_voices = rows;
                        }
                        self.cloud_loaded = true;
                        self.cloud_error.clear();
                    }
                    Err(error) => {
                        cloud_ai::forget_if_expired(&error);
                        self.cloud_error = error;
                    }
                }
            }
            SpeechMsg::Close => self.open = false,
            SpeechMsg::TextEdited(text) => {
                self.text = text;
                // 用户没亲手挑过模型时跟着输入语言换：中文默认本地中文
                // 音色，英文默认录音棚音色；挑过之后按用户挑的来。
                if !self.model_touched {
                    let sources = self.sources(studio);
                    if let Some(row) = Self::language_source(&sources, &self.text) {
                        if sources[row].family != self.family {
                            self.model = row;
                            self.family = sources[row].family;
                            self.voice = self.voice_row(None);
                            self.saved_voice = None;
                            self.use_sample = false;
                            self.ensure_cloud_voices(studio);
                        }
                    }
                }
            }
            // The row past the named voices is "Clone from a sample…":
            // the sample switch, turned on from where a voice is chosen.
            // Past them, the saved voices, for a model that reads in one.
            SpeechMsg::VoiceChanged(index) => {
                let index = index.max(0) as usize;
                if self.family == Family::Cloud {
                    let count = self.cloud_rows().len();
                    self.voice = index.min(count.saturating_sub(1));
                    return;
                }
                let named = self.offered().len();
                let saved = self.saved_offered();
                if index < named {
                    self.voice = index;
                    self.saved_voice = None;
                } else if index < named + saved {
                    self.saved_voice = Some(index - named);
                } else if self.sample_voice().is_some() {
                    self.use_sample = true;
                }
            }
            SpeechMsg::ModelChanged(index) => {
                let sources = self.sources(studio);
                self.model = (index.max(0) as usize).min(sources.len().saturating_sub(1));
                // Another model may read with other voices: the list is
                // rebuilt, and the row points at a voice on it.
                let chosen = self.offered().get(self.voice).map(|voice| voice.id);
                self.family = sources
                    .get(self.model)
                    .map(|source| source.family)
                    .unwrap_or_default();
                self.voice = self.voice_row(chosen);
                self.model_touched = true;
                self.use_sample = false;
                if self.sample_voice().is_none() {
                    self.saved_voice = None;
                }
                self.ensure_cloud_voices(studio);
            }
            SpeechMsg::SpeedChanged(speed) => {
                self.speed = if speed.is_finite() {
                    speed.clamp(SPEEDS.0, SPEEDS.1)
                } else {
                    1.0
                }
            }
            SpeechMsg::ReferenceStartChanged(seconds) => {
                self.reference_start = if seconds.is_finite() {
                    f64::from(seconds.max(0.0))
                } else {
                    0.0
                }
            }
            SpeechMsg::UseSampleChanged(on) => self.use_sample = on,
            SpeechMsg::SampleChanged(index) => {
                self.sample = index.max(0) as usize;
                self.reference_start = 0.0;
            }
            SpeechMsg::Begin => self.run(studio),
            SpeechMsg::Cancel => {
                studio.host.speech.cancel();
                self.running = false;
                self.open = false;
            }
            SpeechMsg::Progress(stage) => self.pace.report(stage),
            SpeechMsg::Finished(result) => {
                self.running = false;
                if let (Ok(_), Some((model, chars))) = (result.as_ref(), self.reading.take()) {
                    self.timings.keep(&model, self.pace.learned(chars));
                }
                match *result {
                    Ok(summary) => {
                        // Marked as read aloud: the bin shelves it under
                        // Generated › Speech, apart from the imports.
                        let mut item = summary.to_new_media();
                        item.origin = Some(MediaOrigin::Speech);
                        let created = studio.apply(Command::AddMedia { item });
                        let media_id = created.or_else(|| {
                            studio
                                .project()
                                .media
                                .iter()
                                .find(|item| item.path == summary.path)
                                .map(|item| item.id.clone())
                        });
                        self.open = false;
                        if let Some(media_id) = media_id {
                            let clip = studio.apply(Command::AddClipAtFirstFree {
                                media_id,
                                start: self.landing,
                            });
                            // In a style: its effects on the clip, as the
                            // library would put them there.
                            if let (Some(clip_id), Some(style)) =
                                (clip, self.style.and_then(|row| STYLES.get(row)))
                            {
                                studio.apply(Command::UpdateClip {
                                    clip_id,
                                    patch: ClipPatch {
                                        filters: Some(
                                            style
                                                .effects
                                                .iter()
                                                .map(|id| AppliedFilter::new(*id))
                                                .collect(),
                                        ),
                                        ..ClipPatch::default()
                                    },
                                });
                            }
                            studio.notify(&t("speech.voiceAddedTimeline"), false);
                        }
                    }
                    // Asked for: the sheet is already on its way down.
                    Err(error) if error.contains("cancel") => self.open = false,
                    // Closed while it ran: the panel is not there to say
                    // it, so the notice does, and the panel too next time.
                    Err(error) => {
                        if !self.open {
                            studio.notify(&error, true);
                        }
                        self.message = error;
                    }
                }
            }
        }
    }

    /// Reads the script on a worker: the WAV lands in the bin and on the
    /// timeline, at the title's start or at the playhead.
    fn run(&mut self, studio: &mut Studio) {
        let text = self.text.trim().to_owned();
        if text.is_empty() {
            self.message = t("speech.nothingToReadYet");
            return;
        }
        if text.chars().count() > MAX_READ_CHARS {
            self.message = tf(
                "speech.textTooLong",
                &[&format!("{}", MAX_READ_CHARS), &format!("{}", text.chars().count())],
            );
            return;
        }
        // 云端音色是另一条链路：上传文字、平台合成、下载音频。它的结果也
        // 走同一个 Finished，落地成时间线上的片段。
        if self.family == Family::Cloud {
            self.run_cloud(studio);
            return;
        }
        if PIPER_SUSPENDED && self.family == Family::Piper {
            // 授权合规审查：本地中文音色暂停分发，明确告诉人改用云端，
            // 不走到「模型没装」那种含糊的失败里。
            self.message = t("speech.piperSuspendedNote");
            return;
        }
        let sources = self.sources(studio);
        let Some(source) = sources.get(self.model).cloned() else {
            self.message = t("speech.downloadVoiceModelFirst");
            return;
        };
        if !source.ready {
            self.message = if source.family == Family::Piper {
                t("speech.piperNotInstalled")
            } else {
                t("speech.downloadVoiceModelFirst")
            };
            return;
        }
        let model = source.model_id.clone();
        let megabytes = source.megabytes;
        // A sample voice is a recording of the sheet's choosing, handed over
        // as the request's reference under the family's own id for one.
        // Otherwise the voice is one the model names.
        let saved = self
            .saved_voice
            .filter(|_| !self.use_sample)
            .and_then(|row| self.saved.get(row));
        let (voice, reference) = if self.family == Family::Piper {
            // 本地中文音色没有样本/克隆：直接用它自己的普通话女声。
            let Some(voice) = self.offered().get(self.voice).map(|speaker| speaker.id) else {
                self.message = t("speech.noVoiceRead");
                return;
            };
            (voice, None)
        } else if let Some(saved) = saved {
            let Some(voice) = self.sample_voice() else {
                self.message = t("speech.needsCloningModel");
                return;
            };
            (
                voice,
                Some(Reference {
                    path: saved.sample().to_string_lossy().into_owned(),
                    start: 0.0,
                }),
            )
        } else if self.use_sample {
            let Some(voice) = self.sample_voice() else {
                self.message = t("speech.modelReadsOnlyOwn");
                return;
            };
            let Some(sample) = self.sample_media(studio).cloned() else {
                self.message = t("speech.pickSampleVoice");
                return;
            };
            let start = self.reference_offset(sample.duration.unwrap_or(0.0));
            (
                voice,
                Some(Reference {
                    path: sample.path,
                    start,
                }),
            )
        } else {
            let Some(voice) = self.offered().get(self.voice).map(|speaker| speaker.id) else {
                self.message = t("speech.noVoiceRead");
                return;
            };
            (voice, None)
        };
        let Some(project) = studio
            .session
            .as_ref()
            .map(|session| session.path().to_owned())
        else {
            return;
        };
        // Remembered: the voice chosen is the voice wanted next time. A
        // sample is a recording in one project, and is not.
        if !self.use_sample && saved.is_none() {
            studio.prefs.tts_voice = Some(voice);
            studio.prefs.save(&studio.host.dirs);
        }
        self.landing = self
            .clip
            .as_ref()
            .and_then(|id| studio.clip(id))
            .map(|clip| clip.start)
            .unwrap_or(f64::from(studio.playhead));
        // The read as the bar lays it out: the model loaded unless it
        // still is, the recording the voice comes from read in (the
        // phone's voice has none), and the reading at the pace the last
        // read with this model measured, or the model's usual pace.
        let chars = text.chars().count() as f32;
        let learned = self.timings.get(&model);
        let loaded = self.family != Family::Piper
            && studio.host.speech.loaded_model().as_deref() == Some(model.as_str());
        let load = if loaded || self.family == Family::Piper {
            0.0
        } else {
            learned.load.unwrap_or(0.5 + megabytes / 150.0)
        };
        let per_char = learned
            .per_unit
            .unwrap_or(reading_pace(self.family) / CHARS_PER_SECOND / self.rate());
        let mut plan = Vec::new();
        if self.family != Family::Piper {
            plan.push((Stage::Loading, load));
        }
        if self.family == Family::Chatterbox {
            plan.push((Stage::Reading, 0.3));
        }
        plan.push((Stage::Working, chars * per_char));
        self.pace = Pace::new(plan);
        self.reading = Some((model.clone(), chars));
        let request = concat_speech::tts::SpeakRequest {
            model_id: model,
            voice,
            reference,
            text,
            speed: self.rate(),
            project,
        };
        let dirs = studio.host.dirs.clone();
        let speech = Arc::clone(&studio.host.speech);
        self.running = true;
        self.message.clear();
        let epoch = crate::host::project_epoch();
        spawn_in_project(
            move || {
                let spoken = speech.speak(&dirs, &request, move |stage| {
                    on_ui_in_project(epoch, move |studio, _, _| {
                        studio.handle(Msg::Speech(SpeechMsg::Progress(stage)));
                    });
                })?;
                let summary = media::probe(&spoken.path)?;
                Ok::<_, String>(summary)
            },
            |studio, _, _, result| {
                studio.handle(Msg::Speech(SpeechMsg::Finished(Box::new(result))))
            },
        );
    }


    /// 弹窗能选的音色来源，按「本地录音棚音色 → 本地中文音色 → 云端音色」
    /// 排。Chatterbox 只有在模型已下载时出现；Piper 插件没装时也留一行，
    /// 选中后会告诉人去 AI 面板安装。
    fn sources(&self, studio: &Studio) -> Vec<Source> {
        let mut out = Vec::new();
        for model in installed(&studio.settings.voices) {
            match family_of(&model.id) {
                Family::Chatterbox => out.push(Source {
                    model_id: model.id.clone(),
                    family: Family::Chatterbox,
                    label: t("speech.modelChatterbox"),
                    detail: t("speech.modelChatterboxDetail"),
                    ready: true,
                    megabytes: model.megabytes,
                }),
                Family::System => out.push(Source {
                    model_id: model.id.clone(),
                    family: Family::System,
                    label: t("speech.modelSystem"),
                    detail: t("speech.modelSystemDetail"),
                    ready: true,
                    megabytes: model.megabytes,
                }),
                Family::Piper | Family::Cloud => {}
            }
        }
        let piper_ready = !PIPER_SUSPENDED && piper_available(&studio.host.dirs);
        out.push(Source {
            model_id: PIPER_ID.to_owned(),
            family: Family::Piper,
            label: t("speech.modelPiper"),
            detail: if PIPER_SUSPENDED {
                t("speech.piperSuspended")
            } else if piper_ready {
                t("speech.modelPiperDetail")
            } else {
                t("speech.modelPiperMissing")
            },
            ready: piper_ready,
            megabytes: 0.0,
        });
        out.push(Source {
            model_id: CLOUD_ID.to_owned(),
            family: Family::Cloud,
            label: t("speech.modelCloud"),
            detail: if studio.account.is_some() {
                t("speech.modelCloudDetail")
            } else {
                t("speech.modelCloudLogin")
            },
            ready: studio.account.is_some(),
            megabytes: 0.0,
        });
        out
    }

    /// 按语言挑默认来源：中文或空脚本 → 本地中文音色；英文 → 录音棚音色。
    /// 首选没就绪时退到另一个；云端放最后。
    fn language_source(sources: &[Source], text: &str) -> Option<usize> {
        let chinese = text.is_empty() || has_chinese(text);
        let pick = |family: Family| {
            sources
                .iter()
                .position(|source| source.family == family && source.ready)
        };
        if chinese {
            pick(Family::Piper)
                .or_else(|| pick(Family::Chatterbox))
                .or_else(|| pick(Family::System))
                .or_else(|| pick(Family::Cloud))
                .or_else(|| sources.iter().position(|_| true))
        } else {
            pick(Family::Chatterbox)
                .or_else(|| pick(Family::Piper))
                .or_else(|| pick(Family::System))
                .or_else(|| pick(Family::Cloud))
                .or_else(|| sources.iter().position(|_| true))
        }
    }

    /// 云端音色的行：平台列表取到了就用它，没有就先用常见中文音色垫着，
    /// 并注明要登录。
    fn cloud_rows(&self) -> Vec<CloudRow> {
        if self.cloud_voices.is_empty() {
            CLOUD_FALLBACK
                .iter()
                .map(|(name, detail)| CloudRow {
                    id: String::new(),
                    name: (*name).to_owned(),
                    detail: format!("{detail} · {}", t("speech.cloudNeedsLogin")),
                })
                .collect()
        } else {
            self.cloud_voices.clone()
        }
    }

    /// 登录着就把云端音色列表取回来，一次。没登录就等用户登录后再取。
    fn ensure_cloud_voices(&mut self, studio: &Studio) {
        if self.cloud_loaded || self.cloud_loading {
            return;
        }
        let Some(account) = studio.account.as_ref() else {
            return;
        };
        if account.token.trim().is_empty() {
            return;
        }
        self.cloud_loading = true;
        crate::host::spawn(
            move || cloud_ai::token().and_then(|token| cloud_ai::list_voices(&token)),
            |studio, _, _, result| {
                studio.handle(Msg::Speech(SpeechMsg::CloudVoices(Box::new(result))));
            },
        );
    }

    /// 云端配音：平台合成 mp3 → 下载进工程的 `ai_voice/` → 走同一个
    /// Finished 落地成媒体与片段。admin 免扣由平台按账号处理，桌面端只展示
    /// 平台返回的提示。
    fn run_cloud(&mut self, studio: &mut Studio) {
        let text = self.text.trim().to_owned();
        if self.family != Family::Cloud {
            return;
        }
        let Ok(_) = cloud_ai::token() else {
            self.message = t("speech.cloudLoginFirst");
            return;
        };
        let rows = self.cloud_rows();
        let Some(row) = rows.get(self.voice.min(rows.len().saturating_sub(1))).cloned() else {
            self.message = t("speech.cloudNoVoice");
            return;
        };
        if row.id.trim().is_empty() {
            self.message = t("speech.cloudLoginFirst");
            return;
        }
        let Some(project) = studio
            .session
            .as_ref()
            .map(|session| session.path().to_owned())
        else {
            return;
        };
        self.landing = self
            .clip
            .as_ref()
            .and_then(|id| studio.clip(id))
            .map(|clip| clip.start)
            .unwrap_or(f64::from(studio.playhead));
        let chars = text.chars().count() as f32;
        self.pace = Pace::new(vec![(Stage::Working, chars / CHARS_PER_SECOND * 0.6)]);
        self.reading = Some((format!("cloud:{}", row.id), chars));
        self.running = true;
        self.message.clear();
        let rate = self.speed;
        let voice_id = row.id.clone();
        let epoch = crate::host::project_epoch();
        spawn_in_project(
            move || -> Result<MediaSummary, String> {
                let token = cloud_ai::token()?;
                let tts = cloud_ai::tts(&token, &text, &voice_id, rate)?;
                let dir = std::path::PathBuf::from(&project).join("ai_voice");
                std::fs::create_dir_all(&dir)
                    .map_err(|error| format!("建不了配音目录：{error}"))?;
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_millis())
                    .unwrap_or(0);
                let out = dir.join(format!("cloud-voice-{stamp}.mp3"));
                let no_cancel = std::sync::atomic::AtomicBool::new(false);
                concat_host::direct::download(&tts.url, &out, &mut |_, _| {}, &no_cancel)?;
                media::probe(&out.to_string_lossy())
            },
            move |studio, _, _, result| {
                let _ = epoch;
                studio.handle(Msg::Speech(SpeechMsg::Finished(Box::new(result))))
            },
        );
    }

    /// What the sheet's script would take to say, for the line under it.
    fn estimate(&self, studio: &Studio) -> String {
        let chars = self.text.trim().chars().count();
        if chars == 0 {
            return t("speech.nothingToReadYet");
        }
        let spoken_per_second = match self.family {
            Family::System => 8.0,
            Family::Chatterbox | Family::Piper | Family::Cloud => 5.0,
        };
        let seconds = chars as f32 / spoken_per_second / self.rate();
        let whole = seconds.round() as i32;
        let voice = if self.use_sample {
            self.sample_media(studio)
                .map(|sample| sample.name.clone())
                .unwrap_or_default()
        } else if let Some(saved) = self.saved_voice.and_then(|row| self.saved.get(row)) {
            saved.name.clone()
        } else if self.family == Family::Cloud {
            self.cloud_rows()
                .get(self.voice)
                .map(|row| row.name.clone())
                .unwrap_or_default()
        } else {
            self.offered()
                .get(self.voice)
                .map(|speaker| voice_label(&speaker.name).0)
                .unwrap_or_default()
        };
        tf(
            "speech.aboutInCharacters",
            &[&(whole / 60), &format!("{:02}", whole % 60), &voice, &chars],
        )
    }

    /// Whether the chosen model reads at a speed of the sheet's choosing.
    /// Chatterbox does not: it reads at its own pace, and the sheet does
    /// not offer what the engine would ignore.
    pub fn reads_at_a_pace(&self) -> bool {
        self.family != Family::Chatterbox
    }

    /// The rate the request carries: the slider's, or 1 for a model that
    /// reads at its own.
    fn rate(&self) -> f32 {
        if self.reads_at_a_pace() {
            self.speed
        } else {
            1.0
        }
    }

    /// The id under which the chosen model reads in a recording of the
    /// sheet's choosing; None for a model that reads only with its own
    /// speakers.
    fn sample_voice(&self) -> Option<i32> {
        match self.family {
            Family::System => None,
            Family::Chatterbox => Some(CHATTERBOX_CLONE),
            Family::Piper | Family::Cloud => None,
        }
    }

    /// The recordings a voice can be taken from: everything in the bin with
    /// sound in it, in the bin's order.
    pub fn samples<'a>(&self, studio: &'a Studio) -> Vec<&'a MediaItem> {
        studio
            .project()
            .media
            .iter()
            .filter(|item| item.has_audio)
            .collect()
    }

    /// The chosen sample.
    fn sample_media<'a>(&self, studio: &'a Studio) -> Option<&'a MediaItem> {
        self.samples(studio).get(self.sample).copied()
    }

    /// The row to open the sample list on: the selected clip's recording,
    /// when one clip with sound in it is selected, else the first.
    fn default_sample(&self, studio: &Studio) -> usize {
        studio
            .sole_selection()
            .and_then(|id| studio.clip(&id))
            .filter(|clip| clip.kind == ClipKind::Video || clip.kind == ClipKind::Audio)
            .and_then(|clip| {
                self.samples(studio)
                    .iter()
                    .position(|item| item.id == clip.media_id)
            })
            .unwrap_or(0)
    }

    /// How far into a recording of `duration` the voice is taken from: what
    /// the sheet asked for, held short of the end so there is always at
    /// least a second of sound left to listen to.
    fn reference_offset(&self, duration: f64) -> f64 {
        self.reference_start.clamp(0.0, (duration - 1.0).max(0.0))
    }

    /// The voices the chosen model reads with, in table order.
    pub fn offered(&self) -> Vec<&VoiceInfo> {
        // The family's clone id is not a voice to pick from a list: it is
        // what a sample is sent under, and the sample switch offers it.
        self.speakers
            .iter()
            .filter(|speaker| speaker.family == self.family && !is_clone(speaker.id))
            .collect()
    }

    /// The row of `wanted` among the offered voices, or of the family's
    /// default, or the first.
    fn voice_row(&self, wanted: Option<i32>) -> usize {
        let offered = self.offered();
        let default = match self.family {
            Family::System => DEFAULT_SYSTEM_VOICE,
            Family::Chatterbox => DEFAULT_CHATTERBOX_VOICE,
            Family::Piper => 4000,
            Family::Cloud => 0,
        };
        wanted
            .and_then(|id| offered.iter().position(|voice| voice.id == id))
            .or_else(|| offered.iter().position(|voice| voice.id == default))
            .unwrap_or(0)
    }

    /// How many saved voices the voice list offers: all of them for a
    /// model that reads in a recording, none for one that does not.
    fn saved_offered(&self) -> usize {
        if self.clone_row() {
            self.saved.len()
        } else {
            0
        }
    }

    /// The saved voices the list offers.
    fn saved_rows(&self) -> &[SavedVoice] {
        &self.saved[..self.saved_offered()]
    }

    /// The voice list's rows: each offered voice's name, then the user's
    /// saved voices, and last, for a model that reads in a recording, the
    /// row that clones one.
    pub fn speaker_rows(&self) -> Vec<SharedString> {
        if self.family == Family::Cloud {
            return self
                .cloud_rows()
                .iter()
                .map(|row| row.name.as_str().into())
                .collect();
        }
        self.offered()
            .iter()
            .map(|speaker| voice_label(&speaker.name).0.into())
            .chain(
                self.saved_rows()
                    .iter()
                    .map(|voice| voice.name.as_str().into()),
            )
            .chain(self.clone_row().then(|| t("speech.cloneFromSample").into()))
            .collect()
    }

    /// The voice list's second lines: each voice's accent and gender, or
    /// where a Pocket voice comes from.
    pub fn speaker_detail_rows(&self) -> Vec<SharedString> {
        if self.family == Family::Cloud {
            return self
                .cloud_rows()
                .iter()
                .map(|row| row.detail.as_str().into())
                .collect();
        }
        self.offered()
            .iter()
            .map(|speaker| voice_label(&speaker.name).1.into())
            .chain(
                self.saved_rows()
                    .iter()
                    .map(|_| t("speech.savedVoice").into()),
            )
            .chain(
                self.clone_row()
                    .then(|| t("speech.fromRecordingInBin").into()),
            )
            .collect()
    }

    /// The voice list's avatars: a voice's picture where it has one, else
    /// the first letter of its name in the colour its name picks; and a
    /// plain mark for the clone row, which is nobody yet.
    pub fn speaker_avatar_rows(&self) -> Vec<crate::ui::AvatarData> {
        if self.family == Family::Cloud {
            return self
                .cloud_rows()
                .iter()
                .map(|row| {
                    let letter = avatar_letter(&row.name);
                    crate::ui::AvatarData {
                        hue: avatar_hue(&row.name),
                        letter: letter.into(),
                        ..Default::default()
                    }
                })
                .collect();
        }
        self.offered()
            .iter()
            .map(|speaker| {
                let name = voice_label(&speaker.name).0;
                let picture = speaker
                    .avatar
                    .as_deref()
                    .and_then(|path| slint::Image::load_from_path(path.as_ref()).ok());
                crate::ui::AvatarData {
                    letter: avatar_letter(&name).into(),
                    hue: avatar_hue(&name),
                    pictured: picture.is_some(),
                    picture: picture.unwrap_or_default(),
                }
            })
            .chain(
                self.saved_rows()
                    .iter()
                    .map(|voice| self.saved_avatar(voice)),
            )
            .chain(self.clone_row().then(|| crate::ui::AvatarData {
                letter: "+".into(),
                hue: -1,
                ..Default::default()
            }))
            .collect()
    }

    /// Whether the voice list ends in "Clone from a sample…": the model
    /// reads in a recording, so one can stand in for a named voice.
    fn clone_row(&self) -> bool {
        self.sample_voice().is_some()
    }

    /// The model list: the voice sources this dialog offers.
    pub fn model_rows(&self, studio: &Studio) -> Vec<SharedString> {
        self.sources(studio)
            .iter()
            .map(|source| SharedString::from(source.label.as_str()))
            .collect()
    }

    /// The model list's second lines: what each source is for, and whether
    /// it needs the network or the login.
    pub fn model_detail_rows(&self, studio: &Studio) -> Vec<SharedString> {
        self.sources(studio)
            .iter()
            .map(|source| SharedString::from(source.detail.as_str()))
            .collect()
    }

    /// The sample list's rows: each recording's name.
    pub fn sample_rows(&self, studio: &Studio) -> Vec<SharedString> {
        self.samples(studio)
            .iter()
            .map(|item| item.name.as_str().into())
            .collect()
    }

    /// The sample list's pictures: each recording's envelope as path
    /// commands, the same drawing the bin's card and the timeline's clip
    /// make of it, or empty while the peaks have not been read yet.
    pub fn sample_wave_rows(&self, studio: &Studio) -> Vec<SharedString> {
        self.samples(studio)
            .iter()
            .map(|item| match studio.peaks.get(&item.id) {
                Some(peaks) => {
                    wave_path(peaks, 0.0, item.duration.unwrap_or(0.0) as f32, 64, 0.75).into()
                }
                None => SharedString::new(),
            })
            .collect()
    }

    /// The sample list's second column: how long each recording is.
    pub fn sample_detail_rows(&self, studio: &Studio) -> Vec<SharedString> {
        self.samples(studio)
            .iter()
            .map(|item| {
                let whole = item.duration.unwrap_or(0.0).round() as i64;
                format!("{}:{:02}", whole / 60, whole % 60).into()
            })
            .collect()
    }

    /// The sheet as Slint shows it.
    pub fn data(&self, studio: &Studio) -> SpeechSheetData {
        let sample = self.sample_media(studio);
        let length = sample.and_then(|item| item.duration).unwrap_or(0.0);
        let named = self.offered().len();
        let cloud = self.cloud_rows().len();
        let sources = self.sources(studio);
        let ready = sources.get(self.model).is_some_and(|source| source.ready);
        let cloud_note = if self.family == Family::Cloud {
            let mut note = if studio.account.is_some() {
                t("speech.cloudNote")
            } else {
                t("speech.cloudLoginNote")
            };
            if !self.cloud_error.is_empty() {
                note.push_str("（");
                note.push_str(&self.cloud_error);
                note.push('）');
            }
            note
        } else {
            String::new()
        };
        let local_note = if self.family == Family::Piper {
            if PIPER_SUSPENDED {
                t("speech.piperSuspendedNote")
            } else {
                t("speech.localNote")
            }
        } else {
            String::new()
        };
        SpeechSheetData {
            open: self.open,
            text: self.text.as_str().into(),
            voice: if self.family == Family::Cloud {
                self.voice.min(cloud.saturating_sub(1)) as i32
            } else {
                self.saved_voice
                    .filter(|row| *row < self.saved_offered())
                    .map_or(self.voice, |row| named + row) as i32
            },
            voice_count: if self.family == Family::Cloud {
                cloud as i32
            } else {
                (named + self.saved_offered()) as i32
            },
            style: self
                .style
                .and_then(|row| STYLES.get(row))
                .map(|style| (style.name)())
                .unwrap_or_default()
                .into(),
            save_name: self.save_name.as_str().into(),
            save_picture: self
                .save_picture
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
            saving: self.saving,
            model: self.model as i32,
            family: match self.family {
                Family::System => 0,
                Family::Piper => 1,
                Family::Chatterbox => 2,
                Family::Cloud => 3,
            },
            speed: self.speed,
            samples_offered: self.sample_voice().is_some(),
            use_sample: self.use_sample,
            sample: self.sample as i32,
            sample_name: sample
                .map(|item| item.name.as_str())
                .unwrap_or_default()
                .into(),
            reference_length: length as f32,
            reference_start: self.reference_offset(length) as f32,
            running: self.running,
            progress: self.pace.floor,
            ceiling: self.pace.ceiling,
            step: self.pace.step,
            left: self.pace.left,
            stage: match self.pace.stage() {
                Some(Stage::Reading) => t("speech.readingTheSample"),
                Some(Stage::Working) => t("speech.generatingVoice"),
                _ => t("speech.loadingTheModel"),
            }
            .into(),
            ready,
            estimate: self.estimate(studio).into(),
            message: self.message.as_str().into(),
            cloud_note: cloud_note.into(),
            local_note: local_note.into(),
        }
    }
}

/// `tag` written into `text` at byte offset `at` - the caret, as the script
/// box reports it - with a space on whichever side is against a word, so
/// `[laugh]` lands as a word of its own and never as the tail of one. An
/// offset inside a character steps back to the character's start; one past
/// the end is the end. A trailing space is always written when the tag is
/// not already followed by one, so the caller can put the caret after it.
pub fn insert_tag(text: &str, tag: &str, at: usize) -> String {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let (before, after) = text.split_at(at);
    let lead = if before.is_empty() || before.ends_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    let trail = if after.starts_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    format!("{before}{lead}{tag}{trail}{after}")
}

impl SpeechPane {
    /// The sheet, closed, with the voices saved on this machine.
    pub fn with_saved(dirs: &concat_host::AppDirs) -> SpeechPane {
        SpeechPane {
            saved: voices::list(&voices::dir_in(dirs)),
            ..SpeechPane::default()
        }
    }

    /// The saved voices, read from disk again.
    pub fn reload_saved(&mut self, studio: &Studio) {
        self.saved = voices::list(&voices::dir_in(&studio.host.dirs));
        self.pictures.borrow_mut().clear();
        if self.saved_voice.is_some_and(|row| row >= self.saved.len()) {
            self.saved_voice = None;
        }
    }

    /// The sheet, just opened, on what the Audio page's card names.
    fn choose(&mut self, choice: VoiceChoice, studio: &Studio) {
        let sources = self.sources(studio);
        let pick_family = |pane: &mut SpeechPane, family: Family| -> bool {
            match sources.iter().position(|source| source.family == family) {
                Some(row) => {
                    pane.model = row;
                    pane.family = family;
                    pane.model_touched = true;
                    true
                }
                None => false,
            }
        };
        match choice {
            VoiceChoice::Builtin(id) => {
                let Some(family) = voice_family(id) else {
                    return;
                };
                if self.family != family && !pick_family(self, family) {
                    self.message = t("speech.downloadModelForVoice");
                    return;
                }
                self.voice = self.voice_row(Some(id));
            }
            VoiceChoice::Saved(id) => {
                if self.sample_voice().is_none() && !pick_family(self, Family::Chatterbox) {
                    self.message = t("speech.needsCloningModel");
                    return;
                }
                self.saved_voice = self.saved.iter().position(|voice| voice.id == id);
                self.use_sample = false;
            }
            VoiceChoice::Style(row) => self.style = (row < STYLES.len()).then_some(row),
        }
    }

    /// Keeps the sample being read in as a voice of the user's own, on a
    /// worker: a few seconds of it from where the sheet listens.
    fn save_voice(&mut self, studio: &mut Studio) {
        let name = self.save_name.trim().to_owned();
        if name.is_empty() || self.saving {
            return;
        }
        let Some(sample) = self.sample_media(studio).cloned() else {
            self.message = t("speech.pickSampleVoice");
            return;
        };
        let start = self.reference_offset(sample.duration.unwrap_or(0.0));
        let picture = self.save_picture.clone();
        let dir = voices::dir_in(&studio.host.dirs);
        self.saving = true;
        self.message.clear();
        crate::host::spawn(
            move || {
                voices::save(
                    &dir,
                    &name,
                    std::path::Path::new(&sample.path),
                    start,
                    picture.as_deref(),
                    None,
                )
            },
            |studio, _, _, result| {
                studio.handle(Msg::Speech(SpeechMsg::VoiceSaved(Box::new(result))));
            },
        );
    }

    /// The New voice sheet on a recording just picked: named for its file,
    /// and listened to on a worker for where its voice is, the start held
    /// until that is known. A save under way keeps the sheet it is saving.
    fn import_voice(&mut self, source: PathBuf) {
        if self.new_voice.saving {
            return;
        }
        let name = source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.new_voice = NewVoice {
            open: true,
            source: source.clone(),
            name,
            listening: true,
            ..NewVoice::default()
        };
        crate::host::spawn(
            move || {
                let heard = voices::listen(&source);
                (source, heard)
            },
            |studio, _, _, (source, heard)| {
                studio.handle(Msg::Speech(SpeechMsg::VoiceHeard(source, Box::new(heard))));
            },
        );
    }

    /// Keeps the New voice sheet's recording as a voice, on a worker: the
    /// few seconds from where the sheet's start is, under its name, with
    /// its picture and its description.
    fn save_new_voice(&mut self, studio: &Studio) {
        let sheet = &mut self.new_voice;
        let name = sheet.name.trim().to_owned();
        if name.is_empty() || sheet.listening || sheet.saving {
            return;
        }
        let dir = voices::dir_in(&studio.host.dirs);
        let source = sheet.source.clone();
        let start = sheet.start;
        let picture = sheet.picture.clone();
        let description = sheet.description.clone();
        sheet.saving = true;
        crate::host::spawn(
            move || {
                voices::save(
                    &dir,
                    &name,
                    &source,
                    start,
                    picture.as_deref(),
                    Some(&description),
                )
            },
            |studio, _, _, result| {
                studio.handle(Msg::Speech(SpeechMsg::NewVoiceSaved(Box::new(result))));
            },
        );
    }

    /// The New voice sheet as Slint shows it.
    pub fn new_voice_data(&self) -> NewVoiceData {
        let sheet = &self.new_voice;
        let file_name = |path: &std::path::Path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        NewVoiceData {
            open: sheet.open,
            source: file_name(&sheet.source).into(),
            name: sheet.name.as_str().into(),
            description: sheet.description.as_str().into(),
            picture: sheet
                .picture
                .as_deref()
                .map(file_name)
                .unwrap_or_default()
                .into(),
            duration: sheet.duration as f32,
            start: sheet.start as f32,
            found: sheet.found as f32,
            listening: sheet.listening,
            saving: sheet.saving,
        }
    }
}

impl SpeechPane {
    /// A saved voice's avatar: its picture, or its initial in its colour.
    pub fn saved_avatar(&self, voice: &SavedVoice) -> crate::ui::AvatarData {
        let picture = voice
            .picture_path()
            .map(|path| {
                self.pictures
                    .borrow_mut()
                    .entry(voice.id.clone())
                    .or_insert_with(|| slint::Image::load_from_path(&path).unwrap_or_default())
                    .clone()
            })
            .filter(|image| image.size().width > 0);
        crate::ui::AvatarData {
            letter: avatar_letter(&voice.name).into(),
            hue: avatar_hue(&voice.name),
            pictured: picture.is_some(),
            picture: picture.unwrap_or_default(),
        }
    }
}

/// A voice's avatar as the Audio page draws it: its initial in the colour
/// its name picks.
pub fn letter_avatar(name: &str) -> crate::ui::AvatarData {
    crate::ui::AvatarData {
        letter: avatar_letter(name).into(),
        hue: avatar_hue(name),
        ..Default::default()
    }
}

/// A voice's name and its second line, as the voice list says them.
pub fn voice_names(name: &str) -> (String, String) {
    voice_label(name)
}

/// "af_heart" as a person would say it: the name, and the accent and
/// gender its prefix encodes; "bf_ruth" is Ruth, British, female.
fn voice_label(name: &str) -> (String, String) {
    if name == "chatterbox_clone" {
        return (
            t("speech.theSelectedClipsVoice"),
            t("speech.readsVoiceHeardClip"),
        );
    }
    // 本地中文音色：华言（普通话女声），模型 id 是 `piper:<plugin id>`。
    if let Some(rest) = name.strip_prefix("piper:") {
        return match rest {
            "zh_CN-huayan-x_low" => (
                t("speech.voiceHuayanLow"),
                t("speech.localLowDetail"),
            ),
            _ => (
                t("speech.voiceHuayanMedium"),
                t("speech.localMediumDetail"),
            ),
        };
    }
    // 录音棚音色的内置嗓音：英文名给出中文名，说明里注明是英文。
    match name {
        "af_elizabeth" => {
            return (t("speech.voiceElizabeth"), t("speech.englishFemale"))
        }
        "am_mark" => return (t("speech.voiceMark"), t("speech.englishMale")),
        "bf_ruth" => return (t("speech.voiceRuth"), t("speech.englishFemale")),
        "xm_bob" => return (t("speech.voiceBob"), t("speech.englishMale")),
        _ => {}
    }
    let (prefix, rest) = name.split_once('_').unwrap_or(("", name));
    let mut chars = rest.chars();
    let title = match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    };
    let accent = match prefix.chars().next() {
        Some('a') => t("speech.american"),
        Some('b') => t("speech.british"),
        Some('e') => t("speech.spanish"),
        Some('f') => t("speech.french"),
        Some('h') => t("speech.hindi"),
        Some('i') => t("speech.italian"),
        Some('j') => t("speech.japanese"),
        Some('p') => t("speech.portuguese"),
        Some('z') => t("speech.chinese"),
        _ => String::new(),
    };
    let gender = match prefix.chars().nth(1) {
        Some('f') => t("speech.female"),
        Some('m') => t("speech.male"),
        _ => String::new(),
    };
    let detail = [accent, gender]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    (title, detail)
}

/// The letter a voice's avatar shows: the first of its first name. A local
/// Chinese voice wears its own family name instead of the plugin id's `p`.
fn avatar_letter(name: &str) -> String {
    if name.starts_with("piper:") {
        return "华".to_owned();
    }
    name.split_whitespace()
        .next()
        .and_then(|first| first.chars().next())
        .map(|letter| letter.to_uppercase().collect())
        .unwrap_or_default()
}

/// Which of the avatar palette's eight hues a voice's name picks: a hash
/// of the name (FNV-1a), so a voice keeps its colour from run to run.
fn avatar_hue(name: &str) -> i32 {
    let hash = name
        .to_lowercase()
        .bytes()
        .fold(0x811c_9dc5_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        });
    (hash % 8) as i32
}

#[cfg(test)]
mod tests {
    use super::{avatar_hue, avatar_letter, insert_tag, voice_label};

    /// An avatar is the first letter of the first name, in a colour the
    /// name settles: the same every time, one of the palette's eight.
    #[test]
    fn a_voice_avatar_is_its_initial_in_its_own_colour() {
        assert_eq!(avatar_letter("Bria"), "B");
        assert_eq!(avatar_letter("émile dupont"), "É");
        assert_eq!(avatar_letter(""), "");
        assert_eq!(avatar_hue("Bria"), avatar_hue("bria"));
        let hues: std::collections::HashSet<i32> = [
            "Bria", "Adam", "Heart", "Lewis", "Nova", "Sky", "Eric", "Alba",
        ]
        .iter()
        .map(|name| avatar_hue(name))
        .collect();
        assert!(hues.iter().all(|hue| (0..8).contains(hue)));
        assert!(hues.len() > 2, "names spread over the palette: {hues:?}");
    }

    /// A tag lands as a word of its own wherever the caret is: spaced off a
    /// word on either side, not doubled against a space already there, and
    /// snapped to a character when the caret is reported inside one.
    #[test]
    fn a_tag_lands_as_its_own_word() {
        assert_eq!(insert_tag("", "[laugh]", 0), "[laugh] ");
        assert_eq!(insert_tag("hello", "[laugh]", 5), "hello [laugh] ");
        assert_eq!(insert_tag("hello world", "[sigh]", 5), "hello [sigh] world");
        assert_eq!(insert_tag("hello world", "[sigh]", 6), "hello [sigh] world");
        assert_eq!(
            insert_tag("hello world", "[sigh]", 3),
            "hel [sigh] lo world"
        );
        assert_eq!(insert_tag("hi", "[chuckle]", 0), "[chuckle] hi");
        // Past the end is the end; inside "é" (two bytes) steps back to it.
        assert_eq!(insert_tag("hi", "[laugh]", 40), "hi [laugh] ");
        assert_eq!(insert_tag("café", "[laugh]", 4), "caf [laugh] é");
        assert_eq!(insert_tag("café ", "[laugh]", 6), "café [laugh] ");
    }

    #[test]
    fn a_voice_name_reads_as_a_person_would_say_it() {
        let _english = crate::i18n::language_lock();
        let (title, detail) = voice_label("af_heart");
        assert_eq!(title, "Heart");
        assert!(detail.contains("American"));
        assert!(detail.contains("female"));
        assert_eq!(voice_label("nova").0, "Nova");
        // The stock Chatterbox voices: a British woman, and a man whose
        // accent goes unsaid.
        let (title, detail) = voice_label("bf_ruth");
        assert_eq!(title, "Ruth");
        assert!(detail.contains("British") && detail.contains("female"));
        let (title, detail) = voice_label("xm_bob");
        assert_eq!(title, "Bob");
        assert!(!detail.contains("·") && detail.contains("male"), "{detail}");
        assert!(voice_label("chatterbox_clone").0.contains("clip"));
    }
}
