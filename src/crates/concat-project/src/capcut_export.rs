// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! A Concat project written out as a CapCut / Jianying (剪映) draft.
//!
//! The write direction of [`crate::capcut`]: a folder holding
//! `draft_content.json` and `draft_meta_info.json`, the two files 剪映专业版
//! reads a draft from, plus the `materials/` scaffolding the request asked
//! for. Media files are *referenced* by their absolute paths, the way 剪映
//! itself stores them, and are not copied: a draft of a two-hour project
//! would otherwise duplicate every gigabyte of it.
//!
//! # Which 剪映 this is for
//!
//! 剪映 5.9 and older keep a draft as plain JSON (`"version": 360000`).
//! 剪映 6 and later encrypt `draft_content.json`, so no outside tool can
//! write one; that is why the format written here is the 5.x one, built
//! from the publicly documented field set (the same one the Python
//! `pyJianYingDraft` library has been generating real, importable drafts
//! with). It has not been verified against an installed 剪映 — none is
//! available on the machine this was built on — so the caller must say so.
//!
//! # What maps
//!
//! Video and still clips ([`ClipKind::Video`], [`ClipKind::Image`]) with
//! their in-points, speed, volume, fades, crop, place, size, turn, opacity
//! and flips; audio clips with their level and fades; titles with their
//! words and the look of their first run; transitions mapped onto 剪映's own
//! built-in packages where one matches. What cannot be mapped - effects,
//! filters, masks, keyframes, speed curves, shape layers, standalone
//! transition clips - is left out and said once per kind in the notes, so
//! the user knows what to redo in 剪映.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::model::{
    Clip, ClipKind, Crop, MediaItem, MediaKind, Project, TextAlign, TextStyle, Timeline, Transition,
    VideoSettings,
};

/// The draft format generation: 剪映 5.x's `version`.
pub const JIANYING_VERSION: u64 = 360_000;

/// The `new_version` 剪映 5.9 writes.
const NEW_VERSION: &str = "110.0.0";

/// Microseconds in a second: every time in a draft is in these.
const MICROS: f64 = 1_000_000.0;

/// A photo material's length in 剪映, which is a still's "do not care":
/// three hours of microseconds. From the public format's reference
/// implementations.
const PHOTO_DURATION: i64 = 10_800_000_000;

/// A title's size over Concat's: 剪映's default title size, 15, as Concat's
/// default title, nine hundredths of the frame's height. The same constant
/// the reader uses in reverse ([`crate::capcut`]).
const TEXT_SIZE: f64 = 0.09 / 15.0;

/// One draft written: where it went, what is in it, and what was left out.
#[derive(Clone, Debug, Default)]
pub struct Written {
    /// The draft's own folder.
    pub folder: PathBuf,
    /// Every file written, `draft_content.json` first.
    pub files: Vec<PathBuf>,
    /// What did not come across, once per kind.
    pub notes: Vec<String>,
    /// The timeline's length in seconds.
    pub duration: f64,
    /// How many clips made it into the draft.
    pub clips: usize,
    /// How many titles made it into the draft.
    pub titles: usize,
}

/// The draft's `draft_content.json` for `project`, and what could not be
/// mapped into it. The active timeline is the one exported.
pub fn content(project: &Project, name: &str) -> (Value, Vec<String>) {
    content_seeded(project, name, seed())
}

/// [`content`] with the id generator seeded: the same seed and the same
/// project give the same bytes, which is what the tests compare.
pub fn content_seeded(project: &Project, name: &str, seed: u64) -> (Value, Vec<String>) {
    let mut draft = Draft::new(project, name, seed);
    draft.build();
    draft.finish()
}

/// The draft's `draft_meta_info.json`: what the drafts list shows. The
/// times are microseconds since the Unix epoch, as 剪映 writes them.
pub fn meta_info(name: &str, folder: &Path, duration: f64) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros() as u64)
        .unwrap_or(0);
    let fold = folder.to_string_lossy().replace('\\', "/");
    let root = folder
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    json!({
        "draft_cloud_capcut_purchase_info": "",
        "draft_cloud_last_action_download": false,
        "draft_cloud_materials": [],
        "draft_cloud_purchase_info": "",
        "draft_cloud_template_id": "",
        "draft_cloud_tutorial_info": "",
        "draft_cloud_videocut_purchase_info": "",
        "draft_cover": "",
        "draft_deeplink_url": "",
        "draft_enterprise_info": {
            "draft_enterprise_extra": "",
            "draft_enterprise_id": "",
            "draft_enterprise_name": "",
            "enterprise_material": []
        },
        "draft_fold_path": fold,
        "draft_id": hex_id(&mut seed_from(now ^ 0x5a17_2028)),
        "draft_is_ai_shorts": false,
        "draft_is_invisible": false,
        "draft_is_web_article_video": false,
        "draft_materials": [],
        "draft_materials_copied_info": [],
        "draft_name": name,
        "draft_new_version": "",
        "draft_removable_storage_device": "",
        "draft_root_path": root,
        "draft_segment_extra_info": [],
        "draft_timeline_materials_size_": 0,
        "draft_type": "",
        "tm_draft_cloud_completed": "0",
        "tm_draft_cloud_created": "0",
        "tm_draft_cloud_modified": "0",
        "tm_draft_create": now,
        "tm_draft_modified": now,
        "tm_draft_removed": "0",
        "tm_duration": micros(duration)
    })
}

/// A folder for one draft under `parent`: `name`, or `name (2)` when that
/// folder is taken. Made only if a draft is about to be written into it.
pub fn free_folder(parent: &Path, name: &str) -> PathBuf {
    let base = if name.trim().is_empty() {
        "星籁剪辑导出"
    } else {
        name.trim()
    };
    (1..)
        .map(|number| {
            if number == 1 {
                parent.join(base)
            } else {
                parent.join(format!("{base} ({number})"))
            }
        })
        .find(|folder| !folder.exists())
        .expect("a free folder name")
}

/// Writes `project` as a 剪映 draft into `folder`, making it.
pub fn write(project: &Project, name: &str, folder: &Path) -> Result<Written, String> {
    let (content, notes) = content(project, name);
    let timeline = project.active();
    let duration = timeline.duration();
    std::fs::create_dir_all(folder)
        .map_err(|error| format!("创建草稿文件夹失败：{error}"))?;
    // The scaffolding the request names. Media is referenced in place, so
    // these stay empty; the README says why.
    for material in ["videos", "audios", "images", "texts", "effects"] {
        std::fs::create_dir_all(folder.join("materials").join(material))
            .map_err(|error| format!("创建 materials/{material} 失败：{error}"))?;
    }
    let mut written = Written {
        folder: folder.to_path_buf(),
        notes: notes.clone(),
        duration,
        ..Written::default()
    };
    let draft = serde_json::to_string_pretty(&content)
        .map_err(|error| format!("草稿序列化失败：{error}"))?;
    let meta = serde_json::to_string_pretty(&meta_info(name, folder, duration))
        .map_err(|error| format!("草稿索引序列化失败：{error}"))?;
    let readme = readme(project, name, duration, &notes);
    for (file_name, text) in [
        ("draft_content.json", draft),
        ("draft_meta_info.json", meta),
        ("README.txt", readme),
    ] {
        let path = folder.join(file_name);
        std::fs::write(&path, text).map_err(|error| format!("写入 {file_name} 失败：{error}"))?;
        written.files.push(path);
    }
    written.clips = timeline
        .clips
        .iter()
        .filter(|clip| matches!(clip.kind, ClipKind::Video | ClipKind::Image | ClipKind::Audio))
        .count();
    written.titles = timeline
        .clips
        .iter()
        .filter(|clip| clip.kind == ClipKind::Text)
        .count();
    Ok(written)
}

/// The README that travels with a draft: what it is, and what will have to
/// be redone in 剪映.
fn readme(project: &Project, name: &str, duration: f64, notes: &[String]) -> String {
    let mut text = String::new();
    text.push_str(&format!("{name} · 剪映草稿（由星籁剪辑导出）\n"));
    text.push_str("========================================\n\n");
    text.push_str(&format!(
        "版本：星籁剪辑 {} · 草稿格式：剪映专业版 5.x（version {}）\n",
        env!("CARGO_PKG_VERSION"),
        JIANYING_VERSION
    ));
    let video = project.active().video;
    let whole = duration.max(0.0);
    text.push_str(&format!(
        "画幅：{}×{} · 帧率：{:.3} fps · 时长：{:02}:{:05.2}\n\n",
        video.width,
        video.height,
        video.rate(),
        (whole / 60.0) as u64,
        whole % 60.0
    ));
    text.push_str("怎么用\n------\n");
    text.push_str("1. 打开剪映专业版（5.9 或更早版本；6.x 的草稿是加密的，读不了这份）。\n");
    text.push_str("2. 把这个文件夹放进剪映的草稿目录，或在剪映里「导入草稿」选择\n");
    text.push_str("   draft_content.json。若列表里看不见，可在剪映里新建一个草稿，\n");
    text.push_str("   再把这里的 draft_content.json 与 draft_meta_info.json 覆盖进去。\n");
    text.push_str("3. 素材按本机原始路径引用，没有复制进草稿；换一台机器打开需要把\n");
    text.push_str("   素材拷到同样的路径，或在剪映里重新链接。\n\n");
    text.push_str("materials/ 目录是草稿结构的一部分（videos/audios/images/texts/\n");
    text.push_str("effects），本导出没有把媒体复制进来，所以它们是空的。\n\n");
    text.push_str("已知限制\n------\n");
    text.push_str("· 剪映 6.x 的草稿是加密的，本文件为 5.x 明文格式；未在装剪映的机器上\n");
    text.push_str("  实机验证过，按公开格式生成。\n");
    text.push_str("· 特效、滤镜、蒙版、抠像、关键帧、速度曲线无法映射，需在剪映里重做。\n");
    text.push_str("· 字幕字体写的是剪映默认字体，自定义字体需在剪映里重新选择。\n");
    if notes.is_empty() {
        text.push_str("· 本次没有需要额外说明的降级项。\n");
    } else {
        for note in notes {
            text.push_str(&format!("· {note}\n"));
        }
    }
    text
}

// ── the shared id generator ────────────────────────────────────────────────

/// A seed for [`content_seeded`] from the clock: two exports of the same
/// project get different ids, as two drafts do in 剪映.
fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0x2028);
    seed_from(nanos ^ std::process::id() as u64)
}

fn seed_from(value: u64) -> u64 {
    value ^ 0x9E37_79B9_7F4A_7C15
}

/// 剪映's own ids are 32 lowercase hex characters; the format is not
/// checked beyond that, so a xorshift walk is enough and needs no crate.
fn hex_id(seed: &mut u64) -> String {
    let mut out = String::with_capacity(32);
    for _ in 0..2 {
        *seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.push_str(&format!("{z:016x}"));
    }
    out
}

// ── the numbers the draft holds ────────────────────────────────────────────

/// Seconds as the draft's integer microseconds, never negative.
fn micros(seconds: f64) -> i64 {
    if seconds.is_finite() {
        (seconds * MICROS).round().max(0.0) as i64
    } else {
        0
    }
}

fn timerange(start: f64, duration: f64) -> Value {
    json!({
        "start": micros(start),
        "duration": micros(duration).max(1),
    })
}

/// `#rrggbb` / `#rrggbbaa` as 剪映's `[r, g, b]` in `0..1`; white for
/// anything that is not a colour.
fn rgb(colour: &str) -> [f64; 3] {
    let digits = colour.trim().trim_start_matches('#');
    let channel = |at: usize| {
        u8::from_str_radix(digits.get(at..at + 2).unwrap_or("ff"), 16).unwrap_or(255) as f64 / 255.0
    };
    if digits.len() >= 6 {
        [channel(0), channel(2), channel(4)]
    } else {
        [1.0, 1.0, 1.0]
    }
}

// ── the draft being built ──────────────────────────────────────────────────

/// What did not come across, counted by what it was, said once each.
#[derive(Default)]
struct Notes(BTreeMap<String, usize>);

impl Notes {
    fn note(&mut self, what: impl Into<String>) {
        *self.0.entry(what.into()).or_insert(0) += 1;
    }

    fn lines(self) -> Vec<String> {
        self.0
            .into_iter()
            .map(|(what, count)| {
                if count == 1 {
                    what
                } else {
                    format!("{what}（×{count}）")
                }
            })
            .collect()
    }
}

struct Draft<'a> {
    project: &'a Project,
    timeline: &'a Timeline,
    video: VideoSettings,
    name: &'a str,
    seed: u64,
    videos: Vec<Value>,
    audios: Vec<Value>,
    texts: Vec<Value>,
    speeds: Vec<Value>,
    fades: Vec<Value>,
    transitions: Vec<Value>,
    tracks: Vec<Value>,
    notes: Notes,
    /// Which material a media item got, by `"{media id}:{as video}"`.
    materials: HashMap<String, String>,
}

impl<'a> Draft<'a> {
    fn new(project: &'a Project, name: &'a str, seed: u64) -> Draft<'a> {
        Draft {
            project,
            timeline: project.active(),
            video: project.active().video,
            name,
            seed,
            videos: Vec::new(),
            audios: Vec::new(),
            texts: Vec::new(),
            speeds: Vec::new(),
            fades: Vec::new(),
            transitions: Vec::new(),
            tracks: Vec::new(),
            notes: Notes::default(),
            materials: HashMap::new(),
        }
    }

    fn id(&mut self) -> String {
        hex_id(&mut self.seed)
    }

    /// The material for `media`: a video (or photo) material, or an audio
    /// one - an audio clip cut from a video file keeps a video material and
    /// plays its sound, which is how 剪映 stores detached audio. A crop is
    /// a material's in 剪映, so clips that crop the same file differently
    /// get a material each.
    fn media_material(
        &mut self,
        media: &MediaItem,
        as_video: bool,
        needed: f64,
        crop: Option<&Crop>,
    ) -> String {
        let crop = crop.map(jianying_crop);
        let key = format!(
            "{}:{}:{}",
            media.id,
            as_video,
            crop.as_ref()
                .map(|crop| crop.to_string())
                .unwrap_or_default()
        );
        if let Some(id) = self.materials.get(&key) {
            return id.clone();
        }
        let id = self.id();
        let duration = media
            .duration
            .filter(|duration| *duration > needed)
            .unwrap_or(needed);
        if media.kind == MediaKind::Audio && !as_video {
            let material = json!({
                "app_id": 0,
                "category_id": "",
                "category_name": "local",
                "check_flag": 1,
                "copyright_limit_type": "none",
                "duration": micros(duration).max(1),
                "effect_id": "",
                "formula_id": "",
                "id": id,
                "intensifies_path": "",
                "is_ai_clone_tone": false,
                "is_text_edit_overdub": false,
                "is_ugc": false,
                "local_material_id": id,
                "music_id": id,
                "name": media.name,
                "path": media.path,
                "query": "",
                "request_id": "",
                "resource_id": "",
                "search_id": "",
                "source_from": "",
                "source_platform": 0,
                "team_id": "",
                "text_id": "",
                "tone_category_id": "",
                "tone_category_name": "",
                "tone_effect_id": "",
                "tone_effect_name": "",
                "tone_platform": "",
                "tone_second_category_id": "",
                "tone_second_category_name": "",
                "tone_speaker": "",
                "tone_type": "",
                "type": "extract_music",
                "video_id": "",
                "wave_points": []
            });
            self.audios.push(material);
        } else {
            let photo = media.kind == MediaKind::Image;
            let material = json!({
                "aigc_type": "none",
                "audio_fade": null,
                "category_id": "",
                "category_name": "local",
                "check_flag": 63487,
                "crop": crop.clone().unwrap_or_else(full_crop),
                "crop_ratio": "free",
                "crop_scale": 1.0,
                "duration": if photo { PHOTO_DURATION } else { micros(duration).max(1) },
                "extra_type_option": 0,
                "formula_id": "",
                "freeze": null,
                "has_audio": media.has_audio && !photo,
                "has_sound_separated": false,
                "height": media.height.unwrap_or(self.video.height).max(2),
                "id": id,
                "intensifies_audio_path": "",
                "intensifies_path": "",
                "is_ai_generate_content": false,
                "is_copyright": false,
                "is_unified_beauty_mode": false,
                "is_voice_change": false,
                "local_id": "",
                "local_material_id": id,
                "material_id": id,
                "material_name": media.name,
                "matting": {
                    "flag": 0,
                    "has_use_quick_brush": false,
                    "has_use_quick_eraser": false,
                    "interactiveTime": [],
                    "path": "",
                    "strokes": []
                },
                "matting_type": "",
                "media_path": "",
                "mime_type": "",
                "object_locked": null,
                "origin_material_id": "",
                "path": media.path,
                "picture_from": "none",
                "picture_set_category_id": "",
                "picture_set_category_name": "",
                "request_id": "",
                "reverse_intensifies_path": "",
                "reverse_path": "",
                "smart_motion": null,
                "source": 0,
                "source_platform": 0,
                "stable": null,
                "team_id": "",
                "type": if photo { "photo" } else { "video" },
                "video_algorithm": {},
                "video_id": "",
                "width": media.width.unwrap_or(self.video.width).max(2),
            });
            self.videos.push(material);
        }
        self.materials.insert(key, id.clone());
        id
    }

    fn speed_material(&mut self, speed: f64) -> String {
        let id = self.id();
        self.speeds.push(json!({
            "curve_speed": null,
            "id": id,
            "mode": 0,
            "speed": speed,
            "type": "speed"
        }));
        id
    }

    fn fade_material(&mut self, fade_in: f64, fade_out: f64) -> Option<String> {
        if fade_in <= 0.0 && fade_out <= 0.0 {
            return None;
        }
        let id = self.id();
        self.fades.push(json!({
            "id": id,
            "fade_in_duration": micros(fade_in),
            "fade_out_duration": micros(fade_out),
            "fade_type": 0,
            "type": "audio_fade"
        }));
        Some(id)
    }

    /// 剪映's own transition for `transition`, or the default crossfade
    /// when nothing maps, saying so.
    fn transition_material(&mut self, transition: &Transition) -> String {
        let mapped = jianying_transition(&transition.id);
        if mapped.is_none() {
            self.notes
                .note("剪映没有对应的转场，已降级为【叠化】".to_owned());
        }
        let (name, resource_id, effect_id) =
            mapped.unwrap_or(("叠化", "6724845717472416269", "322577"));
        let id = self.id();
        let duration = micros(transition.duration).clamp(100_000, 5_000_000);
        self.transitions.push(json!({
            "category_id": "",
            "category_name": "",
            "duration": duration,
            "effect_id": effect_id,
            "id": id,
            "is_overlap": true,
            "name": name,
            "platform": "all",
            "resource_id": resource_id,
            "type": "transition"
        }));
        id
    }

    /// A title's material: the words and the look of their first run, in
    /// the JSON-inside-JSON 剪映 keeps `content` in.
    fn text_material(&mut self, style: &TextStyle, opacity: f64) -> String {
        let id = self.id();
        let content = style.display_content();
        let words = content.chars().count() as u64;
        let colour = rgb(&style.color);
        let mut run = json!({
            "fill": {
                "alpha": 1.0,
                "content": {
                    "render_type": "solid",
                    "solid": { "alpha": 1.0, "color": colour }
                }
            },
            "range": [0, words],
            "size": (style.font_size / TEXT_SIZE).clamp(1.0, 100.0),
            "bold": style.font_weight >= 600.0,
            "italic": style.italic,
            "underline": style.underline,
            "strokes": []
        });
        if style.stroke_width > 0.0 {
            run["strokes"] = json!([{
                "content": {
                    "solid": {
                        "alpha": 1.0,
                        "color": rgb(&style.stroke_color)
                    }
                },
                "width": style.stroke_width
                    * f64::from(self.video.height.max(1))
                    / 1080.0
            }]);
        }
        let content = json!({
            "styles": [run],
            "text": content
        });
        let mut material = json!({
            "add_type": 0,
            "alignment": match style.align {
                TextAlign::Left => 0,
                TextAlign::Center => 1,
                TextAlign::Right => 2,
            },
            "background_alpha": 1.0,
            "background_color": "",
            "bold": style.font_weight >= 600.0,
            "check_flag": 7
                | if style.stroke_width > 0.0 { 8 } else { 0 }
                | if style.background.trim().is_empty() { 0 } else { 16 },
            "content": content.to_string(),
            "font": "text-layout-font",
            "font_id": "",
            "font_name": "",
            "font_path": "",
            "font_size": (style.font_size / TEXT_SIZE).clamp(1.0, 100.0),
            "global_alpha": opacity.clamp(0.0, 1.0),
            "has_shadow": style.shadow,
            "id": id,
            "initial_scale": 1.0,
            "is_rich_text": false,
            "italic": style.italic,
            "letter_spacing": 0.0,
            "line_feed": 1,
            "line_max_width": 0.82,
            "line_spacing": 0.02 + (style.line_height - 1.0).max(0.0) * 0.05,
            "force_apply_line_max_width": false,
            "type": "text",
            "typesetting": 0,
            "underline": false,
            "words": { "end_time": -1, "start_time": -1, "text": "" }
        });
        if style.shadow {
            material["shadow_alpha"] = json!(0.9);
            material["shadow_angle"] = json!(-45.0);
            material["shadow_color"] = json!("#000000");
            material["shadow_distance"] = json!(5.0);
            material["shadow_point"] = json!({ "x": 0.6363961030678928, "y": -0.6363961030678928 });
            material["shadow_smoothing"] = json!(0.45);
        }
        if !style.background.trim().is_empty() {
            let alpha = style.background.get(7..9).map(|channel| {
                u8::from_str_radix(channel, 16).unwrap_or(255) as f64 / 255.0
            });
            material["background_style"] = json!(1);
            material["background_color"] = json!(format!(
                "#{}",
                style
                    .background
                    .trim_start_matches('#')
                    .get(0..6)
                    .unwrap_or("000000")
            ));
            material["background_alpha"] = json!(alpha.unwrap_or(1.0));
            material["background_round_radius"] = json!(style.background_radius);
            material["background_height"] = json!(0.14);
            material["background_width"] = json!(0.14);
            material["background_horizontal_offset"] = json!(0.0);
            material["background_vertical_offset"] = json!(0.0);
        }
        self.texts.push(material);
        id
    }

    /// One visual clip as a 剪映 segment: its place on the timeline, the
    /// piece of its source it shows, and how it is drawn.
    fn visual_segment(
        &mut self,
        clip: &Clip,
        transition: Option<&Transition>,
        track_visible: bool,
        render_index: i64,
    ) -> Option<Value> {
        let media = self.project.media_by_id(&clip.media_id)?;
        let needed = clip.source_start + clip.duration * clip.speed.max(0.01);
        let material = self.media_material(media, true, needed, clip.crop.as_ref());
        let speed = self.speed_material(clip.speed.max(0.01));
        let mut extras = vec![Value::String(speed)];
        if let Some(fade) = self.fade_material(clip.fade_in, clip.fade_out) {
            extras.push(Value::String(fade));
        }
        if let Some(transition) = transition {
            extras.push(Value::String(self.transition_material(transition)));
        }
        let photo = media.kind == MediaKind::Image;
        let source = if photo {
            Value::Null
        } else {
            timerange(clip.source_start, clip.duration * clip.speed.max(0.01))
        };
        let scale = clip.scale.max(0.01);
        Some(json!({
            "caption_info": null,
            "cartoon_path": "",
            "category_id": "",
            "category_name": "local",
            "check_flag": 1,
            "clip": {
                "alpha": clip.opacity.clamp(0.0, 1.0),
                "flip": { "horizontal": clip.flip_h, "vertical": clip.flip_v },
                "rotation": if clip.rotation.is_finite() { clip.rotation } else { 0.0 },
                "scale": {
                    "x": scale * clip.stretch_x.max(0.01),
                    "y": scale * clip.stretch_y.max(0.01)
                },
                "transform": {
                    "x": clip.offset_x * 2.0,
                    "y": -clip.offset_y * 2.0
                }
            },
            "common_keyframes": [],
            "enable_adjust": true,
            "enable_color_correct_adjust": false,
            "enable_color_curves": true,
            "enable_color_match_adjust": false,
            "enable_color_wheels": true,
            "enable_lut": true,
            "enable_smart_color_adjust": false,
            "extra_material_refs": extras,
            "group_id": "",
            "hdr_settings": { "intensity": 1.0, "mode": 1, "nits": 1000 },
            "id": self.id(),
            "intensifies_audio": false,
            "is_placeholder": false,
            "is_tone_modify": false,
            "keyframe_refs": [],
            "last_nonzero_volume": 1.0,
            "material_id": material,
            "render_index": render_index,
            "responsive_layout": {
                "enable": false,
                "horizontal_pos_layout": 0,
                "size_layout": 0,
                "target_follow": "",
                "vertical_pos_layout": 0
            },
            "reverse": false,
            "source_timerange": source,
            "speed": clip.speed.max(0.01),
            "target_timerange": timerange(clip.start, clip.duration),
            "template_id": "",
            "template_scene": "default",
            "track_attribute": 0,
            "track_render_index": 0,
            "uniform_scale": { "on": true, "value": 1.0 },
            "visible": track_visible && clip.opacity > 0.0,
            "volume": clip.volume.clamp(0.0, 1.0),
        }))
    }

    /// One sound clip as a 剪映 audio segment.
    fn audio_segment(&mut self, clip: &Clip, muted: bool) -> Option<Value> {
        let media = self.project.media_by_id(&clip.media_id)?;
        let needed = clip.source_start + clip.duration * clip.speed.max(0.01);
        let as_video = media.kind != MediaKind::Audio;
        let material = self.media_material(media, as_video, needed, None);
        let speed = self.speed_material(clip.speed.max(0.01));
        let mut extras = vec![Value::String(speed)];
        if let Some(fade) = self.fade_material(clip.fade_in, clip.fade_out) {
            extras.push(Value::String(fade));
        }
        Some(json!({
            "caption_info": null,
            "category_id": "",
            "category_name": "local",
            "check_flag": 1,
            "clip": Value::Null,
            "common_keyframes": [],
            "enable_adjust": true,
            "enable_color_correct_adjust": false,
            "enable_color_curves": true,
            "enable_color_match_adjust": false,
            "enable_color_wheels": true,
            "enable_lut": true,
            "enable_smart_color_adjust": false,
            "extra_material_refs": extras,
            "group_id": "",
            "hdr_settings": Value::Null,
            "id": self.id(),
            "intensifies_audio": false,
            "is_placeholder": false,
            "is_tone_modify": false,
            "keyframe_refs": [],
            "last_nonzero_volume": 1.0,
            "material_id": material,
            "render_index": 0,
            "responsive_layout": {
                "enable": false,
                "horizontal_pos_layout": 0,
                "size_layout": 0,
                "target_follow": "",
                "vertical_pos_layout": 0
            },
            "reverse": false,
            "source_timerange": timerange(clip.source_start, clip.duration * clip.speed.max(0.01)),
            "speed": clip.speed.max(0.01),
            "target_timerange": timerange(clip.start, clip.duration),
            "template_id": "",
            "template_scene": "default",
            "track_attribute": 0,
            "track_render_index": 0,
            "uniform_scale": { "on": true, "value": 1.0 },
            "visible": true,
            "volume": if muted { 0.0 } else { clip.volume.clamp(0.0, 1.0) },
        }))
    }

    /// One title as a 剪映 text segment.
    fn text_segment(&mut self, clip: &Clip) -> Option<Value> {
        let style = clip.text.as_ref()?;
        if style.content.trim().is_empty() {
            return None;
        }
        let material = self.text_material(style, clip.opacity.clamp(0.0, 1.0));
        let speed = self.speed_material(1.0);
        Some(json!({
            "caption_info": null,
            "category_id": "",
            "category_name": "local",
            "check_flag": 1,
            "clip": {
                "alpha": clip.opacity.clamp(0.0, 1.0),
                "flip": { "horizontal": clip.flip_h, "vertical": clip.flip_v },
                "rotation": if clip.rotation.is_finite() { clip.rotation } else { 0.0 },
                "scale": {
                    "x": clip.scale.max(0.01) * clip.stretch_x.max(0.01),
                    "y": clip.scale.max(0.01) * clip.stretch_y.max(0.01)
                },
                "transform": {
                    "x": clip.offset_x * 2.0,
                    "y": -clip.offset_y * 2.0
                }
            },
            "common_keyframes": [],
            "enable_adjust": true,
            "enable_color_correct_adjust": false,
            "enable_color_curves": true,
            "enable_color_match_adjust": false,
            "enable_color_wheels": true,
            "enable_lut": true,
            "enable_smart_color_adjust": false,
            "extra_material_refs": [speed],
            "group_id": "",
            "id": self.id(),
            "intensifies_audio": false,
            "is_placeholder": false,
            "is_tone_modify": false,
            "keyframe_refs": [],
            "last_nonzero_volume": 1.0,
            "material_id": material,
            "render_index": 15_000,
            "responsive_layout": {
                "enable": false,
                "horizontal_pos_layout": 0,
                "size_layout": 0,
                "target_follow": "",
                "vertical_pos_layout": 0
            },
            "reverse": false,
            "source_timerange": Value::Null,
            "speed": 1.0,
            "target_timerange": timerange(clip.start, clip.duration),
            "template_id": "",
            "template_scene": "default",
            "track_attribute": 0,
            "track_render_index": 0,
            "uniform_scale": { "on": true, "value": 1.0 },
            "visible": true,
            "volume": 1.0,
        }))
    }

    /// Walks the timeline: every lane's clips into 剪映's typed tracks, the
    /// unsupported kinds counted into the notes.
    fn build(&mut self) {
        // Copied so the walk over the timeline does not hold the draft
        // borrowed while its own methods push materials.
        let timeline: &'a Timeline = self.timeline;
        let mut dropped: Vec<&'static str> = Vec::new();
        let mut text_clips: Vec<&Clip> = Vec::new();
        let mut visual_render = 0i64;

        for track in &timeline.tracks {
            let mut on_lane: Vec<&Clip> = timeline
                .clips
                .iter()
                .filter(|clip| clip.track_id == track.id)
                .map(|clip| clip.as_ref())
                .collect();
            on_lane.sort_by(|a, b| a.start.total_cmp(&b.start));

            let mut visual: Vec<&Clip> = Vec::new();
            let mut audio: Vec<&Clip> = Vec::new();
            for clip in on_lane {
                match clip.kind {
                    ClipKind::Video | ClipKind::Image => visual.push(clip),
                    ClipKind::Audio => audio.push(clip),
                    ClipKind::Text => text_clips.push(clip),
                    ClipKind::Layer | ClipKind::SoundLayer | ClipKind::Shape => {
                        dropped.push("形状/图层片段未写入剪映草稿，需在剪映里重做")
                    }
                    ClipKind::Transition => {
                        dropped.push("独立转场片段未写入剪映草稿；剪映的转场挂在片段上")
                    }
                }
            }
            self.count_dropped_extras(timeline, &visual, &audio);

            if !visual.is_empty() {
                let mut segments = Vec::new();
                for (index, clip) in visual.iter().enumerate() {
                    let transition = visual
                        .get(index + 1)
                        .and_then(|next| next.transition_in.as_ref());
                    if transition.is_some() && index + 1 == visual.len() {
                        // A transition into a clip not on this lane is nonsense.
                    }
                    if let Some(segment) =
                        self.visual_segment(clip, transition, track.visible, visual_render)
                    {
                        segments.push(segment);
                    }
                }
                segments.sort_by(|a, b| {
                    let at = |segment: &Value| {
                        segment
                            .pointer("/target_timerange/start")
                            .and_then(Value::as_i64)
                            .unwrap_or(0)
                    };
                    at(a).cmp(&at(b))
                });
                if !segments.is_empty() {
                    let id = self.id();
                    self.tracks.push(json!({
                        "attribute": 0,
                        "flag": 0,
                        "id": id,
                        "is_default_name": true,
                        "name": "",
                        "segments": segments,
                        "type": "video"
                    }));
                    visual_render += 1;
                }
            }
            if !audio.is_empty() {
                let mut segments = Vec::new();
                for clip in &audio {
                    if let Some(segment) = self.audio_segment(clip, track.muted) {
                        segments.push(segment);
                    }
                }
                segments.sort_by(|a, b| {
                    let at = |segment: &Value| {
                        segment
                            .pointer("/target_timerange/start")
                            .and_then(Value::as_i64)
                            .unwrap_or(0)
                    };
                    at(a).cmp(&at(b))
                });
                if !segments.is_empty() {
                    let id = self.id();
                    self.tracks.push(json!({
                        "attribute": if track.muted { 1 } else { 0 },
                        "flag": 0,
                        "id": id,
                        "is_default_name": true,
                        "name": "",
                        "segments": segments,
                        "type": "audio"
                    }));
                }
            }
            // The first transition of a lane hangs on the clip before it;
            // the one into the first clip has nowhere to go.
            if visual
                .first()
                .is_some_and(|first| first.transition_in.is_some())
            {
                dropped.push("首段片段上的转场没有上一段可挂，未写入");
            }
        }

        if !text_clips.is_empty() {
            text_clips.sort_by(|a, b| a.start.total_cmp(&b.start));
            // Titles that share a stretch of time were on different Concat
            // lanes (a typing title above the captions, say); 剪映 wants one
            // lane's segments never to overlap, so they are packed into as
            // many text lanes as their times need, top first.
            let mut lanes: Vec<Vec<&Clip>> = Vec::new();
            for clip in &text_clips {
                let fits = lanes.iter_mut().find(|lane| {
                    lane.last()
                        .is_none_or(|last| last.start + last.duration <= clip.start + 1e-6)
                });
                match fits {
                    Some(lane) => lane.push(clip),
                    None => lanes.push(vec![clip]),
                }
            }
            for (index, lane) in lanes.iter().enumerate() {
                let mut segments = Vec::new();
                for clip in lane {
                    if let Some(segment) = self.text_segment(clip) {
                        segments.push(segment);
                    }
                }
                if !segments.is_empty() {
                    let id = self.id();
                    self.tracks.push(json!({
                        "attribute": 0,
                        "flag": 0,
                        "id": id,
                        "is_default_name": true,
                        "name": "",
                        "segments": segments,
                        "type": "text",
                        "render_index": 15_000 + index as i64
                    }));
                }
            }
        }

        for what in dropped {
            self.notes.note(what);
        }
    }

    /// Every keyed or effect-carrying thing that will not cross over, said
    /// once per kind.
    fn count_dropped_extras(&mut self, timeline: &Timeline, visual: &[&Clip], audio: &[&Clip]) {
        let mut effects = 0usize;
        let mut filters = 0usize;
        let mut keys = 0usize;
        let mut curves = 0usize;
        let mut masks = 0usize;
        let mut loud = 0usize;
        for clip in timeline.clips.iter() {
            effects += clip.video_effects.len();
            filters += clip.filters.len();
            keys += clip.keys.len();
            masks += clip.masks.len()
                + usize::from(clip.cutout.is_some())
                + usize::from(clip.motion.is_some())
                + usize::from(clip.stabilize.is_some())
                + usize::from(clip.voice.is_some());
            if clip.speed_curve.is_some() {
                curves += 1;
            }
            if clip.volume > 1.0 {
                loud += 1;
            }
        }
        if effects > 0 {
            self.notes
                .note("特效/滤镜未写入剪映草稿，需在剪映里重做");
        }
        if filters > 0 {
            self.notes
                .note("音频滤镜未写入剪映草稿，需在剪映里重做");
        }
        if keys > 0 {
            self.notes.note("关键帧未写入剪映草稿，动画需在剪映里重做");
        }
        if curves > 0 {
            self.notes
                .note("变速曲线未写入，剪映里按平均速度播放");
        }
        if masks > 0 {
            self.notes
                .note("蒙版/抠像/防抖/智能构图未写入剪映草稿");
        }
        if loud > 0 {
            self.notes.note("超过 100% 的音量按 100% 写入剪映草稿");
        }
        if visual.iter().any(|clip| {
            clip.text.as_ref().is_some_and(|style| {
                !style.font_family.trim().is_empty() && style.font_family != "Noto Sans CJK SC"
            })
        }) {
            self.notes
                .note("自定义字体未随草稿导出，剪映里将用默认字体");
        }
        if !audio.is_empty() && audio.iter().any(|clip| clip.preserve_pitch && clip.speed != 1.0)
        {
            self.notes
                .note("变速人声的变调保持交给剪映默认处理");
        }
    }

    fn finish(self) -> (Value, Vec<String>) {
        let duration = self.timeline.duration();
        let video = self.video;
        let mut seed = self.seed;
        let notes = self.notes.lines();
        let content = json!({
            "canvas_config": {
                "height": video.height,
                "ratio": "original",
                "width": video.width
            },
            "color_space": 0,
            "config": {
                "adjust_max_index": 1,
                "attachment_info": [],
                "combination_max_index": 1,
                "export_range": null,
                "extract_audio_last_index": 1,
                "lyrics_recognition_id": "",
                "lyrics_sync": true,
                "lyrics_taskinfo": [],
                "maintrack_adsorb": true,
                "material_save_mode": 0,
                "multi_language_current": "none",
                "multi_language_list": [],
                "multi_language_main": "none",
                "multi_language_mode": "none",
                "original_sound_last_index": 1,
                "record_audio_last_index": 1,
                "sticker_max_index": 1,
                "subtitle_keywords_config": null,
                "subtitle_recognition_id": "",
                "subtitle_sync": true,
                "subtitle_taskinfo": [],
                "system_font_list": [],
                "video_mute": false,
                "zoom_info_params": null
            },
            "cover": null,
            "create_time": 0,
            "duration": micros(duration),
            "extra_info": null,
            "fps": video.rate(),
            "free_render_index_mode_on": false,
            "group_container": null,
            "id": hex_id(&mut seed),
            "keyframe_graph_list": [],
            "keyframes": {
                "adjusts": [], "audios": [], "effects": [], "filters": [],
                "handwrites": [], "stickers": [], "texts": [], "videos": []
            },
            "last_modified_platform": {
                "app_id": 3704,
                "app_source": "lv",
                "app_version": "5.9.0",
                "os": "windows"
            },
            "materials": {
                "ai_translates": [],
                "audio_balances": [],
                "audio_effects": [],
                "audio_fades": self.fades,
                "audio_track_indexes": [],
                "audios": self.audios,
                "beats": [],
                "canvases": [],
                "chromas": [],
                "color_curves": [],
                "digital_humans": [],
                "drafts": [],
                "effects": [],
                "flowers": [],
                "green_screens": [],
                "handwrites": [],
                "hsl": [],
                "images": [],
                "log_color_wheels": [],
                "loudnesses": [],
                "manual_deformations": [],
                "masks": [],
                "material_animations": [],
                "material_colors": [],
                "multi_language_refs": [],
                "placeholders": [],
                "plugin_effects": [],
                "primary_color_wheels": [],
                "realtime_denoises": [],
                "shapes": [],
                "smart_crops": [],
                "smart_relights": [],
                "sound_channel_mappings": [],
                "speeds": self.speeds,
                "stickers": [],
                "tail_leaders": [],
                "text_templates": [],
                "texts": self.texts,
                "time_marks": [],
                "transitions": self.transitions,
                "video_effects": [],
                "video_trackings": [],
                "videos": self.videos,
                "vocal_beautifys": [],
                "vocal_separations": []
            },
            "mutable_config": null,
            "name": self.name,
            "new_version": NEW_VERSION,
            "relationships": [],
            "render_index_track_mode_on": false,
            "retouch_cover": null,
            "source": "default",
            "static_cover_image_path": "",
            "time_marks": null,
            "tracks": self.tracks,
            "update_time": 0,
            "version": JIANYING_VERSION
        });
        (content, notes)
    }
}

/// The whole picture, for a material with no crop of its own.
fn full_crop() -> Value {
    json!({
        "lower_left_x": 0.0,
        "lower_left_y": 1.0,
        "lower_right_x": 1.0,
        "lower_right_y": 1.0,
        "upper_left_x": 0.0,
        "upper_left_y": 0.0,
        "upper_right_x": 1.0,
        "upper_right_y": 0.0
    })
}

/// Concat's four edges as 剪映's four corners. The reader in
/// [`crate::capcut`] takes it back the same way.
fn jianying_crop(crop: &Crop) -> Value {
    let crop = crop.tidy();
    json!({
        "lower_left_x": crop.left,
        "lower_left_y": 1.0 - crop.bottom,
        "lower_right_x": 1.0 - crop.right,
        "lower_right_y": 1.0 - crop.bottom,
        "upper_left_x": crop.left,
        "upper_left_y": crop.top,
        "upper_right_x": 1.0 - crop.right,
        "upper_right_y": crop.top
    })
}

/// 剪映's own built-in transition for one of Concat's packages: its name,
/// `resource_id` and `effect_id`, all of which 剪映 ships itself. Only the
/// free packages are listed - a VIP effect would show up locked - and an
/// id that matches none falls back to the crossfade in the caller.
fn jianying_transition(id: &str) -> Option<(&'static str, &'static str, &'static str)> {
    let entry = |name, resource_id, effect_id| Some((name, resource_id, effect_id));
    match id.rsplit('.').next().unwrap_or(id) {
        "dissolve" => entry("叠化", "6724845717472416269", "322577"),
        "blur-dissolve" => entry("模糊", "6911569618171597320", "4212596"),
        "fade-black" => entry("闪黑", "6724239388189921806", "321493"),
        "fade-white" => entry("闪白", "6724845376098013708", "322575"),
        "glitch-transition" | "glitch-warp" => entry("故障", "6725771847444468236", "2918080"),
        "zoom-transition" | "zoom-blur-transition" => entry("推近", "6724226861666144779", "359359"),
        "push" => entry("左移", "6726711499676455435", "2917286"),
        "slide" => entry("滑动", "6757982416649851399", "4212349"),
        "linear-wipe" => entry("向右擦除", "6724849898857959950", "2917284"),
        "clock-wipe" => entry("圆形扫描", "6851775006418932238", "813992"),
        "iris" => entry("圆形遮罩", "6725767129519362573", "2916676"),
        "spin" => entry("顺时针旋转", "6724226684721041932", "359421"),
        "swirl" => entry("漩涡", "6851810799510360583", "4211780"),
        "rgb-split-wipe" => entry("色差顺时针", "6940520116035523080", "1069374"),
        "cube" => entry("立方体", "6785042367498949127", "519784"),
        "shape-wipe" => entry("横向分割", "7083771238564237861", "4211685"),
        "whip-pan" => entry("左移", "6726711499676455435", "2917286"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capcut::{self, Draft as ReadDraft, DraftTimeline};
    use crate::model::{
        AppliedFilter, Clip, ClipKey, Crop, KeyEase, KeyProperty, MediaItem, Track, Transition,
    };
    use std::sync::Arc;

    /// A project with one clip of everything an export maps, for the
    /// structure tests and the round trip.
    fn fixture() -> Project {
        let mut project = Project::new();
        project.media = vec![
            MediaItem {
                id: "m1".into(),
                path: "C:/media/first.mp4".into(),
                name: "first.mp4".into(),
                duration: Some(12.0),
                kind: MediaKind::Video,
                width: Some(1920),
                height: Some(1080),
                has_audio: true,
                ..MediaItem::default()
            },
            MediaItem {
                id: "m2".into(),
                path: "C:/media/logo.png".into(),
                name: "logo.png".into(),
                duration: None,
                kind: MediaKind::Image,
                width: Some(400),
                height: Some(400),
                ..MediaItem::default()
            },
            MediaItem {
                id: "m3".into(),
                path: "C:/media/music.mp3".into(),
                name: "music.mp3".into(),
                duration: Some(30.0),
                kind: MediaKind::Audio,
                ..MediaItem::default()
            },
        ];
        let timeline = project.active_mut();
        timeline.tracks = vec![
            Track {
                id: "T1".into(),
                ..Track::default()
            },
            Track {
                id: "T2".into(),
                ..Track::default()
            },
            Track {
                id: "T3".into(),
                ..Track::default()
            },
        ];
        let mut video = Clip::blank("c1", "T1", ClipKind::Video, "first.mp4", 0.0, 3.0);
        video.media_id = "m1".into();
        video.source_start = 1.5;
        video.speed = 2.0;
        video.volume = 0.8;
        video.fade_in = 0.2;
        video.offset_x = 0.1;
        video.offset_y = -0.05;
        video.scale = 1.2;
        video.rotation = 15.0;
        video.opacity = 0.6;
        video.video_effects = vec![AppliedFilter::new("concat.warm")];
        video.keys = vec![ClipKey {
            property: KeyProperty::Rotation,
            at: 0.5,
            value: 15.0,
            ease: KeyEase::LINEAR,
        }];
        video.crop = Some(Crop {
            left: 0.1,
            top: 0.0,
            right: 0.2,
            bottom: 0.0,
        });
        let mut second = Clip::blank("c2", "T1", ClipKind::Video, "first.mp4", 3.0, 2.0);
        second.media_id = "m1".into();
        second.source_start = 5.0;
        second.transition_in = Some(Transition {
            id: "concat.dissolve".into(),
            duration: 0.5,
            params: BTreeMap::new(),
        });
        let mut still = Clip::blank("c3", "T2", ClipKind::Image, "logo.png", 0.5, 2.5);
        still.media_id = "m2".into();
        let mut title = Clip::blank("c4", "T2", ClipKind::Text, "标题", 0.2, 4.0);
        title.text = Some(TextStyle {
            content: "星籁剪辑 · 可编辑草稿".into(),
            font_family: "Noto Sans CJK SC".into(),
            font_size: 0.07,
            font_weight: 700.0,
            color: "#ffffff".into(),
            stroke_width: 0.01,
            stroke_color: "#000000".into(),
            shadow: true,
            background: "#00000080".into(),
            ..TextStyle::default()
        });
        let mut music = Clip::blank("c5", "T3", ClipKind::Audio, "music.mp3", 0.0, 6.0);
        music.media_id = "m3".into();
        music.volume = 0.5;
        music.fade_out = 0.5;
        timeline.clips = vec![
            Arc::new(video),
            Arc::new(second),
            Arc::new(still),
            Arc::new(title),
            Arc::new(music),
        ];
        project
    }

    #[test]
    fn the_draft_has_the_shape_jianying_reads() {
        let project = fixture();
        let (content, notes) = content_seeded(&project, "测试草稿", 42);
        assert_eq!(content["version"], serde_json::json!(JIANYING_VERSION));
        assert_eq!(content["canvas_config"]["width"], serde_json::json!(1920));
        assert_eq!(content["fps"], serde_json::json!(30.0));
        assert_eq!(content["duration"], serde_json::json!(6_000_000));
        assert_eq!(content["materials"]["videos"].as_array().unwrap().len(), 3);
        assert_eq!(content["materials"]["audios"].as_array().unwrap().len(), 1);
        assert_eq!(content["materials"]["texts"].as_array().unwrap().len(), 1);
        assert_eq!(
            content["materials"]["transitions"].as_array().unwrap().len(),
            1
        );
        let tracks = content["tracks"].as_array().unwrap();
        let kinds: Vec<&str> = tracks
            .iter()
            .map(|track| track["type"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"video"));
        assert!(kinds.contains(&"audio"));
        assert!(kinds.contains(&"text"));
        // Two video lanes, one above the other, then audio and the title.
        let videos = kinds.iter().filter(|kind| **kind == "video").count();
        assert_eq!(videos, 2);
        // ids are the 32 hex characters every 剪映 id is.
        let id = content["id"].as_str().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|character| character.is_ascii_hexdigit()));
        for track in tracks {
            for segment in track["segments"].as_array().unwrap() {
                let material = segment["material_id"].as_str().unwrap();
                assert_eq!(material.len(), 32, "{material} is not an id");
                assert!(segment["target_timerange"]["duration"].as_i64().unwrap() > 0);
            }
        }
        assert!(notes.iter().any(|note| note.contains("关键帧")));
    }

    #[test]
    fn the_times_angles_and_places_are_the_readers_inverse() {
        let project = fixture();
        let (content, _) = content_seeded(&project, "round trip", 7);
        let tracks = content["tracks"].as_array().unwrap();
        let video = tracks
            .iter()
            .find(|track| track["type"] == "video")
            .expect("a video track");
        let first = &video["segments"][0];
        assert_eq!(first["target_timerange"]["start"], serde_json::json!(0));
        assert_eq!(
            first["target_timerange"]["duration"],
            serde_json::json!(3_000_000)
        );
        // A 2x clip covers twice its timeline length of source.
        assert_eq!(
            first["source_timerange"]["duration"],
            serde_json::json!(6_000_000)
        );
        assert_eq!(first["source_timerange"]["start"], serde_json::json!(1_500_000));
        assert_eq!(first["clip"]["scale"]["x"], serde_json::json!(1.2));
        assert_eq!(first["clip"]["transform"]["x"], serde_json::json!(0.2));
        // 剪映's y goes up; Concat's offset goes down.
        assert_eq!(first["clip"]["transform"]["y"], serde_json::json!(0.1));
        assert_eq!(first["clip"]["rotation"], serde_json::json!(15.0));
        assert_eq!(first["clip"]["alpha"], serde_json::json!(0.6));
        assert_eq!(first["speed"], serde_json::json!(2.0));
        // The crossfade hangs on the clip *before* the cut, as 剪映 stores it.
        let transitions = content["materials"]["transitions"].as_array().unwrap();
        let id = transitions[0]["id"].as_str().unwrap();
        assert!(
            video["segments"][0]["extra_material_refs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|extra| extra == id),
            "the transition is on the outgoing clip"
        );
        // Titles: 剪映's size unit is 15 for Concat's 0.09 of the frame.
        let text = content["materials"]["texts"][0].clone();
        assert!(text["content"].as_str().unwrap().contains("星籁剪辑"));
        let run = &text["content"]
            .as_str()
            .map(|content| serde_json::from_str::<Value>(content).unwrap())
            .unwrap()["styles"][0];
        assert!(
            (run["size"].as_f64().unwrap() - 0.07 / TEXT_SIZE).abs() < 0.01,
            "{}",
            run["size"]
        );
        assert_eq!(run["fill"]["content"]["solid"]["color"][0], json!(1.0));
    }

    #[test]
    fn an_import_of_our_own_draft_gives_the_cut_back() {
        let project = fixture();
        let (content, _) = content_seeded(&project, "round trip", 11);
        let draft = ReadDraft {
            name: "round trip".into(),
            folder: "C:/media".into(),
            timelines: vec![DraftTimeline {
                name: "Timeline 1".into(),
                edit: content,
            }],
        };
        let imported = capcut::import(&draft).expect("our own draft reads back");
        let timeline = imported.project.active();
        let videos = timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Video)
            .count();
        let audios = timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Audio)
            .count();
        let titles = timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Text)
            .count();
        assert_eq!((videos, audios, titles), (2, 1, 1));
        let first = timeline
            .clips
            .iter()
            .find(|clip| clip.start == 0.0 && clip.kind == ClipKind::Video)
            .expect("the opening clip");
        assert!((first.source_start - 1.5).abs() < 0.001);
        assert!((first.duration - 3.0).abs() < 0.001);
        // The 4/5 title lane came back as a title, words and all.
        let title = timeline
            .clips
            .iter()
            .find(|clip| clip.kind == ClipKind::Text)
            .expect("the title");
        assert_eq!(
            title.text.as_ref().map(|text| text.content.as_str()),
            Some("星籁剪辑 · 可编辑草稿")
        );
    }

    #[test]
    fn writing_makes_the_folder_and_says_what_was_left_out() {
        let project = fixture();
        let folder = std::env::temp_dir().join(format!("concat-jianying-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let written = write(&project, "示例工程", &folder).expect("writes");
        assert!(written.files.iter().any(|file| file.ends_with("draft_content.json")));
        assert!(written.files.iter().any(|file| file.ends_with("draft_meta_info.json")));
        assert!(folder.join("materials/videos").is_dir());
        assert!(folder.join("materials/texts").is_dir());
        let content: Value =
            serde_json::from_slice(&std::fs::read(folder.join("draft_content.json")).unwrap())
                .expect("valid JSON");
        assert_eq!(content["version"], serde_json::json!(JIANYING_VERSION));
        let meta: Value =
            serde_json::from_slice(&std::fs::read(folder.join("draft_meta_info.json")).unwrap())
                .expect("valid JSON");
        assert_eq!(meta["draft_name"], serde_json::json!("示例工程"));
        assert_eq!(meta["tm_duration"], serde_json::json!(6_000_000));
        assert!(written.notes.iter().any(|note| note.contains("关键帧")));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn titles_that_share_a_lane_pile_into_separate_text_lanes() {
        // Two of Concat's lanes' titles overlapping in time (a typing title
        // over captions) must not share one 剪映 text lane: 剪映 wants a
        // lane's segments end to end.
        let project = fixture();
        let mut project = project;
        let mut second = Clip::blank("c9", "T3", ClipKind::Text, "同时的标题", 0.5, 2.0);
        second.text = Some(TextStyle {
            content: "另一条同时的标题".into(),
            ..TextStyle::default()
        });
        project.active_mut().clips.push(Arc::new(second));
        let (content, _) = content_seeded(&project, "two titles", 3);
        let text_tracks: Vec<&Value> = content["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|track| track["type"] == "text")
            .collect();
        assert_eq!(text_tracks.len(), 2, "overlapping titles need two lanes");
        for track in &text_tracks {
            let mut end = 0i64;
            for segment in track["segments"].as_array().unwrap() {
                let start = segment["target_timerange"]["start"].as_i64().unwrap();
                assert!(start >= end, "segments of one lane overlap");
                end = start + segment["target_timerange"]["duration"].as_i64().unwrap();
            }
        }
    }

    #[test]
    fn a_free_folder_never_collides() {
        let root = std::env::temp_dir().join(format!("concat-jianying-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("我的片子")).unwrap();
        assert_eq!(
            free_folder(&root, "我的片子"),
            root.join("我的片子 (2)")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
