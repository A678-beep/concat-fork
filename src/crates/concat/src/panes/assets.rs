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
//! 卡片上的「可商用」来自 `license.commercial_ok`；需要署名的（CC BY 音乐等）
//! 显示授权名与 `attribution`。下载好之后按钮才变「插入时间轴」：视频/图片
//! 进主轨，音乐/音效进音频轨（`AddClipAtFirstFree` 按媒体类型选轨）。

use std::collections::HashMap;
use std::path::PathBuf;

use concat_host::assets::{self, AssetItem, Industry};
use concat_host::AppDirs;
use concat_project::Command;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::i18n::{t, tf};
use crate::studio::Studio;
use crate::ui::{App, AssetCardData, Assets};

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
    Loaded(Box<Result<Bundle, String>>),
    MoreLoaded(Box<Result<assets::AssetPage, String>>),
    Thumbs(Box<Vec<(String, PathBuf)>>),
    Progress {
        id: String,
        fraction: f32,
    },
    Downloaded {
        id: String,
        result: Result<PathBuf, String>,
    },
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
    loading: bool,
    loaded_once: bool,
    message: String,
    phase: HashMap<String, Phase>,
    progress: HashMap<String, f32>,
    thumbs: HashMap<String, slint::Image>,
    /// 状态真变了才 +1；卡片模型里带缩略图，不能每帧重建。
    revision: std::cell::Cell<u64>,
    published: std::cell::Cell<u64>,
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
            loading: false,
            loaded_once: false,
            message: String::new(),
            phase: HashMap::new(),
            progress: HashMap::new(),
            thumbs: HashMap::new(),
            revision: std::cell::Cell::new(0),
            published: std::cell::Cell::new(u64::MAX),
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

    fn bump(&self) {
        self.revision.set(self.revision.get() + 1);
    }

    fn dirs(&self) -> Option<AppDirs> {
        self.dirs.clone()
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

    /// 左侧「行业」一列：全部 + 平台返回的行业（带数量）。
    fn industries(&self) -> Vec<SharedString> {
        let mut rows = vec![SharedString::from(t("assets.allIndustries"))];
        for industry in &self.industries {
            let label = if industry.count > 0 {
                format!("{} ({})", industry.name, industry.count)
            } else {
                industry.name.clone()
            };
            rows.push(SharedString::from(label));
        }
        rows
    }

    /// 分类下拉：全部 + 平台返回的分类。
    fn categories(&self) -> Vec<SharedString> {
        let mut rows = vec![SharedString::from(t("assets.allCategories"))];
        for category in &self.categories {
            rows.push(SharedString::from(category.clone()));
        }
        rows
    }

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

    fn category_name(&self) -> String {
        if self.category == 0 {
            String::new()
        } else {
            self.categories
                .get(self.category - 1)
                .cloned()
                .unwrap_or_default()
        }
    }

    /// 第一次打开时把行业、分类和第一页一起取回来。
    fn load(&mut self) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.message.clear();
        self.bump();
        let query = self.query.clone();
        let kind = self.kind.param().to_owned();
        let industry = self.industry_name();
        let category = self.category_name();
        crate::host::spawn(
            move || -> Result<Bundle, String> {
                let industries = assets::industries().unwrap_or_default();
                let categories = assets::categories().unwrap_or_default();
                let page = assets::search(&query, &kind, &industry, &category, 1, 30)?;
                Ok(Bundle {
                    industries,
                    categories,
                    page,
                })
            },
            |studio, _, _, result| {
                studio.handle(crate::panes::Msg::Assets(AssetsMsg::Loaded(Box::new(
                    result,
                ))));
            },
        );
    }

    /// 下一页，追加到现有列表后面。
    fn load_more(&mut self) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.bump();
        let query = self.query.clone();
        let kind = self.kind.param().to_owned();
        let industry = self.industry_name();
        let category = self.category_name();
        let page = self.page + 1;
        crate::host::spawn(
            move || assets::search(&query, &kind, &industry, &category, page, 30),
            |studio, _, _, result| {
                studio.handle(crate::panes::Msg::Assets(AssetsMsg::MoreLoaded(Box::new(
                    result,
                ))));
            },
        );
    }

    /// 给刚回来的一页补缩略图：后台逐个下载，好了交回事件循环。
    fn fetch_thumbs(&self) {
        let Some(dirs) = self.dirs() else {
            return;
        };
        let mut wanted = Vec::new();
        for item in &self.items {
            if self.thumbs.contains_key(&item.id) {
                continue;
            }
            wanted.push(item.clone());
        }
        if wanted.is_empty() {
            return;
        }
        crate::host::spawn(
            move || {
                let mut done = Vec::new();
                for item in &wanted {
                    if let Ok(path) = assets::thumb(&dirs, item) {
                        done.push((item.id.clone(), path));
                    }
                }
                done
            },
            |studio, _, _, done| {
                studio.handle(crate::panes::Msg::Assets(AssetsMsg::Thumbs(Box::new(
                    done,
                ))));
            },
        );
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
        self.bump();
        let key = item.id.clone();
        let id2 = key.clone();
        crate::host::spawn(
            move || {
                let cancel = std::sync::atomic::AtomicBool::new(false);
                let report = key.clone();
                assets::download(
                    &dirs,
                    &item,
                    &mut |fraction, _| {
                        let report = report.clone();
                        crate::host::on_ui(move |studio, _, _| {
                            studio.handle(crate::panes::Msg::Assets(AssetsMsg::Progress {
                                id: report.clone(),
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
            Ok(_) => {
                self.phase.insert(id.to_owned(), Phase::Cached);
                self.progress.remove(id);
            }
            Err(error) => {
                self.phase.insert(id.to_owned(), Phase::Failed);
                self.message = error;
            }
        }
        self.bump();
    }

    fn action(&mut self, id: &str, studio: &mut Studio) {
        match self.phase.get(id).copied().unwrap_or(Phase::Idle) {
            Phase::Cached => {
                let Some(dirs) = self.dirs() else {
                    return;
                };
                if let Some(path) = assets::cached(&dirs, id) {
                    self.insert(id, &path, studio);
                } else {
                    self.download(id);
                }
            }
            Phase::Downloading => {}
            Phase::Idle | Phase::Failed => self.download(id),
        }
    }

    /// 缓存好的文件进工程：探一次时长/类型，再进媒体库和第一条空轨。
    fn insert(&mut self, id: &str, path: &PathBuf, studio: &mut Studio) {
        let Some(item) = self.items.iter().find(|item| item.id == id).cloned() else {
            return;
        };
        if kind_of(&item) == Kind::Font {
            self.message = tf("assets.fontCached", &[&item.title]);
            self.bump();
            return;
        }
        let summary = match concat_host::media::probe(path.to_string_lossy().as_ref()) {
            Ok(summary) => summary,
            Err(error) => {
                self.message = error;
                self.bump();
                return;
            }
        };
        let mut media = summary.to_new_media();
        media.name = item.title.clone();
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
            let toast = tf("assets.insertedToast", &[&item.title]);
            studio.notify(&toast, false);
            self.message = tf("assets.inserted", &[&item.title]);
        } else {
            self.message = t("assets.insertFailed");
        }
        self.bump();
    }

    /// 面板状态变了才发布：卡片模型里带着缩略图，不能每帧重建。
    pub fn publish(&self, app: &App) {
        let global = app.global::<Assets>();
        let revision = self.revision.get();
        if self.published.get() == revision && global.get_kind() == self.kind.index() {
            return;
        }
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
            tf(
                "assets.stats",
                &[&self.items.len().to_string(), &self.total.to_string()],
            )
        } else {
            String::new()
        };
        global.set_stats(SharedString::from(stats));
        let mut cards = Vec::with_capacity(self.items.len());
        for item in &self.items {
            let phase = self.phase.get(&item.id).copied().unwrap_or(Phase::Idle);
            let progress = self.progress.get(&item.id).copied().unwrap_or(0.0);
            cards.push(AssetCardData {
                id: SharedString::from(item.id.clone()),
                title: SharedString::from(item.title.clone()),
                detail: SharedString::from(self.detail(item)),
                kind: kind_of(item).index(),
                thumb: self.thumbs.get(&item.id).cloned().unwrap_or_default(),
                has_thumb: self.thumbs.contains_key(&item.id),
                commercial: item.license.commercial_ok,
                attribution: SharedString::from(self.attribution(item)),
                state: phase.code(),
                progress,
                action: SharedString::from(self.action_label(item, phase, progress)),
            });
        }
        global.set_items(ModelRc::new(VecModel::from(cards)));
        self.published.set(revision);
    }

    fn detail(&self, item: &AssetItem) -> String {
        let mut parts = Vec::new();
        if item.duration > 0.5 {
            parts.push(format!("{:.0}s", item.duration));
        }
        if item.width > 0 && item.height > 0 {
            parts.push(format!("{}×{}", item.width, item.height));
        }
        if !item.category.trim().is_empty() {
            parts.push(item.category.clone());
        } else if item.industry.trim().is_empty() {
            parts.push(t("assets.official"));
        } else {
            parts.push(item.industry.clone());
        }
        parts.join(" · ")
    }

    fn attribution(&self, item: &AssetItem) -> String {
        let name = if item.license.name.trim().is_empty() {
            item.license.code.clone()
        } else {
            item.license.name.clone()
        };
        if item.license.attribution_required && !item.attribution.trim().is_empty() {
            format!("{} · 需署名：{}", name, item.attribution)
        } else if item.license.attribution_required {
            format!("{} · {}", name, t("assets.attribution"))
        } else {
            name
        }
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
                    t("assets.cached")
                } else {
                    t("assets.insert")
                }
            }
            Phase::Failed => t("assets.retry"),
        }
    }

    pub fn update(&mut self, msg: AssetsMsg, studio: &mut Studio) {
        match msg {
            AssetsMsg::Shown(on) => {
                if on && !self.loaded_once {
                    self.loaded_once = true;
                    self.load();
                }
            }
            AssetsMsg::KindChanged(index) => {
                let kind = Kind::from_index(index);
                if kind.index() != self.kind.index() {
                    self.kind = kind;
                    self.page = 1;
                    self.items.clear();
                    self.thumbs.clear();
                    self.load();
                }
            }
            AssetsMsg::IndustryChanged(index) => {
                let index = index.max(0) as usize;
                if index <= self.industries.len() && index != self.industry {
                    self.industry = index;
                    self.page = 1;
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
                self.bump();
            }
            AssetsMsg::Search => {
                self.page = 1;
                self.load();
            }
            AssetsMsg::LoadMore => self.load_more(),
            AssetsMsg::Action(id) => self.action(&id, studio),
            AssetsMsg::Loaded(result) => {
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
                        self.page = 1;
                        self.items = bundle.page.items;
                        self.message.clear();
                        self.bump();
                        self.fetch_thumbs();
                    }
                    Err(error) => {
                        self.message = error;
                        self.bump();
                    }
                }
            }
            AssetsMsg::MoreLoaded(result) => {
                self.loading = false;
                match *result {
                    Ok(page) => {
                        self.page = page.page;
                        self.total = page.total;
                        self.items.extend(page.items);
                        self.bump();
                        self.fetch_thumbs();
                    }
                    Err(error) => {
                        self.message = error;
                        self.bump();
                    }
                }
            }
            AssetsMsg::Thumbs(done) => {
                let mut changed = false;
                for (id, path) in *done {
                    if let Ok(image) = slint::Image::load_from_path(&path) {
                        self.thumbs.insert(id, image);
                        changed = true;
                    }
                }
                if changed {
                    self.bump();
                }
            }
            AssetsMsg::Progress { id, fraction } => {
                self.progress.insert(id, fraction.clamp(0.0, 1.0));
                self.bump();
            }
            AssetsMsg::Downloaded { id, result } => {
                let title = self
                    .items
                    .iter()
                    .find(|item| item.id == id)
                    .map(|item| item.title.clone())
                    .unwrap_or_else(|| id.clone());
                self.note_downloaded(&id, result);
                if self.phase.get(&id).copied() == Some(Phase::Cached) {
                    self.message = tf("assets.downloaded", &[&title]);
                }
            }
        }
    }
}
