// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! A Concat project as XML another editor can open.
//!
//! Two dialects are written from the same walk over the timeline:
//!
//! * **FCPXML 1.9** (`.fcpxml`) - Final Cut Pro 10.6+ and DaVinci Resolve.
//!   The bottom-most picture lane is the primary storyline; every other
//!   picture lane, every sound lane and every title becomes a connected
//!   clip on its own lane above (pictures, titles) or below (sound). Titles
//!   are written as FCPXML titles, so the words come across as editable
//!   text rather than baked-in pictures.
//! * **Premiere-compatible XML** (`.xml`, FCP 7 `xmeml` version 4) -
//!   Premiere Pro imports this with its tracks intact. Premiere's format
//!   has no text clip that does not point at an external title file, so the
//!   titles travel beside it as a `.srt` subtitle file instead.
//!
//! Neither dialect survives every effect Concat can make: filters, masks,
//! keyframes and speed curves are not written, and the notes say so. What
//! does cross over is the cut: every lane, every clip's place, in-point,
//! length, speed and level, the picture's size, place, turn and opacity,
//! and the titles' words and timing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::{Clip, ClipKind, MediaItem, MediaKind, Project, TextAlign, VideoSettings};

/// What one XML export wrote.
#[derive(Clone, Debug, Default)]
pub struct XmlWritten {
    /// Where the files went.
    pub folder: PathBuf,
    /// Every file written, the FCPXML first.
    pub files: Vec<PathBuf>,
    /// What did not come across, once per kind.
    pub notes: Vec<String>,
    /// The timeline's length in seconds.
    pub duration: f64,
    /// Frames the sequence is long, at the timeline's rate.
    pub frames: i64,
    /// How many clips were written in all.
    pub clips: usize,
}

/// Writes `project` as FCPXML, Premiere XML and (when it has titles) SRT
/// into `folder`, making it.
pub fn write(project: &Project, name: &str, folder: &Path) -> Result<XmlWritten, String> {
    if project.active().clips.is_empty() {
        return Err("时间线还是空的：先放几段素材再导出 XML".to_owned());
    }
    let (fcpxml, mut notes) = document(project, name);
    let (premiere, premiere_notes) = premiere(project, name);
    notes.extend(premiere_notes);
    std::fs::create_dir_all(folder).map_err(|error| format!("创建导出文件夹失败：{error}"))?;
    let stem = file_stem(name);
    let mut written = XmlWritten {
        folder: folder.to_path_buf(),
        duration: project.active().duration(),
        frames: Clock::of(project.active().video).frames(project.active().duration()),
        ..XmlWritten::default()
    };
    let mut files: Vec<(String, String)> = vec![
        (format!("{stem}.fcpxml"), fcpxml),
        (format!("{stem}.xml"), premiere),
    ];
    let cues = crate::srt::title_cues(project.active(), None);
    if !cues.is_empty() {
        files.push((format!("{stem}.srt"), crate::srt::write(&cues)));
        notes.push(format!(
            "字幕另存为 {stem}.srt：FCPXML 里是可编辑文本，Premiere XML 需导入这份 SRT"
        ));
    }
    for (file_name, text) in files {
        let path = folder.join(&file_name);
        std::fs::write(&path, text).map_err(|error| format!("写入 {file_name} 失败：{error}"))?;
        written.files.push(path);
    }
    written.notes = notes;
    written.clips = project.active().clips.len();
    Ok(written)
}

/// The FCPXML 1.9 document for `project` and what could not be mapped.
pub fn document(project: &Project, name: &str) -> (String, Vec<String>) {
    let timeline = project.active();
    let clock = Clock::of(timeline.video);
    let total = clock.frames(timeline.duration()).max(1);
    let mut notes = Vec::new();

    // Which lanes hold what. Lanes are bottom-first in the model, which is
    // the order FCP stacks them in: the first picture lane is the primary
    // storyline.
    let mut pictures: Vec<(&str, Vec<&Clip>)> = Vec::new();
    let mut sounds: Vec<(&str, Vec<&Clip>)> = Vec::new();
    let mut titles: Vec<&Clip> = Vec::new();
    for track in &timeline.tracks {
        let mut visuals: Vec<&Clip> = Vec::new();
        let mut audio: Vec<&Clip> = Vec::new();
        for clip in timeline.clips.iter() {
            if clip.track_id != track.id {
                continue;
            }
            match clip.kind {
                ClipKind::Video | ClipKind::Image => visuals.push(clip),
                ClipKind::Audio => audio.push(clip),
                ClipKind::Text => titles.push(clip),
                _ => {}
            }
        }
        visuals.sort_by(|a, b| a.start.total_cmp(&b.start));
        audio.sort_by(|a, b| a.start.total_cmp(&b.start));
        if !visuals.is_empty() {
            pictures.push((track.id.as_str(), visuals));
        }
        if !audio.is_empty() {
            sounds.push((track.id.as_str(), audio));
        }
    }
    titles.sort_by(|a, b| a.start.total_cmp(&b.start));
    if pictures.is_empty() {
        notes.push("时间线没有画面轨，FCPXML 是一条空序列加声音".to_owned());
    }
    if timeline.clips.iter().any(|clip| {
        matches!(
            clip.kind,
            ClipKind::Layer | ClipKind::SoundLayer | ClipKind::Shape | ClipKind::Transition
        )
    }) {
        notes.push("图层/形状/独立转场片段没有对应的 FCPXML 元素，已省略".to_owned());
    }
    if timeline.clips.iter().any(|clip| {
        !clip.video_effects.is_empty()
            || !clip.filters.is_empty()
            || !clip.keys.is_empty()
            || clip.speed_curve.is_some()
            || !clip.masks.is_empty()
            || clip.cutout.is_some()
    }) {
        notes.push("特效/滤镜/蒙版/关键帧/变速曲线不写入 XML，需在 PR/达芬奇里重做".to_owned());
    }

    // Every file used, in the order the walk meets it.
    let mut assets = Assets::default();
    let primary = pictures.first().map(|(_, clips)| clips.clone()).unwrap_or_default();
    for clip in &primary {
        assets.asset(clip, project, &clock, total);
    }
    for (_, clips) in pictures.iter().skip(1) {
        for clip in clips {
            assets.asset(clip, project, &clock, total);
        }
    }
    for (_, clips) in &sounds {
        for clip in clips {
            assets.asset(clip, project, &clock, total);
        }
    }

    // The primary storyline: the clips, and gaps where nothing plays.
    let mut anchors: Vec<Anchor> = Vec::new();
    let mut cursor = 0i64;
    for clip in &primary {
        let start = clock.frames(clip.start).min(total);
        let end = clock.frames(clip.start + clip.duration).min(total);
        if start > cursor {
            anchors.push(Anchor {
                start: cursor,
                end: start,
                clip: None,
                children: Vec::new(),
            });
        }
        anchors.push(Anchor {
            start,
            end: end.max(start + 1).min(total.max(start + 1)),
            clip: Some(clip),
            children: Vec::new(),
        });
        cursor = anchors.last().map(|anchor| anchor.end).unwrap_or(cursor);
    }
    if anchors.is_empty() || cursor < total {
        anchors.push(Anchor {
            start: cursor,
            end: total,
            clip: None,
            children: Vec::new(),
        });
    }
    // Clips that start before the first picture and the trailing gap both
    // need somewhere to hang; a gap that covers no frame is not a child.
    let attach = |anchors: &mut Vec<Anchor>, at: i64| -> Option<usize> {
        anchors
            .iter()
            .position(|anchor| anchor.start <= at && at < anchor.end)
    };

    // The primary picture's own in-point travels as the clip's start; each
    // overlay and sound is split at the primary clips' edges so it can hang
    // off one spine element at a time.
    let mut connected: Vec<Connected> = Vec::new();
    for (lane, (_, clips)) in pictures.iter().enumerate().skip(1) {
        for clip in clips {
            connected.push(Connected {
                clip,
                lane: lane as i64,
                kind: ConnectedKind::Picture,
            });
        }
    }
    for (lane, (_, clips)) in sounds.iter().enumerate() {
        for clip in clips {
            connected.push(Connected {
                clip,
                lane: -(lane as i64 + 1),
                kind: ConnectedKind::Sound,
            });
        }
    }
    for clip in &titles {
        connected.push(Connected {
            clip,
            lane: pictures.len() as i64 + 1,
            kind: ConnectedKind::Title,
        });
    }
    for piece in connected {
        let start = clock.frames(piece.clip.start).max(0);
        let end = clock.frames(piece.clip.start + piece.clip.duration).max(start + 1);
        let mut at = start;
        while at < end {
            let Some(index) = attach(&mut anchors, at) else {
                // Before the first anchor or past the end: a gap is added
                // below for the tail, and the head is covered by the first
                // anchor's own start.
                break;
            };
            let anchor = &anchors[index];
            let piece_end = end.min(anchor.end).max(at + 1);
            let element = piece.element(at, piece_end - at, anchor.start, project, &assets, &clock);
            anchors[index].children.push(element);
            at = piece_end;
        }
    }

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n");
    xml.push_str("<fcpxml version=\"1.9\">\n");
    xml.push_str("  <resources>\n");
    xml.push_str(&format!(
        "    <format id=\"r1\" name=\"FFVideoFormat{}x{}p{}\" frameDuration=\"{}\" width=\"{}\" height=\"{}\"/>\n",
        timeline.video.width,
        timeline.video.height,
        clock.fps().round() as i64,
        clock.frame_duration(),
        timeline.video.width,
        timeline.video.height
    ));
    let mut resource = 2;
    for asset in &assets.order {
        xml.push_str(&asset.xml(resource));
        resource += 1;
    }
    if !titles.is_empty() {
        xml.push_str(
            "    <effect id=\"r-title\" name=\"Basic Title\" uid=\".../Titles.localized/Basic Text/Basic Title.moti\"/>\n",
        );
    }
    xml.push_str("  </resources>\n");
    xml.push_str("  <library>\n");
    xml.push_str(&format!(
        "    <event name=\"{}\">\n",
        escape_attribute(&file_stem(name))
    ));
    xml.push_str(&format!(
        "      <project name=\"{}\">\n",
        escape_attribute(name)
    ));
    xml.push_str(&format!(
        "        <sequence format=\"r1\" duration=\"{}\" tcStart=\"0s\" tcFormat=\"NDF\" audioLayout=\"stereo\" audioRate=\"48k\">\n",
        clock.time(total)
    ));
    xml.push_str("          <spine>\n");
    for anchor in &anchors {
        let offset = clock.time(anchor.start);
        let duration = clock.time(anchor.end - anchor.start);
        let (element, head) = match anchor.clip {
            Some(clip) => {
                let media = project.media_by_id(&clip.media_id);
                let reference = media
                    .map(|media| assets.id(&media.id))
                    .unwrap_or_else(|| "r2".to_owned());
                let start = clock.time(clock.frames(clip.source_start));
                (
                    "asset-clip",
                    format!(
                        "<asset-clip ref=\"{reference}\" name=\"{}\" offset=\"{offset}\" duration=\"{duration}\" start=\"{start}\" format=\"r1\"",
                        escape_attribute(&clip.name)
                    ),
                )
            }
            None => (
                "gap",
                format!("<gap offset=\"{offset}\" duration=\"{duration}\""),
            ),
        };
        if anchor.children.is_empty() {
            xml.push_str(&format!("            {head}/>\n"));
        } else {
            xml.push_str(&format!("            {head}>\n"));
            for child in &anchor.children {
                xml.push_str(child);
            }
            xml.push_str(&format!("            </{element}>\n"));
        }
    }
    xml.push_str("          </spine>\n");
    xml.push_str("        </sequence>\n");
    xml.push_str("      </project>\n");
    xml.push_str("    </event>\n");
    xml.push_str("  </library>\n");
    xml.push_str("</fcpxml>\n");
    (xml, notes)
}

/// The Premiere-compatible (FCP 7 `xmeml` version 4) document for
/// `project`, and what could not be mapped.
pub fn premiere(project: &Project, name: &str) -> (String, Vec<String>) {
    let timeline = project.active();
    let clock = Clock::of(timeline.video);
    let total = clock.frames(timeline.duration()).max(1);
    let mut notes = Vec::new();
    if timeline.clips.iter().any(|clip| clip.kind == ClipKind::Text) {
        notes.push("Premiere XML 不带字幕轨：字幕以同名 .srt 给出，导入 PR 后可当字幕轨用".to_owned());
    }
    let mut picture_lanes: Vec<Vec<&Clip>> = Vec::new();
    let mut sound_lanes: Vec<Vec<&Clip>> = Vec::new();
    for track in &timeline.tracks {
        let mut visuals: Vec<&Clip> = Vec::new();
        let mut audio: Vec<&Clip> = Vec::new();
        for clip in timeline.clips.iter() {
            if clip.track_id != track.id {
                continue;
            }
            match clip.kind {
                ClipKind::Video | ClipKind::Image => visuals.push(clip),
                ClipKind::Audio => audio.push(clip),
                _ => {}
            }
        }
        visuals.sort_by(|a, b| a.start.total_cmp(&b.start));
        audio.sort_by(|a, b| a.start.total_cmp(&b.start));
        if !visuals.is_empty() {
            picture_lanes.push(visuals);
        }
        if !audio.is_empty() {
            sound_lanes.push(audio);
        }
    }
    let mut file_count = 0usize;
    let mut file_ids: HashMap<String, String> = HashMap::new();
    let mut xml = String::new();
    let rate = format!(
        "            <rate><timebase>{}</timebase><ntsc>{}</ntsc></rate>\n",
        clock.timebase(),
        if clock.ntsc() { "TRUE" } else { "FALSE" }
    );
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n");
    xml.push_str("<xmeml version=\"4\">\n");
    xml.push_str("  <sequence id=\"sequence-1\">\n");
    xml.push_str(&format!(
        "    <name>{}</name>\n",
        escape_text(&file_stem(name))
    ));
    xml.push_str(&format!("    <duration>{total}</duration>\n"));
    xml.push_str(&rate.replace("            ", "    "));
    xml.push_str("    <media>\n      <video>\n");
    xml.push_str("        <format>\n          <samplecharacteristics>\n");
    xml.push_str(&format!(
        "            <width>{}</width>\n            <height>{}</height>\n            <pixelaspectratio>square</pixelaspectratio>\n            <anamorphic>FALSE</anamorphic>\n",
        timeline.video.width, timeline.video.height
    ));
    xml.push_str(&rate);
    xml.push_str("          </samplecharacteristics>\n        </format>\n");
    for lane in &picture_lanes {
        xml.push_str("        <track>\n");
        for clip in lane {
            let media = project.media_by_id(&clip.media_id);
            xml.push_str(&clipitem(
                project, &clock, clip, media, &mut file_count, &mut file_ids, false,
            ));
        }
        xml.push_str("        </track>\n");
    }
    xml.push_str("      </video>\n");
    if !sound_lanes.is_empty() {
        xml.push_str("      <audio>\n        <format>\n          <samplecharacteristics>\n");
        xml.push_str("            <depth>16</depth>\n            <samplerate>48000</samplerate>\n");
        xml.push_str("          </samplecharacteristics>\n        </format>\n");
        for lane in &sound_lanes {
            xml.push_str("        <track>\n");
            for clip in lane {
                let media = project.media_by_id(&clip.media_id);
                xml.push_str(&clipitem(
                    project, &clock, clip, media, &mut file_count, &mut file_ids, true,
                ));
            }
            xml.push_str("        </track>\n");
        }
        xml.push_str("      </audio>\n");
    }
    xml.push_str("    </media>\n");
    xml.push_str("  </sequence>\n");
    xml.push_str("</xmeml>\n");
    (xml, notes)
}

/// One `<clipitem>` of the Premiere XML.
fn clipitem(
    project: &Project,
    clock: &Clock,
    clip: &Clip,
    media: Option<&MediaItem>,
    file_count: &mut usize,
    file_ids: &mut HashMap<String, String>,
    audio: bool,
) -> String {
    let _ = project;
    let start = clock.frames(clip.start);
    let length = clock.frames(clip.duration).max(1);
    let in_point = clock.frames(clip.source_start);
    // The source the clip covers: its speed is what the two differ by, which
    // Premiere's importer reads as a speed change.
    let out_point = in_point + clock.frames(clip.duration * clip.speed.max(0.01)).max(1);
    let mut item = String::new();
    item.push_str(&format!(
        "          <clipitem id=\"clipitem-{}\">\n",
        escape_attribute(&clip.id)
    ));
    item.push_str(&format!(
        "            <name>{}</name>\n",
        escape_text(&clip.name)
    ));
    item.push_str(&format!(
        "            <duration>{length}</duration>\n"
    ));
    item.push_str(&format!(
        "            <rate><timebase>{}</timebase><ntsc>{}</ntsc></rate>\n",
        clock.timebase(),
        if clock.ntsc() { "TRUE" } else { "FALSE" }
    ));
    item.push_str(&format!(
        "            <start>{start}</start>\n            <end>{}</end>\n            <in>{in_point}</in>\n            <out>{out_point}</out>\n",
        start + length
    ));
    match media {
        None => {
            item.push_str("            <file id=\"missing-media\">\n              <name>(缺失素材)</name>\n            </file>\n");
        }
        Some(media) => {
            if !file_ids.contains_key(&media.id) {
                *file_count += 1;
                file_ids.insert(media.id.clone(), format!("file-{file_count}"));
            }
            let file_id = file_ids.get(&media.id).cloned().unwrap_or_default();
            item.push_str(&format!("            <file id=\"{file_id}\">\n"));
            item.push_str(&format!(
                "              <name>{}</name>\n",
                escape_text(&media.name)
            ));
            item.push_str(&format!(
                "              <pathurl>{}</pathurl>\n",
                escape_text(&file_url(&media.path))
            ));
            item.push_str(&format!(
                "              <duration>{}</duration>\n",
                clock
                    .frames(media.duration.unwrap_or(clip.start + clip.duration))
                    .max(1)
            ));
            item.push_str(&format!(
                "              <rate><timebase>{}</timebase><ntsc>{}</ntsc></rate>\n",
                clock.timebase(),
                if clock.ntsc() { "TRUE" } else { "FALSE" }
            ));
            if media.kind != MediaKind::Audio {
                item.push_str("              <media>\n                <video>\n                  <samplecharacteristics>\n");
                item.push_str(&format!(
                    "                    <width>{}</width>\n                    <height>{}</height>\n",
                    media.width.unwrap_or(0),
                    media.height.unwrap_or(0)
                ));
                item.push_str(
                    "                    <pixelaspectratio>square</pixelaspectratio>\n                  </samplecharacteristics>\n                </video>\n",
                );
                if media.has_audio {
                    item.push_str("                <audio>\n                  <samplecharacteristics>\n                    <depth>16</depth>\n                    <samplerate>48000</samplerate>\n                  </samplecharacteristics>\n                  <channelcount>2</channelcount>\n                </audio>\n");
                }
                item.push_str("              </media>\n");
            } else {
                item.push_str("              <media>\n                <audio>\n                  <samplecharacteristics>\n                    <depth>16</depth>\n                    <samplerate>48000</samplerate>\n                  </samplecharacteristics>\n                  <channelcount>2</channelcount>\n                </audio>\n              </media>\n");
            }
            item.push_str("            </file>\n");
        }
    }
    if audio {
        item.push_str("            <sourcetrack><mediatype>audio</mediatype><trackindex>1</trackindex></sourcetrack>\n");
        item.push_str(&format!(
            "            <volume><level>{:.4}</level></volume>\n",
            clip.volume.clamp(0.0, 2.0)
        ));
    } else {
        if clip.kind == ClipKind::Image {
            item.push_str("            <stillframe>TRUE</stillframe>\n");
        }
        // Place, size, turn and opacity: the Basic Motion pair Premiere
        // reads as its own Motion and Opacity effects.
        item.push_str("            <filter>\n              <effect>\n                <name>Basic Motion</name>\n                <effectid>basic</effectid>\n                <effectcategory>motion</effectcategory>\n                <effecttype>motion</effecttype>\n                <mediatype>video</mediatype>\n");
        item.push_str(&format!(
            "                <parameter><parameterid>scale</parameterid><name>Scale</name><value>{:.2}</value></parameter>\n",
            clip.scale * 100.0
        ));
        item.push_str(&format!(
            "                <parameter><parameterid>rotation</parameterid><name>Rotation</name><value>{:.2}</value></parameter>\n",
            clip.rotation
        ));
        item.push_str(&format!(
            "                <parameter><parameterid>center</parameterid><name>Center</name><value><horiz>{:.4}</horiz><vert>{:.4}</vert></value></parameter>\n",
            clip.offset_x, clip.offset_y
        ));
        item.push_str("              </effect>\n            </filter>\n");
        item.push_str(&format!(
            "            <filter>\n              <effect>\n                <name>Opacity</name>\n                <effectid>opacity</effectid>\n                <effectcategory>opacity</effectcategory>\n                <effecttype>motion</effecttype>\n                <mediatype>video</mediatype>\n                <parameter><parameterid>opacity</parameterid><name>Opacity</name><value>{:.2}</value></parameter>\n              </effect>\n            </filter>\n",
            clip.opacity.clamp(0.0, 1.0) * 100.0
        ));
    }
    item.push_str("          </clipitem>\n");
    item
}

/// One connected clip of the FCPXML, as many pieces as the primary
/// storyline splits it into.
struct Connected<'a> {
    clip: &'a Clip,
    lane: i64,
    kind: ConnectedKind,
}

enum ConnectedKind {
    Picture,
    Sound,
    Title,
}

impl Connected<'_> {
    /// The element for the piece of this clip that plays from `at` for
    /// `frames` frames, hanging off a spine element starting at `anchor`.
    fn element(
        &self,
        at: i64,
        frames: i64,
        anchor: i64,
        project: &Project,
        assets: &Assets,
        clock: &Clock,
    ) -> String {
        let offset = clock.time(at - anchor);
        let duration = clock.time(frames);
        match self.kind {
            ConnectedKind::Picture | ConnectedKind::Sound => {
                let media = project.media_by_id(&self.clip.media_id);
                let reference = media
                    .map(|media| assets.id(&media.id))
                    .unwrap_or_else(|| "r2".to_owned());
                let start = clock.time(
                    clock.frames(self.clip.source_start)
                        + (at - clock.frames(self.clip.start)).max(0),
                );
                let role = if matches!(self.kind, ConnectedKind::Sound) {
                    " audioRole=\"dialogue\""
                } else {
                    ""
                };
                format!(
                    "              <asset-clip lane=\"{}\" ref=\"{reference}\" name=\"{}\" offset=\"{offset}\" duration=\"{duration}\" start=\"{start}\"{role} format=\"r1\"/>\n",
                    self.lane,
                    escape_attribute(&self.clip.name)
                )
            }
            ConnectedKind::Title => {
                let style = self.clip.text.as_ref();
                let (words, font_size, colour, align) = match style {
                    Some(style) => (
                        style.display_content(),
                        (style.font_size * f64::from(clock.height)).max(8.0),
                        hex_channels(&style.color),
                        match style.align {
                            TextAlign::Left => "left",
                            TextAlign::Center => "center",
                            TextAlign::Right => "right",
                        },
                    ),
                    None => (self.clip.name.clone(), 48.0, "1 1 1 1".to_owned(), "center"),
                };
                let id = format!("title{}", escape_attribute(&self.clip.id));
                format!(
                    "              <title lane=\"{}\" ref=\"r-title\" name=\"{}\" offset=\"{offset}\" duration=\"{duration}\">\n                <text>\n                  <text-style ref=\"ts-{id}\">{}</text-style>\n                </text>\n                <text-style-def id=\"ts-{id}\">\n                  <text-style font=\"PingFang SC\" fontSize=\"{font_size:.1}\" fontColor=\"{colour}\" alignment=\"{align}\"/>\n                </text-style-def>\n              </title>\n",
                    self.lane,
                    escape_attribute(&self.clip.name),
                    escape_text(&words)
                )
            }
        }
    }
}

/// The assets of one FCPXML: each file used, and the resource id it got.
#[derive(Default)]
struct Assets {
    order: Vec<Asset>,
    ids: HashMap<String, String>,
}

struct Asset {
    media: MediaItem,
    /// The file's length as FCPXML rational time.
    duration: String,
}

impl Assets {
    /// Records the clip's media, with enough of the file to cover the clips
    /// that use it.
    fn asset(&mut self, clip: &Clip, project: &Project, clock: &Clock, total: i64) {
        if self.ids.contains_key(&clip.media_id) {
            return;
        }
        let Some(media) = project.media_by_id(&clip.media_id).cloned() else {
            return;
        };
        let needed = clock.frames(clip.source_start + clip.duration * clip.speed.max(0.01));
        let duration = media
            .duration
            .map(|duration| clock.frames(duration))
            .filter(|duration| *duration > needed)
            .unwrap_or(needed.max(total))
            .max(1);
        let id = format!("r{}", self.order.len() + 2);
        self.ids.insert(media.id.clone(), id);
        self.order.push(Asset {
            media,
            duration: clock.time(duration),
        });
    }

    fn id(&self, media_id: &str) -> String {
        self.ids
            .get(media_id)
            .cloned()
            .unwrap_or_else(|| "r2".to_owned())
    }
}

impl Asset {
    fn xml(&self, resource: usize) -> String {
        let media = &self.media;
        let (video, audio) = match media.kind {
            MediaKind::Audio => (0, 1),
            MediaKind::Image => (1, 0),
            MediaKind::Video => (1, u8::from(media.has_audio)),
        };
        let mut line = format!(
            "    <asset id=\"r{resource}\" name=\"{}\" start=\"0s\" duration=\"{}\" hasVideo=\"{video}\" hasAudio=\"{audio}\"",
            escape_attribute(&media.name),
            self.duration,
        );
        if video == 1 {
            line.push_str(" format=\"r1\"");
        }
        if audio == 1 {
            line.push_str(" audioSources=\"1\" audioChannels=\"2\" audioRate=\"48000\"");
        }
        line.push_str(&format!(
            ">\n      <media-rep kind=\"original-media\" src=\"{}\"/>\n    </asset>\n",
            escape_attribute(&file_url(&media.path))
        ));
        line
    }
}

/// The primary storyline's elements: either a picture clip of the primary
/// lane or a gap, and whatever hangs off it.
struct Anchor<'a> {
    start: i64,
    end: i64,
    clip: Option<&'a Clip>,
    children: Vec<String>,
}

/// A timebase that turns seconds into whole frames and back.
struct Clock {
    num: i64,
    den: i64,
    height: u32,
}

impl Clock {
    fn of(video: VideoSettings) -> Clock {
        Clock {
            num: video.rate_num.max(1),
            den: video.rate_den.max(1),
            height: video.height,
        }
    }

    fn fps(&self) -> f64 {
        self.num as f64 / self.den as f64
    }

    fn frames(&self, seconds: f64) -> i64 {
        if !seconds.is_finite() {
            return 0;
        }
        (seconds * self.fps()).round() as i64
    }

    /// Frames as FCPXML's rational time, reduced: `"3s"`, `"1001/30000s"`.
    fn time(&self, frames: i64) -> String {
        let mut numerator = frames.max(0) * self.den;
        let mut denominator = self.num;
        let divisor = gcd(numerator, denominator);
        if divisor > 1 {
            numerator /= divisor;
            denominator /= divisor;
        }
        if denominator == 1 {
            format!("{numerator}s")
        } else {
            format!("{numerator}/{denominator}s")
        }
    }

    fn frame_duration(&self) -> String {
        let divisor = gcd(self.den, self.num);
        format!("{}/{}s", self.den / divisor, self.num / divisor)
    }

    /// The whole-number timebase Premiere's XML counts in: 30 for 29.97.
    fn timebase(&self) -> i64 {
        let rate = self.fps();
        if (rate - 29.97).abs() < 0.02 || (rate - 59.94).abs() < 0.04 {
            (rate.round() as i64).max(1)
        } else {
            (rate.round() as i64).max(1)
        }
    }

    fn ntsc(&self) -> bool {
        let rate = self.fps();
        (rate - 29.97).abs() < 0.02 || (rate - 59.94).abs() < 0.04
    }
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a.max(1)
}

/// A file as a URL another machine can read: forward slashes, and percent
/// escapes for everything a URL cannot carry.
fn file_url(path: &str) -> String {
    let mut url = String::from("file:///");
    for byte in path.replace('\\', "/").trim_start_matches('/').bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b':' | b'~' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

fn hex_channels(colour: &str) -> String {
    let digits = colour.trim().trim_start_matches('#');
    let channel = |at: usize| {
        u8::from_str_radix(digits.get(at..at + 2).unwrap_or("ff"), 16).unwrap_or(255) as f64 / 255.0
    };
    if digits.len() >= 6 {
        format!(
            "{:.4} {:.4} {:.4} 1",
            channel(0),
            channel(2),
            channel(4)
        )
    } else {
        "1 1 1 1".to_owned()
    }
}

/// A name as a file name: the characters no file system takes become `_`.
fn file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| match character {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "星籁剪辑导出".to_owned()
    } else {
        cleaned.to_owned()
    }
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attribute(text: &str) -> String {
    escape_text(text)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clip, MediaItem, Timeline, Track, VideoSettings};
    use std::sync::Arc;

    fn fixture() -> Project {
        let mut project = Project {
            timelines: vec![Arc::new(Timeline {
                id: "TL1".into(),
                name: "时间线 1".into(),
                video: VideoSettings {
                    width: 1920,
                    height: 1080,
                    rate_num: 30,
                    rate_den: 1,
                    ..VideoSettings::default()
                },
                ..Timeline::default()
            })],
            active_timeline_id: "TL1".into(),
            ..Project::default()
        };
        project.media = vec![
            MediaItem {
                id: "m1".into(),
                path: "C:/素材/first clip.mp4".into(),
                name: "first clip.mp4".into(),
                duration: Some(10.0),
                kind: MediaKind::Video,
                width: Some(1920),
                height: Some(1080),
                has_audio: true,
                ..MediaItem::default()
            },
            MediaItem {
                id: "m2".into(),
                path: "C:/素材/music.mp3".into(),
                name: "music.mp3".into(),
                duration: Some(20.0),
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
        let mut opening = Clip::blank("c1", "T1", ClipKind::Video, "first clip.mp4", 0.0, 3.0);
        opening.media_id = "m1".into();
        opening.source_start = 1.0;
        let mut overlay = Clip::blank("c2", "T2", ClipKind::Video, "first clip.mp4", 1.5, 1.0);
        overlay.media_id = "m1".into();
        overlay.offset_x = 0.2;
        overlay.opacity = 0.7;
        let mut title = Clip::blank("c3", "T2", ClipKind::Text, "标题", 0.5, 2.0);
        title.text = Some(crate::model::TextStyle {
            content: "你好 & <星籁>".into(),
            font_size: 0.06,
            ..crate::model::TextStyle::default()
        });
        let mut music = Clip::blank("c4", "T3", ClipKind::Audio, "music.mp3", 0.0, 4.0);
        music.media_id = "m2".into();
        music.volume = 0.6;
        timeline.clips = vec![
            Arc::new(opening),
            Arc::new(overlay),
            Arc::new(title),
            Arc::new(music),
        ];
        project
    }

    /// Whether the bytes are well-formed XML: every tag closed in order,
    /// attributes quoted, entities only the five there are.
    fn well_formed(xml: &str) -> Result<(), String> {
        let bytes = xml.as_bytes();
        let mut at = 0usize;
        let mut stack: Vec<String> = Vec::new();
        while at < bytes.len() {
            if bytes[at] != b'<' {
                at += 1;
                continue;
            }
            // A comment, a declaration or a doctype: skipped.
            if xml[at..].starts_with("<?") {
                let end = xml[at..].find("?>").ok_or("unclosed declaration")?;
                at += end + 2;
                continue;
            }
            if xml[at..].starts_with("<!--") {
                let end = xml[at..].find("-->").ok_or("unclosed comment")?;
                at += end + 3;
                continue;
            }
            if xml[at..].starts_with("<!DOCTYPE") {
                let end = xml[at..].find('>').ok_or("unclosed doctype")?;
                at += end + 1;
                continue;
            }
            let close = xml[at..].find('>').ok_or("unclosed tag")?;
            let tag = &xml[at + 1..at + close];
            let closing = tag.starts_with('/');
            let self_closing = tag.ends_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .trim_end_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_owned();
            if name.is_empty() {
                return Err(format!("a tag with no name at {at}"));
            }
            // Quotes inside the tag must pair.
            let quotes = tag.matches('"').count();
            if quotes % 2 != 0 {
                return Err(format!("unbalanced quotes in <{tag}>"));
            }
            if closing {
                let open = stack.pop().ok_or_else(|| format!("</{name}> with nothing open"))?;
                if open != name {
                    return Err(format!("</{name}> closes <{open}>"));
                }
            } else if !self_closing {
                stack.push(name);
            }
            at += close + 1;
        }
        if let Some(open) = stack.pop() {
            return Err(format!("<{open}> never closed"));
        }
        Ok(())
    }

    #[test]
    fn the_fcpxml_is_well_formed_and_has_the_spine() {
        let project = fixture();
        let (xml, notes) = document(&project, "示例工程");
        well_formed(&xml).expect("well-formed");
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<fcpxml version=\"1.9\">"));
        assert!(xml.contains("frameDuration=\"1/30s\""));
        assert!(xml.contains("<asset-clip ref=\"r2\""));
        // The overlay is a connected clip on lane 1; the sound on lane -1.
        assert!(xml.contains("lane=\"1\""));
        assert!(xml.contains("lane=\"-1\""));
        // The title is a text element, not a picture.
        assert!(xml.contains("<title lane="));
        assert!(xml.contains("你好 &amp; &lt;星籁&gt;"));
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn the_premiere_xml_is_well_formed_and_has_the_tracks() {
        let project = fixture();
        let (xml, notes) = premiere(&project, "示例工程");
        well_formed(&xml).expect("well-formed");
        assert!(xml.contains("<xmeml version=\"4\">"));
        assert!(xml.contains("<sequence id=\"sequence-1\">"));
        let video_tracks = xml.matches("<track>").count();
        assert!(video_tracks >= 3, "picture and sound tracks: {video_tracks}");
        assert!(xml.contains("<clipitem id=\"clipitem-c1\">"));
        assert!(xml.contains("<pathurl>file:///C:/%E7%B4%A0%E6%9D%90/first%20clip.mp4</pathurl>"));
        assert!(notes.iter().any(|note| note.contains("srt")));
    }

    #[test]
    fn writing_puts_the_documents_on_disk_with_an_srt() {
        let project = fixture();
        let folder = std::env::temp_dir().join(format!("concat-xml-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let written = write(&project, "示例工程", &folder).expect("writes");
        let names: Vec<String> = written
            .files
            .iter()
            .map(|file| file.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|name| name.ends_with(".fcpxml")));
        assert!(names.iter().any(|name| name.ends_with(".xml")));
        assert!(names.iter().any(|name| name.ends_with(".srt")));
        let srt = std::fs::read_to_string(folder.join("示例工程.srt")).expect("the srt");
        assert!(srt.contains("你好 & <星籁>"), "{srt}");
        assert!(srt.contains("00:00:00,500 --> 00:00:02,500"));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn an_empty_timeline_is_refused_in_words() {
        let project = Project::new();
        let error = write(
            &project,
            "空",
            &std::env::temp_dir().join("concat-xml-empty"),
        )
        .expect_err("refused");
        assert!(error.contains("空"), "{error}");
    }
}


