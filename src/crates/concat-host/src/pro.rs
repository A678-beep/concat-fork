// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 影视专业面板（0.3.7）：把网页平台「专业影视」一整排能力接到桌面。
//!
//! 九块面板的接口都在这一层：导演编排器（`/director/*`）、导演监视器
//! （`/filmgen/monitor/*`）、发行中心（`/aidrama/*` 的发行子集）、授权护照
//! （`/passport/*`）、合规沙盘（`/sandbox/*`）、插件市场（`/plugins/*`）、
//! 模板市场（`/templates-market/*`），以及本机统一任务中心的本地记录。
//! 素材库复用 [`crate::assets`]（0.2.20 起已对接 `/assets/*`），这里只补
//! 一个「打开网页版」的地址。
//!
//! 网络走登录同一条 OpenSSL 直连（[`crate::account::api_call`]），登录态与
//! 电影向导共用一份 token；本模块只发请求、拆 JSON、存本地任务记录，不做
//! 任何编辑决定。服务端未被修改，也不重启生产。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 星籁平台网页入口。
pub const SITE: &str = "https://xinglaiai.com";

/// 平台接口根地址（与登录同一份，`XINGLAI_API_BASE` 可换）。
pub fn endpoint(path: &str) -> String {
    let base = std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::account::API_BASE.to_owned());
    format!("{base}{path}")
}

/// 网页版备用地址：接口不通用它把用户送到网页，不是假按钮。
pub fn web_url(panel: &str) -> String {
    let page = match panel {
        "director" => "director",
        "monitor" => "monitor",
        "publish" => "publish",
        "passport" => "passport",
        "sandbox" => "sandbox",
        "plugins" => "plugins",
        "assets" => "assets",
        "templates" => "templates",
        // 统一任务中心在网页没有独立页：送到导演任务的网页入口。
        "tasks" => "director",
        _ => "",
    };
    if page.is_empty() {
        format!("{SITE}/ai-nav/")
    } else {
        format!("{SITE}/ai-nav/{page}/")
    }
}

/// 短请求的读等待。
const READ: Duration = Duration::from_secs(30);
/// 规划 / 打包 / 生成类请求的读等待（规划过一次大模型，通常 10–40s）。
const LONG_READ: Duration = Duration::from_secs(200);
/// 发行打包与护照批量的读等待（平台要压 zip / 逐素材算哈希）。
const PACK_READ: Duration = Duration::from_secs(300);

/// 当前登录 token（没登录就没有）。
pub fn token() -> Option<String> {
    crate::movie::token()
}

/// 是否已登录（面板「未登录」引导用）。
pub fn signed_in() -> bool {
    token().is_some()
}

/// 一次 JSON 调用：2xx + `ok != false` 才算成功；平台的中文提示原样带回。
pub fn call(method: &str, path: &str, body: Option<&Value>, read: Duration) -> Result<Value, String> {
    call_at(method, &endpoint(path), body, read)
}

/// 同上，但地址已经拼好（下载、绝对地址）。
pub fn call_at(method: &str, url: &str, body: Option<&Value>, read: Duration) -> Result<Value, String> {
    let token = token();
    let payload = match body {
        Some(value) => serde_json::to_vec(value).map_err(|error| format!("请求编码失败：{error}"))?,
        None => Vec::new(),
    };
    let (status, text) = crate::account::api_call(
        method,
        url,
        "application/json",
        &payload,
        token.as_deref(),
        read,
    )
    .map_err(|error| format!("连不上星籁平台：{error}"))?;
    let value = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| {
        json!({"ok": false, "hint": text.chars().take(200).collect::<String>()})
    });
    if !(200..300).contains(&status) {
        let hint = api_hint(&value);
        if status == 401 {
            return Err(format!("请先登录星籁账号：{hint}"));
        }
        return Err(format!("平台拒绝了这次请求（HTTP {status}）：{hint}"));
    }
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(format!("平台返回失败：{}", api_hint(&value)));
    }
    Ok(value)
}

/// 平台错误里的一句话（`hint` / `error_cn` / `error`）。
pub fn api_hint(value: &Value) -> String {
    value
        .get("hint")
        .and_then(Value::as_str)
        .or_else(|| value.get("error_cn").and_then(Value::as_str))
        .or_else(|| value.get("error").and_then(Value::as_str))
        .map(str::to_owned)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "没有给出原因".to_owned())
}

fn get(path: &str) -> Result<Value, String> {
    call("GET", path, None, READ)
}

fn post(path: &str, body: &Value) -> Result<Value, String> {
    call("POST", path, Some(body), READ)
}

fn post_read(path: &str, body: &Value, read: Duration) -> Result<Value, String> {
    call("POST", path, Some(body), read)
}

/// 平台返回的相对地址换成完整 URL（绝对地址原样返回）。
pub fn absolute_url(path: &str) -> String {
    let path = path.trim();
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_owned();
    }
    if path.starts_with("/ai-nav/") {
        return format!("{SITE}{path}");
    }
    if path.starts_with("/api/") {
        return format!("{SITE}/ai-nav{path}");
    }
    if path.starts_with('/') {
        return format!("{}{path}", crate::account::API_BASE.trim_end_matches('/'));
    }
    format!("{}/{}", crate::account::API_BASE.trim_end_matches('/'), path)
}

/// 百分比编码（查询参数里只放中文 / 空格 / 斜杠这几类）。
pub fn enc(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
// 1. 导演编排器 `/director/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /director/models`：厂商接入、能力矩阵、三档路由与三档定价。
pub fn director_models() -> Result<Value, String> {
    get("/director/models")
}

/// `GET /director/pricing`：三档单镜 / 单集成本（与模型矩阵同源）。
pub fn director_pricing() -> Result<Value, String> {
    get("/director/pricing")
}

/// `POST /director/plan`：按档位把分镜路由到模型，返回每镜模型 / 成本 / 降级警告；
/// 这一步不调用厂商，不产生费用。
pub fn director_plan(body: &Value) -> Result<Value, String> {
    post_read("/director/plan", body, LONG_READ)
}

/// `POST /director/run`：真实生成任务（异步），返回任务摘要。
pub fn director_run(body: &Value) -> Result<Value, String> {
    post_read("/director/run", body, LONG_READ)
}

/// `GET /director/task/{id}`：逐镜进度、成本、成片 / 工程包地址。
pub fn director_task(id: &str) -> Result<Value, String> {
    get(&format!("/director/task/{}", enc(id)))
}

/// `GET /director/tasks?limit=`：最近的导演任务（任务中心用）。
pub fn director_tasks(limit: u32) -> Result<Value, String> {
    get(&format!("/director/tasks?limit={}", limit.clamp(1, 50)))
}

// ─────────────────────────────────────────────────────────────────────
// 2. 导演监视器 `/filmgen/monitor/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /filmgen/monitor/health`：路由清单与计数。
pub fn monitor_health() -> Result<Value, String> {
    get("/filmgen/monitor/health")
}

/// `GET /filmgen/monitor/types`：9 类影视 / 短剧模板。
pub fn monitor_types() -> Result<Value, String> {
    get("/filmgen/monitor/types")
}

/// `GET /filmgen/monitor/types/{mode_id}`：单类模板的 15 步流程与角色权重。
pub fn monitor_type(id: &str) -> Result<Value, String> {
    get(&format!("/filmgen/monitor/types/{}", enc(id)))
}

/// `GET /filmgen/monitor/board?project_id=&episode=&page=`：镜头状态板。
pub fn monitor_board(project: &str, episode: &str, page: u32) -> Result<Value, String> {
    let mut path = format!(
        "/filmgen/monitor/board?project_id={}&limit=24&page={}",
        enc(project),
        page.max(1)
    );
    if !episode.trim().is_empty() {
        path.push_str(&format!("&episode={}", enc(episode.trim())));
    }
    get(&path)
}

/// `GET /filmgen/monitor/progress?project_id=`：进度 / 预算 / 集。
pub fn monitor_progress(project: &str) -> Result<Value, String> {
    get(&format!(
        "/filmgen/monitor/progress?project_id={}",
        enc(project)
    ))
}

/// `GET /filmgen/monitor/continuity?project_id=`，`run` 时走 `POST .../run`。
pub fn monitor_continuity(project: &str, run: bool, episode: &str) -> Result<Value, String> {
    let mut path = format!(
        "/filmgen/monitor/continuity{}?project_id={}",
        if run { "/run" } else { "" },
        enc(project)
    );
    if !episode.trim().is_empty() {
        path.push_str(&format!("&episode={}", enc(episode.trim())));
    }
    if run {
        call("POST", &path, Some(&json!({})), LONG_READ)
    } else {
        get(&path)
    }
}

/// `GET /filmgen/monitor/retakes?project_id=`：重拍队列。
pub fn monitor_retakes(project: &str) -> Result<Value, String> {
    get(&format!("/filmgen/monitor/retakes?project_id={}", enc(project)))
}

/// `GET /filmgen/monitor/audit?project_id=&limit=`：审计日志。
pub fn monitor_audit(project: &str, limit: u32) -> Result<Value, String> {
    get(&format!(
        "/filmgen/monitor/audit?project_id={}&limit={}",
        enc(project),
        limit.clamp(1, 100)
    ))
}

/// `GET /filmgen/monitor/shot/{id}`：单镜详情（takes / notes / 连续性）。
pub fn monitor_shot(shot: &str) -> Result<Value, String> {
    get(&format!("/filmgen/monitor/shot/{}", enc(shot)))
}

/// `POST /filmgen/monitor/shot/{id}/take`：登记一条 take。
pub fn monitor_take(shot: &str, body: &Value) -> Result<Value, String> {
    post(&format!("/filmgen/monitor/shot/{}/take", enc(shot)), body)
}

/// `POST /filmgen/monitor/shot/{id}/take/{take}/decision`：接受 / 退回 / 待定。
pub fn monitor_decision(shot: &str, take: &str, decision: &str) -> Result<Value, String> {
    post(
        &format!(
            "/filmgen/monitor/shot/{}/take/{}/decision",
            enc(shot),
            enc(take)
        ),
        &json!({ "decision": decision }),
    )
}

/// `POST /filmgen/monitor/shot/{id}/notes`：加一条批注。
pub fn monitor_note(shot: &str, body: &Value) -> Result<Value, String> {
    post(&format!("/filmgen/monitor/shot/{}/notes", enc(shot)), body)
}

/// `PATCH /filmgen/monitor/note/{id}`：批注标记已解决。
pub fn monitor_note_resolve(note: &str) -> Result<Value, String> {
    call(
        "PATCH",
        &format!("/filmgen/monitor/note/{}", enc(note)),
        Some(&json!({ "resolved": true })),
        READ,
    )
}

/// `POST /filmgen/monitor/shot/{id}/retake`：一键重拍登记（只登记，由编排器消费）。
pub fn monitor_retake(shot: &str, body: &Value) -> Result<Value, String> {
    post(&format!("/filmgen/monitor/shot/{}/retake", enc(shot)), body)
}

/// `POST /filmgen/monitor/projects/{id}/shots/sync`：把分镜镜头导入监视器。
pub fn monitor_sync(project: &str, body: &Value) -> Result<Value, String> {
    post(
        &format!("/filmgen/monitor/projects/{}/shots/sync", enc(project)),
        body,
    )
}

// ─────────────────────────────────────────────────────────────────────
// 3. 发行中心 `/aidrama/*`（短剧 + 发行记录）
// ─────────────────────────────────────────────────────────────────────

/// `GET /aidrama/list?limit=&page=`：短剧列表。
pub fn dramas(limit: u32) -> Result<Value, String> {
    get(&format!("/aidrama/list?limit={}&page=1", limit.clamp(1, 100)))
}

/// `GET /aidrama/distributions?limit=&page=`：发行记录（含文件清单）。
pub fn distributions(limit: u32) -> Result<Value, String> {
    get(&format!(
        "/aidrama/distributions?limit={}&page=1",
        limit.clamp(1, 100)
    ))
}

/// `POST /aidrama/distribute`：按平台 / 语言打发行包（异步生成 zip）。
pub fn distribute(body: &Value) -> Result<Value, String> {
    post_read("/aidrama/distribute", body, PACK_READ)
}

/// `POST /aidrama/distributions/{id}/stats`：数据回填（播放 / 点赞 / 转化）。
pub fn distribution_stats(id: &str, body: &Value) -> Result<Value, String> {
    post(&format!("/aidrama/distributions/{}/stats", enc(id)), body)
}

// ─────────────────────────────────────────────────────────────────────
// 4. 授权护照 `/passport/*`
// ─────────────────────────────────────────────────────────────────────

/// `POST /passport/asset`：单素材护照（asset_id 或 asset_url 二选一）。
pub fn passport_asset(body: &Value) -> Result<Value, String> {
    post_read("/passport/asset", body, LONG_READ)
}

/// `POST /passport/project`：整项目批量护照（pack_id / project_id）。
pub fn passport_project(body: &Value) -> Result<Value, String> {
    post_read("/passport/project", body, LONG_READ)
}

/// `GET /passport/{id}`：读取护照。
pub fn passport_get(id: &str) -> Result<Value, String> {
    get(&format!("/passport/{}", enc(id)))
}

/// `GET /passport/verify/{id}?sha256=`：校验哈希链、签名与台账。
pub fn passport_verify(id: &str, sha256: &str) -> Result<Value, String> {
    let mut path = format!("/passport/verify/{}", enc(id));
    let sha = sha256.trim();
    if !sha.is_empty() {
        path.push_str(&format!("?sha256={}", enc(sha)));
    }
    get(&path)
}

/// `GET /passport/list?limit=`：最近的护照。
pub fn passport_list(limit: u32) -> Result<Value, String> {
    get(&format!("/passport/list?limit={}", limit.clamp(1, 50)))
}

/// `GET /passport/stats`：护照统计。
pub fn passport_stats() -> Result<Value, String> {
    get("/passport/stats")
}

/// `GET /passport/signing`：公钥与签名算法（独立验签用）。
pub fn passport_signing() -> Result<Value, String> {
    get("/passport/signing")
}

// ─────────────────────────────────────────────────────────────────────
// 5. 合规沙盘 `/sandbox/*`
// ─────────────────────────────────────────────────────────────────────

/// `POST /sandbox/check`：发布前合规彩排 / 整改复评（带 parent_check_id）。
pub fn sandbox_check(body: &Value) -> Result<Value, String> {
    post_read("/sandbox/check", body, LONG_READ)
}

/// `GET /sandbox/version`：规则库版本、分类、平台严度。
pub fn sandbox_version() -> Result<Value, String> {
    get("/sandbox/version")
}

/// `GET /sandbox/history?limit=`：复检历史。
pub fn sandbox_history(limit: u32) -> Result<Value, String> {
    get(&format!("/sandbox/history?limit={}", limit.clamp(1, 50)))
}

/// `GET /sandbox/history/{id}`：单次结果。
pub fn sandbox_history_one(id: &str) -> Result<Value, String> {
    get(&format!("/sandbox/history/{}", enc(id)))
}

// ─────────────────────────────────────────────────────────────────────
// 6. 插件市场 `/plugins/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /plugins/market?size=&sort=`：市场列表（官方 / 第三方、分类、认证）。
pub fn plugin_market(size: u32) -> Result<Value, String> {
    get(&format!(
        "/plugins/market?size={}&sort=downloads",
        size.clamp(1, 100)
    ))
}

/// `GET /plugins/installed`：已安装插件（含配置 schema 摘要）。
pub fn plugin_installed() -> Result<Value, String> {
    get("/plugins/installed")
}

/// `GET /plugins/{id}`：插件详情。
pub fn plugin_detail(id: &str) -> Result<Value, String> {
    get(&format!("/plugins/{}", enc(id)))
}

/// `GET /plugins/{id}/config`：配置与 config_schema。
pub fn plugin_config(id: &str) -> Result<Value, String> {
    get(&format!("/plugins/{}/config", enc(id)))
}

/// `PUT /plugins/{id}/config`：保存配置。
pub fn plugin_save_config(id: &str, config: Value) -> Result<Value, String> {
    call(
        "PUT",
        &endpoint(&format!("/plugins/{}/config", enc(id))),
        Some(&json!({ "config": config, "replace": true })),
        READ,
    )
}

/// `POST /plugins/install/{market_id}`：安装市场插件。
pub fn plugin_install(market_id: &str) -> Result<Value, String> {
    post(&format!("/plugins/install/{}", enc(market_id)), &json!({}))
}

/// `POST /plugins/{id}/enable` / `disable`。
pub fn plugin_set_enabled(id: &str, on: bool) -> Result<Value, String> {
    post(
        &format!("/plugins/{}/{}", enc(id), if on { "enable" } else { "disable" }),
        &json!({}),
    )
}

/// `POST /plugins/{id}/run`：在平台沙箱里跑一次插件。
pub fn plugin_run(id: &str, input: Value) -> Result<Value, String> {
    post_read(
        &format!("/plugins/{}/run", enc(id)),
        &json!({ "input": input }),
        LONG_READ,
    )
}

/// `GET /plugins/{id}/runs?size=`：运行历史。
pub fn plugin_runs(id: &str, size: u32) -> Result<Value, String> {
    get(&format!(
        "/plugins/{}/runs?size={}",
        enc(id),
        size.clamp(1, 50)
    ))
}

// ─────────────────────────────────────────────────────────────────────
// 7. 素材库：复用 `crate::assets`，这一层只给网页地址（见 web_url）。
// ─────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────
// 8. 模板市场 `/templates-market/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /templates-market/meta`：行业 / 场景 / 时长 / 热门模板。
pub fn template_meta() -> Result<Value, String> {
    get("/templates-market/meta")
}

/// `GET /templates-market/list?`：筛选列表。
pub fn template_list(query: &str) -> Result<Value, String> {
    let mut path = String::from("/templates-market/list?limit=60");
    let query = query.trim().trim_start_matches('?');
    if !query.is_empty() {
        path.push('&');
        path.push_str(query);
    }
    get(&path)
}

/// `GET /templates-market/detail/{id}`：模板详情（脚本 / 画面 / 配音设定）。
pub fn template_detail(id: &str) -> Result<Value, String> {
    get(&format!("/templates-market/detail/{}", enc(id)))
}

/// `POST /templates-market/favorite`：收藏 / 取消收藏。
pub fn template_favorite(id: &str, on: bool) -> Result<Value, String> {
    post(
        "/templates-market/favorite",
        &json!({ "template_id": id, "action": if on { "add" } else { "remove" } }),
    )
}

/// `POST /templates-market/use`：一键套用生成（异步渲染任务）。
pub fn template_use(body: &Value) -> Result<Value, String> {
    post_read("/templates-market/use", body, PACK_READ)
}

/// `GET /templates-market/task/{id}`：生成任务进度 / 成片 / 工程包。
pub fn template_task(id: &str) -> Result<Value, String> {
    get(&format!("/templates-market/task/{}", enc(id)))
}

/// `GET /movie/tasks?limit=`：电影成片任务（统一任务中心合并显示）。
pub fn movie_tasks(limit: u32) -> Result<Value, String> {
    get(&format!("/movie/tasks?limit={}", limit.clamp(1, 50)))
}

// ─────────────────────────────────────────────────────────────────────
// 9. 统一任务中心：本机记录（任务从哪来、跑到哪、花了多少）
// ─────────────────────────────────────────────────────────────────────

/// 一条本机任务记录。`kind` 决定怎么轮询远端：
/// `director` / `template` / `none`（一步完成的记录，只留痕）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LocalTask {
    #[serde(default)]
    pub id: String,
    /// 模块名：导演编排器 / 模板市场 / 发行中心 / 导演监视器 / 插件市场 / 授权护照 / 合规沙盘。
    #[serde(default)]
    pub module: String,
    /// 标题（人可读）。
    #[serde(default)]
    pub title: String,
    /// 轮询种类：director / template / none。
    #[serde(default)]
    pub kind: String,
    /// 远端任务 id。
    #[serde(default)]
    pub remote: String,
    /// 状态原文（中文优先）。
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub cost: f64,
    /// 一句话补充（错误 / 产物名 / 平台提示）。
    #[serde(default)]
    pub detail: String,
    /// 本地文件（成片 / zip / 工程包）。
    #[serde(default)]
    pub file: String,
    /// 远端地址（成片 / 下载）。
    #[serde(default)]
    pub url: String,
    /// 创建 / 更新（秒，Unix 时间）。
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub updated: i64,
}

impl LocalTask {
    /// 现在（Unix 秒）。
    pub fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// 任务跑完了没有（完成 / 失败 / 取消都不再轮询）。
    pub fn finished(&self) -> bool {
        matches!(
            self.status.as_str(),
            "完成" | "失败" | "部分完成" | "预算中止" | "已取消" | "done" | "failed"
        ) || self.progress >= 100.0
    }
}

/// 本地任务记录文件：`%APPDATA%\app.concat.editor\pro-tasks.json`。
pub fn tasks_path(config: &Path) -> PathBuf {
    config.join("pro-tasks.json")
}

/// 读本机任务（文件坏了就当空表，不拦住面板）。
pub fn load_tasks(config: &Path) -> Vec<LocalTask> {
    let path = tasks_path(config);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str::<Vec<LocalTask>>(&text).unwrap_or_default()
}

/// 写回本机任务。
pub fn save_tasks(config: &Path, tasks: &[LocalTask]) -> Result<(), String> {
    let path = tasks_path(config);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("建不了任务目录：{error}"))?;
    }
    let text = serde_json::to_string_pretty(tasks).map_err(|error| format!("任务记录编码失败：{error}"))?;
    std::fs::write(&path, text).map_err(|error| format!("任务记录写不下：{error}"))
}

/// 二进制下载（成片 / 发行 zip / 护照 zip）：走登录同一条 OpenSSL 直连。
pub fn download(url: &str, dest: &Path, cancel: &AtomicBool) -> Result<u64, String> {
    let url = absolute_url(url);
    if url.trim().is_empty() {
        return Err("没有可下载的地址".to_owned());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("建不了下载目录：{error}"))?;
    }
    let token = token();
    crate::account::api_download(&url, token.as_deref(), dest, PACK_READ, cancel)
        .map_err(|error| format!("下载失败：{error}"))
}

/// 把取消标志置位（幂等）。
pub fn cancel(flag: &AtomicBool) {
    flag.store(true, Ordering::Relaxed);
}

/// 任务的短 id（日志用）。
pub fn short_id(id: &str) -> String {
    id.chars().take(20).collect()
}

/// 一句话把平台任务状态翻成中文（面板不直接显示英文状态）。
pub fn status_text(status: &str, stage: &str) -> String {
    if !stage.trim().is_empty() {
        return stage.trim().to_owned();
    }
    match status {
        "queued" => "排队中".to_owned(),
        "running" => "生成中".to_owned(),
        "done" => "完成".to_owned(),
        "failed" => "失败".to_owned(),
        "partial" => "部分完成".to_owned(),
        "aborted_budget" => "预算中止".to_owned(),
        "interrupted" => "服务重启中断（可续跑）".to_owned(),
        "cancelled" => "已取消".to_owned(),
        other => other.to_owned(),
    }
}
