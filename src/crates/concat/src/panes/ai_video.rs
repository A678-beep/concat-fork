// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! AI 面板的第四页：星籁 AI 生视频（云端）。
//!
//! 三条路：
//! - **文生视频**：提示词 → 平台 `/aivideo`；平台当前唯一开通的模型是
//!   图生视频，所以桌面端先用 ffmpeg 生成一张纯净底图当首帧，如实注明。
//! - **图生视频**：本地上传一张图（或从时间轴取当前帧）当首帧。
//! - **本地模型**（预留）：检测显存并列出 Wan2.2-I2V-A14B / LTX-Video 的
//!   体积与最低显存；模型包未发布，按钮只说明「即将开放」，不做假下载。
//!
//! 生成长视频：平台每条最多 5 秒；10/30/60 秒 = 多段顺序生成，上一段最后
//! 一帧续写下一段，最后 ffmpeg 拼接（界面常驻「质量与一致性可能下降」）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use concat_host::aivideo::{self, Inventory};
use concat_host::hardware::Hardware;
use concat_host::media;
use concat_project::Command;

use crate::host;
use crate::panes::Msg;
use crate::studio::Studio;
use crate::ui::AiVideoData;

/// 视频模型 → 一句话说明（平台没给说明时兜底）。
const MODELS_EMPTY_HINT: &str = "还没有取到平台模型列表：点「刷新模型」重试；未登录时也能看列表，但生成需先登录。";

/// 本地模型的固定说明（预留）。
const LOCAL_NOTE: &str = "本地视频模型（Wan2.2-I2V-A14B / LTX-Video）需要先下载 20–35GB 的模型包，且要有 16–24GB 显存。模型包本版本尚未发布：按钮只显示「即将开放」，不做假下载；本地推理接口已经预留，发布后接入。";

/// 一条 5 秒片段在平台侧的实际行为说明。
const SEGMENT_NOTE: &str = "平台每条最多生成 5 秒；10 秒 = 2 段、30 秒 = 6 段、60 秒 = 12 段顺序续写拼接。多段拼接的**质量与一致性可能下降**。";

/// 生视频面板的消息。
#[derive(Debug)]
pub enum VideoMsg {
    /// 界面上的一个动作：action + 下拉下标 + 文本。
    Action {
        /// `mode` / `prompt` / `generate` / `cancel` / `pick-image` …
        action: String,
        /// 下拉或按钮的下标。
        index: i32,
        /// 文字输入。
        text: String,
    },
    /// 后台进度。
    Progress {
        /// 0..1。
        fraction: f32,
        /// 正在做什么。
        stage: String,
    },
    /// 平台模型列表回来了。
    Models(Box<Result<Inventory, String>>),
    /// 生成结束。
    Done(Box<Result<aivideo::Output, String>>),
}

/// 面板状态。
#[derive(Default)]
pub struct AiVideoPane {
    /// 页内 tab：0 文生视频、1 图生视频、2 本地模型。
    pub mode: i32,
    /// 提示词。
    pub prompt: String,
    /// 负面提示（平台当前版本会忽略，仍随请求发送）。
    pub negative: String,
    /// 比例：0 9:16、1 16:9、2 1:1。
    pub aspect: i32,
    /// 时长档：0 5 秒、1 10 秒（2 段续写）。
    pub duration: i32,
    /// 模型下标。
    pub model: i32,
    /// 平台模型列表。
    pub inventory: Option<Inventory>,
    /// 模型列表没取到的原因。
    pub models_error: String,
    /// 正在取模型列表。
    pub loading: bool,
    /// 首帧：文件名、MIME、字节。
    pub image: Option<(String, String, Vec<u8>)>,
    /// 续写拼接开关（长视频）。
    pub extend: bool,
    /// 长视频目标：0 30 秒、1 60 秒。
    pub target: i32,
    /// 生成中。
    pub busy: bool,
    /// 生成进度 0..1。
    pub progress: f32,
    /// 正在做什么。
    pub stage: String,
    /// 常驻说明 / 结果。
    pub message: String,
    /// 上一次生成的一句话结果。
    pub last: String,
    /// 最后一次生成的成片。
    pub last_output: Option<PathBuf>,
    /// 取消标志。
    pub cancel: Option<Arc<AtomicBool>>,
    /// 本机硬件（本地模型页显示）。
    pub hardware: Option<Hardware>,
    /// 最近一次提示，避免同一句反复弹。
    last_alert: String,
}

impl AiVideoPane {
    /// 打开第 4 页时调用：还没取过模型列表就去取一次。
    pub fn ensure_models(&mut self, studio: &mut Studio) {
        if self.inventory.is_some() || self.loading {
            return;
        }
        self.load_models(studio);
    }

    /// 取平台模型列表。
    pub fn load_models(&mut self, studio: &mut Studio) {
        if self.loading || self.busy {
            return;
        }
        let _ = studio;
        self.loading = true;
        self.models_error.clear();
        let token = concat_host::account::load().map(|account| account.token);
        host::spawn(
            move || -> Result<Inventory, String> {
                aivideo::models(token.as_deref())
            },
            |studio, _, _, result| {
                studio.handle(Msg::Ai(crate::panes::ai::AiMsg::Video(VideoMsg::Models(
                    Box::new(result),
                ))));
            },
        );
    }

    /// 处理一条消息。
    pub fn update(&mut self, msg: VideoMsg, studio: &mut Studio) {
        match msg {
            VideoMsg::Action {
                action,
                index,
                text,
            } => self.action(&action, index, text, studio),
            VideoMsg::Progress { fraction, stage } => {
                self.progress = fraction.clamp(0.0, 1.0);
                self.stage = stage.clone();
                self.message = stage;
            }
            VideoMsg::Models(result) => {
                self.loading = false;
                match *result {
                    Ok(inventory) => {
                        let chosen = self
                            .inventory
                            .as_ref()
                            .and_then(|old| old.models.get(self.model.max(0) as usize))
                            .map(|model| model.id.clone());
                        self.model = inventory
                            .models
                            .iter()
                            .position(|model| {
                                chosen.as_deref() == Some(model.id.as_str()) && model.enabled
                            })
                            .or_else(|| inventory.default_index())
                            .map_or(0, |index| index as i32);
                        self.models_error.clear();
                        let usable = inventory.models.iter().filter(|m| m.enabled).count();
                        if usable == 0 {
                            self.message = inventory.hint.clone();
                        }
                        self.inventory = Some(inventory);
                    }
                    Err(error) => {
                        self.models_error = error.clone();
                        self.message = format!("取模型列表失败：{error}");
                    }
                }
            }
            VideoMsg::Done(result) => self.finished(studio, *result),
        }
    }

    fn action(&mut self, action: &str, index: i32, text: String, studio: &mut Studio) {
        match action {
            "mode" => {
                self.mode = index.clamp(0, 2);
                if self.mode < 2 {
                    self.ensure_models(studio);
                }
            }
            "prompt" => self.prompt = text.chars().take(600).collect(),
            "negative" => self.negative = text.chars().take(300).collect(),
            "aspect" => self.aspect = index.clamp(0, 2),
            "duration" => self.duration = index.clamp(0, 1),
            "target" => self.target = index.clamp(0, 1),
            "model" => self.model = index.max(0),
            "extend" => self.extend = !self.extend,
            "pick-image" => self.pick_image(studio),
            "clear-image" => {
                self.image = None;
                self.message = "已清掉首帧图片".to_owned();
            }
            "generate" => self.generate(studio),
            "cancel" => {
                if let Some(cancel) = self.cancel.take() {
                    cancel.store(true, Ordering::Relaxed);
                    self.stage = "已取消".to_owned();
                    self.message = "已取消：平台任务可能仍在后台跑完".to_owned();
                }
            }
            "insert" => self.insert_last(studio),
            "refresh-models" => self.load_models(studio),
            "open-folder" => {
                let dir = studio.host.dirs.config.join("ai-video");
                let _ = std::fs::create_dir_all(&dir);
                if opener::open(&dir).is_err() {
                    self.message = format!("打开目录失败：{}", dir.display());
                }
            }
            "local-download" => {
                let default = Hardware::default();
                let rows = aivideo::local_models(self.hardware.as_ref().unwrap_or(&default));
                let row = rows.get(index.max(0) as usize);
                self.message = match row {
                    Some(row) => format!(
                        "{}：{}。模型包尚未发布，本版本不提供下载，也不做假下载；发布后这里会变成真正的下载按钮。",
                        row.name, row.status
                    ),
                    None => "没有这个本地模型".to_owned(),
                };
                studio.notify("本地模型包尚未发布：等待模型包发布", true);
            }
            "local-generate" => {
                let error = aivideo::local_generate(
                    &aivideo::Request::default(),
                    &studio.host.dirs,
                )
                .err()
                .unwrap_or_default();
                self.message = error;
                studio.notify("本地推理还没开放（模型包未发布）", true);
            }
            "login" => {
                studio.notify("请先在首屏登录星籁账号", true);
                self.message = "请先登录星籁账号：登录后回到本页点「生成」".to_owned();
            }
            _ => {}
        }
    }

    /// 选一张本地图片当首帧。
    fn pick_image(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("选择首帧图片（JPG/PNG/WebP，≤10MB）")
            .add_filter("图片", &["jpg", "jpeg", "png", "webp"])
            .pick_file()
        else {
            return;
        };
        let size = std::fs::metadata(&path).map_or(0, |meta| meta.len());
        if size > 10 * 1024 * 1024 {
            self.message = "图片超过 10MB，请压缩后再选".to_owned();
            studio.notify("图片超过 10MB，请压缩后再选", true);
            return;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.message = format!("图片读不出来：{error}");
                return;
            }
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "首帧".to_owned());
        let extension = path
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let mime = match extension.as_str() {
            "png" => "image/png",
            "webp" => "image/webp",
            _ => "image/jpeg",
        };
        self.message = format!("已选首帧：{name}");
        self.image = Some((name, mime.to_owned(), bytes));
        studio.notify("首帧已选好", false);
    }

    /// 生成：单条或续写拼接。
    fn generate(&mut self, studio: &mut Studio) {
        if self.busy {
            return;
        }
        let Ok(token) = aivideo::token() else {
            self.message = "请先登录星籁账号：登录后回到本页再生成".to_owned();
            return;
        };
        let Some(inventory) = self.inventory.as_ref() else {
            self.message = "模型列表还没取到：先点「刷新模型」".to_owned();
            return;
        };
        let Some(model) = inventory.chosen(self.model.max(0) as usize) else {
            self.message = "选中的视频模型本账号未开通：换一个模型（MiniMax H3 可用）".to_owned();
            return;
        };
        if self.mode == 1 && self.image.is_none() {
            self.message = "图生视频需要先选一张首帧图片".to_owned();
            studio.notify("请先选一张首帧图片", true);
            return;
        }
        if self.prompt.trim().is_empty() {
            self.message = "请先写一句画面描述（提示词）".to_owned();
            return;
        }
        let seconds = if self.extend {
            if self.target == 1 { 60.0 } else { 30.0 }
        } else if self.duration == 1 {
            10.0
        } else {
            5.0
        };
        let segments = aivideo::segments_for(seconds);
        let aspect = match self.aspect {
            1 => "16:9",
            2 => "1:1",
            _ => "9:16",
        }
        .to_owned();
        let image = if self.mode == 1 {
            self.image.as_ref().map(|(_, mime, bytes)| (bytes.clone(), mime.clone()))
        } else {
            None
        };
        let request = aivideo::Request {
            prompt: self.prompt.trim().to_owned(),
            negative: self.negative.trim().to_owned(),
            model: model.id.clone(),
            aspect,
            image: image.as_ref().map(|(bytes, _)| bytes.clone()),
            image_mime: image
                .as_ref()
                .map(|(_, mime)| mime.clone())
                .unwrap_or_else(|| "image/jpeg".to_owned()),
        };
        let price = inventory.price_per_sec;
        let dirs = studio.host.dirs.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.busy = true;
        self.progress = 0.0;
        self.stage = "生成中".to_owned();
        self.message = if segments > 1 {
            format!("开始生成 {segments} 段 × 5 秒（续写拼接），预计约 ¥{:.1}", (seconds * price).max(0.0))
        } else {
            format!("开始生成 5 秒，预计约 ¥{:.1}", (5.0 * price).max(0.0))
        };
        let model_id = request.model.clone();
        let aspect_label = request.aspect.clone();
        host::spawn_in_project(
            move || -> Result<aivideo::Output, String> {
                let mut last_report: Option<Instant> = None;
                let mut report = |fraction: f32, stage: String| {
                    let now = Instant::now();
                    let due = last_report.is_none_or(|at| now.duration_since(at) >= std::time::Duration::from_millis(700));
                    if !due {
                        return;
                    }
                    last_report = Some(now);
                    host::on_ui(move |studio, _, _| {
                        studio.handle(Msg::Ai(crate::panes::ai::AiMsg::Video(
                            VideoMsg::Progress {
                                fraction,
                                stage,
                            },
                        )));
                    });
                };
                let output = aivideo::generate(
                    &token,
                    &dirs,
                    &request,
                    segments,
                    &mut report,
                    &cancel,
                )?;
                aivideo::log_usage(&dirs, &output, &model_id, &aspect_label);
                Ok(output)
            },
            |studio, _, _, result| {
                studio.handle(Msg::Ai(crate::panes::ai::AiMsg::Video(VideoMsg::Done(
                    Box::new(result),
                ))));
            },
        );
    }

    fn finished(&mut self, studio: &mut Studio, result: Result<aivideo::Output, String>) {
        self.busy = false;
        self.cancel = None;
        self.stage.clear();
        self.progress = 0.0;
        match result {
            Ok(output) => {
                let inserted = self.insert(studio, &output);
                self.last = match inserted {
                    Some(start) => format!(
                        "{} · {:.1} 秒 · {} 段 · 已插入时间轴 {:.2}s · 文件 {}",
                        output.cost_text,
                        output.duration,
                        output.segments,
                        start,
                        output.path.display()
                    ),
                    None => format!(
                        "{} · {:.1} 秒 · 文件 {}（没有打开的工程，未插入）",
                        output.cost_text,
                        output.duration,
                        output.path.display()
                    ),
                };
                self.message = self.last.clone();
                self.last_output = Some(output.path.clone());
                studio.notify("AI 视频已生成并插入时间轴", false);
            }
            Err(error) => {
                if error.contains("已取消") {
                    self.message = "已取消（平台任务可能仍在后台跑完）".to_owned();
                    self.last = self.message.clone();
                    return;
                }
                self.message = format!("生成失败：{error}");
                self.last = self.message.clone();
                if self.last_alert != self.message {
                    self.last_alert = self.message.clone();
                    studio.notify(&self.message, true);
                }
            }
        }
    }

    fn insert_last(&mut self, studio: &mut Studio) {
        let Some(path) = self.last_output.clone() else {
            self.message = "还没有生成的视频：先生成一条".to_owned();
            return;
        };
        let output = aivideo::Output {
            path,
            ..aivideo::Output::default()
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

    /// 成片进素材库并放到时间轴（播放头）；返回落点秒数。
    fn insert(&mut self, studio: &mut Studio, output: &aivideo::Output) -> Option<f64> {
        let summary = media::probe(&output.path.to_string_lossy()).ok()?;
        let name = output
            .path
            .file_stem()
            .map(|stem| format!("AI 视频 · {}", stem.to_string_lossy()))
            .unwrap_or_else(|| "AI 视频".to_owned());
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

    /// 发布给 Slint 的数据。
    pub fn data(
        &self,
        signed_in: bool,
        account_line: &str,
        hardware: Option<&Hardware>,
    ) -> AiVideoData {
        let inventory = self.inventory.as_ref();
        let price = inventory.map_or(0.0, |inventory| inventory.price_per_sec);
        let seconds = if self.extend {
            if self.target == 1 { 60.0 } else { 30.0 }
        } else if self.duration == 1 {
            10.0
        } else {
            5.0
        };
        let cost = aivideo::estimate(seconds, price);
        let model = inventory.and_then(|inventory| inventory.chosen(self.model.max(0) as usize));
        let model_note = match (inventory, model) {
            (None, _) => MODELS_EMPTY_HINT.to_owned(),
            (Some(_), Some(model)) => {
                let enabled = if model.enabled { "" } else { "（本账号未开通）" };
                format!("{}{enabled}", model.note.trim())
            }
            (Some(_), None) => "模型列表是空的：平台账号可能没有开通任何视频模型".to_owned(),
        };
        let segments = aivideo::segments_for(seconds);
        let gpu = hardware.map(|hardware| &hardware.gpu);
        let fit = gpu.is_some_and(|gpu| gpu.discrete && gpu.vram_mb >= 16 * 1024);
        let gpu_line = match hardware {
            Some(hardware) => {
                let gpu = if hardware.gpu.name.trim().is_empty() {
                    "未识别显卡".to_owned()
                } else if hardware.gpu.vram_mb > 0 {
                    format!("{} · {}GB 显存", hardware.gpu.name, hardware.gpu.vram_mb / 1024)
                } else {
                    hardware.gpu.name.clone()
                };
                format!(
                    "{} · {}GB 内存 · 系统盘剩 {}GB",
                    gpu,
                    hardware.ram_mb / 1024,
                    hardware.disk_free_gb
                )
            }
            None => "还没有检测本机硬件（打开 AI 面板时会检测）".to_owned(),
        };
        let local_fit = if fit {
            "本机满足本地生成的最低要求（≥16GB 显存），模型包发布后可用".to_owned()
        } else if gpu.is_some_and(|gpu| gpu.discrete) {
            "本机显存低于本地生成最低要求（16GB），只能用云端".to_owned()
        } else {
            "本机没有检测到独立显卡，不满足本地生成（需 16–24GB 显存）".to_owned()
        };
        AiVideoData {
            mode: self.mode,
            prompt: self.prompt.as_str().into(),
            negative: self.negative.as_str().into(),
            aspect: self.aspect,
            duration: self.duration,
            model: self.model,
            model_note: model_note.into(),
            models_hint: inventory
                .map(|inventory| inventory.hint.as_str())
                .unwrap_or(MODELS_EMPTY_HINT)
                .into(),
            cost: (if self.busy {
                String::new()
            } else {
                format!(
                    "本次预计消耗：约 ¥{cost:.1}（{segments} 段 × 5 秒 × ¥{:.2}/秒；平台返回的为准，管理员免扣由平台侧处理）",
                    if price > 0.0 { price } else { 0.5 }
                )
            })
            .into(),
            image_name: self
                .image
                .as_ref()
                .map(|(name, _, _)| name.clone())
                .unwrap_or_default()
                .into(),
            has_image: self.image.is_some(),
            extend: self.extend,
            target: self.target,
            segment_hint: format!(
                "{} 本次 {} 秒 = {} 段。{}",
                if self.extend { "长视频（续写拼接）已开。" } else { "" },
                seconds,
                segments,
                SEGMENT_NOTE
            )
            .into(),
            busy: self.busy,
            progress: self.progress,
            stage: self.stage.as_str().into(),
            message: self.message.as_str().into(),
            last: self.last.as_str().into(),
            has_output: self.last_output.is_some(),
            output_name: self
                .last_output
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
                .into(),
            signed_in,
            account_line: account_line.into(),
            loading: self.loading,
            models_error: self.models_error.as_str().into(),
            gpu_line: gpu_line.into(),
            local_fit: local_fit.into(),
            local_fit_ok: fit,
            local_note: LOCAL_NOTE.into(),
        }
    }

    /// 模型下拉的行。
    pub fn model_rows(&self) -> Vec<slint::SharedString> {
        match self.inventory.as_ref() {
            Some(inventory) => inventory
                .models
                .iter()
                .map(|model| {
                    let state = if model.enabled { "" } else { "（未开通）" };
                    slint::SharedString::from(format!("{}{state}", model.display()))
                })
                .collect(),
            None => vec![slint::SharedString::from("（模型列表未加载）")],
        }
    }

    /// 本地模型页的说明行（预留）。
    pub fn local_rows(&self) -> Vec<slint::SharedString> {
        let default = Hardware::default();
        let hardware = self.hardware.as_ref().unwrap_or(&default);
        aivideo::local_models(hardware)
            .iter()
            .map(|model| {
                let fit = if model.fit { "本机可跑" } else { "本机不满足" };
                slint::SharedString::from(format!(
                    "{} · {} · 最低 {} 显存 · {} · {} · 下载体积 {}",
                    model.name,
                    model.summary,
                    model.vram,
                    model.fit_note,
                    fit,
                    model.size
                ))
            })
            .collect()
    }
}
