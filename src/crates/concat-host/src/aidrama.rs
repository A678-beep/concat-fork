// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 星籁 AI 短剧（0.3.1）：桌面内完成「剧本框架 → 剧本分镜 → 成片资产 → 分镜产物
//! → 本集拼接」五阶段的整条闭环。
//!
//! 平台接口（已上线，按生产契约对接）：
//!
//! - `POST /aidrama/script {theme, episodes, duration, ratio, style}`
//!   → `{drama_id, title, logline, characters:[{name,role,look}],
//!      episodes:[{no,title,hook,cover_text,duration,scenes:[{shot,desc,line,duration}]}],
//!      ratio, ai_label, cost_detail, ...}`
//! - `POST /aidrama/render {script:<episode>, episode_no, drama_id, ratio, voice, assets?}`
//!   → `{task_id}`
//! - `GET  /aidrama/task/{id}`
//!   → `{status, progress, video_url, cover, cost_detail, duration, size}`
//! - `GET  /aidrama/list` → 历史短剧
//! - `GET  /aidrama/download/{drama_id}` → 整剧 zip
//!
//! 平台限制（界面要如实提示）：单集 ≤190 秒、≤14 分镜（推荐 5–8），单音色
//! （edge-tts）；渲染全局串行，单片 1–3 分钟；未登录按 IP 限流。
//!
//! 登录态与 AI 工作室同一份（`Authorization: Bearer`，DPAPI 本机加密）；桌面不
//! 内置、不保存任何 API Key。平台返回的是成本价（元）而不是积分：面板如实写
//! 「平台成本」，管理员在平台侧免扣、成本照记。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::AppDirs;

/// 网页版入口（桌面为主，网页仅备用；也用于查看平台侧历史/状态）。
pub const DRAMA_WEB: &str = "https://xinglaiai.com/ai-nav/aidrama/";

/// 平台限制，一句话，面板常驻提示（照契约，不臆造）。
pub const LIMITS: &str = "平台限制：单集 ≤190 秒、≤14 个分镜（推荐 5–8）；单音色（edge-tts）；\
渲染全局串行，单片 1–3 分钟；未登录按 IP 限流。成片右上角带「AI 生成」标注。";

/// 合规声明：与剪映同类的句式（本人素材 / 已授权 / 遵守规范）。
pub const COMPLIANCE: &str = "我确认：短剧使用的素材与角色形象为本人所有或已获得合法授权，遵守平台与法律法规；\
AI 生成的剧本、画面与配音会带「AI 生成」标注，剧情为虚构、不影射真实人物与事件。";

/// 平台接口根地址（与登录同一份，`XINGLAI_API_BASE` 可换）。
fn endpoint(path: &str) -> String {
    let base = std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::account::API_BASE.to_owned());
    format!("{base}{path}")
}

/// 剧本生成的读等待（大模型 3–10 秒，排队时更久）。
const SCRIPT_READ: Duration = Duration::from_secs(120);
/// 短请求的读等待（列表 / 任务轮询）。
const LIST_READ: Duration = Duration::from_secs(30);
/// 单集渲染最长等这么久（平台实测 95–163 秒；排队时更久）。
const RENDER_READ: Duration = Duration::from_secs(15 * 60);
/// 轮询间隔。
const POLL: Duration = Duration::from_secs(2);

/// 一个角色卡（③ 成片资产）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Character {
    /// 姓名。
    #[serde(default)]
    pub name: String,
    /// 身份 / 人物功能。
    #[serde(default)]
    pub role: String,
    /// 外形与气质。
    #[serde(default)]
    pub look: String,
}

impl Character {
    /// 一行：`姓名 · 身份 · 外形`。
    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        for part in [&self.name, &self.role, &self.look] {
            if !part.trim().is_empty() {
                parts.push(part.trim());
            }
        }
        if parts.is_empty() {
            "（空角色，点输入框写）".to_owned()
        } else {
            parts.join(" · ")
        }
    }

    /// 从一行文字拆回三个字段（面板只给一个输入框，够用就好）。
    pub fn from_line(line: &str) -> Character {
        let parts: Vec<&str> = line.split('·').map(str::trim).collect();
        let mut out = Character::default();
        if let Some(name) = parts.first() {
            out.name = (*name).to_owned();
        }
        if let Some(role) = parts.get(1) {
            out.role = (*role).to_owned();
        }
        if let Some(look) = parts.get(2) {
            out.look = (*look).to_owned();
        }
        out
    }
}

/// 一次分镜。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Scene {
    /// 镜头名（平台给的是「雨夜告别」这类短标签；也可能是序号）。
    #[serde(default)]
    pub shot: String,
    /// 画面描述。
    #[serde(default)]
    pub desc: String,
    /// 台词 / 旁白。
    #[serde(default)]
    pub line: String,
    /// 时长（秒，平台给的）。
    #[serde(default)]
    pub duration: f64,
    /// 已配的素材（站内素材标题，或「AI 生成」）。
    #[serde(default)]
    pub asset: String,
    /// 素材候选的显示名（与 `asset` 平行，给「换一个」用）。
    #[serde(default)]
    pub candidates: Vec<String>,
    /// 当前候选下标。
    #[serde(default)]
    pub pick: usize,
}

/// 一集剧本。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Episode {
    /// 集数（1 起）。
    #[serde(default)]
    pub no: u32,
    /// 标题。
    #[serde(default)]
    pub title: String,
    /// 钩子：这一集开头抓住人的那句话。
    #[serde(default)]
    pub hook: String,
    /// 封面文案。
    #[serde(default)]
    pub cover_text: String,
    /// 这一集的时长（秒，平台给的）。
    #[serde(default)]
    pub duration: f64,
    /// 分镜。
    #[serde(default)]
    pub scenes: Vec<Scene>,
    /// ③ 本集配音音色（界面标签，空 = 平台默认）。
    #[serde(default)]
    pub voice: String,
    /// ③ 本集 BGM（界面标签，空 = 不用）。
    #[serde(default)]
    pub bgm: String,
}

impl Episode {
    /// 提交渲染时的 JSON（契约里 `script` 就是一集）。
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "no": self.no,
            "title": self.title,
            "hook": self.hook,
            "cover_text": self.cover_text,
            "duration": self.duration,
            "scenes": self.scenes.iter().map(|scene| serde_json::json!({
                "shot": scene.shot,
                "desc": scene.desc,
                "line": scene.line,
                "duration": scene.duration,
                "asset": scene.asset,
            })).collect::<Vec<_>>(),
        })
    }

    /// 分镜总时长（秒）。
    pub fn shot_seconds(&self) -> f64 {
        self.scenes.iter().map(|scene| scene.duration).sum()
    }
}

/// 整部剧本。
#[derive(Clone, Debug, Default)]
pub struct Script {
    /// 平台短剧 id（历史/整剧下载用）。
    pub drama_id: String,
    /// 剧名。
    pub title: String,
    /// 一句话卖点。
    pub logline: String,
    /// 画幅（`9:16` / `16:9`）。
    pub ratio: String,
    /// 风格。
    pub style: String,
    /// 设定：题材。
    pub theme: String,
    /// 引擎：`deepseek` / `local-template`。
    pub engine: String,
    /// 平台 AI 标注（「AI 生成」）。
    pub ai_label: String,
    /// 平台成本那一行（生成时返回）。
    pub cost_line: String,
    /// 平台给的合规提示。
    pub note: String,
    /// 角色卡（全剧共用，③ 可改）。
    pub characters: Vec<Character>,
    /// 分集。
    pub episodes: Vec<Episode>,
}

impl Script {
    /// 本地模板生成（不调平台）：断网/联调期也能把五阶段走完。面板如实显示
    /// 「本地模板示例」。
    pub fn local(theme: &str, theme_label: &str, episodes: u32, seconds: u32, style: &str) -> Self {
        let theme = theme.trim();
        let label = if theme.is_empty() { theme_label } else { theme };
        let count = episodes.clamp(1, 10);
        let beats = [
            ("开场", "用一个意外把主角推进麻烦里", "这口气，我咽不下。"),
            ("反转", "对手露出真正的目的", "你以为你赢了？才刚刚开始。"),
            ("抉择", "主角必须在两条路里选一条", "这一次，我自己选。"),
            ("升压", "代价出现，身边的人被牵连", "别把我身边的人扯进来。"),
            ("真相", "埋下的线索被翻出来", "原来从一开始，就是局。"),
            ("对峙", "正面对上，台词短、节奏快", "今天，把话说清楚。"),
            ("反转再反转", "看似赢了，其实输了半步", "赢了吗？未必。"),
            ("收束", "把这一集的问题收住，抛出下一集", "下一集，更难看。"),
        ];
        let mut out = Vec::new();
        for index in 0..count {
            let no = index + 1;
            let shots = ((seconds / 6).clamp(4, 8)) as usize;
            let per = f64::from(seconds) / shots as f64;
            let mut scenes = Vec::new();
            for shot in 0..shots {
                let (beat, desc, line) = beats[(index as usize + shot) % beats.len()];
                scenes.push(Scene {
                    shot: beat.to_owned(),
                    desc: format!(
                        "{beat}：{desc}。题材「{label}」，{style}风格，{} 秒一集里的第 {} 个镜头。",
                        seconds,
                        shot + 1
                    ),
                    line: line.to_owned(),
                    duration: (per * 10.0).round() / 10.0,
                    asset: String::new(),
                    candidates: Vec::new(),
                    pick: 0,
                });
            }
            out.push(Episode {
                no,
                title: format!("{label} · 第 {no} 集「{}」", beats[index as usize % beats.len()].0),
                hook: format!(
                    "「{}」——{}",
                    beats[index as usize % beats.len()].2,
                    beats[index as usize % beats.len()].1
                ),
                cover_text: format!("{label} · 第 {no} 集"),
                duration: f64::from(seconds),
                scenes,
                voice: "zh-CN-XiaoxiaoNeural".to_owned(),
                bgm: "悬疑压迫".to_owned(),
            });
        }
        Self {
            drama_id: String::new(),
            title: format!("{label}（本地模板示例）"),
            logline: format!("{label}：{style}风格的多集短剧，本地模板示例，可逐集逐镜改编。"),
            ratio: "9:16".to_owned(),
            style: style.to_owned(),
            theme: label.to_owned(),
            engine: "local-template".to_owned(),
            ai_label: "AI 生成".to_owned(),
            cost_line: String::new(),
            note: "本地模板示例：没有调用平台。接口可直接用「平台生成剧本」出稿；\
现在可以逐集改编、配角色/音频、配分镜素材、导出 JSON。"
                .to_owned(),
            characters: vec![
                Character {
                    name: "林晚".to_owned(),
                    role: "主角 · 被推着往前走的人".to_owned(),
                    look: "28 岁，黑长发，洗得发白的衬衫，眼神硬".to_owned(),
                },
                Character {
                    name: "赵总".to_owned(),
                    role: "对手 · 这一集的麻烦源头".to_owned(),
                    look: "45 岁，西装，金丝眼镜，笑面".to_owned(),
                },
                Character {
                    name: "小满".to_owned(),
                    role: "朋友 · 递关键线索的人".to_owned(),
                    look: "24 岁，短夹克，说话快，笑起来有虎牙".to_owned(),
                },
            ],
            episodes: out,
        }
    }

    /// 剧本里共有几个分镜。
    pub fn scene_count(&self) -> usize {
        self.episodes.iter().map(|episode| episode.scenes.len()).sum()
    }

    /// 导出成契约里的 JSON 形状。
    pub fn to_json(&self) -> serde_json::Value {
        let characters: Vec<serde_json::Value> = self
            .characters
            .iter()
            .map(|character| {
                serde_json::json!({
                    "name": character.name,
                    "role": character.role,
                    "look": character.look,
                })
            })
            .collect();
        let episodes: Vec<serde_json::Value> = self
            .episodes
            .iter()
            .map(|episode| {
                let mut value = episode.to_json();
                value["voice"] = serde_json::json!(episode.voice);
                value["bgm"] = serde_json::json!(episode.bgm);
                value
            })
            .collect();
        serde_json::json!({
            "drama_id": self.drama_id,
            "title": self.title,
            "logline": self.logline,
            "theme": self.theme,
            "style": self.style,
            "ratio": self.ratio,
            "engine": self.engine,
            "ai_label": self.ai_label,
            "characters": characters,
            "episodes": episodes,
        })
    }

    /// 写一份 JSON 到 `path`。
    pub fn save_json(&self, path: &std::path::Path) -> Result<u64, String> {
        let text = serde_json::to_string_pretty(&self.to_json())
            .map_err(|error| format!("剧本序列化失败：{error}"))?;
        std::fs::write(path, text.as_bytes()).map_err(|error| format!("写不了剧本文件：{error}"))?;
        Ok(text.len() as u64)
    }
}

/// 后端是否就绪：面板常驻一行状态。
#[derive(Clone, Debug, Default)]
pub struct Backend {
    /// 接口在：`GET /aidrama/list` 回答了 200。
    pub ready: bool,
    /// 一句话说明。
    pub line: String,
}

/// 登录态（短剧接口和 AI 工作室同一份账号）。
pub fn token() -> Result<String, String> {
    crate::ai_studio::token()
}

/// 服务器说不行时翻成一句人话：优先用平台给的中文提示。
fn refusal(status: u16, text: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(text).ok();
    let field = |name: &str| {
        value
            .as_ref()
            .and_then(|value| value.get(name))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let message = field("hint")
        .or_else(|| field("message"))
        .or_else(|| field("error"));
    match status {
        0 => "服务器没有给出回复（连接被中途关闭），请稍后重试".to_owned(),
        401 => "登录已失效，请重新登录星籁账号".to_owned(),
        402 | 403 => message.unwrap_or_else(|| {
            "积分/额度不足或没有这个功能的权限，请在网页工作台充值或升级套餐".to_owned()
        }),
        404 => "AI 短剧接口不存在：请确认服务端已上线 /api/aidrama/*（本版按生产契约对接）".to_owned(),
        413 => "上传的文件太大，请压缩后再试".to_owned(),
        429 => message.unwrap_or_else(|| {
            "请求太频繁：平台限流（剧本 10 次/10 分钟、渲染 6 次/10 分钟），稍后再试".to_owned()
        }),
        500..=599 => message
            .unwrap_or_else(|| format!("星籁云端 AI 暂时不可用（HTTP {status}），请稍后再试")),
        _ => message.unwrap_or_else(|| format!("云端 AI 返回了 HTTP {status}")),
    }
}

fn json_reply(status: u16, text: &str) -> Result<serde_json::Value, String> {
    if !(200..300).contains(&status) {
        return Err(refusal(status, text));
    }
    serde_json::from_str(text).map_err(|error| format!("服务器的回复不是 JSON：{error}"))
}

fn check_ok(value: &serde_json::Value) -> Result<(), String> {
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
        return Err(value
            .get("hint")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("message").and_then(serde_json::Value::as_str))
            .or_else(|| value.get("error").and_then(serde_json::Value::as_str))
            .unwrap_or("云端 AI 拒绝了这次请求")
            .to_owned());
    }
    Ok(())
}

/// 一次 GET。
fn get(path: &str, token: Option<&str>, read: Duration) -> Result<serde_json::Value, String> {
    let (status, text) =
        crate::account::api_call("GET", &endpoint(path), "application/json", &[], token, read)?;
    json_reply(status, &text)
}

/// 一次 POST。
fn post(
    path: &str,
    body: &serde_json::Value,
    token: Option<&str>,
    read: Duration,
) -> Result<serde_json::Value, String> {
    let body = body.to_string();
    let (status, text) = crate::account::api_call(
        "POST",
        &endpoint(path),
        "application/json",
        body.as_bytes(),
        token,
        read,
    )?;
    json_reply(status, &text)
}

/// 成本价（元）的显示：`0.008` → `0.008 元`。
pub fn rmb(value: f64) -> String {
    if value <= 0.0 {
        return "0 元".to_owned();
    }
    if value < 0.001 {
        format!("{value:.4} 元")
    } else if value < 1.0 {
        format!("{value:.3} 元")
    } else {
        format!("{value:.2} 元")
    }
}

/// 平台 `cost_detail` 里抽一行成本说明。
fn cost_line(value: &serde_json::Value, admin: bool) -> String {
    let detail = value.get("cost_detail");
    let charged = detail
        .and_then(|detail| detail.get("charged"))
        .and_then(serde_json::Value::as_f64)
        .or_else(|| value.get("cost").and_then(serde_json::Value::as_f64))
        .unwrap_or(0.0);
    let internal = detail
        .and_then(|detail| detail.get("internal_cost_cny"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    if admin || (charged <= 0.0 && internal > 0.0) {
        format!(
            "内部账号免扣积分（成本照记：{}）",
            if internal > 0.0 {
                rmb(internal)
            } else {
                rmb(charged)
            }
        )
    } else if charged > 0.0 {
        format!("平台计费 {}", rmb(charged))
    } else {
        String::new()
    }
}

/// 探测后端状态（打开面板时调一次；不阻塞界面）。
pub fn availability(token: Option<&str>) -> Backend {
    match get("/aidrama/list", token, LIST_READ) {
        Ok(_) => Backend {
            ready: true,
            line: "AI 短剧后端已就绪：平台剧本、逐集渲染、历史短剧、整剧下载都可以在桌面完成"
                .to_owned(),
        },
        Err(error) => Backend {
            ready: false,
            line: format!("后端不可用：{error}"),
        },
    }
}

/// 从平台 JSON 里读一部剧本（字段名对几种叫法都认，后端小改不会把桌面打挂）。
fn parse_script(value: &serde_json::Value, theme: &str, style: &str) -> Result<Script, String> {
    check_ok(value)?;
    let text = |value: &serde_json::Value, keys: &[&str]| -> String {
        for key in keys {
            if let Some(found) = value.get(*key).and_then(serde_json::Value::as_str) {
                if !found.trim().is_empty() {
                    return found.trim().to_owned();
                }
            }
        }
        String::new()
    };
    let number = |value: &serde_json::Value, keys: &[&str]| -> f64 {
        for key in keys {
            if let Some(found) = value.get(*key).and_then(serde_json::Value::as_f64) {
                return found;
            }
        }
        0.0
    };
    let mut characters = Vec::new();
    if let Some(rows) = value
        .get("characters")
        .and_then(serde_json::Value::as_array)
    {
        for row in rows {
            if row.is_string() {
                characters.push(Character::from_line(row.as_str().unwrap_or_default()));
            } else {
                characters.push(Character {
                    name: text(row, &["name"]),
                    role: text(row, &["role", "identity"]),
                    look: text(row, &["look", "looks", "appearance", "desc"]),
                });
            }
        }
    }
    let mut episodes = Vec::new();
    if let Some(rows) = value.get("episodes").and_then(serde_json::Value::as_array) {
        for (index, row) in rows.iter().enumerate() {
            let no = row
                .get("no")
                .or_else(|| row.get("episode_no"))
                .and_then(serde_json::Value::as_u64)
                .map(|no| no as u32)
                .unwrap_or(index as u32 + 1);
            let mut scenes = Vec::new();
            if let Some(shots) = row.get("scenes").and_then(serde_json::Value::as_array) {
                for (shot_index, shot) in shots.iter().enumerate() {
                    let label = match shot.get("shot") {
                        Some(serde_json::Value::String(label)) if !label.trim().is_empty() => {
                            label.trim().to_owned()
                        }
                        Some(serde_json::Value::Number(no)) => format!("镜 {no}"),
                        _ => format!("镜 {}", shot_index + 1),
                    };
                    scenes.push(Scene {
                        shot: label,
                        desc: text(shot, &["desc", "description", "prompt"]),
                        line: text(shot, &["line", "dialogue", "narration", "text"]),
                        duration: number(shot, &["duration", "seconds"]),
                        asset: String::new(),
                        candidates: Vec::new(),
                        pick: 0,
                    });
                }
            }
            episodes.push(Episode {
                no,
                title: {
                    let title = text(row, &["title", "name"]);
                    if title.is_empty() {
                        format!("第 {no} 集")
                    } else {
                        title
                    }
                },
                hook: text(row, &["hook", "opening", "summary"]),
                cover_text: text(row, &["cover_text", "cover"]),
                duration: number(row, &["duration", "seconds"]),
                scenes,
                voice: text(row, &["voice"]),
                bgm: text(row, &["bgm"]),
            });
        }
    }
    if episodes.is_empty() {
        return Err("平台返回的剧本里没有分集（episodes 为空）".to_owned());
    }
    let summary = value
        .get("script")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let title = {
        let title = text(value, &["title"]);
        if title.is_empty() { text(&summary, &["title"]) } else { title }
    };
    let logline = {
        let logline = text(value, &["logline"]);
        if logline.is_empty() { text(&summary, &["logline"]) } else { logline }
    };
    if characters.is_empty() {
        if let Some(rows) = summary
            .get("characters")
            .and_then(serde_json::Value::as_array)
        {
            for row in rows {
                characters.push(Character {
                    name: text(row, &["name"]),
                    role: text(row, &["role"]),
                    look: text(row, &["look"]),
                });
            }
        }
    }
    Ok(Script {
        drama_id: text(value, &["drama_id", "id"]),
        title,
        logline,
        ratio: {
            let ratio = text(value, &["ratio"]);
            if ratio.is_empty() { "9:16".to_owned() } else { ratio }
        },
        style: {
            let got = text(value, &["style"]);
            if got.is_empty() { style.to_owned() } else { got }
        },
        theme: theme.to_owned(),
        engine: {
            let engine = text(value, &["engine"]);
            if engine.is_empty() { "platform".to_owned() } else { engine }
        },
        ai_label: {
            let label = text(value, &["ai_label"]);
            if label.is_empty() { "AI 生成".to_owned() } else { label }
        },
        cost_line: cost_line(value, false),
        note: {
            let mut note = text(value, &["note"]);
            if note.is_empty() {
                if let Some(compliance) = value.get("compliance") {
                    note = text(compliance, &["disclaimer", "rule"]);
                }
            }
            note
        },
        characters,
        episodes,
    })
}

/// 生成分集剧本（`POST /aidrama/script`）。
pub fn script(
    token: &str,
    theme: &str,
    episodes: u32,
    seconds: u32,
    ratio: &str,
    style: &str,
) -> Result<Script, String> {
    let body = serde_json::json!({
        "theme": theme,
        "episodes": episodes.clamp(1, 10),
        "duration": seconds,
        "ratio": ratio,
        "style": style,
    });
    let value = post("/aidrama/script", &body, Some(token), SCRIPT_READ)?;
    parse_script(&value, theme, style)
}

/// 提交一集成片（`POST /aidrama/render`），返回任务 id。
///
/// 发契约里验证过的字段：`script`（这一集）、`episode_no`、`drama_id`、`ratio`；
/// 用户选了音色就把 edge-tts 的音色 id 一起发（`voice`，平台默认
/// `zh-CN-XiaoxiaoNeural`）。站内素材匹配是创作参考，由平台自己按分镜出片，
/// 不塞进渲染请求，免得改到平台的素材匹配逻辑。
pub fn render(token: &str, whole: &Script, episode: &Episode) -> Result<String, String> {
    let mut body = serde_json::json!({
        "script": episode.to_json(),
        "episode_no": episode.no,
        "drama_id": whole.drama_id,
        "ratio": whole.ratio,
    });
    if !episode.voice.trim().is_empty() {
        body["voice"] = serde_json::json!(episode.voice.trim());
    }
    let value = post("/aidrama/render", &body, Some(token), SCRIPT_READ)?;
    check_ok(&value)?;
    let task = value
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if task.is_empty() {
        return Err("平台没返回 task_id，稍后重试或到网页版查看".to_owned());
    }
    Ok(task)
}

/// 一次渲染任务的状态。
#[derive(Clone, Debug, Default)]
pub struct TaskStatus {
    /// queued / running / done / failed。
    pub status: String,
    /// 0..1。
    pub progress: f32,
    /// 成片地址。
    pub video_url: String,
    /// 封面地址。
    pub cover: String,
    /// 失败原因。
    pub error: String,
    /// 计费说明一行。
    pub cost_line: String,
    /// 成片时长（秒）。
    pub duration: f64,
    /// 字节数。
    pub size: u64,
    /// 内部账号免扣。
    pub admin_exempt: bool,
}

impl TaskStatus {
    /// 终态了吗。
    pub fn finished(&self) -> bool {
        matches!(
            self.status.as_str(),
            "done" | "succeeded" | "success" | "finished" | "failed" | "error" | "cancelled" | "canceled"
        )
    }
    /// 成功了吗。
    pub fn ok(&self) -> bool {
        matches!(self.status.as_str(), "done" | "succeeded" | "success" | "finished")
    }
}

/// 查一次任务（`GET /aidrama/task/{id}`）。
pub fn task(token: &str, id: &str) -> Result<TaskStatus, String> {
    let mut path = String::from("/aidrama/task/");
    path.push_str(&percent_encode(id));
    let value = get(&path, Some(token), LIST_READ)?;
    check_ok(&value)?;
    let text = |name: &str| {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let raw_progress = value
        .get("progress")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let progress = if raw_progress > 1.0 {
        (raw_progress / 100.0) as f32
    } else {
        raw_progress as f32
    };
    Ok(TaskStatus {
        status: text("status").to_lowercase(),
        progress: progress.clamp(0.0, 1.0),
        video_url: {
            let url = text("video_url");
            if url.is_empty() { text("url") } else { url }
        },
        cover: text("cover"),
        error: {
            let error = text("error");
            if error.is_empty() { text("hint") } else { error }
        },
        cost_line: cost_line(&value, false),
        duration: value
            .get("duration")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
        size: value
            .get("size")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        admin_exempt: value
            .get("admin_exempt")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// 一条下载好的成片。
#[derive(Clone, Debug, Default)]
pub struct EpisodeOutput {
    /// 集数。
    pub no: u32,
    /// 落盘的 mp4。
    pub path: PathBuf,
    /// 平台任务 id。
    pub task_id: String,
    /// 时长，秒。
    pub duration: f64,
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// 字节数。
    pub size: u64,
    /// 计费说明。
    pub credits: String,
    /// 内部账号免扣。
    pub admin_exempt: bool,
}

impl EpisodeOutput {
    /// 界面上的一行。
    pub fn summary(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut text = format!(
            "第 {} 集 · {:.1} 秒 · {}×{} · {:.1} MB · {name}",
            self.no,
            self.duration,
            self.width,
            self.height,
            self.size as f64 / (1024.0 * 1024.0)
        );
        if !self.credits.trim().is_empty() {
            text.push_str(" · ");
            text.push_str(self.credits.trim());
        }
        if self.admin_exempt {
            text.push_str(" · 内部账号免扣积分（成本照记）");
        }
        text
    }
}

/// 等一集渲染完并下载成片。
pub fn wait_episode(
    dirs: &AppDirs,
    token: &str,
    episode_no: u32,
    task_id: &str,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<EpisodeOutput, String> {
    let deadline = Instant::now() + RENDER_READ;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消（平台上的任务可能仍在后台跑完）".to_owned());
        }
        if Instant::now() > deadline {
            return Err(format!(
                "等平台渲染超时（{} 分钟）：可以到网页版查看任务，或重试",
                RENDER_READ.as_secs() / 60
            ));
        }
        let status = task(token, task_id)?;
        progress(
            status.progress.clamp(0.0, 0.95),
            format!(
                "第 {episode_no} 集 · {}{}",
                human_status(&status.status),
                if status.progress > 0.0 {
                    format!(" {}%", (status.progress * 100.0).round() as i32)
                } else {
                    String::new()
                }
            ),
        );
        if status.finished() {
            if !status.ok() {
                let reason = if status.error.trim().is_empty() {
                    format!("平台返回 {}", human_status(&status.status))
                } else {
                    status.error.clone()
                };
                return Err(format!("第 {episode_no} 集渲染失败：{reason}"));
            }
            if status.video_url.trim().is_empty() {
                return Err("任务完成，但平台没有给出成片地址".to_owned());
            }
            let (path, size) =
                download(dirs, episode_no, task_id, &status.video_url, progress, cancel)?;
            let summary = crate::media::probe(&path.to_string_lossy()).ok();
            let duration = summary
                .as_ref()
                .and_then(|media| media.duration)
                .unwrap_or(status.duration);
            let (width, height) = summary
                .as_ref()
                .and_then(|media| media.video.as_ref())
                .map(|video| (video.width, video.height))
                .unwrap_or((0, 0));
            return Ok(EpisodeOutput {
                no: episode_no,
                path,
                task_id: task_id.to_owned(),
                duration,
                width,
                height,
                size,
                credits: status.cost_line,
                admin_exempt: status.admin_exempt,
            });
        }
        std::thread::sleep(POLL);
    }
}

/// 任务状态的中文说法。
pub fn human_status(status: &str) -> &'static str {
    match status {
        "queued" | "pending" | "waiting" => "排队中",
        "running" | "processing" | "rendering" => "渲染中",
        "done" | "succeeded" | "success" | "finished" => "已完成",
        "failed" | "error" => "失败",
        "cancelled" | "canceled" => "已取消",
        "" => "提交中",
        _ => "处理中",
    }
}

/// 下载一集到 `%APPDATA%\app.concat.editor\ai-output\`。
fn download(
    dirs: &AppDirs,
    episode_no: u32,
    task_id: &str,
    url: &str,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<(PathBuf, u64), String> {
    let dir = crate::ai_studio::output_dir(dirs);
    std::fs::create_dir_all(&dir).map_err(|error| format!("建不了 ai-output 目录：{error}"))?;
    let stamp = crate::ai_studio::stamp_ms();
    let dest = dir.join(format!(
        "aidrama-ep{episode_no}-{stamp}-{}.mp4",
        short_id(task_id)
    ));
    let temporary = dest.with_extension("part");
    crate::direct::download(
        &crate::ai_studio::absolute(url),
        &temporary,
        &mut |fraction, detail| {
            progress(
                fraction.clamp(0.0, 1.0),
                format!("第 {episode_no} 集 · 下载中 · {detail}"),
            );
        },
        cancel,
    )?;
    let size = std::fs::metadata(&temporary).map_or(0, |meta| meta.len());
    if size < 30_000 {
        let _ = std::fs::remove_file(&temporary);
        return Err("下载到的成片不完整（文件太小），请重试".to_owned());
    }
    std::fs::rename(&temporary, &dest).map_err(|error| format!("存不下成片：{error}"))?;
    Ok((dest, size))
}

/// 整剧打包下载（`GET /aidrama/download/{drama_id}` → zip 二进制）。
pub fn download_zip(
    dirs: &AppDirs,
    token: &str,
    drama_id: &str,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    if drama_id.trim().is_empty() {
        return Err("这部剧本还没有平台 id（本地模板示例）：先「平台生成剧本」再打包".to_owned());
    }
    let dir = crate::ai_studio::output_dir(dirs);
    std::fs::create_dir_all(&dir).map_err(|error| format!("建不了 ai-output 目录：{error}"))?;
    let stamp = crate::ai_studio::stamp_ms();
    let dest = dir.join(format!("aidrama-{}-{stamp}.zip", short_id(drama_id)));
    let path = format!("/aidrama/download/{}", percent_encode(drama_id));
    let url = endpoint(&path);
    let bytes = crate::account::api_download(&url, Some(token), &dest, RENDER_READ, cancel)?;
    if bytes < 1024 {
        let _ = std::fs::remove_file(&dest);
        return Err("下载到的整剧包不完整（文件太小），请重试".to_owned());
    }
    Ok(dest)
}

/// 任务 id 里取 8 个字符，做文件名。
fn short_id(id: &str) -> String {
    id.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(8)
        .collect()
}

/// 历史短剧（`GET /aidrama/list`）。
pub fn history(token: &str) -> Result<Vec<String>, String> {
    let value = get("/aidrama/list", Some(token), LIST_READ)?;
    check_ok(&value)?;
    let rows = value
        .get("items")
        .or_else(|| value.get("list"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut lines = Vec::new();
    for row in rows {
        let text = |name: &str| {
            row.get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let title = {
            let title = text("title");
            if title.is_empty() { text("theme") } else { title }
        };
        let episodes = row
            .get("episodes_n")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let rendered = row
            .get("rendered_n")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let created = text("created_at");
        let status = text("status");
        lines.push(format!(
            "{}{}{}{}{}",
            if title.is_empty() {
                "未命名短剧".to_owned()
            } else {
                title
            },
            if episodes > 0 {
                format!(" · {episodes} 集")
            } else {
                String::new()
            },
            if rendered > 0 {
                format!(" · 已成片 {rendered}")
            } else {
                String::new()
            },
            if status.is_empty() {
                String::new()
            } else {
                format!(" · {status}")
            },
            if created.is_empty() {
                String::new()
            } else {
                format!(" · {created}")
            }
        ));
    }
    Ok(lines)
}

/// 本剧的成本估算。平台按内部成本计费（实测 30 秒单集约 0.008–0.01 元），
/// 面板如实写「估算，以平台返回为准」；管理员在平台侧免扣、成本照记。
pub fn estimated_cost(episodes: u32, seconds: u32) -> f64 {
    let per_episode = 0.006 + f64::from(seconds) * 0.00012;
    f64::from(episodes.clamp(1, 10)) * per_episode
}

/// 一行的成本说明（生成前）。
pub fn estimate_line(episodes: u32, seconds: u32, admin: bool) -> String {
    let total = estimated_cost(episodes, seconds);
    if admin {
        format!(
            "本剧预计平台成本约 {}（{episodes} 集 × {seconds}s，实测口径估算）· 内部账号（管理员）免扣积分，成本照记；以平台返回为准",
            rmb(total)
        )
    } else {
        format!(
            "本剧预计平台成本约 {}（{episodes} 集 × {seconds}s，实测口径估算）；以平台实际返回为准",
            rmb(total)
        )
    }
}

/// 素材候选：给分镜自动配站内商用素材（复用 0.3.0 的素材匹配接口）。
pub fn match_assets(token: &str, query: &str, limit: u32) -> Result<Vec<String>, String> {
    let items = crate::ai_studio::library(token, query, limit)?;
    Ok(items
        .into_iter()
        .map(|item| {
            let title = if item.title_zh.trim().is_empty() {
                item.title.clone()
            } else {
                item.title_zh.clone()
            };
            let title = if title.trim().is_empty() {
                format!("素材 #{}", item.id)
            } else {
                title
            };
            if item.duration_text.trim().is_empty() {
                title
            } else {
                format!("{title} · {}", item.duration_text)
            }
        })
        .collect())
}

/// 从分镜描述里猜一个素材关键词（够用就好，不追求聪明）。
pub fn asset_query(desc: &str) -> String {
    const KNOWN: &[(&str, &str)] = &[
        ("夜", "城市 夜景"),
        ("街", "街道"),
        ("咖啡", "咖啡"),
        ("办公", "办公"),
        ("公司", "办公"),
        ("家", "家庭"),
        ("厨房", "厨房"),
        ("餐", "美食"),
        ("公园", "公园"),
        ("海", "海边"),
        ("山", "山"),
        ("车", "汽车"),
        ("手机", "手机"),
        ("教室", "教室"),
        ("医院", "医院"),
        ("雨", "下雨"),
        ("笑", "人物 微笑"),
        ("跑", "跑步"),
        ("门", "门口"),
        ("电梯", "电梯"),
        ("会议", "会议"),
    ];
    for (hint, query) in KNOWN {
        if desc.contains(hint) {
            return (*query).to_owned();
        }
    }
    "生活 场景".to_owned()
}

/// `urlencode`，只给任务 id / 查询用。
fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_script_has_the_episodes_and_scenes_asked_for() {
        let script = Script::local("都市逆袭", "都市逆袭", 3, 60, "写实");
        assert_eq!(script.episodes.len(), 3);
        assert!(script
            .episodes
            .iter()
            .all(|episode| !episode.scenes.is_empty()));
        assert_eq!(script.engine, "local-template");
        assert!(!script.characters.is_empty());
        let json = script.to_json();
        assert_eq!(json["episodes"].as_array().unwrap().len(), 3);
        assert!(json["episodes"][0]["scenes"][0]["desc"].as_str().is_some());
        assert!(json["characters"][0]["name"].as_str().is_some());
    }

    #[test]
    fn an_estimate_grows_with_episodes_and_seconds() {
        assert!(estimated_cost(5, 60) > estimated_cost(3, 60));
        assert!(estimated_cost(5, 60) > estimated_cost(5, 30));
        assert!(estimate_line(2, 45, true).contains("免扣"));
        assert!(rmb(0.008).contains("0.008"));
    }

    #[test]
    fn a_character_line_round_trips() {
        let character = Character {
            name: "林晚".to_owned(),
            role: "主角".to_owned(),
            look: "28 岁".to_owned(),
        };
        let line = character.line();
        assert_eq!(line, "林晚 · 主角 · 28 岁");
        let back = Character::from_line(&line);
        assert_eq!(back.name, "林晚");
        assert_eq!(back.look, "28 岁");
    }

    #[test]
    fn a_platform_script_reads_the_contract_fields() {
        let value = serde_json::json!({
            "ok": true,
            "drama_id": "ad1",
            "title": "被顶替的实习生",
            "logline": "一句话",
            "characters": [{"name": "林晚", "role": "女主", "look": "黑长发"}],
            "episodes": [{
                "no": 1,
                "title": "第一集",
                "hook": "钩子",
                "cover_text": "封面",
                "duration": 30.0,
                "scenes": [{"shot": "雨夜告别", "desc": "描述", "line": "台词", "duration": 5.0}]
            }],
            "ratio": "9:16",
            "style": "写实",
            "engine": "deepseek",
            "ai_label": "AI 生成",
            "cost_detail": {"charged": 0.0, "internal_cost_cny": 0.0065}
        });
        let script = parse_script(&value, "都市逆袭", "写实").expect("parse");
        assert_eq!(script.drama_id, "ad1");
        assert_eq!(script.episodes[0].scenes[0].shot, "雨夜告别");
        assert_eq!(script.episodes[0].scenes[0].duration, 5.0);
        assert_eq!(script.characters[0].name, "林晚");
        assert!(script.cost_line.contains("免扣"));
    }
}
