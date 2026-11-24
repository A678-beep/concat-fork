// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 专业后期底座（0.3.6）：示波器、色彩管理、QC、媒体校验、版本快照与
//! 交付包，以及命令面板。
//!
//! 这一半是窗口：读工程与预览帧、调引擎的调色链、开文件对话框、把结果
//! 写回 Slint；取帧 / 质检 / sha256 / 打包 / 平台请求全部在后台线程里跑，
//! 算完用 `host::spawn` 回到事件循环线程落屏——示波器不占 UI 线程。
//!
//! 示波器读的是预览那一帧：随包 ffmpeg 在播放头处取 1 帧、降到 256×144，
//! 再在 CPU 上算波形 / RGB Parade / 矢量 / 直方图。调色预览用同一帧，
//! 按 ASC CDL → LUT 的顺序算前后两张；LUT 本身通过引擎的 look 包套在
//! 片段链上（GPU 预览 + 导出都生效），不是只在面板里假演。
//!
//! 平台 `/api/post/*` 由 `concat-host::post` 调；平台不通时 QC / EDL /
//! OTIO / 交付包都有本地兜底，界面上的来源会如实写明是哪一边。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::host::{self, Shell};
use crate::platform;
use crate::studio::{Models, Studio};
use crate::ui::*;

/// 预览帧的采样尺寸（取帧与调色预览用）。
const FRAME_W: u32 = 256;
const FRAME_H: u32 = 144;
/// 示波器画布：取样 256×144，画成 2 倍大让格线与点清楚。
const SCOPE_W: u32 = 512;
const SCOPE_H: u32 = 288;

const TAB_SCOPE: i32 = 0;
const TAB_COLOR: i32 = 1;
const TAB_QC: i32 = 2;
const TAB_CHECK: i32 = 3;
const TAB_SNAPSHOT: i32 = 4;
const TAB_HANDOFF: i32 = 5;

const SCOPE_NAMES: [&str; 4] = ["波形图", "RGB Parade", "矢量示波器", "直方图"];

/// 色彩空间标签，与 Slint 的 space-labels 一一对应。
const SPACES: [&str; 6] = ["sRGB", "Rec.709", "ACEScct", "ACEScg", "HLG", "PQ"];

// ─────────────────────────────────────────────────────────────────────
// 状态
// ─────────────────────────────────────────────────────────────────────

/// ASC CDL：slope / offset / power 各三通道，外加饱和度。
#[derive(Clone, Copy, Debug)]
struct Cdl {
    slope: [f32; 3],
    offset: [f32; 3],
    power: [f32; 3],
    sat: f32,
}

impl Default for Cdl {
    fn default() -> Self {
        Cdl {
            slope: [1.0; 3],
            offset: [0.0; 3],
            power: [1.0; 3],
            sat: 1.0,
        }
    }
}

impl Cdl {
    fn is_identity(&self) -> bool {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        self.slope.iter().all(|v| near(*v, 1.0))
            && self.offset.iter().all(|v| near(*v, 0.0))
            && self.power.iter().all(|v| near(*v, 1.0))
            && near(self.sat, 1.0)
    }

    fn key(&self) -> String {
        let mut out = String::new();
        for channel in 0..3 {
            out.push_str(&format!(
                "{},",
                (self.slope[channel] * 1000.0).round() as i64
            ));
        }
        for channel in 0..3 {
            out.push_str(&format!(
                "{},",
                (self.offset[channel] * 1000.0).round() as i64
            ));
        }
        for channel in 0..3 {
            out.push_str(&format!(
                "{},",
                (self.power[channel] * 1000.0).round() as i64
            ));
        }
        out.push_str(&format!("{}", (self.sat * 1000.0).round() as i64));
        out
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "slope": self.slope,
            "offset": self.offset,
            "power": self.power,
            "sat": self.sat,
        })
    }

    fn from_json(value: &serde_json::Value) -> Cdl {
        let triple = |key: &str, fallback: [f32; 3]| {
            value
                .get(key)
                .and_then(|v| v.as_array())
                .map(|rows| {
                    let mut out = fallback;
                    for (slot, row) in out.iter_mut().zip(rows.iter()) {
                        if let Some(number) = row.as_f64() {
                            *slot = number as f32;
                        }
                    }
                    out
                })
                .unwrap_or(fallback)
        };
        Cdl {
            slope: triple("slope", [1.0; 3]),
            offset: triple("offset", [0.0; 3]),
            power: triple("power", [1.0; 3]),
            sat: value.get("sat").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
        }
    }
}

/// 一段可读的预览帧来源。
#[derive(Clone)]
struct FrameSource {
    path: String,
    seconds: f64,
    clip_name: String,
    source_start: f64,
    is_image: bool,
    fps: f64,
    /// `帧号|是否图片`：换帧才重新解码。
    key: String,
    timecode: String,
}

struct Frame {
    rgba: Vec<u8>,
}

#[derive(Clone)]
struct LutEntry {
    id: String,
    name: String,
    space: String,
    table: Option<Vec<f32>>,
    size: u32,
}

#[derive(Clone)]
struct Row {
    label: String,
    detail: String,
    level: String,
    at: f32,
    value: String,
}

struct PostState {
    open: bool,
    tab: i32,
    busy: bool,
    backend: String,
    hint: String,
    status_line: String,
    status_level: String,

    scope_view: i32,
    scope_live: bool,
    scope_info: String,
    frame_info: String,
    frame: Option<Frame>,
    frame_key: String,
    grade_key: String,
    pending: bool,
    force: bool,
    last_refresh: Option<Instant>,
    scope_image: Image,
    before_image: Image,
    after_image: Image,
    preview_info: String,

    project_space: i32,
    clip_space: i32,
    clip_line: String,
    space_info: String,
    luts: Vec<LutEntry>,
    lut_selected: i32,
    lut_strength: f32,
    lut_line: String,
    cdl: Cdl,

    qc_status: String,
    qc_line: String,
    qc_target: String,
    qc_busy: bool,
    qc_rows: Vec<Row>,
    qc_report: Option<serde_json::Value>,
    qc_clip: Option<(f64, f64, f64)>,
    qc_media_path: String,

    check_rows: Vec<Row>,
    check_line: String,
    check_busy: bool,

    snapshot_rows: Vec<Row>,
    snapshot_line: String,
    snapshot_pick: Option<String>,

    handoff_line: String,
    handoff_path: String,

    palette_open: bool,
    command_query: String,
}

impl PostState {
    fn new() -> Self {
        PostState {
            open: false,
            tab: TAB_SCOPE,
            busy: false,
            backend: "平台 /api/post：尚未检测".to_owned(),
            hint: "面向影视 / 短剧公司：示波器监看、色彩管理、交付 QC、媒体校验与一键交付包".to_owned(),
            status_line: String::new(),
            status_level: "info".to_owned(),
            scope_view: 0,
            scope_live: true,
            scope_info: String::new(),
            frame_info: String::new(),
            frame: None,
            frame_key: String::new(),
            grade_key: String::new(),
            pending: false,
            force: false,
            last_refresh: None,
            scope_image: Image::default(),
            before_image: Image::default(),
            after_image: Image::default(),
            preview_info: String::new(),
            project_space: 1,
            clip_space: 1,
            clip_line: "没有选中片段：选中一段视频再打色彩空间标签。".to_owned(),
            space_info: "色彩空间是交给下游的标签：写进 OTIO / 交付包与色彩说明，不改本机素材。HLG / PQ 与工程输出色彩空间一致时相安无事；ACES 系列只作交换 / 调色工作空间标记。".to_owned(),
            luts: Vec::new(),
            lut_selected: -1,
            lut_strength: 100.0,
            lut_line: "LUT：还没有导入（点「导入 .cube…」）。导入的 LUT 存本机，可反复套用。".to_owned(),
            cdl: Cdl::default(),
            qc_status: String::new(),
            qc_line: "还没有跑过质检：点「一键质检」先走平台，平台不通会落到本机。".to_owned(),
            qc_target: "质检对象：选中片段 / 时间线第一条素材".to_owned(),
            qc_busy: false,
            qc_rows: Vec::new(),
            qc_report: None,
            qc_clip: None,
            qc_media_path: String::new(),
            check_rows: Vec::new(),
            check_line: "还没有校验结果：点「计算校验和并比对」".to_owned(),
            check_busy: false,
            snapshot_rows: Vec::new(),
            snapshot_line: String::new(),
            snapshot_pick: None,
            handoff_line: String::new(),
            handoff_path: String::new(),
            palette_open: false,
            command_query: String::new(),
        }
    }

    fn set_status(&mut self, level: &str, line: impl Into<String>) {
        self.status_level = level.to_owned();
        self.status_line = line.into();
    }

    fn current_lut(&self) -> Option<&LutEntry> {
        if self.lut_selected < 0 {
            return None;
        }
        self.luts.get(self.lut_selected as usize)
    }

    fn grade_snapshot(&self) -> GradeJob {
        let lut = self
            .current_lut()
            .and_then(|entry| entry.table.clone().map(|table| (table, entry.size)))
            .map(|(table, size)| LutJob { table, size });
        GradeJob {
            cdl: self.cdl,
            lut,
            strength: self.lut_strength / 100.0,
        }
    }

    fn grade_key(&self) -> String {
        let lut = self.current_lut().map(|entry| entry.id.clone()).unwrap_or_default();
        format!("{}|{}|{}", self.cdl.key(), lut, self.lut_strength.round() as i64)
    }
}

#[derive(Clone)]
struct GradeJob {
    cdl: Cdl,
    lut: Option<LutJob>,
    strength: f32,
}

#[derive(Clone)]
struct LutJob {
    table: Vec<f32>,
    size: u32,
}

thread_local! {
    static POST: RefCell<PostState> = RefCell::new(PostState::new());
}

fn with_post<T>(f: impl FnOnce(&mut PostState) -> T) -> T {
    POST.with(|cell| f(&mut cell.borrow_mut()))
}

// ─────────────────────────────────────────────────────────────────────
// 发布到 Slint
// ─────────────────────────────────────────────────────────────────────

fn rows_model(rows: &[Row]) -> ModelRc<PostRow> {
    let items: Vec<PostRow> = rows
        .iter()
        .map(|row| PostRow {
            label: row.label.clone().into(),
            detail: row.detail.clone().into(),
            level: row.level.clone().into(),
            at: row.at,
            value: row.value.clone().into(),
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(items)))
}

fn publish(app: &App) {
    POST.with(|cell| {
        let st = cell.borrow();
        let data = PostData {
            open: st.open,
            tab: st.tab,
            busy: st.busy,
            backend: st.backend.clone().into(),
            hint: st.hint.clone().into(),
            scope_view: st.scope_view,
            scope_live: st.scope_live,
            scope_image: st.scope_image.clone(),
            scope_info: st.scope_info.clone().into(),
            frame_info: st.frame_info.clone().into(),
            project_space: st.project_space,
            clip_space: st.clip_space,
            clip_line: st.clip_line.clone().into(),
            space_info: st.space_info.clone().into(),
            lut_selected: st.lut_selected,
            lut_strength: st.lut_strength,
            lut_line: st.lut_line.clone().into(),
            cdl_slope_r: st.cdl.slope[0],
            cdl_slope_g: st.cdl.slope[1],
            cdl_slope_b: st.cdl.slope[2],
            cdl_offset_r: st.cdl.offset[0],
            cdl_offset_g: st.cdl.offset[1],
            cdl_offset_b: st.cdl.offset[2],
            cdl_power_r: st.cdl.power[0],
            cdl_power_g: st.cdl.power[1],
            cdl_power_b: st.cdl.power[2],
            cdl_sat: st.cdl.sat,
            preview_before: st.before_image.clone(),
            preview_after: st.after_image.clone(),
            preview_info: st.preview_info.clone().into(),
            qc_status: st.qc_status.clone().into(),
            qc_line: st.qc_line.clone().into(),
            qc_target: st.qc_target.clone().into(),
            qc_busy: st.qc_busy,
            check_line: st.check_line.clone().into(),
            check_busy: st.check_busy,
            snapshot_line: st.snapshot_line.clone().into(),
            handoff_line: st.handoff_line.clone().into(),
            handoff_path: st.handoff_path.clone().into(),
            status_line: st.status_line.clone().into(),
            status_level: st.status_level.clone().into(),
        };
        app.set_post(data);
        let luts: Vec<PostLutItem> = st
            .luts
            .iter()
            .map(|lut| PostLutItem {
                id: lut.id.clone().into(),
                name: lut.name.clone().into(),
                space: lut.space.clone().into(),
            })
            .collect();
        app.set_post_luts(ModelRc::from(Rc::new(VecModel::from(luts))));
        app.set_post_qc_rows(rows_model(&st.qc_rows));
        app.set_post_check_rows(rows_model(&st.check_rows));
        app.set_post_snapshot_rows(rows_model(&st.snapshot_rows));
    });
}

// ─────────────────────────────────────────────────────────────────────
// 打开 / 关闭 / 定时刷新
// ─────────────────────────────────────────────────────────────────────

/// 从首页创作中心把「打开专业后期面板第 N 页」放进队列：首页链接在
/// Studio 借用里处理，面板要等借用放掉才开（同 `pro::want` 的做法）。
thread_local! {
    static WANTED: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

/// 请求打开专业后期面板第 `tab` 页（消息处理之后由 [`take_wanted`] 兑现）。
pub fn want_tab(tab: i32) {
    WANTED.with(|slot| slot.set(Some(tab.clamp(0, TAB_HANDOFF))));
}

/// 窗口消息处理完之后取走「要开哪一页」。
pub fn take_wanted() -> Option<i32> {
    WANTED.with(|slot| slot.take())
}

pub fn open_tab(weak: slint::Weak<App>, tab: i32) {
    Shell::with(|shell, app| {
        let studio = shell.studio.borrow();
        with_post(|st| {
            st.open = true;
            st.tab = tab.clamp(0, TAB_HANDOFF);
            st.status_line.clear();
            load_grade(&studio, st);
            refresh_luts(&shell.models, st);
            ensure_lut_table(&studio, st);
            refresh_clip_line(&studio, st);
            refresh_snapshots(&studio, st);
            refresh_qc_target(&studio, st);
        });
        publish(&app);
    });
    tick(weak);
}

pub fn close(weak: slint::Weak<App>) {
    if let Some(app) = weak.upgrade() {
        with_post(|st| {
            st.open = false;
            st.pending = false;
        });
        publish(&app);
    }
}

/// Timer 的一拍：换帧才解码；换调色参数或视图只重算缓存帧。
pub fn tick(weak: slint::Weak<App>) {
    Shell::with(|shell, app| {
        let studio = shell.studio.borrow();
        let mut spawn_job: Option<(FrameSource, i32, GradeJob, i32)> = None;
        let mut recompute = false;
        let mut publish_now = false;
        with_post(|st| {
            if !st.open {
                return;
            }
            refresh_luts_quick(&shell.models, st);
            ensure_lut_table(&studio, st);
            refresh_clip_line(&studio, st);
            if st.tab != TAB_SCOPE && st.tab != TAB_COLOR {
                return;
            }
            let force = st.force;
            st.force = false;
            let Some(source) = current_frame_source(&studio) else {
                st.scope_info =
                    "时间线上没有可读的预览帧：把播放头放到一段视频上，或先导入素材。".to_owned();
                st.frame_info.clear();
                publish_now = true;
                return;
            };
            let grade_key = st.grade_key();
            let frame_known = st.frame_key == source.key && st.frame.is_some();
            let grade_changed = st.grade_key != grade_key;
            if !frame_known {
                if st.pending {
                    return;
                }
                if studio.playing
                    && st
                        .last_refresh
                        .is_some_and(|last| last.elapsed() < Duration::from_millis(1100))
                {
                    return;
                }
                st.pending = true;
                st.last_refresh = Some(Instant::now());
                st.scope_info = "正在取预览帧…".to_owned();
                spawn_job = Some((source, st.scope_view, st.grade_snapshot(), st.tab));
            } else if grade_changed || force {
                st.grade_key = grade_key;
                if !st.scope_live && !force {
                    return;
                }
                recompute = true;
            } else {
                return;
            }
        });
        if let Some((source, view, grade, tab)) = spawn_job {
            let job_grade_key = with_post(|st| st.grade_key());
            drop(studio);
            publish(&app);
            host::spawn(
                move || -> Result<FrameJob, String> {
                    let rgba = decode_frame(&source)?;
                    let scope = if tab == TAB_SCOPE {
                        Some(render_scope(&rgba, view))
                    } else {
                        None
                    };
                    let after = apply_grade(&rgba, &grade);
                    Ok(FrameJob {
                        source,
                        rgba,
                        scope,
                        after,
                        grade_key: job_grade_key,
                    })
                },
                |_studio, app, _models, result| {
                    with_post(|st| {
                        st.pending = false;
                        if !st.open {
                            return;
                        }
                        match result {
                            Ok(job) => {
                                st.frame_key = job.source.key.clone();
                                st.grade_key = job.grade_key.clone();
                                st.frame_info = format!(
                                    "{}  ·  {}  ·  第 {} 帧",
                                    job.source.clip_name,
                                    job.source.timecode,
                                    (job.source.seconds * job.source.fps).round() as i64
                                );
                                st.scope_info = if st.tab == TAB_SCOPE {
                                    format!(
                                        "{}  ·  取样 256×144  ·  {}",
                                        SCOPE_NAMES[st.scope_view.clamp(0, 3) as usize],
                                        if st.scope_live { "自动刷新" } else { "已暂停" }
                                    )
                                } else {
                                    String::new()
                                };
                                if let Some(scope) = job.scope {
                                    st.scope_image = image_of(SCOPE_W, SCOPE_H, &scope);
                                }
                                st.before_image = image_of(FRAME_W, FRAME_H, &job.rgba);
                                st.after_image = image_of(FRAME_W, FRAME_H, &job.after);
                                st.preview_info = preview_line(&st.grade_snapshot());
                                st.frame = Some(Frame { rgba: job.rgba });
                            }
                            Err(error) => {
                                st.scope_info = error.clone();
                                st.preview_info = error;
                            }
                        }
                    });
                    publish(&app);
                },
            );
        } else if recompute {
            with_post(|st| {
                if let Some(frame) = st.frame.as_ref() {
                    let grade = st.grade_snapshot();
                    let after = apply_grade(&frame.rgba, &grade);
                    if st.tab == TAB_SCOPE {
                        let scope = render_scope(&frame.rgba, st.scope_view);
                        st.scope_image = image_of(SCOPE_W, SCOPE_H, &scope);
                    }
                    st.before_image = image_of(FRAME_W, FRAME_H, &frame.rgba);
                    st.after_image = image_of(FRAME_W, FRAME_H, &after);
                    st.preview_info = preview_line(&grade);
                }
            });
            publish(&app);
        } else if publish_now {
            publish(&app);
        }
        let _ = weak;
    });
}

struct FrameJob {
    source: FrameSource,
    rgba: Vec<u8>,
    scope: Option<Vec<u8>>,
    after: Vec<u8>,
    grade_key: String,
}

fn preview_line(grade: &GradeJob) -> String {
    let cdl = &grade.cdl;
    format!(
        "CDL slope {:.2}/{:.2}/{:.2} · offset {:.3}/{:.3}/{:.3} · power {:.2}/{:.2}/{:.2} · sat {:.2} · {} · 强度 {:.0}% · 256×144 预览",
        cdl.slope[0],
        cdl.slope[1],
        cdl.slope[2],
        cdl.offset[0],
        cdl.offset[1],
        cdl.offset[2],
        cdl.power[0],
        cdl.power[1],
        cdl.power[2],
        cdl.sat,
        if grade.lut.is_some() { "LUT 已选" } else { "未选 LUT" },
        grade.strength * 100.0
    )
}

// ─────────────────────────────────────────────────────────────────────
// 当前帧
// ─────────────────────────────────────────────────────────────────────

fn media_of_clip<'a>(
    studio: &'a Studio,
    clip: &concat_project::model::Clip,
) -> Option<&'a concat_project::model::MediaItem> {
    studio
        .project()
        .media
        .iter()
        .find(|item| item.id == clip.media_id)
}

fn current_frame_source(studio: &Studio) -> Option<FrameSource> {
    use concat_project::model::ClipKind;
    let timeline = studio.timeline();
    let playhead = f64::from(studio.playhead);
    let fps = {
        let num = timeline.video.rate_num as f64;
        let den = timeline.video.rate_den as f64;
        if num > 0.0 && den > 0.0 {
            num / den
        } else {
            25.0
        }
    };
    let mut chosen: Option<&concat_project::model::Clip> = None;
    for track in timeline.tracks.iter().rev() {
        for clip in timeline.clips.iter() {
            if clip.track_id != track.id {
                continue;
            }
            if matches!(clip.kind, ClipKind::Audio | ClipKind::Text | ClipKind::Shape) {
                continue;
            }
            if clip.media_id.is_empty() {
                continue;
            }
            if clip.start <= playhead && playhead < clip.start + clip.duration {
                chosen = Some(clip.as_ref());
            }
        }
    }
    let clip = chosen.or_else(|| {
        timeline
            .clips
            .iter()
            .map(|clip| clip.as_ref())
            .find(|clip| {
                !matches!(clip.kind, ClipKind::Audio | ClipKind::Text | ClipKind::Shape)
                    && !clip.media_id.is_empty()
            })
    })?;
    let media = media_of_clip(studio, clip)?;
    let path = media.path.clone();
    if !Path::new(&path).is_file() {
        return None;
    }
    let is_image = matches!(clip.kind, ClipKind::Image);
    let inside = clip.start <= playhead && playhead < clip.start + clip.duration;
    let seconds = (if inside {
        clip.source_start + (playhead - clip.start) * clip.speed
    } else {
        clip.source_start
    })
    .max(0.0);
    let frame = (seconds * fps).round() as i64;
    Some(FrameSource {
        path,
        seconds,
        clip_name: clip.name.clone(),
        source_start: clip.source_start,
        is_image,
        fps,
        key: format!("{frame}|{is_image}"),
        timecode: timeline_timecode(seconds, fps),
    })
}

fn timeline_timecode(seconds: f64, fps: f64) -> String {
    let fps = if fps > 0.0 { fps } else { 25.0 };
    let whole = fps.round().max(1.0) as u64;
    let total = (seconds * fps).round().max(0.0) as u64;
    let frames = total % whole;
    let secs = total / whole;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60,
        frames
    )
}

// ─────────────────────────────────────────────────────────────────────
// 取帧 / 示波器 / 调色数学
// ─────────────────────────────────────────────────────────────────────

fn decode_frame(source: &FrameSource) -> Result<Vec<u8>, String> {
    let mut command = std::process::Command::new(concat_host::video_preview::ffmpeg());
    command.args(["-hide_banner", "-v", "error"]);
    if !source.is_image {
        command.args(["-ss", &format!("{:.3}", source.seconds)]);
    }
    command
        .arg("-i")
        .arg(&source.path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale=256:144",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-",
        ]);
    let output = command
        .output()
        .map_err(|error| format!("起不了 ffmpeg 取预览帧：{error}"))?;
    let expected = (FRAME_W * FRAME_H * 4) as usize;
    if output.stdout.len() < expected {
        let reason = String::from_utf8_lossy(&output.stderr);
        let reason: String = reason.lines().rev().take(2).collect::<Vec<_>>().join(" / ");
        return Err(format!(
            "取不到预览帧（{}）：{reason}",
            Path::new(&source.path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
    }
    Ok(output.stdout[..expected].to_vec())
}

fn image_of(width: u32, height: u32, rgba: &[u8]) -> Image {
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba, width, height);
    Image::from_rgba8(buffer)
}

fn put(buffer: &mut [u8], x: i32, y: i32, color: [u8; 3], alpha: f32) {
    if x < 0 || y < 0 || x >= SCOPE_W as i32 || y >= SCOPE_H as i32 {
        return;
    }
    let at = ((y as u32 * SCOPE_W + x as u32) * 4) as usize;
    let alpha = alpha.clamp(0.0, 1.0);
    for channel in 0..3 {
        let old = f32::from(buffer[at + channel]);
        let new = f32::from(color[channel]);
        buffer[at + channel] = (old + (new - old) * alpha).round().clamp(0.0, 255.0) as u8;
    }
    buffer[at + 3] = 255;
}

fn fill(buffer: &mut [u8], color: [u8; 3]) {
    for pixel in buffer.chunks_exact_mut(4) {
        pixel[0] = color[0];
        pixel[1] = color[1];
        pixel[2] = color[2];
        pixel[3] = 255;
    }
}

fn luma(rgba: &[u8], at: usize) -> f32 {
    let r = f32::from(rgba[at]) / 255.0;
    let g = f32::from(rgba[at + 1]) / 255.0;
    let b = f32::from(rgba[at + 2]) / 255.0;
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn render_scope(rgba: &[u8], view: i32) -> Vec<u8> {
    let mut buffer = vec![0u8; (SCOPE_W * SCOPE_H * 4) as usize];
    fill(&mut buffer, [8, 9, 12]);
    match view.clamp(0, 3) {
        0 => render_waveform(rgba, &mut buffer),
        1 => render_parade(rgba, &mut buffer),
        2 => render_vectorscope(rgba, &mut buffer),
        _ => render_histogram(rgba, &mut buffer),
    }
    buffer
}

fn grid(buffer: &mut [u8], rows: &[i32], color: [u8; 3]) {
    for row in rows {
        let y = SCOPE_H as i32 - 1 - row;
        for x in 0..SCOPE_W as i32 {
            put(buffer, x, y, color, 0.35);
        }
    }
}

fn render_waveform(rgba: &[u8], buffer: &mut [u8]) {
    grid(buffer, &[0, 72, 144, 216, 287], [90, 100, 120]);
    let mut bins = vec![0u32; (SCOPE_W * SCOPE_H) as usize];
    for x in 0..SCOPE_W {
        let src_x = (x as f32 / (SCOPE_W - 1) as f32 * 255.0).round() as u32;
        for sy in 0..SCOPE_H {
            let src_y = (sy as f32 / (SCOPE_H - 1) as f32 * (FRAME_H - 1) as f32).round() as u32;
            let at = ((src_y * FRAME_W + src_x) * 4) as usize;
            let row = ((1.0 - luma(rgba, at)) * (SCOPE_H - 1) as f32)
                .round()
                .clamp(0.0, (SCOPE_H - 1) as f32) as usize;
            bins[row * SCOPE_W as usize + x as usize] += 1;
        }
    }
    let max = *bins.iter().max().unwrap_or(&1) as f32;
    let scale = max.ln_1p();
    for row in 0..SCOPE_H as usize {
        for x in 0..SCOPE_W as usize {
            let count = bins[row * SCOPE_W as usize + x];
            if count == 0 {
                continue;
            }
            let amount = ((count as f32).ln_1p() / scale).min(1.0);
            put(
                buffer,
                x as i32,
                SCOPE_H as i32 - 1 - row as i32,
                [
                    (70.0 + 60.0 * amount) as u8,
                    (190.0 + 65.0 * amount) as u8,
                    (140.0 + 80.0 * amount) as u8,
                ],
                amount,
            );
        }
    }
}

fn render_parade(rgba: &[u8], buffer: &mut [u8]) {
    grid(buffer, &[0, 72, 144, 216, 287], [90, 100, 120]);
    let width = (SCOPE_W / 3) as usize;
    for channel in 0..3usize {
        let base = channel * width;
        let color = match channel {
            0 => [255, 86, 90],
            1 => [90, 230, 130],
            _ => [110, 150, 255],
        };
        let mut bins = vec![0u32; width * SCOPE_H as usize];
        for x in 0..width {
            let src_x = ((base + x) as f32 / (SCOPE_W - 1) as f32 * 255.0).round() as u32;
            for sy in 0..SCOPE_H {
                let src_y = (sy as f32 / (SCOPE_H - 1) as f32 * (FRAME_H - 1) as f32).round() as u32;
                let at = ((src_y * FRAME_W + src_x) * 4) as usize;
                let value = f32::from(rgba[at + channel]) / 255.0;
                let row = ((1.0 - value) * (SCOPE_H - 1) as f32)
                    .round()
                    .clamp(0.0, (SCOPE_H - 1) as f32) as usize;
                bins[row * width + x] += 1;
            }
        }
        let max = *bins.iter().max().unwrap_or(&1) as f32;
        let scale = max.ln_1p();
        for row in 0..SCOPE_H as usize {
            for x in 0..width {
                let count = bins[row * width + x];
                if count == 0 {
                    continue;
                }
                let amount = ((count as f32).ln_1p() / scale).min(1.0);
                put(
                    buffer,
                    (base + x) as i32,
                    SCOPE_H as i32 - 1 - row as i32,
                    color,
                    amount,
                );
            }
        }
        if channel > 0 {
            for y in 0..SCOPE_H as i32 {
                put(buffer, base as i32, y, [40, 44, 52], 0.8);
            }
        }
    }
}

fn render_vectorscope(rgba: &[u8], buffer: &mut [u8]) {
    let cx = SCOPE_W as f32 / 2.0;
    let cy = SCOPE_H as f32 / 2.0;
    let radius = (SCOPE_H as f32) * 0.44;
    for step in 0..360 {
        let angle = step as f32 * std::f32::consts::PI / 180.0;
        put(
            buffer,
            (cx + radius * angle.cos()) as i32,
            (cy + radius * angle.sin()) as i32,
            [46, 52, 64],
            0.8,
        );
    }
    for x in 0..SCOPE_W as i32 {
        put(buffer, x, cy as i32, [40, 46, 56], 0.35);
    }
    for y in 0..SCOPE_H as i32 {
        put(buffer, cx as i32, y, [40, 46, 56], 0.35);
    }
    let targets: [([f32; 2], [u8; 3]); 6] = [
        ([0.0, 1.0], [255, 96, 96]),
        ([-0.866, 0.5], [255, 220, 90]),
        ([-0.866, -0.5], [90, 230, 130]),
        ([0.0, -1.0], [90, 220, 240]),
        ([0.866, -0.5], [110, 150, 255]),
        ([0.866, 0.5], [230, 120, 230]),
    ];
    for (unit, color) in targets {
        let x = cx + unit[0] * radius * 0.85;
        let y = cy - unit[1] * radius * 0.85;
        for dy in -2..=2 {
            for dx in -2..=2 {
                put(buffer, x as i32 + dx, y as i32 + dy, color, 0.7);
            }
        }
    }
    let mut bins = vec![0u32; (SCOPE_W * SCOPE_H) as usize];
    for sy in 0..SCOPE_H {
        let src_y = (sy as f32 / (SCOPE_H - 1) as f32 * (FRAME_H - 1) as f32).round() as u32;
        for sx in 0..SCOPE_W {
            let src_x = (sx as f32 / (SCOPE_W - 1) as f32 * 255.0).round() as u32;
            let at = ((src_y * FRAME_W + src_x) * 4) as usize;
            let r = f32::from(rgba[at]) / 255.0;
            let g = f32::from(rgba[at + 1]) / 255.0;
            let b = f32::from(rgba[at + 2]) / 255.0;
            let cb = -0.1146 * r - 0.3854 * g + 0.5 * b;
            let cr = 0.5 * r - 0.4542 * g - 0.0458 * b;
            let x = cx + cr * radius * 2.2;
            let y = cy - cb * radius * 2.2;
            if x < 0.0 || y < 0.0 || x >= SCOPE_W as f32 || y >= SCOPE_H as f32 {
                continue;
            }
            bins[y as usize * SCOPE_W as usize + x as usize] += 1;
        }
    }
    let max = *bins.iter().max().unwrap_or(&1) as f32;
    let scale = max.ln_1p();
    for row in 0..SCOPE_H as usize {
        for x in 0..SCOPE_W as usize {
            let count = bins[row * SCOPE_W as usize + x];
            if count == 0 {
                continue;
            }
            let amount = ((count as f32).ln_1p() / scale).min(1.0);
            put(buffer, x as i32, row as i32, [180, 230, 255], amount);
        }
    }
}

fn render_histogram(rgba: &[u8], buffer: &mut [u8]) {
    grid(buffer, &[0, 72, 144, 216, 287], [90, 100, 120]);
    let mut bins = [[0u32; 256]; 3];
    for at in (0..rgba.len()).step_by(4) {
        for channel in 0..3 {
            bins[channel][rgba[at + channel] as usize] += 1;
        }
    }
    let max = bins
        .iter()
        .flat_map(|channel| channel.iter())
        .copied()
        .max()
        .unwrap_or(1) as f32;
    let colors = [[255u8, 86, 90], [90, 230, 130], [110, 150, 255]];
    for channel in 0..3 {
        for x in 0..SCOPE_W as usize {
            let count = bins[channel][(x * 256) / SCOPE_W as usize];
            if count == 0 {
                continue;
            }
            let height = ((count as f32 / max).sqrt() * 240.0) as i32;
            for y in 0..height {
                put(buffer, x as i32, SCOPE_H as i32 - 1 - y, colors[channel], 0.42);
            }
        }
    }
}

/// ASC CDL：`out = clamp(in * slope + offset) ^ (1 / power)`，再按 Rec.709
/// 亮度权重做饱和度；最后按强度混入 LUT。
fn apply_grade(rgba: &[u8], grade: &GradeJob) -> Vec<u8> {
    let mut out = rgba.to_vec();
    let cdl = &grade.cdl;
    let identity = cdl.is_identity();
    for at in (0..out.len()).step_by(4) {
        let mut rgb = [
            f32::from(rgba[at]) / 255.0,
            f32::from(rgba[at + 1]) / 255.0,
            f32::from(rgba[at + 2]) / 255.0,
        ];
        if !identity {
            for channel in 0..3 {
                let value = (rgb[channel] * cdl.slope[channel] + cdl.offset[channel]).max(0.0);
                rgb[channel] = value.powf(1.0 / cdl.power[channel].max(0.0001));
            }
            let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
            for channel in 0..3 {
                rgb[channel] = luma + (rgb[channel] - luma) * cdl.sat;
            }
        }
        if let Some(lut) = grade.lut.as_ref() {
            let graded = sample_lut(lut, rgb);
            for channel in 0..3 {
                rgb[channel] += (graded[channel] - rgb[channel]) * grade.strength;
            }
        }
        for channel in 0..3 {
            out[at + channel] = (rgb[channel].clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    out
}

fn sample_lut(lut: &LutJob, rgb: [f32; 3]) -> [f32; 3] {
    let size = lut.size.max(2);
    let scale = (size - 1) as f32;
    let coord = [
        rgb[0].clamp(0.0, 1.0) * scale,
        rgb[1].clamp(0.0, 1.0) * scale,
        rgb[2].clamp(0.0, 1.0) * scale,
    ];
    let low = [
        coord[0].floor() as u32,
        coord[1].floor() as u32,
        coord[2].floor() as u32,
    ];
    let high = [
        (low[0] + 1).min(size - 1),
        (low[1] + 1).min(size - 1),
        (low[2] + 1).min(size - 1),
    ];
    let frac = [
        coord[0] - low[0] as f32,
        coord[1] - low[1] as f32,
        coord[2] - low[2] as f32,
    ];
    let texel = |r: u32, g: u32, b: u32| -> [f32; 3] {
        let index = ((r + g * size + b * size * size) * 4) as usize;
        if index + 2 >= lut.table.len() {
            return rgb;
        }
        [
            lut.table[index],
            lut.table[index + 1],
            lut.table[index + 2],
        ]
    };
    let mut out = [0.0f32; 3];
    for channel in 0..3 {
        let c000 = texel(low[0], low[1], low[2])[channel];
        let c100 = texel(high[0], low[1], low[2])[channel];
        let c010 = texel(low[0], high[1], low[2])[channel];
        let c110 = texel(high[0], high[1], low[2])[channel];
        let c001 = texel(low[0], low[1], high[2])[channel];
        let c101 = texel(high[0], low[1], high[2])[channel];
        let c011 = texel(low[0], high[1], high[2])[channel];
        let c111 = texel(high[0], high[1], high[2])[channel];
        let c00 = c000 + (c100 - c000) * frac[0];
        let c10 = c010 + (c110 - c010) * frac[0];
        let c01 = c001 + (c101 - c001) * frac[0];
        let c11 = c011 + (c111 - c011) * frac[0];
        let c0 = c00 + (c10 - c00) * frac[1];
        let c1 = c01 + (c11 - c01) * frac[1];
        out[channel] = c0 + (c1 - c0) * frac[2];
    }
    out
}

fn parse_lut_file(path: &Path) -> Option<(Vec<f32>, u32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let lut = concat_effects::cube::parse(&text).ok()?;
    Some((lut.rgba.to_vec(), lut.size))
}

// ─────────────────────────────────────────────────────────────────────
// 色彩管理 / LUT
// ─────────────────────────────────────────────────────────────────────

fn looks_dir(studio: &Studio) -> PathBuf {
    Studio::looks_dir(&studio.host.dirs)
}

fn space_index_from_timeline(studio: &Studio) -> i32 {
    use concat_project::model::ColorSpace;
    match studio.timeline().video.color_space {
        ColorSpace::Sdr => 1,
        ColorSpace::Hlg => 4,
        ColorSpace::Pq => 5,
    }
}

/// 从效果目录重建 LUT 列表；条目没变就保留缓存过的表。
fn refresh_luts(models: &Models, st: &mut PostState) {
    let mut ids = Vec::new();
    let mut entries = Vec::new();
    for item in models.catalogue_luts.iter() {
        let id = item.id.to_string();
        if id.is_empty() {
            continue;
        }
        ids.push(id.clone());
        let space = if id.to_ascii_lowercase().contains("acescct")
            || item.name.to_ascii_lowercase().contains("acescct")
        {
            "ACEScct"
        } else {
            "Display"
        };
        entries.push(LutEntry {
            id,
            name: item.name.to_string(),
            space: space.to_owned(),
            table: None,
            size: 0,
        });
    }
    let old_ids: Vec<String> = st.luts.iter().map(|lut| lut.id.clone()).collect();
    if old_ids != ids {
        // 表按 id 继承，别因为重建列表把预览里的 LUT 弄丢。
        for entry in &mut entries {
            if let Some(old) = st.luts.iter().find(|old| old.id == entry.id) {
                entry.table = old.table.clone();
                entry.size = old.size;
            }
        }
        st.luts = entries;
    }
    if st.luts.is_empty() {
        st.lut_selected = -1;
        if st.lut_line.is_empty() || st.lut_line.starts_with("LUT：还没有导入") {
            st.lut_line = "LUT：还没有导入（点「导入 .cube…」）。导入的 LUT 存本机，可反复套用。".to_owned();
        }
        return;
    }
    let keep = st
        .current_lut()
        .map(|entry| entry.id.clone())
        .and_then(|id| st.luts.iter().position(|entry| entry.id == id));
    st.lut_selected = keep.map(|index| index as i32).unwrap_or(0);
    if let Some(entry) = st.luts.get(st.lut_selected as usize) {
        if !st.lut_line.starts_with("LUT：") || st.lut_line.len() < 8 {
            st.lut_line = format!(
                "LUT：{}（{}）· 已选中；套用后编辑器预览即真实效果。",
                entry.name, entry.space
            );
        }
    }
}

fn refresh_luts_quick(models: &Models, st: &mut PostState) {
    let count = models.catalogue_luts.row_count();
    if count != st.luts.len() {
        refresh_luts(models, st);
    }
}

fn ensure_lut_table(studio: &Studio, st: &mut PostState) {
    let Some(entry) = st.current_lut() else { return };
    if entry.table.is_some() {
        return;
    }
    let path = looks_dir(studio).join(&entry.id).join("look.cube");
    if let Some((table, size)) = parse_lut_file(&path) {
        if let Some(slot) = st.luts.get_mut(st.lut_selected as usize) {
            slot.table = Some(table);
            slot.size = size;
        }
    }
}

fn grade_path(studio: &Studio) -> Option<PathBuf> {
    let session = studio.session.as_ref()?;
    let dir = PathBuf::from(session.path());
    if dir.as_os_str().is_empty() {
        None
    } else {
        Some(dir.join("post").join("grade.json"))
    }
}

fn load_grade(studio: &Studio, st: &mut PostState) {
    st.project_space = space_index_from_timeline(studio);
    st.clip_space = st.project_space;
    st.cdl = Cdl::default();
    let Some(path) = grade_path(studio) else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    if let Some(space) = value.get("project_space").and_then(|v| v.as_i64()) {
        st.project_space = (space as i32).clamp(0, 5);
    }
    if let Some(space) = value.get("clip_space").and_then(|v| v.as_i64()) {
        st.clip_space = (space as i32).clamp(0, 5);
    }
    if let Some(cdl) = value.get("cdl") {
        st.cdl = Cdl::from_json(cdl);
    }
}

fn save_grade(studio: &Studio, st: &PostState) -> Result<PathBuf, String> {
    let path = grade_path(studio).ok_or_else(|| "没有打开的工程，色彩标签无处保存".to_owned())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("建不了 post 目录：{error}"))?;
    }
    let value = serde_json::json!({
        "schema": "xinglai.post.grade.v1",
        "project_space": st.project_space,
        "project_space_label": SPACES[st.project_space.clamp(0, 5) as usize],
        "clip_space": st.clip_space,
        "clip_space_label": SPACES[st.clip_space.clamp(0, 5) as usize],
        "cdl": st.cdl.to_json(),
        "updated": local_timestamp(),
    });
    let text = serde_json::to_string_pretty(&value)
        .map_err(|error| format!("写不了 grade.json：{error}"))?;
    std::fs::write(&path, text).map_err(|error| format!("写不了 grade.json：{error}"))?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::write(parent.join("grade.cdl"), cdl_text(&st.cdl));
        let _ = std::fs::write(parent.join("grade.ccc"), ccc_text(&st.cdl));
    }
    Ok(path)
}

fn cdl_text(cdl: &Cdl) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<ColorDecisionList xmlns=\"urn:ASC:CDL:v1.01\">\n\
  <ColorDecision>\n\
    <ColorCorrection id=\"xinglai\">\n\
      <SOPNode>\n\
        <Slope>{:.6} {:.6} {:.6}</Slope>\n\
        <Offset>{:.6} {:.6} {:.6}</Offset>\n\
        <Power>{:.6} {:.6} {:.6}</Power>\n\
      </SOPNode>\n\
      <SatNode><Saturation>{:.6}</Saturation></SatNode>\n\
    </ColorCorrection>\n\
  </ColorDecision>\n\
</ColorDecisionList>\n",
        cdl.slope[0],
        cdl.slope[1],
        cdl.slope[2],
        cdl.offset[0],
        cdl.offset[1],
        cdl.offset[2],
        cdl.power[0],
        cdl.power[1],
        cdl.power[2],
        cdl.sat
    )
}

fn ccc_text(cdl: &Cdl) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<ColorCorrectionCollection xmlns=\"urn:ASC:CDL:v1.01\">\n\
  <ColorCorrection id=\"xinglai\">\n\
    <SOPNode>\n\
      <Slope>{:.6} {:.6} {:.6}</Slope>\n\
      <Offset>{:.6} {:.6} {:.6}</Offset>\n\
      <Power>{:.6} {:.6} {:.6}</Power>\n\
    </SOPNode>\n\
    <SatNode><Saturation>{:.6}</Saturation></SatNode>\n\
  </ColorCorrection>\n\
</ColorCorrectionCollection>\n",
        cdl.slope[0],
        cdl.slope[1],
        cdl.slope[2],
        cdl.offset[0],
        cdl.offset[1],
        cdl.offset[2],
        cdl.power[0],
        cdl.power[1],
        cdl.power[2],
        cdl.sat
    )
}

fn selected_clip(studio: &Studio) -> Option<&concat_project::model::Clip> {
    let id = studio.selection.first()?;
    studio
        .timeline()
        .clips
        .iter()
        .map(|clip| clip.as_ref())
        .find(|clip| &clip.id == id)
}

fn refresh_clip_line(studio: &Studio, st: &mut PostState) {
    let Some(clip) = selected_clip(studio) else {
        st.clip_line = "没有选中片段：选中一段视频再打色彩空间标签。".to_owned();
        return;
    };
    let media = media_of_clip(studio, clip);
    let name = media
        .map(|item| item.name.clone())
        .unwrap_or_else(|| clip.name.clone());
    let media_space = media
        .map(|item| match item.color_space {
            concat_project::model::ColorSpace::Sdr => "Rec.709",
            concat_project::model::ColorSpace::Hlg => "HLG",
            concat_project::model::ColorSpace::Pq => "PQ",
        })
        .unwrap_or("Rec.709");
    st.clip_line = format!(
        "选中：{}（片段 {}）· 素材标签 {} · 交付标签 {}",
        name,
        clip.id,
        media_space,
        SPACES[st.clip_space.clamp(0, 5) as usize]
    );
}

fn apply_lut_to_selection(studio: &mut Studio, st: &PostState) -> (String, String) {
    let Some(entry) = st.current_lut().cloned() else {
        return ("warn".to_owned(), "先在列表里选一个 LUT".to_owned());
    };
    studio.apply_lut(&entry.id, &entry.name);
    if let Some(clip) = selected_clip(studio) {
        if let Some(index) = clip
            .video_effects
            .iter()
            .rposition(|filter| filter.id == entry.id)
        {
            studio.chain_set_param(false, index as i32, "intensity", st.lut_strength);
        }
    }
    (
        "info".to_owned(),
        format!(
            "已把 LUT「{}」套到片段链上（强度 {:.0}%）：编辑器预览 / 导出都生效。",
            entry.name, st.lut_strength
        ),
    )
}

// ─────────────────────────────────────────────────────────────────────
// 质检
// ─────────────────────────────────────────────────────────────────────

fn qc_target_of(
    studio: &Studio,
    path: Option<&str>,
) -> Option<(String, Option<(f64, f64, f64)>)> {
    if let Some(path) = path {
        if Path::new(path).is_file() {
            let clip = studio.timeline().clips.iter().find_map(|clip| {
                let media = media_of_clip(studio, clip)?;
                (media.path == path).then(|| (clip.start, clip.source_start, clip.speed))
            });
            return Some((path.to_owned(), clip));
        }
    }
    let clip = selected_clip(studio).or_else(|| {
        studio
            .timeline()
            .clips
            .iter()
            .map(|clip| clip.as_ref())
            .find(|clip| {
                use concat_project::model::ClipKind;
                !matches!(clip.kind, ClipKind::Audio | ClipKind::Text | ClipKind::Shape)
                    && !clip.media_id.is_empty()
            })
    })?;
    let media = media_of_clip(studio, clip)?;
    Some((
        media.path.clone(),
        Some((clip.start, clip.source_start, clip.speed)),
    ))
}

fn refresh_qc_target(studio: &Studio, st: &mut PostState) {
    let Some((path, clip)) = qc_target_of(studio, Some(st.qc_media_path.as_str())) else {
        st.qc_target = "质检对象：时间线还没有素材（先导入一段视频）".to_owned();
        st.qc_clip = None;
        return;
    };
    let name = Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    st.qc_target = format!(
        "质检对象：{}{}",
        name,
        if clip.is_some() {
            "（时间线素材）"
        } else {
            "（本机文件）"
        }
    );
    st.qc_clip = clip;
    st.qc_media_path = path;
}

fn qc_run(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    let mut wanted = None;
    Shell::with(|shell, _app| {
        let studio = shell.studio.borrow();
        with_post(|st| {
            st.qc_busy = true;
            st.set_status("info", "正在质检：平台优先，平台不通落到本机…");
            if let Some((path, clip)) = qc_target_of(&studio, Some(st.qc_media_path.as_str())) {
                st.qc_media_path = path.clone();
                st.qc_clip = clip;
                let fps = {
                    let num = studio.timeline().video.rate_num as f64;
                    let den = studio.timeline().video.rate_den as f64;
                    if num > 0.0 && den > 0.0 {
                        num / den
                    } else {
                        25.0
                    }
                };
                wanted = Some((path, clip, fps));
            }
        });
    });
    let Some((path, clip, fps)) = wanted else {
        with_post(|st| {
            st.qc_busy = false;
            st.set_status("fail", "时间线还没有素材：先导入一段视频");
        });
        publish(&app);
        return;
    };
    publish(&app);
    host::spawn(
        move || -> (serde_json::Value, String) {
            let platform = concat_host::post::health()
                .and_then(|_| concat_host::post::qc_upload(Path::new(&path), fps));
            match platform {
                Ok(value) => (value, "平台 /api/post/qc".to_owned()),
                Err(reason) => match concat_host::post::qc_local(Path::new(&path), fps) {
                    Ok(mut value) => {
                        value["platform_error"] = serde_json::Value::String(reason);
                        (value, "本机 ffmpeg（平台不可用）".to_owned())
                    }
                    Err(error) => (
                        serde_json::json!({
                            "ok": false,
                            "status": "FAIL",
                            "hint": format!("{reason}；本机质检也失败：{error}"),
                        }),
                        "本地失败".to_owned(),
                    ),
                },
            }
        },
        move |_studio, app, _models, (report, source)| {
            with_post(|st| {
                st.qc_busy = false;
                st.qc_report = Some(report.clone());
                apply_qc_report(st, &report, &source, clip.as_ref());
            });
            publish(&app);
        },
    );
}

fn apply_qc_report(
    st: &mut PostState,
    report: &serde_json::Value,
    source: &str,
    clip: Option<&(f64, f64, f64)>,
) {
    if report.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        st.qc_status = "FAIL".to_owned();
        st.qc_line = format!(
            "平台与本机都没跑成：{}",
            report
                .get("hint")
                .and_then(|v| v.as_str())
                .unwrap_or("没有给出原因")
        );
        st.set_status("fail", st.qc_line.clone());
        return;
    }
    let status = report
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("WARN")
        .to_owned();
    st.qc_status = status.clone();
    let summary = report.get("summary").cloned().unwrap_or_default();
    let number = |key: &str| summary.get(key).and_then(|v| v.as_f64());
    let fast_num = |key: &str| summary.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    let duration = number("duration_s").unwrap_or(0.0);
    let tc = summary
        .get("duration_tc")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let loudness = number("integrated_lufs").unwrap_or(0.0);
    let peak = number("true_peak_dbtp").unwrap_or(0.0);
    let fps = number("fps").unwrap_or(25.0);
    let width = fast_num("width");
    let height = fast_num("height");
    st.qc_line = format!(
        "来源 {source} · 时长 {duration:.2}s（{tc}）· {width}×{height} · {fps:.2} fps · 整体响度 {loudness:.1} LUFS · 真峰 {peak:.1} dBTP · 黑场 {} 处 · 静音 {} 处 · 冻结 {} 处",
        fast_num("black_count"),
        fast_num("silence_count"),
        fast_num("freeze_count"),
    );
    if let Some(reason) = report.get("platform_error").and_then(|v| v.as_str()) {
        st.qc_line.push_str(&format!(" · 平台原因：{reason}"));
    }
    let mut rows = Vec::new();
    if let Some(issues) = report.get("issues").and_then(|v| v.as_array()) {
        for issue in issues {
            let start = issue.get("start_s").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let timeline_at = match clip {
                Some((clip_start, source_start, speed)) => {
                    clip_start + (start - source_start) / speed.max(0.0001)
                }
                None => -1.0,
            };
            let label = issue
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("问题")
                .to_owned();
            let level = issue
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("warn")
                .to_owned();
            let message = issue
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let start_tc = issue
                .get("start_tc")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            rows.push(Row {
                label,
                detail: format!("{message} · {start_tc}"),
                level,
                at: timeline_at as f32,
                value: start.to_string(),
            });
        }
    }
    st.qc_rows = rows;
    let level = match status.as_str() {
        "PASS" => "info",
        "WARN" => "warn",
        _ => "fail",
    };
    st.set_status(
        level,
        format!(
            "质检 {status}：{} 个问题项（来源：{source}）",
            st.qc_rows.len()
        ),
    );
}

fn qc_jump(weak: slint::Weak<App>, index: usize) {
    let Some(app) = weak.upgrade() else { return };
    Shell::with(|shell, app| {
        let mut studio = shell.studio.borrow_mut();
        let at = with_post(|st| st.qc_rows.get(index).map(|row| row.at));
        if let Some(at) = at {
            if at >= 0.0 {
                studio.pause();
                studio.seek(at);
                with_post(|st| {
                    st.set_status("info", format!("已把播放头跳到 {:.2}s 的问题位置", at));
                });
            } else {
                with_post(|st| {
                    st.set_status(
                        "warn",
                        "这个问题项来自本机文件，不在时间线上：先把文件导入工程才能跳转。",
                    );
                });
            }
        }
        studio.publish(&app, &shell.models);
    });
    publish(&app);
}

// ─────────────────────────────────────────────────────────────────────
// 媒体校验
// ─────────────────────────────────────────────────────────────────────

fn manifest_path(studio: &Studio) -> Option<PathBuf> {
    let session = studio.session.as_ref()?;
    let dir = PathBuf::from(session.path());
    if dir.as_os_str().is_empty() {
        None
    } else {
        Some(dir.join("media_manifest.json"))
    }
}

type MediaRow = (String, String, String, f64, u32, u32, f64, String);

fn media_rows(studio: &Studio) -> Vec<MediaRow> {
    studio
        .project()
        .media
        .iter()
        .map(|item| {
            (
                item.id.clone(),
                item.name.clone(),
                item.path.clone(),
                item.duration.unwrap_or(0.0),
                item.width.unwrap_or(0),
                item.height.unwrap_or(0),
                item.frame_rate.unwrap_or(0.0),
                match item.color_space {
                    concat_project::model::ColorSpace::Sdr => "Rec.709".to_owned(),
                    concat_project::model::ColorSpace::Hlg => "HLG".to_owned(),
                    concat_project::model::ColorSpace::Pq => "PQ".to_owned(),
                },
            )
        })
        .collect()
}

fn check_run(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    let mut wanted = None;
    Shell::with(|shell, _app| {
        let studio = shell.studio.borrow();
        let manifest_value = manifest_path(&studio)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        with_post(|st| {
            st.check_busy = true;
            st.set_status("info", "正在逐个文件算 sha256（大工程要一会儿）…");
        });
        wanted = Some((media_rows(&studio), manifest_value));
    });
    let Some((media, manifest)) = wanted else { return };
    publish(&app);
    host::spawn(
        move || -> Vec<(String, String, String, u64, String)> {
            media
                .iter()
                .map(|(id, name, path, ..)| {
                    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
                    let hash = concat_host::post::sha256_file(Path::new(path)).unwrap_or_default();
                    (id.clone(), name.clone(), path.clone(), size, hash)
                })
                .collect()
        },
        move |_studio, app, _models, hashes| {
            with_post(|st| {
                st.check_busy = false;
                let manifest_rows = manifest
                    .as_ref()
                    .and_then(|value| value.get("media").and_then(|v| v.as_array()));
                let mut rows = Vec::new();
                let mut ok = 0usize;
                let mut missing = 0usize;
                let mut mismatch = 0usize;
                let mut relinked = 0usize;
                for (id, name, path, size, hash) in &hashes {
                    let _ = id;
                    if !Path::new(path).is_file() {
                        let mut relink_to: Option<String> = None;
                        if let Some(list) = manifest_rows {
                            for item in list {
                                let same_name =
                                    item.get("name").and_then(|v| v.as_str()) == Some(name.as_str());
                                let manifest_hash =
                                    item.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
                                let manifest_file =
                                    item.get("path").and_then(|v| v.as_str()).unwrap_or("");
                                if same_name
                                    && !hash.is_empty()
                                    && manifest_hash == hash
                                    && Path::new(manifest_file).is_file()
                                {
                                    relink_to = Some(manifest_file.to_owned());
                                    break;
                                }
                            }
                        }
                        if let Some(found) = relink_to {
                            relinked += 1;
                            rows.push(Row {
                                label: "重链".to_owned(),
                                detail: format!("{name} → {found}"),
                                level: "relinked".to_owned(),
                                at: -1.0,
                                value: found,
                            });
                        } else {
                            missing += 1;
                            rows.push(Row {
                                label: "缺失".to_owned(),
                                detail: format!("{name} 不在 {path}"),
                                level: "fail".to_owned(),
                                at: -1.0,
                                value: path.clone(),
                            });
                        }
                        continue;
                    }
                    let recorded = manifest_rows.and_then(|list| {
                        list.iter().find(|item| {
                            item.get("path").and_then(|v| v.as_str()) == Some(path.as_str())
                        })
                    });
                    match recorded
                        .and_then(|item| item.get("sha256"))
                        .and_then(|v| v.as_str())
                    {
                        Some(expected) if expected == hash => {
                            ok += 1;
                            rows.push(Row {
                                label: "一致".to_owned(),
                                detail: format!(
                                    "{name} · {} B · sha256 {}…",
                                    size,
                                    &hash[..hash.len().min(12)]
                                ),
                                level: "ok".to_owned(),
                                at: -1.0,
                                value: path.clone(),
                            });
                        }
                        Some(expected) => {
                            mismatch += 1;
                            rows.push(Row {
                                label: "不一致".to_owned(),
                                detail: format!(
                                    "{name} · 清单 {}… / 现在 {}…",
                                    &expected[..expected.len().min(12)],
                                    &hash[..hash.len().min(12)]
                                ),
                                level: "fail".to_owned(),
                                at: -1.0,
                                value: path.clone(),
                            });
                        }
                        None => {
                            ok += 1;
                            rows.push(Row {
                                label: "一致".to_owned(),
                                detail: format!(
                                    "{name} · {} B · sha256 {}…（清单未收录）",
                                    size,
                                    &hash[..hash.len().min(12)]
                                ),
                                level: "ok".to_owned(),
                                at: -1.0,
                                value: path.clone(),
                            });
                        }
                    }
                }
                st.check_rows = rows;
                st.check_line =
                    format!("一致 {ok} · 缺失 {missing} · 重链 {relinked} · 不一致 {mismatch}");
                st.set_status(
                    if missing + mismatch == 0 { "info" } else { "warn" },
                    format!("媒体校验完成：{}", st.check_line),
                );
            });
            publish(&app);
        },
    );
}

fn check_manifest_save(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    let mut wanted = None;
    Shell::with(|shell, _app| {
        let studio = shell.studio.borrow();
        with_post(|st| {
            st.check_busy = true;
            st.set_status("info", "正在算 sha256 并写媒体清单…");
        });
        wanted = Some((media_rows(&studio), manifest_path(&studio)));
    });
    let Some((media, path)) = wanted else {
        return;
    };
    publish(&app);
    host::spawn(
        move || -> Result<PathBuf, String> {
            let path = path.ok_or_else(|| "没有打开的工程，清单无处保存".to_owned())?;
            let mut rows = Vec::new();
            for (id, name, media_path, duration, width, height, fps, space) in &media {
                let size = std::fs::metadata(media_path).map(|meta| meta.len()).unwrap_or(0);
                let hash =
                    concat_host::post::sha256_file(Path::new(media_path)).unwrap_or_default();
                rows.push(serde_json::json!({
                    "media_id": id,
                    "name": name,
                    "path": media_path,
                    "size": size,
                    "sha256": hash,
                    "exists": Path::new(media_path).is_file(),
                    "duration": duration,
                    "width": width,
                    "height": height,
                    "fps": fps,
                    "color_space": space,
                }));
            }
            let value = serde_json::json!({
                "schema": "xinglai.media_manifest.v1",
                "generated_by": "星籁剪辑 0.3.6",
                "generated_at": local_timestamp(),
                "media": rows,
            });
            let text = serde_json::to_string_pretty(&value)
                .map_err(|error| format!("清单编码失败：{error}"))?;
            std::fs::write(&path, text).map_err(|error| format!("清单写不了：{error}"))?;
            Ok(path)
        },
        |_studio, app, _models, result| {
            with_post(|st| {
                st.check_busy = false;
                match result {
                    Ok(path) => {
                        st.check_line = format!("媒体清单已写入 {}", path.display());
                        st.set_status(
                            "info",
                            "媒体清单已生成：可交给下游按 sha256 / 文件名重链。",
                        );
                    }
                    Err(error) => st.set_status("fail", error),
                }
            });
            publish(&app);
        },
    );
}

// ─────────────────────────────────────────────────────────────────────
// 版本快照
// ─────────────────────────────────────────────────────────────────────

fn project_folder(studio: &Studio) -> Option<PathBuf> {
    let session = studio.session.as_ref()?;
    let dir = PathBuf::from(session.path());
    if dir.as_os_str().is_empty() {
        None
    } else {
        Some(dir)
    }
}

fn snapshot_folder(studio: &Studio) -> Option<PathBuf> {
    project_folder(studio).map(|dir| dir.join("snapshots"))
}

/// 保存前自动快照：同一分钟内只留一份；没有工程就什么都不做。
pub fn snapshot_before_save(studio: &Studio) {
    let (Some(folder), Some(project_dir)) = (snapshot_folder(studio), project_folder(studio)) else {
        return;
    };
    let doc = project_dir.join("concat.json");
    if !doc.is_file() || std::fs::create_dir_all(&folder).is_err() {
        return;
    }
    let stamp = snapshot_stamp();
    let minute = &stamp[..13.min(stamp.len())];
    let already = std::fs::read_dir(&folder)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("auto-{minute}"))
        });
    if already {
        return;
    }
    let _ = std::fs::copy(&doc, folder.join(format!("auto-{stamp}.json")));
}

fn snapshot_stamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d, hh, mm, ss) = local_parts(seconds);
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

fn local_parts(seconds: u64) -> (i64, u32, u32, u64, u64, u64) {
    let shifted = seconds + 8 * 3600;
    let days = (shifted / 86400) as i64;
    let rem = shifted % 86400;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, rem / 3600, (rem / 60) % 60, rem % 60)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn local_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d, hh, mm, ss) = local_parts(seconds);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

fn snapshot_label(name: &str) -> String {
    let stem = name.trim_end_matches(".json");
    let (kind, stamp) = stem.split_once('-').unwrap_or(("auto", stem));
    let (date, time) = stamp.split_once('-').unwrap_or((stamp, ""));
    let date = if date.len() == 8 {
        format!("{}-{}-{}", &date[0..4], &date[4..6], &date[6..8])
    } else {
        date.to_owned()
    };
    let time = if time.len() == 6 {
        format!("{}:{}:{}", &time[0..2], &time[2..4], &time[4..6])
    } else {
        time.to_owned()
    };
    format!(
        "{date} {time} · {}",
        if kind == "auto" { "自动（保存前）" } else { "手动" }
    )
}

fn refresh_snapshots(studio: &Studio, st: &mut PostState) {
    let Some(folder) = snapshot_folder(studio) else {
        st.snapshot_rows.clear();
        st.snapshot_line = "没有打开的工程".to_owned();
        return;
    };
    let mut rows = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&folder) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        for file in files.into_iter().take(50) {
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let size = std::fs::metadata(&file).map(|meta| meta.len()).unwrap_or(0);
            let picked = st.snapshot_pick.as_deref() == Some(name.as_str());
            rows.push(Row {
                label: snapshot_label(&name),
                detail: format!("{name} · {:.1} KB", size as f64 / 1024.0),
                level: if picked { "ok" } else { "info" }.to_owned(),
                at: -1.0,
                value: name,
            });
        }
    }
    st.snapshot_line = format!("{} 份快照 · 存在 {}", rows.len(), folder.display());
    st.snapshot_rows = rows;
}

fn snapshot_create(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    Shell::with(|shell, app| {
        let studio = shell.studio.borrow();
        let result = (|| -> Result<String, String> {
            let folder = snapshot_folder(&studio).ok_or_else(|| "没有打开的工程".to_owned())?;
            let project_dir = project_folder(&studio).ok_or_else(|| "没有打开的工程".to_owned())?;
            let doc = project_dir.join("concat.json");
            if !doc.is_file() {
                return Err("工程文档还没落盘：先保存一次工程再快照".to_owned());
            }
            std::fs::create_dir_all(&folder)
                .map_err(|error| format!("建不了 snapshots 目录：{error}"))?;
            let stamp = snapshot_stamp();
            let name = format!("manual-{stamp}.json");
            std::fs::copy(&doc, folder.join(&name))
                .map_err(|error| format!("快照写不了：{error}"))?;
            Ok(name)
        })();
        with_post(|st| {
            match &result {
                Ok(name) => {
                    st.snapshot_pick = Some(name.clone());
                    st.set_status("info", format!("已创建快照 {name}"));
                }
                Err(error) => st.set_status("fail", error),
            }
            refresh_snapshots(&studio, st);
        });
        studio.publish(&app, &shell.models);
    });
    publish(&app);
}

fn snapshot_diff(weak: slint::Weak<App>, index: usize) {
    let Some(app) = weak.upgrade() else { return };
    Shell::with(|shell, app| {
        let studio = shell.studio.borrow();
        let Some(folder) = snapshot_folder(&studio) else { return };
        let Some(name) = with_post(|st| st.snapshot_rows.get(index).map(|row| row.value.clone()))
        else {
            return;
        };
        let snapshot = folder.join(&name);
        let current = project_folder(&studio).map(|dir| dir.join("concat.json"));
        let diff = match (
            std::fs::read_to_string(&snapshot),
            current.and_then(|path| std::fs::read_to_string(path).ok()),
        ) {
            (Ok(a), Some(b)) => diff_documents(&a, &b),
            (Ok(_), None) => "当前工程文档不在磁盘上：先保存一次再对比".to_owned(),
            (Err(error), _) => format!("读不了快照：{error}"),
        };
        with_post(|st| {
            st.snapshot_pick = Some(name.clone());
            st.set_status("info", format!("对比 {name}：{diff}"));
            refresh_snapshots(&studio, st);
        });
        studio.publish(&app, &shell.models);
    });
    publish(&app);
}

fn diff_documents(snapshot: &str, current: &str) -> String {
    let (Ok(a), Ok(b)) = (
        serde_json::from_str::<serde_json::Value>(snapshot),
        serde_json::from_str::<serde_json::Value>(current),
    ) else {
        return "文档解析失败（不是工程 JSON？）".to_owned();
    };
    let clips = |doc: &serde_json::Value| -> BTreeMap<String, (f64, f64)> {
        let mut out = BTreeMap::new();
        if let Some(timelines) = doc.get("timelines").and_then(|v| v.as_array()) {
            for timeline in timelines {
                if let Some(rows) = timeline.get("clips").and_then(|v| v.as_array()) {
                    for clip in rows {
                        let id = clip.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let start = clip.get("start").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let duration =
                            clip.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        out.insert(id.to_owned(), (start, duration));
                    }
                }
            }
        }
        out
    };
    let a = clips(&a);
    let b = clips(&b);
    let added = b.keys().filter(|id| !a.contains_key(*id)).count();
    let removed = a.keys().filter(|id| !b.contains_key(*id)).count();
    let changed = a
        .iter()
        .filter(|(id, (start, duration))| {
            b.get(*id).is_some_and(|(start2, duration2)| {
                (start - start2).abs() > 0.001 || (duration - duration2).abs() > 0.001
            })
        })
        .count();
    let media = |doc: &serde_json::Value| {
        doc.get("media")
            .and_then(|v| v.as_array())
            .map(|rows| rows.len())
            .unwrap_or(0)
    };
    format!(
        "片段 +{added} / -{removed} / 改动 {changed} · 素材 {} → {} 条",
        media(&serde_json::from_str::<serde_json::Value>(snapshot).unwrap_or_default()),
        media(&serde_json::from_str::<serde_json::Value>(current).unwrap_or_default())
    )
}

fn snapshot_rollback(weak: slint::Weak<App>, index: usize) {
    let Some(app) = weak.upgrade() else { return };
    Shell::with(|shell, app| {
        let mut studio = shell.studio.borrow_mut();
        let Some(folder) = snapshot_folder(&studio) else { return };
        let Some(name) = with_post(|st| st.snapshot_rows.get(index).map(|row| row.value.clone()))
        else {
            return;
        };
        let snapshot = folder.join(&name);
        let Some(project_dir) = project_folder(&studio) else { return };
        if !snapshot.is_file() {
            with_post(|st| st.set_status("fail", "快照文件不见了，刷新列表看看"));
            return;
        }
        snapshot_before_save(&studio);
        let target = project_dir.join("concat.json");
        if let Err(error) = std::fs::copy(&snapshot, &target) {
            with_post(|st| st.set_status("fail", format!("回滚失败：{error}")));
            return;
        }
        let path = project_dir.to_string_lossy().into_owned();
        match studio.open_project_at(&path) {
            Ok(()) => {
                with_post(|st| {
                    st.snapshot_pick = Some(name.clone());
                    st.set_status(
                        "info",
                        format!("已回滚到 {name} 并重新打开工程；回滚前的状态也留了一份自动快照。"),
                    );
                    refresh_snapshots(&studio, st);
                    load_grade(&studio, st);
                });
                studio.notify("工程已回滚到快照版本", false);
            }
            Err(error) => {
                with_post(|st| st.set_status("fail", format!("回滚写入了但重开失败：{error}")));
            }
        }
        studio.publish(&app, &shell.models);
    });
    publish(&app);
}

// ─────────────────────────────────────────────────────────────────────
// 交付
// ─────────────────────────────────────────────────────────────────────

fn handoff_find(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    let mut start = String::new();
    Shell::with(|shell, _app| {
        let studio = shell.studio.borrow();
        start = project_folder(&studio)
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default();
    });
    with_post(|st| {
        st.busy = true;
        st.set_status("info", "选择交付包放哪儿…");
    });
    publish(&app);
    host::after_dialog(
        move || platform::pick_folder("选择交付包 / EDL / OTIO 的导出文件夹", &start),
        |studio, app, _models, folder| {
            with_post(|st| {
                st.busy = false;
                st.status_line.clear();
            });
            build_handoff(app, studio, folder);
            publish(&app);
        },
    );
}

fn build_handoff(app: &App, studio: &Studio, folder: PathBuf) {
    if studio.session.is_none() {
        with_post(|st| st.set_status("fail", "没有打开的工程：先打开或新建一个工程"));
        publish(&app);
        return;
    }
    if studio.timeline().clips.is_empty() {
        with_post(|st| st.set_status("warn", "时间线是空的：先放几段素材再导出交付包"));
        publish(&app);
        return;
    }
    let project = studio.project().clone();
    let name = if studio.project_name.trim().is_empty() {
        "星籁剪辑工程".to_owned()
    } else {
        studio.project_name.trim().to_owned()
    };
    let safe = safe_name(&name);
    let document = studio
        .session
        .as_ref()
        .map(|session| session.document())
        .unwrap_or_default();
    let project_value = concat_host::post::project_json(&project);
    let edl = concat_host::post::edl_text(&project, &name);
    let otio = concat_host::post::otio_json(&project);
    let project_json_text = serde_json::to_string_pretty(&project_value).unwrap_or_default();
    let document_text = serde_json::to_string_pretty(&document).unwrap_or_default();
    let media = media_rows(studio);
    let (grade_cdl, grade_space, lut_bytes) = with_post(|st| {
        let slug = st
            .current_lut()
            .map(|lut| lut.id.clone())
            .unwrap_or_default();
        let lut_path = if slug.is_empty() {
            None
        } else {
            let path = Studio::looks_dir(&studio.host.dirs)
                .join(&slug)
                .join("look.cube");
            path.is_file().then_some(path)
        };
        let bytes = lut_path.and_then(|path| std::fs::read(path).ok());
        (
            st.cdl,
            SPACES[st.project_space.clamp(0, 5) as usize],
            bytes.map(|bytes| (slug, bytes)),
        )
    });
    let qc_report = with_post(|st| st.qc_report.clone());
    let fps = {
        let num = studio.timeline().video.rate_num as f64;
        let den = studio.timeline().video.rate_den as f64;
        if num > 0.0 && den > 0.0 {
            num / den
        } else {
            25.0
        }
    };
    let width = studio.timeline().video.width;
    let height = studio.timeline().video.height;
    let _ = std::fs::create_dir_all(&folder);
    host::spawn(
        move || -> HandoffOutcome {
            let platform_health = concat_host::post::health();
            if platform_health.is_ok() {
                let body = serde_json::json!({
                    "project": project_value.clone(),
                    "version": "v1",
                    "media_mode": "reference",
                    "include_media": false,
                    "include_reference": false,
                    "qc": false,
                });
                if let Ok(response) = concat_host::post::handoff(&body) {
                    let url = response
                        .get("zip")
                        .and_then(|zip| zip.get("download_url"))
                        .and_then(|v| v.as_str())
                        .map(concat_host::post::absolute_url)
                        .or_else(|| {
                            response
                                .get("zip")
                                .and_then(|zip| zip.get("url"))
                                .and_then(|v| v.as_str())
                                .filter(|url| url.starts_with("http"))
                                .map(str::to_owned)
                        });
                    if let Some(url) = url {
                        let file_name = response
                            .get("zip")
                            .and_then(|zip| zip.get("file"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("handoff.zip");
                        let dest = folder.join(file_name);
                        if let Ok(size) = concat_host::post::download(&url, &dest) {
                            return HandoffOutcome {
                                source: "平台 /api/post/handoff".to_owned(),
                                zip: dest,
                                size,
                                note: "平台打包：工程 / EDL / OTIO / QC 由平台生成后下载到本地。".to_owned(),
                            };
                        }
                    }
                }
            }
            // 本机兜底。
            let mut files: Vec<(String, Vec<u8>)> = Vec::new();
            files.push((
                "README.md".to_owned(),
                readme_text(&name, &grade_space, fps, width, height).into_bytes(),
            ));
            files.push(("project.json".to_owned(), document_text.into_bytes()));
            files.push((
                "project.xinglai.json".to_owned(),
                project_json_text.into_bytes(),
            ));
            files.push(("timelines/project.edl".to_owned(), edl.into_bytes()));
            files.push((
                "timelines/project.otio".to_owned(),
                serde_json::to_string_pretty(&otio).unwrap_or_default().into_bytes(),
            ));
            files.push((
                "color/color_notes.md".to_owned(),
                color_notes(&grade_space).into_bytes(),
            ));
            files.push((
                "color/cdl/xinglai.cdl".to_owned(),
                cdl_text(&grade_cdl).into_bytes(),
            ));
            files.push((
                "color/cdl/xinglai.ccc".to_owned(),
                ccc_text(&grade_cdl).into_bytes(),
            ));
            if let Some((slug, bytes)) = lut_bytes {
                files.push((format!("color/luts/{slug}.cube"), bytes));
            }
            let mut manifest_rows = Vec::new();
            for (_id, name, path, duration, width, height, fps, space) in &media {
                let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
                let hash =
                    concat_host::post::sha256_file(Path::new(path)).unwrap_or_default();
                manifest_rows.push(serde_json::json!({
                    "name": name,
                    "path": path,
                    "size": size,
                    "sha256": hash,
                    "exists": Path::new(path).is_file(),
                    "duration": duration,
                    "width": width,
                    "height": height,
                    "fps": fps,
                    "color_space": space,
                }));
            }
            let media_manifest = serde_json::json!({
                "schema": "xinglai.media_manifest.v1",
                "generated_by": "星籁剪辑 0.3.6",
                "generated_at": local_timestamp(),
                "media": manifest_rows,
            });
            files.push((
                "media_manifest.json".to_owned(),
                serde_json::to_string_pretty(&media_manifest)
                    .unwrap_or_default()
                    .into_bytes(),
            ));
            if let Some(report) = qc_report {
                files.push((
                    "qc/qc_report.json".to_owned(),
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_default()
                        .into_bytes(),
                ));
                let status = report
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("WARN");
                let line = report
                    .get("summary")
                    .and_then(|s| s.get("integrated_lufs"))
                    .and_then(|v| v.as_f64())
                    .map(|lufs| format!("{status} · {lufs:.1} LUFS"))
                    .unwrap_or_else(|| status.to_owned());
                files.push(("qc/qc_report.html".to_owned(), qc_html(&name, &line).into_bytes()));
            } else {
                files.push((
                    "qc/qc_report.json".to_owned(),
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema": "xinglai.qc_report.v1",
                        "status": "SKIPPED",
                        "note": "导出交付包时还没有跑过质检：在「交付质检 QC」页跑一次再导出。",
                    }))
                    .unwrap_or_default()
                    .into_bytes(),
                ));
            }
            let (manifest_text, checksums) = concat_host::post::handoff_manifest(&files);
            files.push(("manifest.json".to_owned(), manifest_text.into_bytes()));
            files.push(("checksums.sha256".to_owned(), checksums.into_bytes()));
            let zip_path = folder.join(format!("{safe}_交付包.zip"));
            match concat_host::post::write_zip(&zip_path, files) {
                Ok(size) => HandoffOutcome {
                    source: "本机生成（平台不可用）".to_owned(),
                    zip: zip_path,
                    size,
                    note: platform_health
                        .err()
                        .unwrap_or_else(|| "平台接口没有返回可下载的包，已按同一结构在本机生成".to_owned()),
                },
                Err(error) => HandoffOutcome {
                    source: "失败".to_owned(),
                    zip: zip_path,
                    size: 0,
                    note: format!("本机打包失败：{error}"),
                },
            }
        },
        move |studio, app, _models, outcome| {
            if outcome.size == 0 {
                with_post(|st| {
                    st.busy = false;
                    st.handoff_line = outcome.note.clone();
                    st.set_status("fail", &outcome.note);
                });
                publish(&app);
                return;
            }
            with_post(|st| {
                st.busy = false;
                st.handoff_path = outcome
                    .zip
                    .parent()
                    .map(|dir| dir.to_string_lossy().into_owned())
                    .unwrap_or_default();
                st.handoff_line = format!(
                    "来源 {} · {} · {:.2} MB\n{}\n内容：project.json / EDL / OTIO / QC / 媒体清单 / 色彩说明 / README / checksums.sha256",
                    outcome.source,
                    outcome
                        .zip
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    outcome.size as f64 / (1024.0 * 1024.0),
                    outcome.note
                );
                st.set_status("info", format!("交付包已生成：{}", st.handoff_path));
            });
            publish(&app);
            studio.notify("交付包已生成，可在面板里打开文件夹", false);
        },
    );
}

struct HandoffOutcome {
    source: String,
    zip: PathBuf,
    size: u64,
    note: String,
}

fn readme_text(name: &str, space: &str, fps: f64, width: u32, height: u32) -> String {
    format!(
        "# {name} · 专业交接包\n\n\
由星籁剪辑 0.3.6 导出（本地兜底包）。\n\n\
## 时间线\n\n\
- 帧率：{fps:.3} fps · 画幅：{width}×{height}\n\
- 时间码起点：01:00:00:00（CMX3600 惯例）\n\
- 色彩空间标记：{space}\n\n\
## 文件\n\n\
| 路径 | 用途 |\n|---|---|\n\
| timelines/project.edl | CMX3600 EDL，单视频轨，达芬奇 / PR / Avid 可直接导入 |\n\
| timelines/project.otio | OpenTimelineIO（0.17），Resolve 18+ / Avid 2025+ 可导入 |\n\
| project.json | 星籁原生工程文档，可回导 |\n\
| project.xinglai.json | 平台 xinglai.post.project.v1 描述 |\n\
| media_manifest.json | 媒体清单（路径 / 大小 / sha256 / 时长 / 分辨率 / 色彩空间） |\n\
| color/ | 色彩说明 + ASC CDL（.cdl / .ccc）+ show LUT（.cube） |\n\
| qc/ | 交付质检报告（JSON / HTML，无则说明未跑） |\n\
| checksums.sha256 | 整包校验：解压后 `sha256sum -c checksums.sha256` |\n\n\
## 下游导入要点\n\n\
1. EDL 只有一个视频轨；多轨、转场、特效以 OTIO 与工程 JSON 为准。\n\
2. LUT：调色页 → Open LUT Folder，把 color/luts/*.cube 放进去再套用。\n\
3. CDL：调色页片段右键 Load CDL 载入 color/cdl/xinglai.cdl（Resolve / Baselight 支持）。\n\
4. 素材重链：按 media_manifest.json 里的文件名与 sha256 找原素材。\n\n\
## 限制（如实说明）\n\n\
- 本地 EDL 不含转场时长、变速曲线与多轨音频混音，这些以 OTIO / 工程 JSON 为准。\n\
- LUT 在引擎里以 GPU look 包应用，导出成片时已烘焙；交给下游的是原始 show LUT 表。\n\
- ACES 系列只作标签与工作空间说明，不写码流 VUI；交付前需按目标 ODT 输出。\n",
        name = name,
        space = space,
        fps = fps,
        width = width,
        height = height
    )
}

fn color_notes(space: &str) -> String {
    format!(
        "# 色彩管理说明\n\n\
- 工程 / 时间线输出色彩空间标记：{space}\n\
- 调色顺序：ASC CDL（slope / offset / power / 饱和度）→ show LUT。\n\
- 交付成片按目标色彩空间输出；ACES 标签只说明交换意图，不替代 ODT。\n\
- color/cdl/ 下是本次调色的 CDL（.cdl）与集合版（.ccc）；color/luts/ 下是 show LUT 原表。\n\
- 下游若只取 EDL：请按 README 的时间码起点与帧率设置工程。\n"
    )
}

fn qc_html(name: &str, line: &str) -> String {
    format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\">\
<title>{name} · 交付质检</title>\
<style>body{{font-family:system-ui,sans-serif;background:#111;color:#eee;padding:24px}}\
h1{{font-size:20px}}code{{color:#9ad}}</style></head><body>\
<h1>{name} · 交付质检</h1><p>结论：<b>{line}</b></p>\
<p>机器可读报告见 <code>qc/qc_report.json</code>。阈值 EBU R128：-23 LUFS ±1 LU，真峰 ≤ -1 dBTP。</p>\
</body></html>"
    )
}

fn export_single(weak: slint::Weak<App>, kind: &'static str) {
    let Some(app) = weak.upgrade() else { return };
    let mut start = String::new();
    Shell::with(|shell, _app| {
        let studio = shell.studio.borrow();
        start = project_folder(&studio)
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default();
    });
    let title = match kind {
        "edl" => "选择 EDL(.edl) 的导出文件夹",
        "otio" => "选择 OTIO(.otio) 的导出文件夹",
        _ => "选择 QC 报告的导出文件夹",
    };
    with_post(|st| {
        st.busy = true;
        st.set_status("info", "选择导出文件夹…");
    });
    publish(&app);
    host::after_dialog(
        move || platform::pick_folder(title, &start),
        move |studio, app, _models, folder| {
            let result = write_single(studio, kind, &folder);
            with_post(|st| {
                st.busy = false;
                match result {
                    Ok((_path, line)) => {
                        st.set_status("info", line.clone());
                        st.handoff_line = line;
                        st.handoff_path = folder.to_string_lossy().into_owned();
                    }
                    Err(error) => st.set_status("fail", error),
                }
            });
            publish(&app);
        },
    );
}

fn write_single(studio: &Studio, kind: &str, folder: &Path) -> Result<(PathBuf, String), String> {
    if studio.session.is_none() {
        return Err("没有打开的工程".to_owned());
    }
    if studio.timeline().clips.is_empty() {
        return Err("时间线是空的：先放几段素材".to_owned());
    }
    let project = studio.project();
    let name = if studio.project_name.trim().is_empty() {
        "星籁剪辑工程".to_owned()
    } else {
        studio.project_name.trim().to_owned()
    };
    let safe = safe_name(&name);
    std::fs::create_dir_all(folder).map_err(|error| format!("建不了导出目录：{error}"))?;
    match kind {
        "edl" => {
            let path = folder.join(format!("{safe}.edl"));
            std::fs::write(&path, concat_host::post::edl_text(project, &name))
                .map_err(|error| format!("EDL 写不了：{error}"))?;
            let count = studio.timeline().clips.len();
            Ok((
                path.clone(),
                format!(
                    "已导出 EDL：{}（CMX3600 · {count} 个片段 · 时间码起点 01:00:00:00）",
                    path.display()
                ),
            ))
        }
        "otio" => {
            let path = folder.join(format!("{safe}.otio"));
            let value = concat_host::post::otio_json(project);
            let text = serde_json::to_string_pretty(&value)
                .map_err(|error| format!("OTIO 编码失败：{error}"))?;
            std::fs::write(&path, text).map_err(|error| format!("OTIO 写不了：{error}"))?;
            Ok((
                path.clone(),
                format!(
                    "已导出 OTIO：{}（Timeline.1 / Stack.1 / Track.1 / Clip.2）",
                    path.display()
                ),
            ))
        }
        _ => {
            let report = with_post(|st| st.qc_report.clone());
            let Some(report) = report else {
                return Err("还没有质检结果：先在「交付质检 QC」页跑一次".to_owned());
            };
            let path = folder.join(format!("{safe}-qc_report.json"));
            let text = serde_json::to_string_pretty(&report)
                .map_err(|error| format!("QC 报告编码失败：{error}"))?;
            std::fs::write(&path, text).map_err(|error| format!("QC 报告写不了：{error}"))?;
            Ok((path.clone(), format!("已导出 QC 报告：{}", path.display())))
        }
    }
}

fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| match character {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' | '\t' => '_',
            other => other,
        })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "星籁剪辑".to_owned()
    } else {
        cleaned.chars().take(60).collect()
    }
}

// ─────────────────────────────────────────────────────────────────────
// 动作分发
// ─────────────────────────────────────────────────────────────────────

/// 面板上的一条动作。`index` / `text` 是 Slint 随手带的行号与数值。
pub fn action(weak: slint::Weak<App>, action: String, index: i32, text: String) {
    match action.as_str() {
        "tab" => {
            if let Some(app) = weak.upgrade() {
                Shell::with(|shell, _app| {
                    let studio = shell.studio.borrow();
                    with_post(|st| {
                        st.tab = index.clamp(0, TAB_HANDOFF);
                        st.force = true;
                        if st.tab == TAB_SNAPSHOT {
                            refresh_snapshots(&studio, st);
                        }
                        if st.tab == TAB_QC {
                            refresh_qc_target(&studio, st);
                        }
                    });
                    publish(&app);
                });
                tick(weak);
            }
        }
        "scope-view" => {
            if let Some(app) = weak.upgrade() {
                with_post(|st| {
                    st.scope_view = index.clamp(0, 3);
                    st.force = true;
                });
                tick(weak);
                publish(&app);
            }
        }
        "scope-live" => {
            if let Some(app) = weak.upgrade() {
                with_post(|st| {
                    st.scope_live = index == 1;
                    st.force = true;
                });
                tick(weak);
                publish(&app);
            }
        }
        "scope-refresh" => {
            with_post(|st| st.force = true);
            tick(weak);
        }
        "space-project" => {
            if let Some(app) = weak.upgrade() {
                Shell::with(|shell, _app| {
                    let studio = shell.studio.borrow();
                    let result = with_post(|st| {
                        st.project_space = index.clamp(0, 5);
                        save_grade(&studio, st)
                    });
                    with_post(|st| {
                        match &result {
                            Ok(_) => st.set_status(
                                "info",
                                format!(
                                    "项目色彩空间标记：{}（写进工程 post/grade.json，交付时随包）",
                                    SPACES[st.project_space.clamp(0, 5) as usize]
                                ),
                            ),
                            Err(error) => st.set_status("fail", error.clone()),
                        }
                    });
                    studio.publish(&app, &shell.models);
                });
                publish(&app);
            }
        }
        "space-clip" => {
            if let Some(app) = weak.upgrade() {
                Shell::with(|shell, _app| {
                    let studio = shell.studio.borrow();
                    let result = with_post(|st| {
                        st.clip_space = index.clamp(0, 5);
                        save_grade(&studio, st)
                    });
                    with_post(|st| match result {
                        Ok(_) => st.set_status(
                            "info",
                            format!(
                                "选中片段的交付色彩空间标签：{}（随 OTIO / 交付包走）",
                                SPACES[st.clip_space.clamp(0, 5) as usize]
                            ),
                        ),
                        Err(error) => st.set_status("fail", error),
                    });
                    studio.publish(&app, &shell.models);
                });
                publish(&app);
            }
        }
        "lut-import" => {
            if let Some(app) = weak.upgrade() {
                Studio::import_lut();
                with_post(|st| {
                    st.set_status(
                        "info",
                        "已打开 .cube 选择框；导入后列表会自动刷新，可反复套用。",
                    )
                });
                publish(&app);
            }
        }
        "lut-select" => {
            if let Some(app) = weak.upgrade() {
                Shell::with(|shell, _app| {
                    let studio = shell.studio.borrow();
                    with_post(|st| {
                        if st.luts.is_empty() {
                            return;
                        }
                        st.lut_selected = index.clamp(0, st.luts.len() as i32 - 1);
                        ensure_lut_table(&studio, st);
                        if let Some(entry) = st.current_lut() {
                            st.lut_line = format!(
                                "LUT：{}（{}）· 选中；预览与套用都用这一条",
                                entry.name, entry.space
                            );
                        }
                    });
                });
                tick(weak);
                publish(&app);
            }
        }
        "lut-strength" => {
            if let Some(app) = weak.upgrade() {
                let value = text
                    .trim()
                    .parse::<f32>()
                    .unwrap_or(100.0)
                    .clamp(0.0, 100.0);
                Shell::with(|shell, _app| {
                    let mut studio = shell.studio.borrow_mut();
                    let lut_id = with_post(|st| {
                        st.lut_strength = value;
                        st.current_lut().map(|lut| lut.id.clone())
                    });
                    if let Some(id) = lut_id {
                        if let Some(clip) = selected_clip(&studio) {
                            if let Some(position) =
                                clip.video_effects.iter().rposition(|filter| filter.id == id)
                            {
                                studio.chain_set_param(false, position as i32, "intensity", value);
                            }
                        }
                    }
                    studio.publish(&app, &shell.models);
                });
                tick(weak);
                publish(&app);
            }
        }
        "lut-apply" => {
            if let Some(app) = weak.upgrade() {
                Shell::with(|shell, app| {
                    let mut studio = shell.studio.borrow_mut();
                    let (level, message) = with_post(|st| apply_lut_to_selection(&mut studio, st));
                    let (level, message) = with_post(|st| {
                        st.set_status(&level, message.clone());
                        (level, message)
                    });
                    studio.notify(&message, level != "info");
                    studio.publish(&app, &shell.models);
                });
                publish(&app);
            }
        }
        "cdl-reset" => {
            if let Some(app) = weak.upgrade() {
                with_post(|st| {
                    st.cdl = Cdl::default();
                    st.set_status(
                        "info",
                        "CDL 已重置到恒等（slope 1 / offset 0 / power 1 / sat 1）",
                    );
                });
                tick(weak);
                publish(&app);
            }
        }
        "cdl-apply" => {
            if let Some(app) = weak.upgrade() {
                Shell::with(|shell, _app| {
                    let studio = shell.studio.borrow();
                    let result = with_post(|st| save_grade(&studio, st));
                    with_post(|st| match result {
                        Ok(path) => st.set_status(
                            "info",
                            format!(
                                "CDL 已保存：{}（同目录还有 grade.cdl / grade.ccc 给下游）",
                                path.display()
                            ),
                        ),
                        Err(error) => st.set_status("fail", error),
                    });
                    studio.publish(&app, &shell.models);
                });
                publish(&app);
            }
        }
        "qc-run" => qc_run(weak),
        "qc-pick" => {
            host::pick_files_then(
                "选择要质检的文件".to_owned(),
                Some((
                    "媒体文件".to_owned(),
                    &["mp4", "mov", "mkv", "wav", "mxf", "webm", "avi"],
                )),
                |_studio, app, _models, paths| {
                    if let Some(path) = paths.first() {
                        with_post(|st| {
                            st.qc_media_path = path.to_string_lossy().into_owned();
                            let name = path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            st.qc_target = format!("质检对象：{name}（本机文件）");
                            st.qc_clip = None;
                            st.set_status("info", "质检对象已换成本机文件；点「一键质检」开始。");
                        });
                        publish(&app);
                    }
                },
            );
        }
        "qc-jump" => qc_jump(weak, index.max(0) as usize),
        "check-run" => check_run(weak),
        "check-manifest-save" => check_manifest_save(weak),
        "check-manifest-open" => {
            let mut path = None;
            Shell::with(|shell, _app| {
                let studio = shell.studio.borrow();
                path = manifest_path(&studio);
            });
            if let Some(path) = path {
                if path.is_file() {
                    let _ = opener::open(path);
                } else if let Some(app) = weak.upgrade() {
                    with_post(|st| st.set_status("warn", "清单还没生成：点「生成 / 覆盖清单」"));
                    publish(&app);
                }
            }
        }
        "check-reveal" => {
            let value = with_post(|st| {
                st.check_rows
                    .get(index.max(0) as usize)
                    .map(|row| row.value.clone())
            });
            if let Some(value) = value {
                if Path::new(&value).is_file() {
                    let _ = opener::reveal(value);
                }
            }
        }
        "snapshot-create" => snapshot_create(weak),
        "snapshot-refresh" => {
            if let Some(app) = weak.upgrade() {
                Shell::with(|shell, _app| {
                    let studio = shell.studio.borrow();
                    with_post(|st| refresh_snapshots(&studio, st));
                    studio.publish(&app, &shell.models);
                });
                publish(&app);
            }
        }
        "snapshot-diff" => snapshot_diff(weak, index.max(0) as usize),
        "snapshot-rollback" => snapshot_rollback(weak, index.max(0) as usize),
        "handoff" => handoff_find(weak),
        "handoff-reveal" => {
            let path = with_post(|st| st.handoff_path.clone());
            if !path.is_empty() {
                let _ = opener::open(path);
            }
        }
        "export-edl" => export_single(weak, "edl"),
        "export-otio" => export_single(weak, "otio"),
        "export-qc" => export_single(weak, "qc"),
        "backend-check" => backend_check(weak),
        cdl if cdl.starts_with("cdl-") => {
            let value = text.trim().parse::<f32>().unwrap_or(0.0);
            let channel = cdl.trim_start_matches("cdl-").to_owned();
            if let Some(app) = weak.upgrade() {
                Shell::with(|shell, _app| {
                    let studio = shell.studio.borrow();
                    with_post(|st| {
                        match channel.as_str() {
                            "slope-r" => st.cdl.slope[0] = value.clamp(0.0, 2.0),
                            "slope-g" => st.cdl.slope[1] = value.clamp(0.0, 2.0),
                            "slope-b" => st.cdl.slope[2] = value.clamp(0.0, 2.0),
                            "offset-r" => st.cdl.offset[0] = value.clamp(-0.5, 0.5),
                            "offset-g" => st.cdl.offset[1] = value.clamp(-0.5, 0.5),
                            "offset-b" => st.cdl.offset[2] = value.clamp(-0.5, 0.5),
                            "power-r" => st.cdl.power[0] = value.clamp(0.2, 3.0),
                            "power-g" => st.cdl.power[1] = value.clamp(0.2, 3.0),
                            "power-b" => st.cdl.power[2] = value.clamp(0.2, 3.0),
                            "sat" => st.cdl.sat = value.clamp(0.0, 2.0),
                            _ => {}
                        }
                    });
                    let _ = with_post(|st| save_grade(&studio, st));
                });
                tick(weak);
                publish(&app);
            }
        }
        _ => {
            log::debug!("post: 未识别的动作 {action}");
        }
    }
}

fn backend_check(weak: slint::Weak<App>) {
    if let Some(app) = weak.upgrade() {
        with_post(|st| {
            st.backend = "平台 /api/post：正在探测…".to_owned();
            st.set_status("info", "正在探测平台 /api/post/health…");
        });
        publish(&app);
    }
    host::spawn(
        move || -> Result<serde_json::Value, String> {
            let health = concat_host::post::health()?;
            let standards = concat_host::post::standards().ok();
            Ok(serde_json::json!({"health": health, "standards": standards}))
        },
        |_studio, app, _models, result| {
            with_post(|st| match result {
                Ok(value) => {
                    let ok = value
                        .get("health")
                        .and_then(|health| health.get("ok"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false) || value.get("health").is_some();
                    st.backend = if ok {
                        "平台 /api/post：在线（EDL / OTIO / LUT / CDL / 校验 / QC / 交付包）".to_owned()
                    } else {
                        "平台 /api/post：有响应但未就绪，已切本地兜底".to_owned()
                    };
                    st.set_status(
                        if ok { "info" } else { "warn" },
                        if ok {
                            "平台后期接口在线：QC 与交付包会优先走平台".to_owned()
                        } else {
                            "平台后期接口未就绪：已启用本地兜底".to_owned()
                        },
                    );
                }
                Err(error) => {
                    st.backend = "平台 /api/post：不可用 · 已启用本地兜底".to_owned();
                    st.set_status(
                        "warn",
                        format!("平台后期接口不可用：{error}（本机 QC / EDL / OTIO / 交付包可用）"),
                    );
                }
            });
            publish(&app);
        },
    );
}

// ─────────────────────────────────────────────────────────────────────
// 命令面板
// ─────────────────────────────────────────────────────────────────────

struct Command {
    id: &'static str,
    label: &'static str,
    category: &'static str,
    shortcut: &'static str,
}

const COMMANDS: &[Command] = &[
    Command { id: "post-panel", label: "专业后期面板（示波器 / 调色 / QC / 交付）", category: "后期", shortcut: "" },
    Command { id: "post-scope", label: "示波器 · 波形 / Parade / 矢量 / 直方图", category: "后期", shortcut: "" },
    Command { id: "post-color", label: "调色 · LUT / CDL / 色彩空间", category: "后期", shortcut: "" },
    Command { id: "qc", label: "一键质检（响度 / 真峰 / 黑帧 / 静音 / 冻结）", category: "后期", shortcut: "" },
    Command { id: "checks", label: "媒体校验（sha256 清单 · 重链）", category: "后期", shortcut: "" },
    Command { id: "snapshots", label: "工程版本快照（对比 / 回滚）", category: "后期", shortcut: "" },
    Command { id: "handoff", label: "导出交付包（EDL / OTIO / QC / 工程 JSON）", category: "交付", shortcut: "" },
    Command { id: "export-edl", label: "导出 EDL(.edl)", category: "交付", shortcut: "" },
    Command { id: "export-otio", label: "导出 OTIO(.otio)", category: "交付", shortcut: "" },
    Command { id: "export-qc", label: "导出 QC 报告(JSON)", category: "交付", shortcut: "" },
    Command { id: "save", label: "保存工程", category: "文件", shortcut: "Ctrl+S" },
    Command { id: "export", label: "导出成片", category: "文件", shortcut: "Ctrl+E" },
    Command { id: "import", label: "导入媒体", category: "文件", shortcut: "Ctrl+I" },
    Command { id: "import-ai-pack", label: "导入 AI 工程包(.zip)", category: "文件", shortcut: "" },
    Command { id: "export-jianying", label: "导出剪映草稿（JSON）", category: "互通", shortcut: "" },
    Command { id: "export-xml", label: "导出 FCPXML / Premiere XML", category: "互通", shortcut: "" },
    Command { id: "settings", label: "设置", category: "应用", shortcut: "Ctrl+," },
    Command { id: "help-shortcuts", label: "快捷键查看 / 改键", category: "应用", shortcut: "" },
    Command { id: "layout-default", label: "布局：默认（左素材 · 中预览/时间线 · 右属性）", category: "视图", shortcut: "" },
    Command { id: "layout-portrait", label: "布局：竖屏创作", category: "视图", shortcut: "" },
    Command { id: "layout-edit", label: "布局：剪辑优先", category: "视图", shortcut: "" },
    Command { id: "layout-reset", label: "重置当前布局", category: "视图", shortcut: "" },
    Command { id: "undo", label: "撤销", category: "编辑", shortcut: "Ctrl+Z" },
    Command { id: "redo", label: "重做", category: "编辑", shortcut: "Ctrl+Shift+Z" },
    Command { id: "split", label: "在播放头切开片段", category: "编辑", shortcut: "" },
    Command { id: "delete", label: "删除选中片段", category: "编辑", shortcut: "Delete" },
    Command { id: "zoom-in", label: "时间线放大", category: "视图", shortcut: "" },
    Command { id: "zoom-out", label: "时间线缩小", category: "视图", shortcut: "" },
    Command { id: "movie-wizard", label: "电影 · 剧集制作向导", category: "AI", shortcut: "" },
    Command { id: "ai-drama-open", label: "AI 短剧", category: "AI", shortcut: "" },
    Command { id: "ai-studio-open", label: "AI 工作室", category: "AI", shortcut: "" },
    Command { id: "ai-voice", label: "AI 配音", category: "AI", shortcut: "" },
];

/// 打开 / 关闭命令面板（Ctrl+K）。
pub fn toggle_palette(weak: slint::Weak<App>) {
    let Some(app) = weak.upgrade() else { return };
    let open = !app.get_palette_open();
    if open {
        with_post(|st| st.palette_open = true);
        app.set_palette_open(true);
        refresh_commands(&app, "");
    } else {
        app.set_palette_open(false);
        with_post(|st| st.palette_open = false);
    }
}

pub fn palette_close(weak: slint::Weak<App>) {
    if let Some(app) = weak.upgrade() {
        app.set_palette_open(false);
        with_post(|st| st.palette_open = false);
    }
}

pub fn palette_query(weak: slint::Weak<App>, query: String) {
    if let Some(app) = weak.upgrade() {
        with_post(|st| st.command_query = query.clone());
        refresh_commands(&app, &query);
    }
}

fn refresh_commands(app: &App, query: &str) {
    let query = query.trim().to_lowercase();
    let mut entries: Vec<CommandItem> = Vec::new();
    for command in COMMANDS {
        let matches = query.is_empty()
            || command.label.to_lowercase().contains(&query)
            || command.category.to_lowercase().contains(&query)
            || command.id.contains(&query);
        if !matches {
            continue;
        }
        entries.push(CommandItem {
            id: command.id.into(),
            label: command.label.into(),
            category: command.category.into(),
            shortcut: command.shortcut.into(),
        });
    }
    let first = entries
        .first()
        .map(|item| item.id.clone())
        .unwrap_or_else(|| SharedString::default());
    // 命令表只写进 Slint 模型，不再存一份在窗口状态里。
    app.set_command_items(ModelRc::from(Rc::new(VecModel::from(entries))));
    app.set_command_first(first);
}

pub fn palette_chosen(weak: slint::Weak<App>, id: String) {
    let Some(app) = weak.upgrade() else { return };
    app.set_palette_open(false);
    with_post(|st| st.palette_open = false);
    match id.as_str() {
        "post-panel" => return open_tab(weak, TAB_SCOPE),
        "post-scope" => return open_tab(weak, TAB_SCOPE),
        "post-color" => return open_tab(weak, TAB_COLOR),
        "qc" => return open_tab(weak, TAB_QC),
        "checks" => return open_tab(weak, TAB_CHECK),
        "snapshots" => return open_tab(weak, TAB_SNAPSHOT),
        "handoff" => {
            open_tab(weak.clone(), TAB_HANDOFF);
            return handoff_find(weak);
        }
        "export-edl" => return export_single(weak, "edl"),
        "export-otio" => return export_single(weak, "otio"),
        "export-qc" => return export_single(weak, "qc"),
        _ => {}
    }
    app.invoke_app_menu_selected(SharedString::from(id));
}

/// 快捷键查看页：设置 → 快捷键（第 7 页）。
pub fn open_shortcuts(weak: slint::Weak<App>) {
    if let Some(_app) = weak.upgrade() {
        Shell::with(|shell, app| {
            {
                let mut studio = shell.studio.borrow_mut();
                studio.handle(crate::panes::Msg::Settings(
                    crate::panes::settings::SettingsMsg::Open,
                ));
                studio.handle(crate::panes::Msg::Settings(
                    crate::panes::settings::SettingsMsg::PageChanged(
                        crate::panes::settings::SHORTCUTS_PAGE,
                    ),
                ));
            }
            let studio = shell.studio.borrow();
            studio.publish(&app, &shell.models);
        });
    }
}
