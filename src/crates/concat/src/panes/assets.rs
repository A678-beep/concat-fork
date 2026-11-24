// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai AI and Concat contributors

//! 素材库面板：类型/行业/分类筛选、搜索、缩略图、下载缓存、插入时间轴。
//!
//! 数据全部来自平台 `/assets/*`（[`concat_host::assets`]），没有 mock：
//! - `/assets/industries`、`/assets/categories` 填左侧筛选
//! - `/assets/search` 出网格
//! - `/assets/{id}/thumb` 出缩略图（缓存在 `asset-cache/thumbs`）
//! - `/assets/{id}/download` 跟随 302 下载（缓存在 `asset-cache/files`）
//!
//! 0.2.21 的几处改动，都是老板实测「卡」「进度不动」「按钮一直下载中」的
//! 对症：
//! - 网格虚拟化（Slint 侧只实例化可视行，见 `asset-page.slint`），缩略图
//!   只为可视范围下载，最多 6 张并发，图片留在内存里复用。
//! - 下载进度在 worker 里合并：256KB 一块的进度最多 250ms 报一次，完成时
//!   另发一次 100%，界面只重画那一张卡（`host::on_ui_assets`），不再每块
//!   都全窗口重发一遍。
//! - 搜索/切分类换一个请求号，旧请求回来直接丢掉，加载中显示骨架屏。
//! - 已缓存的素材加载出来就是「插入时间轴」，不用先点一次下载；下载完/
//!   失败都复位状态，失败可重试。
//!
//! 卡片上的「可商用」来自 `license.commercial_ok`；需要署名的（CC BY 音乐等）
//! 显示「需署名」并带上 `attribution`。下载好之后按钮才是「插入时间轴」：
//! 视频/图片进主轨，音乐/音效进音频轨（`AddClipAtFirstFree` 按媒体类型选轨）。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use concat_host::assets::{self, AssetItem, Industry};
use concat_host::audio_preview::AudioPreview;
use concat_host::AppDirs;
use concat_project::Command;
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, VecModel};

use crate::i18n::{t, tf};
use crate::studio::Studio;
use crate::ui::{App, AssetCardData, Assets};

/// 一页多少条。
const PAGE_SIZE: u32 = 30;
/// 同时最多下载几张缩略图。
const THUMB_WORKERS: usize = 6;
/// 打开一页时先要几行缩略图（够铺满一屏），其余滚到再要。
const THUMB_FIRST: usize = 18;
/// 内存里最多留多少张解码后的缩略图（一张 512×288 的 RGBA 约 0.6MB）。
const THUMB_MEMORY_CAP: usize = 96;
/// 下载进度最多多久刷新一次界面。
const PROGRESS_GAP: Duration = Duration::from_millis(250);
/// 悬停预览只取开头这么多字节（视频 MP4 的 moov 在文件头，够解前几秒）。
const PREVIEW_VIDEO_BYTES: u64 = 4_000_000;
/// 音乐/音效试听取开头这么多字节，够听 5–8 秒。
const PREVIEW_AUDIO_BYTES: u64 = 1_800_000;
/// 视频预览：帧数、帧率、缩略宽度、循环时长 4 秒。
const PREVIEW_FRAMES: u32 = 40;
const PREVIEW_FPS: u32 = 10;
const PREVIEW_WIDTH: u32 = 360;
/// 音乐/音效悬停试听最多听几秒。
const PREVIEW_AUDIO_SECONDS: f64 = 8.0;

/// 五类素材。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Video,
    Image,
    Music,
    Sfx,
    Font,
}

impl Kind {
    fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Image,
            2 => Self::Music,
            3 => Self::Sfx,
            4 => Self::Font,
            _ => Self::Video,
        }
    }

    fn index(self) -> i32 {
        match self {
            Self::Video => 0,
            Self::Image => 1,
            Self::Music => 2,
            Self::Sfx => 3,
            Self::Font => 4,
        }
    }

    /// 平台接口的 `type` 参数。
    fn param(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Image => "image",
            Self::Music => "music",
            Self::Sfx => "sfx",
            Self::Font => "font",
        }
    }

    /// 卡片上标的素材类型。
    fn label(self) -> String {
        match self {
            Self::Video => t("assets.video"),
            Self::Image => t("assets.image"),
            Self::Music => t("assets.music"),
            Self::Sfx => t("assets.sfx"),
            Self::Font => t("assets.font"),
        }
    }
}

fn kind_of(item: &AssetItem) -> Kind {
    match item.kind.as_str() {
        "image" => Kind::Image,
        "music" => Kind::Music,
        "sfx" => Kind::Sfx,
        "font" => Kind::Font,
        _ => Kind::Video,
    }
}

/// 把一份字体字节注册进窗口的字体集合（Slint 的 fontique 集合），拿它
/// 自己的 family 名。字体卡示例文字用这个名字就真的用该字体渲染；同一
/// family 注册两次没有害处，预览和安装都会走这里。
fn register_family(bytes: Vec<u8>) -> Option<String> {
    let mut collection = slint::fontique_011::shared_collection();
    let blob = slint::fontique_011::fontique::Blob::new(std::sync::Arc::new(bytes));
    let registered = collection.register_fonts(blob, None);
    registered
        .iter()
        .find_map(|(family, _)| collection.family_name(*family).map(str::to_owned))
}

/// 是不是汉字（含扩展 A 与兼容区，够认平台的中文标签）。
fn is_han(ch: char) -> bool {
    ('\u{3400}'..='\u{4dbf}').contains(&ch)
        || ('\u{4e00}'..='\u{9fff}').contains(&ch)
        || ('\u{f900}'..='\u{faff}').contains(&ch)
}

fn has_han(text: &str) -> bool {
    text.chars().any(is_han)
}

/// 文本里第一段连续汉字：字体名「思源宋体 CN Bold」只留「思源宋体」。
fn han_head(text: &str) -> String {
    let mut run = String::new();
    for ch in text.chars() {
        if is_han(ch) {
            run.push(ch);
        } else if !run.is_empty() {
            break;
        }
    }
    run
}

/// 平台给的中文标签：`sub_tags` 在前（具体），`tags_zh` 在后（内容词）。
/// 只要带汉字的，去重，保持顺序。
fn chinese_tags(item: &AssetItem) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for tag in item.sub_tags.iter().chain(item.tags_zh.iter()) {
        let tag = tag.trim();
        if tag.is_empty() || !has_han(tag) {
            continue;
        }
        if tags.iter().any(|kept| kept.as_str() == tag) {
            continue;
        }
        tags.push(tag.to_owned());
    }
    tags
}

/// 来源显示名小字：`fonts/官方字体源（Google Noto / 思源）` → `官方字体源`。
fn source_label(item: &AssetItem) -> String {
    let raw = if item.source_name.trim().is_empty() {
        item.source.trim()
    } else {
        item.source_name.trim()
    };
    let raw = raw.rsplit('/').next().unwrap_or(raw).trim();
    let short = raw.split(['（', '(']).next().unwrap_or(raw).trim();
    if short.is_empty() {
        raw.to_owned()
    } else {
        short.to_owned()
    }
}

/// 时长：不到一分钟说「13s」，再长说「1:32」。
fn short_duration(seconds: f64) -> String {
    let total = seconds.round().max(1.0) as u32;
    if total >= 60 {
        format!("{}:{:02}", total / 60, total % 60)
    } else {
        format!("{total}s")
    }
}

/// 一次加载回来的一整包：行业、分类与第一页。
#[derive(Debug)]
pub struct Bundle {
    industries: Vec<Industry>,
    categories: Vec<String>,
    page: assets::AssetPage,
}

/// 一张卡片的下载状态。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Downloading,
    Cached,
    Failed,
}

impl Phase {
    fn code(self) -> i32 {
        match self {
            Self::Idle => 0,
            Self::Downloading => 1,
            Self::Cached => 2,
            Self::Failed => 3,
        }
    }
}

/// 发给素材库的一件事。
#[derive(Debug)]
pub enum AssetsMsg {
    /// 素材库页被打开或离开；第一次打开才发请求，之后保留结果。
    Shown(bool),
    KindChanged(i32),
    IndustryChanged(i32),
    CategoryChanged(i32),
    QueryEdited(String),
    Search,
    LoadMore,
    /// 卡片上的按钮：没缓存就下载，缓存了就把素材插进时间轴。
    Action(String),
    /// 网格滚到哪了：只给可视范围（外加一两行）要缩略图。
    Visible { first: i32, last: i32 },
    /// 一次搜索/筛选的结果。`generation` 是发请求时的请求号，旧的结果
    /// 回来时直接丢掉，不会盖掉新的。
    Loaded(u64, Box<Result<Bundle, String>>),
    MoreLoaded(u64, Box<Result<assets::AssetPage, String>>),
    /// 一张缩略图下好了，`result` 是它在缓存里的路径。
    Thumb {
        id: String,
        result: Result<PathBuf, String>,
    },
    Progress {
        id: String,
        fraction: f32,
    },
    Downloaded {
        id: String,
        result: Result<PathBuf, String>,
    },
    /// 卡片缩略图的悬停起停：`on` 是进去还是离开。
    Hover {
        id: String,
        on: bool,
    },
    /// 视频预览帧解好了（worker 里读好的图片）。
    PreviewVideo {
        id: String,
        generation: u64,
        /// worker 解好的预览帧文件；图片在 UI 线程读（slint::Image 不能跨线程）。
        result: Result<Vec<PathBuf>, String>,
    },
    /// 试听 PCM 解好了：48k 立体声交错 f32。
    PreviewAudio {
        id: String,
        generation: u64,
        result: Result<(Vec<f32>, u32, u16), String>,
    },
    /// 字体文件取回来了，UI 线程注册进窗口字体集合。
    PreviewFont {
        id: String,
        generation: u64,
        result: Result<Vec<u8>, String>,
    },
    /// 光标已经离开、但预览是趁离开前那一下准备出来的：露一小会儿就收。
    PreviewExpire {
        id: String,
        generation: u64,
    },
}

/// 一张卡的悬停预览。
#[derive(Clone, Default)]
struct CardPreview {
    /// 0 无 / 1 准备中 / 2 视频帧 / 3 试听 / 4 字体 / 5 不可用
    state: i32,
    frames: Vec<Image>,
    /// 字体预览用的 family 名（注册进窗口字体集合后的名字）。
    family: String,
}

/// 素材库面板的状态。
pub struct AssetsPane {
    dirs: Option<AppDirs>,
    kind: Kind,
    /// 0 = 全部行业，n = `industries[n-1]`。
    industry: usize,
    /// 0 = 全部分类，n = `categories[n-1]`。
    category: usize,
    query: String,
    industries: Vec<Industry>,
    categories: Vec<String>,
    items: Vec<AssetItem>,
    total: i64,
    page: u32,
    /// 每次搜索/翻页 +1；回来的结果对不上就是旧请求。
    request: u64,
    loading: bool,
    loaded_once: bool,
    message: String,
    phase: HashMap<String, Phase>,
    progress: HashMap<String, f32>,
    thumbs: HashMap<String, Image>,
    /// 下载失败的缩略图，不再为它重试。
    thumbs_failed: RefCell<HashSet<String>>,
    /// 还在队列里等缩略图的 id（不重复排队）。
    thumb_waiting: RefCell<HashSet<String>>,
    thumb_queue: RefCell<VecDeque<String>>,
    /// 正在下载的缩略图数，和 [`THUMB_WORKERS`] 比。
    thumb_running: Cell<usize>,
    /// 网格最近一次报上来的可视范围。
    visible: Cell<(usize, usize)>,
    /// 卡片模型；只有整页/整行换掉时才重建，进度和缩略图只改一行。
    cards: RefCell<Option<Rc<VecModel<AssetCardData>>>>,
    index_of: RefCell<HashMap<String, usize>>,
    /// 整块重发（列表、筛选、状态行变了）。
    full: Cell<bool>,
    /// 只需要改一行的卡 id。
    rows: RefCell<HashSet<String>>,
    /// 悬停预览的请求号：移开/换卡后旧 worker 的结果直接丢。
    preview_generation: Cell<u64>,
    /// 正在准备预览的卡；空是没在准备。
    preview_id: RefCell<String>,
    /// 只允许一个预览 worker 在跑（任务卡要求限并发）。
    preview_busy: Cell<bool>,
    /// 每张卡的预览内容（同一时刻最多一张有）。
    previews: RefCell<HashMap<String, CardPreview>>,
    /// 悬停试听的小播放器，第一次需要时才开输出流。
    audio_preview: RefCell<Option<AudioPreview>>,
    /// 已注册进窗口字体集合的素材字体：id -> family。
    font_families: RefCell<HashMap<String, String>>,
    /// 已「安装」进本应用的字体素材：字体文件复制进应用字体目录、注册到
    /// 工程，文字面板的字体列表里能选到（0.2.27 的「一键安装」）。
    font_installed: RefCell<HashSet<String>>,
    /// 光标在预览准备途中离开了：让这次预览做完、露一小会儿再收，而不是
    /// 把刚花 1–2 秒取回来的片段扔掉（指针从卡片边缘滑过时不该闪一下）。
    hover_ended: Cell<bool>,
}

impl Default for AssetsPane {
    fn default() -> Self {
        Self {
            dirs: None,
            kind: Kind::Video,
            industry: 0,
            category: 0,
            query: String::new(),
            industries: Vec::new(),
            categories: Vec::new(),
            items: Vec::new(),
            total: 0,
            page: 1,
            request: 0,
            loading: false,
            loaded_once: false,
            message: String::new(),
            phase: HashMap::new(),
            progress: HashMap::new(),
            thumbs: HashMap::new(),
            thumbs_failed: RefCell::new(HashSet::new()),
            thumb_waiting: RefCell::new(HashSet::new()),
            thumb_queue: RefCell::new(VecDeque::new()),
            thumb_running: Cell::new(0),
            visible: Cell::new((0, THUMB_FIRST - 1)),
            cards: RefCell::new(None),
            index_of: RefCell::new(HashMap::new()),
            full: Cell::new(true),
            rows: RefCell::new(HashSet::new()),
            preview_generation: Cell::new(0),
            preview_id: RefCell::new(String::new()),
            preview_busy: Cell::new(false),
            previews: RefCell::new(HashMap::new()),
            audio_preview: RefCell::new(None),
            font_families: RefCell::new(HashMap::new()),
            font_installed: RefCell::new(HashSet::new()),
            hover_ended: Cell::new(false),
        }
    }
}

impl AssetsPane {
    /// 打开面板，带上素材缓存要用的目录。
    pub fn open(dirs: &AppDirs) -> Self {
        let mut pane = Self::default();
        pane.dirs = Some(dirs.clone());
        pane
    }

    fn dirs(&self) -> Option<AppDirs> {
        self.dirs.clone()
    }

    /// 整块重发一次：列表、计数、按钮状态。下一次 publish 会重建卡片模型。
    fn bump_full(&self) {
        self.full.set(true);
    }

    /// 只改一张卡。
    fn bump_row(&self, id: &str) {
        self.rows.borrow_mut().insert(id.to_owned());
    }

    /// 类型 tab 上的五个词。
    fn kinds(&self) -> Vec<SharedString> {
        [
            "assets.video",
            "assets.image",
            "assets.music",
            "assets.sfx",
            "assets.font",
        ]
        .iter()
        .map(|key| SharedString::from(t(key)))
        .collect()
    }

    /// 左侧「行业」一列：平台返回的行业（带数量）。
    ///
    /// 不含「全部」：media-pane.slint 自己画第一行「全部行业」，这里再放一个
    /// 会让所有行整体错位——0.2.23 里点了「餐饮」实际查的是下一行的行业。
    fn industries(&self) -> Vec<SharedString> {
        self.industries
            .iter()
            .map(|industry| {
                let label = if industry.count > 0 {
                    format!("{} ({})", industry.name, industry.count)
                } else {
                    industry.name.clone()
                };
                SharedString::from(label)
            })
            .collect()
    }

    /// 分类下拉：全部 + 平台返回的分类。
    fn categories(&self) -> Vec<SharedString> {
        let mut rows = vec![SharedString::from(t("assets.allCategories"))];
        for category in &self.categories {
            rows.push(SharedString::from(category.clone()));
        }
        rows
    }

    /// 选中行业的显示名（也是返回结果防御性过滤用的值）。
    fn industry_name(&self) -> String {
        if self.industry == 0 {
            String::new()
        } else {
            self.industries
                .get(self.industry - 1)
                .map(|industry| industry.name.clone())
                .unwrap_or_default()
        }
    }

    /// 请求里带的行业参数：优先后端给的 code（`catering`），没有代码就用
    /// 中文名（平台两个都认）。
    fn industry_param(&self) -> String {
        if self.industry == 0 {
            String::new()
        } else {
            self.industries
                .get(self.industry - 1)
                .map(|industry| {
                    if industry.code.trim().is_empty() {
                        industry.name.clone()
                    } else {
                        industry.code.clone()
                    }
                })
                .unwrap_or_default()
        }
    }

    fn category_name(&self) -> String {
        if self.category == 0 {
            String::new()
        } else {
            self.categories.get(self.category - 1).cloned().unwrap_or_default()
        }
    }

    /// 第一次打开时把行业、分类和第一页一起取回来。
    fn load(&mut self) {
        self.request = self.request.wrapping_add(1);
        let generation = self.request;
        self.loading = true;
        self.message.clear();
        // 旧列表先清掉：换类型/换行业/重新搜时显示骨架屏，而不是留着
        // 上一个关键词的结果假装还在加载。
        self.items.clear();
        self.phase.clear();
        self.progress.clear();
        self.thumbs.clear();
        self.thumbs_failed.borrow_mut().clear();
        self.clear_previews();
        self.bump_full();
        let query = self.query.clone();
        let kind = self.kind.param().to_owned();
        let industry = self.industry_param();
        let category = self.category_name();
        crate::host::spawn(
            move || -> Result<Bundle, String> {
                let industries = assets::industries().unwrap_or_default();
                let categories = assets::categories().unwrap_or_default();
                let page = assets::search(&query, &kind, &industry, &category, "", 1, PAGE_SIZE)?;
                Ok(Bundle {
                    industries,
                    categories,
                    page,
                })
            },
            move |studio, _, _, result| {
                studio.handle(crate::panes::Msg::Assets(AssetsMsg::Loaded(
                    generation,
                    Box::new(result),
                )));
            },
        );
    }

    /// 下一页，追加到现有列表后面。
    fn load_more(&mut self) {
        if self.loading || self.items.is_empty() {
            return;
        }
        self.loading = true;
        self.request = self.request.wrapping_add(1);
        let generation = self.request;
        self.bump_full();
        let query = self.query.clone();
        let kind = self.kind.param().to_owned();
        let industry = self.industry_param();
        let category = self.category_name();
        let page = self.page + 1;
        crate::host::spawn(
            move || assets::search(&query, &kind, &industry, &category, "", page, PAGE_SIZE),
            move |studio, _, _, result| {
                studio.handle(crate::panes::Msg::Assets(AssetsMsg::MoreLoaded(
                    generation,
                    Box::new(result),
                )));
            },
        );
    }

    /// 防御性过滤：返回结果里行业和当前选中行业不一致的不渲染。
    ///
    /// 正常时一条都不会掉；平台给某条素材打了别家行业的标签时就靠这里兜底，
    /// 日志记下丢了多少条，方便回头修平台的标签。
    fn drop_foreign_items(&mut self) {
        let wanted = self.industry_name();
        if wanted.is_empty() {
            return;
        }
        let before = self.items.len();
        self.items
            .retain(|item| item.industry.is_empty() || item.industry == wanted);
        let dropped = before - self.items.len();
        if dropped > 0 {
            log::warn!(
                "assets: dropped {dropped} row(s) whose industry is not {wanted:?} (first page had {before})"
            );
        }
    }

    /// 一页回来之后：卡片状态按本机缓存先定好，缩略图按可视范围排队。
    fn settle_page(&mut self) {
        self.mark_cached();
        self.thumbs_failed.borrow_mut().clear();
        // 先铺满第一屏；网格把真实可视范围报上来之后，再补后面几行。
        self.request_thumbs(0, THUMB_FIRST.saturating_sub(1));
        let (first, last) = self.visible.get();
        self.request_thumbs(first, last);
    }

    /// 已下载的素材直接显示「插入时间轴」，不用先点一次下载。
    fn mark_cached(&mut self) {
        let Some(dirs) = self.dirs() else {
            return;
        };
        let cached = assets::cached_ids(&dirs);
        for item in &self.items {
            if cached.contains(&assets::cache_stem(&item.id)) {
                self.phase.insert(item.id.clone(), Phase::Cached);
            }
        }
    }

    /// 给可视范围（外加上下一行）要缩略图；已在内存、在排队或失败过的不再要。
    fn request_thumbs(&self, first: usize, last: usize) {
        if self.items.is_empty() || self.dirs.is_none() {
            return;
        }
        let last = last.min(self.items.len() - 1);
        if first > last {
            return;
        }
        let failed = self.thumbs_failed.borrow();
        let mut waiting = self.thumb_waiting.borrow_mut();
        let mut queue = self.thumb_queue.borrow_mut();
        for index in first..=last {
            let item = &self.items[index];
            if self.thumbs.contains_key(&item.id)
                || failed.contains(&item.id)
                || waiting.contains(&item.id)
            {
                continue;
            }
            waiting.insert(item.id.clone());
            queue.push_back(item.id.clone());
        }
        drop(failed);
        drop(waiting);
        drop(queue);
        self.pump_thumbs();
    }

    /// 起缩略图下载线程，最多 [`THUMB_WORKERS`] 个同时跑。
    fn pump_thumbs(&self) {
        while self.thumb_running.get() < THUMB_WORKERS {
            let Some(id) = self.thumb_queue.borrow_mut().pop_front() else {
                break;
            };
            let Some(item) = self.items.iter().find(|item| item.id == id).cloned() else {
                self.thumb_waiting.borrow_mut().remove(&id);
                continue;
            };
            let Some(dirs) = self.dirs() else {
                return;
            };
            self.thumb_running.set(self.thumb_running.get() + 1);
            let thread_id = id.clone();
            let spawned = std::thread::Builder::new()
                .name("concat-asset-thumb".into())
                .spawn(move || {
                    let result = assets::thumb(&dirs, &item);
                    crate::host::on_ui_assets(move |studio, _| {
                        studio.handle(crate::panes::Msg::Assets(AssetsMsg::Thumb {
                            id: thread_id,
                            result,
                        }));
                    });
                });
            if spawned.is_err() {
                log::warn!("assets: 起不了缩略图线程，{id} 先留占位图");
                self.thumb_running.set(self.thumb_running.get().saturating_sub(1));
                self.thumb_waiting.borrow_mut().remove(&id);
            }
        }
    }

    /// 下载一个素材；缓存里已有就只把状态写对。
    fn download(&mut self, id: &str) {
        let Some(item) = self.items.iter().find(|item| item.id == id).cloned() else {
            return;
        };
        let Some(dirs) = self.dirs() else {
            return;
        };
        if let Some(path) = assets::cached(&dirs, &item.id) {
            self.note_downloaded(&item.id, Ok(path));
            return;
        }
        self.phase.insert(item.id.clone(), Phase::Downloading);
        self.progress.insert(item.id.clone(), 0.0);
        self.bump_row(&item.id);
        let key = item.id.clone();
        let id2 = key.clone();
        crate::host::spawn(
            move || {
                let cancel = std::sync::atomic::AtomicBool::new(false);
                // 合并进度：一个 256KB 块报一条，界面会被重绘淹没；
                // 250ms 一条足够看出在动，完成时另有一次强制刷新。
                let mut last: Option<Instant> = None;
                let report = key;
                assets::download(
                    &dirs,
                    &item,
                    &mut |fraction, _| {
                        let now = Instant::now();
                        if last.is_some_and(|at: Instant| now.duration_since(at) < PROGRESS_GAP) {
                            return;
                        }
                        last = Some(now);
                        let id = report.clone();
                        crate::host::on_ui_assets(move |studio, _| {
                            studio.handle(crate::panes::Msg::Assets(AssetsMsg::Progress {
                                id: id.clone(),
                                fraction,
                            }));
                        });
                    },
                    &cancel,
                )
            },
            move |studio, _, _, result: Result<PathBuf, String>| {
                studio.handle(crate::panes::Msg::Assets(AssetsMsg::Downloaded {
                    id: id2,
                    result,
                }));
            },
        );
    }

    fn note_downloaded(&mut self, id: &str, result: Result<PathBuf, String>) {
        match result {
            Ok(path) => {
                self.phase.insert(id.to_owned(), Phase::Cached);
                self.progress.insert(id.to_owned(), 1.0);
                // 字体下载完就先注册进窗口字体集合：卡的示例文字用的就是
                // 该字体，不用等悬停；「安装」按钮再把它装进文字面板。
                if self
                    .items
                    .iter()
                    .find(|item| item.id == id)
                    .is_some_and(|item| kind_of(item) == Kind::Font)
                    && let Ok(bytes) = std::fs::read(&path)
                    && let Some(family) = register_family(bytes)
                {
                    log::info!("assets: font {id} downloaded, registered as {family}");
                    self.font_families.borrow_mut().insert(id.to_owned(), family);
                }
            }
            Err(error) => {
                self.phase.insert(id.to_owned(), Phase::Failed);
                self.progress.remove(id);
                self.message = error;
            }
        }
        self.bump_full();
    }

    fn action(&mut self, id: &str, studio: &mut Studio) {
        let font = self
            .items
            .iter()
            .find(|item| item.id == id)
            .is_some_and(|item| kind_of(item) == Kind::Font);
        match self.phase.get(id).copied().unwrap_or(Phase::Idle) {
            Phase::Downloading => {}
            Phase::Cached => {
                let Some(dirs) = self.dirs() else {
                    return;
                };
                if let Some(path) = assets::cached(&dirs, id) {
                    if font {
                        self.install_font(id, &path, studio);
                    } else {
                        self.insert(id, &path, studio);
                    }
                } else {
                    // 缓存文件被外面删了：重新下载，按钮回到下载中。
                    self.phase.remove(id);
                    self.download(id);
                }
            }
            Phase::Idle | Phase::Failed => {
                if let Some(dirs) = self.dirs()
                    && let Some(path) = assets::cached(&dirs, id)
                {
                    // 打开面板时它还没缓存，现在有了：点一下直接插，不再下载。
                    self.phase.insert(id.to_owned(), Phase::Cached);
                    if font {
                        self.install_font(id, &path, studio);
                    } else {
                        self.insert(id, &path, studio);
                    }
                    return;
                }
                self.download(id);
            }
        }
    }

    /// 字体素材装进本应用（0.2.27）：字体文件读回来注册进窗口字体集合，
    /// 卡的示例文字从此用该字体；再交给 `Studio::import_fonts` 复制到
    /// 应用字体目录并注册到工程，文字面板的字体列表里就能选到。和文字
    /// 面板自己的「导入字体」走同一条路，不是第二套。
    fn install_font(&mut self, id: &str, path: &PathBuf, studio: &mut Studio) {
        let Some(item) = self.items.iter().find(|item| item.id == id).cloned() else {
            return;
        };
        let display = self.title_of(&item, Kind::Font);
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::warn!("assets: 字体 {id} 读不回缓存文件：{error}");
                self.message = tf("assets.fontInstallFailed", &[&display]);
                self.bump_full();
                return;
            }
        };
        let Some(family) = register_family(bytes) else {
            log::warn!("assets: 字体 {id} 注册不进窗口字体集合");
            self.message = tf("assets.fontInstallFailed", &[&display]);
            self.bump_full();
            return;
        };
        log::info!("assets: 字体 {id} 安装为 {family}");
        self.font_families
            .borrow_mut()
            .insert(id.to_owned(), family);
        // 复制进 app 的字体目录 + 注册到当前工程，文字面板即可选用；
        // import_fonts 自己会弹一条「已导入字体」的提示。
        studio.import_fonts(vec![path.clone()]);
        self.font_installed.borrow_mut().insert(id.to_owned());
        self.message = tf("assets.fontInstalled", &[&display]);
        self.bump_full();
    }

    /// 缓存好的文件进工程：探一次时长/类型，再进媒体库和第一条空轨。
    fn insert(&mut self, id: &str, path: &PathBuf, studio: &mut Studio) {
        let Some(item) = self.items.iter().find(|item| item.id == id).cloned() else {
            return;
        };
        // toast、媒体库、时间线都用卡片上的中文名，英文标题不上界面。
        let kind = kind_of(&item);
        let display = self.title_of(&item, kind);
        if kind == Kind::Font {
            self.message = tf("assets.fontCached", &[&display]);
            self.bump_full();
            return;
        }
        let summary = match concat_host::media::probe(path.to_string_lossy().as_ref()) {
            Ok(summary) => summary,
            Err(error) => {
                self.message = error;
                self.bump_full();
                return;
            }
        };
        let mut media = summary.to_new_media();
        media.name = display.clone();
        let created = studio.apply(Command::AddMedia { item: media });
        let media_id = created.or_else(|| {
            studio
                .project()
                .media
                .iter()
                .find(|media| media.path == summary.path)
                .map(|media| media.id.clone())
        });
        if let Some(media_id) = media_id {
            let start = f64::from(studio.playhead);
            studio.apply(Command::AddClipAtFirstFree { media_id, start });
            let toast = tf("assets.insertedToast", &[&display]);
            studio.notify(&toast, false);
            self.message = tf("assets.inserted", &[&display]);
        } else {
            self.message = t("assets.insertFailed");
        }
        self.bump_full();
    }

    /// 卡片模型里的一张卡：状态、进度、缩略图都从当前状态现取。
    fn card_data(&self, item: &AssetItem) -> AssetCardData {
        let phase = self.phase.get(&item.id).copied().unwrap_or(Phase::Idle);
        let progress = self.progress.get(&item.id).copied().unwrap_or(0.0);
        let kind = kind_of(item);
        let preview = self.previews.borrow().get(&item.id).cloned().unwrap_or_default();
        // 悬停预览注册过的 family，或下载/安装时注册过的（后者在移开鼠标
        // 之后还留着，字体卡的示例文字因此一直是该字体的样子）。
        let family = if preview.family.is_empty() {
            self.font_families
                .borrow()
                .get(&item.id)
                .cloned()
                .unwrap_or_default()
        } else {
            preview.family.clone()
        };
        let duration = if matches!(kind, Kind::Music | Kind::Sfx) && item.duration > 0.5 {
            short_duration(item.duration)
        } else {
            String::new()
        };
        AssetCardData {
            id: SharedString::from(item.id.clone()),
            title: SharedString::from(self.title_of(item, kind)),
            detail: SharedString::from(self.detail_of(item, kind)),
            source: SharedString::from(source_label(item)),
            tags: SharedString::from(self.tags_line(item)),
            license: SharedString::from(self.license_line(item)),
            kind: kind.index(),
            kind_label: SharedString::from(kind.label()),
            thumb: self.thumbs.get(&item.id).cloned().unwrap_or_default(),
            has_thumb: self.thumbs.contains_key(&item.id),
            commercial: item.license.commercial_ok,
            needs_attribution: item.license.attribution_required,
            state: phase.code(),
            progress,
            action: SharedString::from(self.action_label(item, phase, progress)),
            preview_state: preview.state,
            preview_frames: ModelRc::new(VecModel::from(preview.frames.clone())),
            font_family: SharedString::from(family),
            duration: SharedString::from(duration),
            font_installed: self.font_installed.borrow().contains(&item.id),
        }
    }

    /// 状态变了才发布：整页、整行分开走，进度和缩略图只重画那一张卡。
    pub fn publish(&self, app: &App) {
        let global = app.global::<Assets>();
        if global.get_kind() == self.kind.index() && !self.full.get() {
            let rows: Vec<String> = self.rows.borrow_mut().drain().collect();
            if rows.is_empty() {
                return;
            }
            let model = self.cards.borrow().clone();
            let Some(model) = model else {
                return;
            };
            let index_of = self.index_of.borrow();
            for id in rows {
                let Some(&index) = index_of.get(&id) else {
                    continue;
                };
                let Some(item) = self.items.get(index) else {
                    continue;
                };
                if index < model.row_count() {
                    model.set_row_data(index, self.card_data(item));
                }
            }
            return;
        }
        self.rows.borrow_mut().clear();
        global.set_kind(self.kind.index());
        global.set_kinds(ModelRc::new(VecModel::from(self.kinds())));
        global.set_industry(self.industry as i32);
        global.set_industries(ModelRc::new(VecModel::from(self.industries())));
        global.set_category(self.category as i32);
        global.set_categories(ModelRc::new(VecModel::from(self.categories())));
        global.set_query(SharedString::from(self.query.clone()));
        global.set_loading(self.loading);
        global.set_message(SharedString::from(self.message.clone()));
        let stats = if self.total > 0 {
            if self.industry > 0 {
                tf(
                    "assets.statsIndustry",
                    &[&self.total.to_string(), &self.items.len().to_string()],
                )
            } else {
                tf(
                    "assets.stats",
                    &[&self.items.len().to_string(), &self.total.to_string()],
                )
            }
        } else {
            String::new()
        };
        global.set_stats(SharedString::from(stats));
        let cards: Vec<AssetCardData> = self.items.iter().map(|item| self.card_data(item)).collect();
        let model = Rc::new(VecModel::from(cards));
        global.set_items(ModelRc::from(model.clone()));
        *self.cards.borrow_mut() = Some(model);
        *self.index_of.borrow_mut() = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id.clone(), index))
            .collect();
        self.full.set(false);
    }

    /// 卡片主标题：只在有中文信息时才出字，绝不把平台英文标题搬上来。
    ///
    /// 优先顺序：`title_zh` → 行业 + 子类标签（`sub_tags`，最多两个）→
    /// `category_zh` → 标题里的汉字（字体名常是「思源宋体 CN Bold」这种）
    /// → 「{类型}素材 · {来源}」兜底。搜索命中时把命中的中文标签放到最前，
    /// 让人一眼看出为什么这张卡在结果里。
    fn title_of(&self, item: &AssetItem, kind: Kind) -> String {
        let title_zh = item.title_zh.trim();
        if has_han(title_zh) {
            return title_zh.to_owned();
        }
        let tags = chinese_tags(item);
        let industry = item.industry.trim();
        let category = item.category_zh.trim();
        let query = self.query.trim();
        // 命中的中文标签比宽分类更能说明搜索意图。
        let hit = if query.is_empty() {
            None
        } else {
            tags.iter()
                .find(|tag| tag.contains(query))
                .cloned()
                .or_else(|| if has_han(query) { Some(query.to_owned()) } else { None })
        };
        let mut parts: Vec<String> = Vec::new();
        let push = |parts: &mut Vec<String>, part: &str| {
            let part = part.trim();
            if !part.is_empty() && !parts.iter().any(|kept| kept.as_str() == part) {
                parts.push(part.to_owned());
            }
        };
        match kind {
            Kind::Video | Kind::Image => {
                if !industry.is_empty() && industry != "通用" {
                    push(&mut parts, industry);
                    if let Some(hit) = hit.as_deref() {
                        push(&mut parts, hit);
                    } else if !tags.is_empty() {
                        // 子类标签（sub_tags 排在前）取前两个，比宽分类具体。
                        push(&mut parts, &tags[0]);
                        if let Some(second) = tags.get(1) {
                            push(&mut parts, second);
                        }
                    } else {
                        push(&mut parts, category);
                    }
                } else {
                    // 行业是「通用」：用分类打头，再补具体子标签。
                    push(&mut parts, category);
                    if let Some(hit) = hit.as_deref() {
                        push(&mut parts, hit);
                    } else if let Some(first) = tags.first() {
                        push(&mut parts, first);
                        if let Some(second) = tags.get(1) {
                            push(&mut parts, second);
                        }
                    }
                }
            }
            Kind::Music | Kind::Sfx | Kind::Font => {
                // 字体有中文名就只显示中文名（「思源宋体」），没有才用分类。
                let han_title = han_head(&item.title);
                if kind == Kind::Font && !han_title.is_empty() {
                    push(&mut parts, &han_title);
                } else {
                    push(&mut parts, &kind.label());
                    let descriptor = hit
                        .as_deref()
                        .filter(|hit| *hit != category)
                        .or(if !category.is_empty() { Some(category) } else { None })
                        .or_else(|| tags.first().map(String::as_str))
                        .or(if !han_title.is_empty() { Some(han_title.as_str()) } else { None });
                    if let Some(descriptor) = descriptor {
                        push(&mut parts, descriptor);
                    }
                }
            }
        }
        if parts.is_empty() {
            // 平台没有给任何中文线索：按任务卡兜底「{类型}素材 · {来源}」。
            parts.push(format!("{}素材", kind.label()));
            let source = source_label(item);
            if !source.is_empty() {
                parts.push(source);
            }
        }
        parts.join(" · ")
    }

    /// 副标题：类型 · 分辨率/时长（来源名在卡片上另起小字）。
    fn detail_of(&self, item: &AssetItem, kind: Kind) -> String {
        let mut parts = vec![kind.label()];
        if item.width > 0 && item.height > 0 {
            parts.push(format!("{}×{}", item.width, item.height));
        }
        if item.duration > 0.5 {
            parts.push(short_duration(item.duration));
        }
        if parts.len() == 1 && !item.resolution.trim().is_empty() {
            parts.push(item.resolution.trim().to_owned());
        }
        parts.join(" · ")
    }

    /// 悬停时露出的中文标签行：前几个具体标签；没有标签就用行业/分类。
    fn tags_line(&self, item: &AssetItem) -> String {
        let tags = chinese_tags(item);
        if !tags.is_empty() {
            return tags.into_iter().take(4).collect::<Vec<_>>().join(" · ");
        }
        if !item.category_zh.trim().is_empty() {
            return item.category_zh.trim().to_owned();
        }
        item.industry.trim().to_owned()
    }

    /// 悬停时露出的来源 + 许可全称（Pixabay License 这类不放在主视觉）。
    fn license_line(&self, item: &AssetItem) -> String {
        let mut parts = Vec::new();
        let source = source_label(item);
        if !source.is_empty() {
            parts.push(source);
        }
        let license = if item.license.name.trim().is_empty() {
            item.license.code.trim()
        } else {
            item.license.name.trim()
        };
        if !license.is_empty() {
            parts.push(license.to_owned());
        }
        if item.license.attribution_required && !item.attribution.trim().is_empty() {
            parts.push(format!("作者 {}", item.attribution.trim()));
        }
        parts.join(" · ")
    }

    fn action_label(&self, item: &AssetItem, phase: Phase, progress: f32) -> String {
        let font = kind_of(item) == Kind::Font;
        match phase {
            Phase::Idle => t("assets.download"),
            Phase::Downloading => {
                tf("assets.downloading", &[&format!("{:.0}%", progress * 100.0)])
            }
            Phase::Cached => {
                if font {
                    if self.font_installed.borrow().contains(&item.id) {
                        t("assets.fontInstalledShort")
                    } else {
                        t("assets.installFont")
                    }
                } else {
                    t("assets.insert")
                }
            }
            Phase::Failed => t("assets.retry"),
        }
    }

    /// 缩略图回来了：进内存、改这一行。
    fn thumb_arrived(&mut self, id: String, result: Result<PathBuf, String>) {
        self.thumb_running.set(self.thumb_running.get().saturating_sub(1));
        self.thumb_waiting.borrow_mut().remove(&id);
        // 换页之后才回来的旧图：不认识的 id 直接丢。
        let known = self.items.iter().any(|item| item.id == id);
        match result {
            Ok(path) if known => {
                if let Ok(image) = Image::load_from_path(&path) {
                    if self.thumbs.len() >= THUMB_MEMORY_CAP {
                        // 内存里图太多：清空后把可视范围重新要一遍（磁盘里有
                        // 缓存，不会再走网络）。
                        self.thumbs.clear();
                        *self.thumbs_failed.borrow_mut() = HashSet::new();
                        self.bump_full();
                        let (first, last) = self.visible.get();
                        self.request_thumbs(first, last);
                    }
                    self.thumbs.insert(id.clone(), image);
                    self.bump_row(&id);
                } else {
                    self.thumbs_failed.borrow_mut().insert(id.clone());
                }
            }
            Ok(_) => {}
            Err(error) => {
                log::debug!("assets: 缩略图 {id} 下载失败了：{error}");
                if known {
                    self.thumbs_failed.borrow_mut().insert(id);
                }
            }
        }
        self.pump_thumbs();
    }


    // ── 悬停预览（0.2.23）──────────────────────────────────────────

    /// 换页/换分类/关面板：停试听、清掉所有预览，旧 worker 的结果作废。
    fn clear_previews(&self) {
        self.hover_ended.set(false);
        self.preview_generation
            .set(self.preview_generation.get().wrapping_add(1));
        self.preview_id.borrow_mut().clear();
        if let Some(player) = self.audio_preview.borrow().as_ref() {
            player.stop();
        }
        self.previews.borrow_mut().clear();
    }

    /// 鼠标停在卡片缩略图上（`on = true`）或移开。停留 400ms 由 Slint 侧
    /// 计时，这里只负责取预览：网络 Range 片段 + 本地解码都在 worker 里，
    /// 同一时刻最多一个 worker、最多一个声音（任务卡要求限并发）。
    fn hover(&mut self, id: String, on: bool) {
        if !on {
            // 预览还在准备时离开：别把已经花时间取回来的片段扔掉，让结果
            // 出来露一小会儿（见 PreviewExpire 与 hover_ended 的注释）。
            if self.preview_busy.get() && self.preview_id.borrow().as_str() == id {
                self.hover_ended.set(true);
                if let Some(player) = self.audio_preview.borrow().as_ref() {
                    player.stop();
                }
                return;
            }
            self.hover_ended.set(false);
            self.preview_generation
                .set(self.preview_generation.get().wrapping_add(1));
            if self.preview_id.borrow().as_str() == id {
                self.preview_id.borrow_mut().clear();
            }
            if let Some(player) = self.audio_preview.borrow().as_ref() {
                player.stop();
            }
            // 只留这一张卡的帧缓存（再悬停时不用重新解），别的清掉。
            let mut had = false;
            self.previews.borrow_mut().retain(|key, preview| {
                if key == &id {
                    preview.state = 0;
                    true
                } else {
                    had = true;
                    false
                }
            });
            self.bump_row(&id);
            if had {
                self.bump_full();
            }
            return;
        }
        self.hover_ended.set(false);
        // 同一张卡还留着上次的解码结果：直接再显示/再试听，不走网络。
        let cached = {
            let previews = self.previews.borrow();
            previews
                .get(&id)
                .map(|preview| {
                    if !preview.frames.is_empty() {
                        2
                    } else if !preview.family.is_empty() {
                        4
                    } else {
                        0
                    }
                })
                .unwrap_or(0)
        };
        if cached != 0 {
            if let Some(preview) = self.previews.borrow_mut().get_mut(&id) {
                preview.state = cached;
            }
            self.bump_row(&id);
            return;
        }
        let Some(item) = self.items.iter().find(|item| item.id == id).cloned() else {
            return;
        };
        // 图片的缩略图就是它本身：不需要悬停预览，硬要一次只会让卡片挂
        // 上「预览不了」的提示，反而像出了错。
        if kind_of(&item) == Kind::Image {
            self.hover_ended.set(false);
            return;
        }
        let Some(dirs) = self.dirs() else {
            return;
        };
        if self.preview_busy.get() {
            return;
        }
        let media_kind = kind_of(&item);
        let generation = self.preview_generation.get();
        *self.preview_id.borrow_mut() = id.clone();
        self.previews.borrow_mut().insert(
            id.clone(),
            CardPreview {
                state: 1,
                ..Default::default()
            },
        );
        self.bump_row(&id);
        self.preview_busy.set(true);
        let report = id.clone();
        let spawned = std::thread::Builder::new()
            .name("concat-asset-preview".into())
            .spawn(move || match media_kind {
                Kind::Video => {
                    let result = (|| -> Result<Vec<PathBuf>, String> {
                        let clip =
                            assets::preview_clip(&dirs, &item, PREVIEW_VIDEO_BYTES, "mp4")?;
                        let dir = assets::preview_frames_dir(&dirs, &item);
                        let paths = concat_host::video_preview::video_frames(
                            &clip,
                            &dir,
                            PREVIEW_FRAMES,
                            PREVIEW_FPS,
                            PREVIEW_WIDTH,
                        )?;
                        Ok(paths.into_iter().take(PREVIEW_FRAMES as usize).collect())
                    })();
                    crate::host::on_ui_assets(move |studio, _| {
                        studio.handle(crate::panes::Msg::Assets(AssetsMsg::PreviewVideo {
                            id: report,
                            generation,
                            result,
                        }));
                    });
                }
                Kind::Music | Kind::Sfx => {
                    let result = (|| -> Result<(Vec<f32>, u32, u16), String> {
                        let clip =
                            assets::preview_clip(&dirs, &item, PREVIEW_AUDIO_BYTES, "mp3")?;
                        let work = assets::preview_dir(&dirs);
                        concat_host::video_preview::audio_pcm(&clip, PREVIEW_AUDIO_SECONDS, &work)
                    })();
                    crate::host::on_ui_assets(move |studio, _| {
                        studio.handle(crate::panes::Msg::Assets(AssetsMsg::PreviewAudio {
                            id: report,
                            generation,
                            result,
                        }));
                    });
                }
                Kind::Font => {
                    let result = (|| -> Result<Vec<u8>, String> {
                        let path = assets::preview_font(&dirs, &item)?;
                        std::fs::read(&path).map_err(|error| format!("读不回字体：{error}"))
                    })();
                    crate::host::on_ui_assets(move |studio, _| {
                        studio.handle(crate::panes::Msg::Assets(AssetsMsg::PreviewFont {
                            id: report,
                            generation,
                            result,
                        }));
                    });
                }
                Kind::Image => {
                    crate::host::on_ui_assets(move |studio, _| {
                        studio.handle(crate::panes::Msg::Assets(AssetsMsg::PreviewVideo {
                            id: report,
                            generation,
                            result: Err("图片不需要悬停预览".to_owned()),
                        }));
                    });
                }
            });
        if spawned.is_err() {
            log::warn!("assets: 起不了预览线程");
            self.preview_busy.set(false);
            self.fail_preview(&id);
        }
    }

    /// 预览结果已经露出来了，但光标早走了（`hover_ended`）：1.8 秒后收掉。
    fn expire_later(&self, id: &str, generation: u64) {
        if !self.hover_ended.get() {
            return;
        }
        let id = id.to_owned();
        std::thread::Builder::new()
            .name("concat-asset-expire".into())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1800));
                crate::host::on_ui_assets(move |studio, _| {
                    studio.handle(crate::panes::Msg::Assets(AssetsMsg::PreviewExpire {
                        id,
                        generation,
                    }));
                });
            })
            .ok();
    }

    fn preview_expire(&mut self, id: String, generation: u64) {
        if generation != self.preview_generation.get() || !self.hover_ended.get() {
            return;
        }
        self.hover_ended.set(false);
        if let Some(player) = self.audio_preview.borrow().as_ref() {
            player.stop();
        }
        self.previews.borrow_mut().remove(&id);
        if self.preview_id.borrow().as_str() == id {
            self.preview_id.borrow_mut().clear();
        }
        self.bump_row(&id);
    }

    /// 预览结果是不是还给这张卡（移开/换卡/换页后回来的一律丢掉）。
    fn preview_current(&self, id: &str, generation: u64) -> bool {
        generation == self.preview_generation.get() && self.preview_id.borrow().as_str() == id
    }

    fn fail_preview(&mut self, id: &str) {
        self.previews.borrow_mut().insert(
            id.to_owned(),
            CardPreview {
                state: 5,
                ..Default::default()
            },
        );
        self.bump_row(id);
    }

    /// 试听准备不了：这台机器没有可用的音频输出设备（见任务卡「试听」，
    /// 卡片上如实说明，而不是假装在放）。6 是卡片上的「没有音频输出设备」。
    fn fail_preview_no_audio(&mut self, id: &str) {
        self.previews.borrow_mut().insert(
            id.to_owned(),
            CardPreview {
                state: 6,
                ..Default::default()
            },
        );
        self.bump_row(id);
    }

    fn video_arrived(
        &mut self,
        id: String,
        generation: u64,
        result: Result<Vec<PathBuf>, String>,
    ) {
        self.preview_busy.set(false);
        if !self.preview_current(&id, generation) {
            return;
        }
        match result {
            Ok(paths) => {
                let frames: Vec<Image> = paths
                    .iter()
                    .filter_map(|path| Image::load_from_path(path).ok())
                    .collect();
                if frames.is_empty() {
                    self.fail_preview(&id);
                    return;
                }
                log::info!(
                    "assets: hover preview video {id}: {} frames from the Range clip",
                    frames.len()
                );
                self.expire_later(&id, generation);
                self.previews.borrow_mut().insert(
                    id.clone(),
                    CardPreview {
                        state: 2,
                        frames,
                        family: String::new(),
                    },
                );
                self.bump_row(&id);
            }
            Err(error) => {
                log::warn!("assets: preview video {id} failed: {error}");
                self.fail_preview(&id);
            }
        }
    }

    fn audio_arrived(
        &mut self,
        id: String,
        generation: u64,
        result: Result<(Vec<f32>, u32, u16), String>,
    ) {
        self.preview_busy.set(false);
        if !self.preview_current(&id, generation) {
            return;
        }
        match result {
            Ok((pcm, rate, channels)) => {
                if self.audio_preview.borrow().is_none() {
                    match AudioPreview::new() {
                        Ok(player) => *self.audio_preview.borrow_mut() = Some(player),
                        Err(error) => {
                            log::warn!("assets: 试听没有输出设备：{error}");
                            self.fail_preview_no_audio(&id);
                            return;
                        }
                    }
                }
                log::info!(
                    "assets: hover preview audio {id} ready: {} samples at {rate} Hz, {channels} ch",
                    pcm.len()
                );
                if let Some(player) = self.audio_preview.borrow().as_ref() {
                    player.play(pcm, rate, channels);
                }
                self.expire_later(&id, generation);
                self.previews.borrow_mut().insert(
                    id.clone(),
                    CardPreview {
                        state: 3,
                        ..Default::default()
                    },
                );
                self.bump_row(&id);
            }
            Err(error) => {
                log::warn!("assets: audio preview {id} failed: {error}");
                self.fail_preview(&id);
            }
        }
    }

    fn font_arrived(&mut self, id: String, generation: u64, result: Result<Vec<u8>, String>) {
        self.preview_busy.set(false);
        if !self.preview_current(&id, generation) {
            return;
        }
        match result {
            Ok(bytes) => {
                // 注册进窗口的字体集合，拿它自己的 family 名：卡片示例文字
                // 用这个 family 就真的用该字体渲染。
                match register_family(bytes) {
                    Some(family) => {
                        log::info!("assets: hover preview font {id} registered as {family}");
                        self.expire_later(&id, generation);
                        self.font_families
                            .borrow_mut()
                            .insert(id.clone(), family.clone());
                        self.previews.borrow_mut().insert(
                            id.clone(),
                            CardPreview {
                                state: 4,
                                frames: Vec::new(),
                                family,
                            },
                        );
                        self.bump_row(&id);
                    }
                    None => self.fail_preview(&id),
                }
            }
            Err(error) => {
                log::warn!("assets: font preview {id} failed: {error}");
                self.fail_preview(&id);
            }
        }
    }

    pub fn update(&mut self, msg: AssetsMsg, studio: &mut Studio) {
        match msg {
            AssetsMsg::Shown(on) => {
                if on && !self.loaded_once {
                    self.loaded_once = true;
                    self.load();
                } else if !on {
                    self.clear_previews();
                }
            }
            AssetsMsg::KindChanged(index) => {
                let kind = Kind::from_index(index);
                if kind.index() != self.kind.index() {
                    self.kind = kind;
                    self.page = 1;
                    self.load();
                }
            }
            AssetsMsg::IndustryChanged(index) => {
                let index = index.max(0) as usize;
                if index <= self.industries.len() && index != self.industry {
                    self.industry = index;
                    // 换行业 = 新查询：页码回 1、旧请求作废（request 号在 load
                    // 里 +1，旧结果回来时 generation 对不上会直接丢掉）。
                    self.page = 1;
                    self.visible.set((0, THUMB_FIRST - 1));
                    log::info!(
                        "assets: industry changed to {:?} (name {:?}, code {:?})",
                        index,
                        self.industry_name(),
                        self.industry_param()
                    );
                    self.load();
                }
            }
            AssetsMsg::CategoryChanged(index) => {
                let index = index.max(0) as usize;
                if index <= self.categories.len() && index != self.category {
                    self.category = index;
                    self.page = 1;
                    self.load();
                }
            }
            AssetsMsg::QueryEdited(text) => {
                self.query = text;
                self.bump_full();
            }
            AssetsMsg::Search => {
                self.page = 1;
                self.load();
            }
            AssetsMsg::LoadMore => self.load_more(),
            AssetsMsg::Action(id) => self.action(&id, studio),
            AssetsMsg::Visible { first, last } => {
                let first = first.max(0) as usize;
                let last = last.max(0) as usize;
                if last < first || (first, last) == self.visible.get() {
                    return;
                }
                self.visible.set((first, last));
                // 上下各多要一行，滚回去时不用等一下。
                self.request_thumbs(first.saturating_sub(THUMB_FIRST / 6), last + THUMB_FIRST / 6);
            }
            AssetsMsg::Loaded(generation, result) => {
                if generation != self.request {
                    return;
                }
                self.loading = false;
                match *result {
                    Ok(bundle) => {
                        if !bundle.industries.is_empty() {
                            self.industries = bundle.industries;
                        }
                        if !bundle.categories.is_empty() {
                            self.categories = bundle.categories;
                        }
                        self.total = bundle.page.total;
                        self.page = bundle.page.page.max(1);
                        self.items = bundle.page.items;
                        self.drop_foreign_items();
                        self.message.clear();
                        self.settle_page();
                        self.bump_full();
                    }
                    Err(error) => {
                        self.message = error;
                        self.items.clear();
                        self.bump_full();
                    }
                }
            }
            AssetsMsg::MoreLoaded(generation, result) => {
                if generation != self.request {
                    return;
                }
                self.loading = false;
                match *result {
                    Ok(page) => {
                        self.page = page.page.max(1);
                        self.total = page.total;
                        self.items.extend(page.items);
                        self.drop_foreign_items();
                        self.mark_cached();
                        self.settle_page();
                        self.bump_full();
                    }
                    Err(error) => {
                        self.message = error;
                        self.bump_full();
                    }
                }
            }
            AssetsMsg::Thumb { id, result } => self.thumb_arrived(id, result),
            AssetsMsg::Progress { id, fraction } => {
                if self.phase.get(&id).copied() == Some(Phase::Downloading) {
                    self.progress.insert(id.clone(), fraction.clamp(0.0, 1.0));
                    self.bump_row(&id);
                }
            }
            AssetsMsg::Downloaded { id, result } => {
                let title = match self.items.iter().find(|item| item.id == id) {
                    Some(item) => {
                        let kind = kind_of(item);
                        self.title_of(item, kind)
                    }
                    None => id.clone(),
                };
                self.note_downloaded(&id, result);
                if self.phase.get(&id).copied() == Some(Phase::Cached) {
                    self.message = tf("assets.downloaded", &[&title]);
                    self.bump_full();
                }
            }
            AssetsMsg::Hover { id, on } => self.hover(id, on),
            AssetsMsg::PreviewVideo {
                id,
                generation,
                result,
            } => self.video_arrived(id, generation, result),
            AssetsMsg::PreviewAudio {
                id,
                generation,
                result,
            } => self.audio_arrived(id, generation, result),
            AssetsMsg::PreviewFont {
                id,
                generation,
                result,
            } => self.font_arrived(id, generation, result),
            AssetsMsg::PreviewExpire { id, generation } => {
                self.preview_expire(id, generation)
            }
        }
    }
}
