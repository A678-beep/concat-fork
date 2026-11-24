// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 影视专业二期（0.4.0）：片头片尾 / 实拍装配 / 特效 / 配乐 / 配音 /
//! 连贯性 / 风格匹配 / 素材质量。和 [`crate::pro`] 一样，这一层只发请求、
//! 拆 JSON；面板状态在 `concat/src/pro2.rs`。
//!
//! 接口全部是已上线的平台接口（本地未起、本仓库未改服务端）：
//! `/api/credits/titles/*`、`/api/assembly/*`、`/api/vfx/*`、
//! `/api/music/*`、`/api/voice/*`、`/api/continuity/*`、`/api/style/*`，
//! 素材质量复用 [`crate::assets::search`]。

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde_json::Value;

use crate::pro::{absolute_url, call, download, enc};

/// 短请求的读等待。
const READ: Duration = Duration::from_secs(40);
/// 规划 / 分析 / 描述类请求的读等待（大模型与逐帧统计都在这一档）。
const LONG_READ: Duration = Duration::from_secs(260);
/// 渲染 / 导出类请求的读等待（平台 2C2G 串行队列）。
const PACK_READ: Duration = Duration::from_secs(320);

fn get(path: &str) -> Result<Value, String> {
    call("GET", path, None, READ)
}

fn post(path: &str, body: &Value, read: Duration) -> Result<Value, String> {
    call("POST", path, Some(body), read)
}

fn get_long(path: &str) -> Result<Value, String> {
    call("GET", path, None, LONG_READ)
}

// ─────────────────────────────────────────────────────────────────────
// 1. 片头片尾 `/credits/titles/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /credits/titles/health`：引擎版本、字体、队列。
pub fn credits_health() -> Result<Value, String> {
    get("/credits/titles/health")
}

/// `GET /credits/titles/templates`：6 种片头 / 片尾风格模板。
pub fn credits_templates() -> Result<Value, String> {
    get("/credits/titles/templates")
}

/// `GET /credits/titles/demo`：示例项目（20 主演 + 6 部门 crew）。
pub fn credits_demo() -> Result<Value, String> {
    get("/credits/titles/demo")
}

/// `GET /credits/titles/projects?limit=`：已保存的片头片尾项目。
pub fn credits_projects(limit: u32) -> Result<Value, String> {
    get(&format!(
        "/credits/titles/projects?limit={}",
        limit.clamp(1, 50)
    ))
}

/// `POST /credits/titles/preview`：同步出预览帧 PNG（5 帧左右）。
pub fn credits_preview(body: &Value) -> Result<Value, String> {
    post("/credits/titles/preview", body, PACK_READ)
}

/// `POST /credits/titles/render`：异步渲染（片头 / 片尾 / 多分辨率）。
pub fn credits_render(body: &Value) -> Result<Value, String> {
    post("/credits/titles/render", body, READ)
}

/// `GET /credits/titles/task/{id}`：渲染进度 / 产物。
pub fn credits_task(id: &str) -> Result<Value, String> {
    get(&format!("/credits/titles/task/{}", enc(id)))
}

/// `GET /credits/titles/qc/{pid}`：排版 QC（字体缺字 / 安全区 / 溢出）。
pub fn credits_qc(project: &str) -> Result<Value, String> {
    get(&format!("/credits/titles/qc/{}", enc(project)))
}

/// 导出 zip 的地址（服务端直接回文件，下载走 [`download`]）。
pub fn credits_export_url(project: &str) -> String {
    absolute_url(&format!("/credits/export/{}", enc(project)))
}

// ─────────────────────────────────────────────────────────────────────
// 2. 实拍装配 `/assembly/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /assembly/health`：自检、依赖、串行 worker 状态。
pub fn assembly_health() -> Result<Value, String> {
    get("/assembly/health")
}

/// `GET /assembly/list`：最近的素材批次 / EDL / 成片。
pub fn assembly_list() -> Result<Value, String> {
    get("/assembly/list")
}

/// `POST /assembly/ingest`：按 URL 登记素材（桌面不传 multipart，走 JSON）。
pub fn assembly_ingest(body: &Value) -> Result<Value, String> {
    post("/assembly/ingest", body, LONG_READ)
}

/// `POST /assembly/plan`：剧本 + 素材 → EDL / 缺口 / 置信度。
pub fn assembly_plan(body: &Value) -> Result<Value, String> {
    post("/assembly/plan", body, LONG_READ)
}

/// `POST /assembly/render`：按 EDL 渲染粗剪 + 工程包。
pub fn assembly_render(body: &Value) -> Result<Value, String> {
    post("/assembly/render", body, LONG_READ)
}

/// `POST /assembly/apply`：人工替换 / 调整入出点 / 确认。
pub fn assembly_apply(body: &Value) -> Result<Value, String> {
    post("/assembly/apply", body, LONG_READ)
}

/// `GET /assembly/task/{id}`：任务进度。
pub fn assembly_task(id: &str) -> Result<Value, String> {
    get(&format!("/assembly/task/{}", enc(id)))
}

/// `GET /assembly/plan/{id}`：EDL / 缺口 / 置信度。
pub fn assembly_plan_of(id: &str) -> Result<Value, String> {
    get(&format!("/assembly/plan/{}", enc(id)))
}

/// `GET /assembly/assets/{id}`：素材镜头表。
pub fn assembly_assets(id: &str) -> Result<Value, String> {
    get(&format!("/assembly/assets/{}", enc(id)))
}

/// `GET /assembly/render/{id}`：成片 / 工程包。
pub fn assembly_render_of(id: &str) -> Result<Value, String> {
    get(&format!("/assembly/render/{}", enc(id)))
}

// ─────────────────────────────────────────────────────────────────────
// 3. 特效 `/vfx/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /vfx/library`：14 个特效 + 参数 schema + 预设。
pub fn vfx_library() -> Result<Value, String> {
    get("/vfx/library")
}

/// `POST /vfx/render`：异步渲染一段特效叠加素材。
pub fn vfx_render(body: &Value) -> Result<Value, String> {
    post("/vfx/render", body, READ)
}

/// `GET /vfx/task/{id}`：渲染进度 / 产物 URL。
pub fn vfx_task(id: &str) -> Result<Value, String> {
    get(&format!("/vfx/task/{}", enc(id)))
}

// ─────────────────────────────────────────────────────────────────────
// 4. 配乐 `/music/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /music/library`：28 风格 + 50 预设 + 168 成品 cue / 12 长音床索引。
pub fn music_library() -> Result<Value, String> {
    get("/music/library")
}

/// `POST /music/score`：场景时间轴 + 情绪点 → 配乐时间线 + 生成任务。
pub fn music_score(body: &Value) -> Result<Value, String> {
    post("/music/score", body, READ)
}

/// `GET /music/task/{id}`：任务状态 / 产物 / cue sheet。
pub fn music_task(id: &str) -> Result<Value, String> {
    get(&format!("/music/task/{}", enc(id)))
}

/// `POST /music/render`：按风格 / 时长直接生成一条配乐。
pub fn music_render(body: &Value) -> Result<Value, String> {
    post("/music/render", body, READ)
}

// ─────────────────────────────────────────────────────────────────────
// 5. 配音 `/voice/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /voice/roles`：40 个预置角色音色（年龄 / 性别 / 情感支持 / 试听）。
pub fn voice_roles() -> Result<Value, String> {
    get("/voice/roles")
}

/// `POST /voice/advise`：配音导演建议（逐句情感 / 强度 / 停顿）。
pub fn voice_advise(body: &Value) -> Result<Value, String> {
    post("/voice/advise", body, LONG_READ)
}

/// `POST /voice/dub`：逐句情感配音（异步任务）。
pub fn voice_dub(body: &Value) -> Result<Value, String> {
    post("/voice/dub", body, READ)
}

/// `GET /voice/task/{id}`：配音任务进度 / 产物。
pub fn voice_task(id: &str) -> Result<Value, String> {
    get(&format!("/voice/task/{}", enc(id)))
}

// ─────────────────────────────────────────────────────────────────────
// 6. 连贯性 `/continuity/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /continuity/health`：模型 / 视觉 / 限制。
pub fn continuity_health() -> Result<Value, String> {
    get("/continuity/health")
}

/// `GET /continuity/list?limit=`：最近检查。
pub fn continuity_list(limit: u32) -> Result<Value, String> {
    get(&format!(
        "/continuity/list?limit={}",
        limit.clamp(1, 50)
    ))
}

/// `POST /continuity/audit`：一键全面检查（成片真跑，几十秒）。
pub fn continuity_audit(body: &Value) -> Result<Value, String> {
    post("/continuity/audit", body, PACK_READ)
}

/// `GET /continuity/run/{id}`：单次检查的完整 JSON。
pub fn continuity_run(id: &str) -> Result<Value, String> {
    get(&format!("/continuity/run/{}", enc(id)))
}

/// HTML 报告的公开地址。
pub fn continuity_report_url(run: &str) -> String {
    absolute_url(&format!("/continuity/report/{}.html", enc(run)))
}

// ─────────────────────────────────────────────────────────────────────
// 7. 风格匹配 `/style/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /style/list?limit=`：已存风格指纹。
pub fn style_list(limit: u32) -> Result<Value, String> {
    get(&format!("/style/list?limit={}", limit.clamp(1, 50)))
}

/// `POST /style/analyze`：参考片 → 风格指纹（节奏 / 运动 / 色彩 / 质感）。
pub fn style_analyze(body: &Value) -> Result<Value, String> {
    post("/style/analyze", body, PACK_READ)
}

/// `POST /style/apply-color`：把指纹的色彩应用到目标片（同步，几十秒）。
pub fn style_apply_color(body: &Value) -> Result<Value, String> {
    post("/style/apply-color", body, PACK_READ)
}

/// `POST /style/lut`：导出 .cube LUT。
pub fn style_lut(body: &Value) -> Result<Value, String> {
    post("/style/lut", body, LONG_READ)
}

/// `GET /style/{id}`：单个指纹详情。
pub fn style_get(id: &str) -> Result<Value, String> {
    get(&format!("/style/{}", enc(id)))
}

/// `GET /style/health`：引擎状态。
pub fn style_health() -> Result<Value, String> {
    get("/style/health")
}

// ─────────────────────────────────────────────────────────────────────
// 8. 素材质量 `/assets/search?quality=`
// ─────────────────────────────────────────────────────────────────────

/// 带质量筛选的素材搜索（`cinema_candidate` / `commercial_hd` / 空=全部）。
pub fn assets_quality(quality: &str, page: u32, limit: u32) -> Result<Value, String> {
    let mut path = format!(
        "/assets/search?page={}&limit={}",
        page.max(1),
        limit.clamp(1, 60)
    );
    if !quality.trim().is_empty() {
        path.push_str("&quality=");
        path.push_str(&crate::pro::enc(quality.trim()));
    }
    get(&path)
}

/// 任何公开文件的下载地址（成片 / 工程包 / LUT / 试听）。
pub fn file_url(path: &str) -> String {
    absolute_url(path)
}

/// 下载到本地（成片 / zip / LUT / 音频），与 0.3.7 同一实现。
pub fn fetch(url: &str, dest: &Path, cancel: &AtomicBool) -> Result<u64, String> {
    download(url, dest, cancel)
}
