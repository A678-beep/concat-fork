// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 影视专业三期（0.5.0）：把 0.5.0 已上线的八个独立服务接到桌面。
//!
//! 接口全部是已上线的平台接口（本仓库不改服务端、不重启生产）：
//! `/prompt/*`（提示词引擎 8818）、`/stylelib/*`（风格库 8817）、
//! `/index/*`（项目资产文件库 8821）、`/aesthetic/*`（AI 审美总监 8822）、
//! `/reverse/*`（视频反推 8823）、`/heal/*`（自愈剪辑 8816）、
//! `/multicam/*`（补机位 / 二次构图 8814）、`/brain/*` 与 `/canvas/*`
//! （导演脑 8815 / 节点画布 8820）。
//!
//! 和 [`crate::pro2`] 一样，这一层只发请求、拆 JSON；面板状态在
//! `concat/src/pro3.rs`。经济模式：只走读接口与免费分析接口，不调付费
//! 视频生成；`multicam_ai_status` 只探余额 / 配置，不做付费生成。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::pro::{absolute_url, call, enc};

/// 短请求的读等待。
const READ: Duration = Duration::from_secs(40);
/// 分析 / 构建类（提示词生成、风格分析、审美分析）。
const LONG_READ: Duration = Duration::from_secs(200);
/// 视频分析 / 渲染类（反推、自愈、二次构图、报告）。
const PACK_READ: Duration = Duration::from_secs(360);

fn get(path: &str) -> Result<Value, String> {
    call("GET", path, None, READ)
}

fn get_long(path: &str) -> Result<Value, String> {
    call("GET", path, None, LONG_READ)
}

fn post(path: &str, body: &Value, read: Duration) -> Result<Value, String> {
    call("POST", path, Some(body), read)
}

// ─────────────────────────────────────────────────────────────────────
// 1. 电影级提示词引擎 `/prompt/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /prompt/health`：引擎版本、模板 / 模型 / 风格数量。
pub fn prompt_health() -> Result<Value, String> {
    get("/prompt/health")
}

/// `GET /prompt/schema`：12 要素字段表（镜头 / 主体 / 服装 …）。
pub fn prompt_schema() -> Result<Value, String> {
    get("/prompt/schema")
}

/// `GET /prompt/templates`：场景模板（都市夜 / 古装宫廷 …）。
pub fn prompt_templates() -> Result<Value, String> {
    get("/prompt/templates")
}

/// `GET /prompt/models`：6 个模型模板（Seedance / Kling / Veo / Wan / Flux）。
pub fn prompt_models() -> Result<Value, String> {
    get("/prompt/models")
}

/// `POST /prompt/build`：表单 → 12 要素 + 中英提示词 + 成本估算（免费）。
pub fn prompt_build(body: &Value) -> Result<Value, String> {
    post("/prompt/build", body, LONG_READ)
}

/// `POST /prompt/estimate`：只估成本（免费）。
pub fn prompt_estimate(body: &Value) -> Result<Value, String> {
    post("/prompt/estimate", body, READ)
}

// ─────────────────────────────────────────────────────────────────────
// 2. 导演风格库 + 构图辅助 `/stylelib/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /stylelib/health`：20 个风格档案与构图辅助线类型。
pub fn stylelib_health() -> Result<Value, String> {
    get("/stylelib/health")
}

/// `GET /stylelib/list`：20 个导演风格卡片（支持 ?q= 关键字）。
pub fn stylelib_list(query: &str) -> Result<Value, String> {
    let mut path = String::from("/stylelib/list");
    let query = query.trim();
    if !query.is_empty() {
        path.push_str("?q=");
        path.push_str(&enc(query));
    }
    get(&path)
}

/// `GET /stylelib/{style_id}`：单个风格详情（色彩 / 构图 / 推荐辅助线）。
pub fn stylelib_style(id: &str) -> Result<Value, String> {
    get(&format!("/stylelib/{}", enc(id)))
}

/// `POST /stylelib/composition-guide`：参数化构图辅助线（返回 SVG 几何）。
pub fn stylelib_composition_guide(body: &Value) -> Result<Value, String> {
    post("/stylelib/composition-guide", body, LONG_READ)
}

/// 构图辅助线 PNG 地址（浏览器 / 面板预览用）。
pub fn stylelib_guide_png_url(style_id: &str, guide: &str, width: u32, ratio: &str) -> String {
    let mut url = format!(
        "/stylelib/guide/{}.png?width={}&ratio={}",
        enc(guide),
        width.max(320),
        enc(ratio)
    );
    if !style_id.trim().is_empty() {
        url.push_str("&style_id=");
        url.push_str(&enc(style_id.trim()));
    }
    absolute_url(&url)
}

/// 风格库 LUT `.cube` 下载地址。
pub fn stylelib_lut_url(style_id: &str) -> String {
    absolute_url(&format!("/stylelib/lut/{}.cube", enc(style_id)))
}

/// 风格匹配指纹（喂给 0.3.7 风格匹配面板用）。
pub fn stylelib_match_profile_url(style_id: &str) -> String {
    absolute_url(&format!(
        "/stylelib/style-match-profile/{}",
        enc(style_id)
    ))
}

// ─────────────────────────────────────────────────────────────────────
// 3. 项目资产文件库 `/index/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /index/health`：条目 / 版本 / 依赖计数。
pub fn index_health() -> Result<Value, String> {
    get("/index/health")
}

/// `GET /index/projects`：项目列表。
pub fn index_projects() -> Result<Value, String> {
    get("/index/projects")
}

/// `GET /index/tree?project_id=`：项目 → 五阶段 → 类型树。
pub fn index_tree(project: &str) -> Result<Value, String> {
    if project.trim().is_empty() {
        get("/index/tree")
    } else {
        get(&format!("/index/tree?project_id={}", enc(project.trim())))
    }
}

/// `GET /index/search?q=`：全库搜索（可带 project / type / status）。
pub fn index_search(query: &str, project: &str, kind: &str, status: &str) -> Result<Value, String> {
    let mut path = format!("/index/search?q={}", enc(query.trim()));
    if !project.trim().is_empty() {
        path.push_str(&format!("&project_id={}", enc(project.trim())));
    }
    if !kind.trim().is_empty() {
        path.push_str(&format!("&type={}", enc(kind.trim())));
    }
    if !status.trim().is_empty() {
        path.push_str(&format!("&status={}", enc(status.trim())));
    }
    get(&path)
}

/// `GET /index/item/{id}`：条目详情（预览 / 版本 / 依赖 / 影响）。
pub fn index_item(id: &str) -> Result<Value, String> {
    get(&format!("/index/item/{}", enc(id)))
}

/// `GET /index/item/{id}/versions`：版本列表。
pub fn index_versions(id: &str) -> Result<Value, String> {
    get(&format!("/index/item/{}/versions", enc(id)))
}

/// `GET /index/item/{id}/impact`：影响分析（改这个会动到哪些下游）。
pub fn index_impact(id: &str) -> Result<Value, String> {
    get(&format!("/index/item/{}/impact", enc(id)))
}

/// `GET /index/export/{project_id}`：资产清单 HTML 报告地址。
pub fn index_export_url(project: &str) -> String {
    absolute_url(&format!("/index/export/{}", enc(project.trim())))
}

// ─────────────────────────────────────────────────────────────────────
// 4. AI 审美总监 `/aesthetic/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /aesthetic/health`：引擎 / ffmpeg / 样本数量。
pub fn aesthetic_health() -> Result<Value, String> {
    get("/aesthetic/health")
}

/// `GET /aesthetic/samples`：内置样片（夜行 1080p / 9:16）。
pub fn aesthetic_samples() -> Result<Value, String> {
    get("/aesthetic/samples")
}

/// `POST /aesthetic/analyze`：选片 → 分数 + 分项问题（同步，约 10–60s）。
pub fn aesthetic_analyze(body: &Value) -> Result<Value, String> {
    post("/aesthetic/analyze", body, PACK_READ)
}

/// `GET /aesthetic/report/{id}`：HTML 报告地址（浏览器打开）。
pub fn aesthetic_report_url(id: &str) -> String {
    absolute_url(&format!("/aesthetic/report/{}", id))
}

/// `GET /aesthetic/result/{id}`：结果 JSON（报告里也有一份）。
pub fn aesthetic_result(id: &str) -> Result<Value, String> {
    get(&format!("/aesthetic/result/{}", enc(id)))
}

// ─────────────────────────────────────────────────────────────────────
// 5. 视频反推提示词 `/reverse/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /reverse/health`：本地 ASR / ffmpeg / 队列能力。
pub fn reverse_health() -> Result<Value, String> {
    get("/reverse/health")
}

/// `GET /reverse/samples`：内置反推样片。
pub fn reverse_samples() -> Result<Value, String> {
    get("/reverse/samples")
}

/// `POST /reverse/analyze`：提交反推任务（异步，返回 job_id）。
/// 支持 `video_url` 或 `sample_id`；桌面把本地文件先上传换 URL。
pub fn reverse_analyze(body: &Value) -> Result<Value, String> {
    post("/reverse/analyze", body, READ)
}

/// `GET /reverse/job/{id}`：任务进度 / 完成后带 result（分镜 + 提示词）。
pub fn reverse_job(id: &str) -> Result<Value, String> {
    call("GET", &format!("/reverse/job/{}", enc(id)), None, PACK_READ)
}

/// `GET /reverse/report/{id}`：HTML 报告地址。
pub fn reverse_report_url(id: &str) -> String {
    absolute_url(&format!("/reverse/report/{}", id))
}

/// `POST /reverse/recreate`：复刻方案（调 0 费用编排，不生成视频）。
pub fn reverse_recreate(job: &str) -> Result<Value, String> {
    post("/reverse/recreate", &serde_json::json!({ "reverse_id": job }), LONG_READ)
}

// ─────────────────────────────────────────────────────────────────────
// 6. 自愈剪辑 `/heal/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /heal/health`：检测器 / 内存 / 队列状态。
pub fn heal_health() -> Result<Value, String> {
    get("/heal/health")
}

/// `POST /heal/analyze`：成片 → 问题清单（同步，约 20–60s）。
pub fn heal_analyze(body: &Value) -> Result<Value, String> {
    post("/heal/analyze", body, PACK_READ)
}

/// `POST /heal/plan`：问题 → 修复方案（自动修 / 重生成 / 补拍）。
pub fn heal_plan(run: &str) -> Result<Value, String> {
    post("/heal/plan", &serde_json::json!({ "run_id": run }), LONG_READ)
}

/// `POST /heal/reshoot-list`：补拍清单（JSON）。
pub fn heal_reshoot_list(run: &str) -> Result<Value, String> {
    post(
        "/heal/reshoot-list",
        &serde_json::json!({ "run_id": run, "format": "json" }),
        LONG_READ,
    )
}

/// `GET /heal/report/{run}`：HTML 自愈报告。
pub fn heal_report_url(run: &str) -> String {
    absolute_url(&format!("/heal/report/{}", run))
}

/// `GET /heal/reshoot/{run}.html`：HTML 补拍清单。
pub fn heal_reshoot_url(run: &str) -> String {
    absolute_url(&format!("/heal/reshoot/{}.html", run))
}

// ─────────────────────────────────────────────────────────────────────
// 7. 补机位 / 二次构图 `/multicam/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /multicam/health`：7 种二次构图模式与生成能力。
pub fn multicam_health() -> Result<Value, String> {
    get("/multicam/health")
}

/// `GET /multicam/modes`：7 种模式 + 6 个目标机位。
pub fn multicam_modes() -> Result<Value, String> {
    get("/multicam/modes")
}

/// `POST /multicam/reframe`：二次构图（真实 ffmpeg 渲染，免费）。
pub fn multicam_reframe(body: &Value) -> Result<Value, String> {
    post("/multicam/reframe", body, PACK_READ)
}

/// `POST /multicam/suggest`：按镜头表建议补哪些机位（免费）。
pub fn multicam_suggest(body: &Value) -> Result<Value, String> {
    post("/multicam/suggest", body, LONG_READ)
}

/// `GET /multicam/providers?balance=1`：只读探测生成通道与余额，不生成。
pub fn multicam_providers() -> Result<Value, String> {
    get("/multicam/providers?balance=1")
}

/// `GET /multicam/tasks?limit=`：最近的补机位任务。
pub fn multicam_tasks(limit: u32) -> Result<Value, String> {
    get(&format!("/multicam/tasks?limit={}", limit.clamp(1, 30)))
}

/// 二次构图产物的下载地址（服务端相对地址换完整 URL）。
pub fn multicam_media_url(url: &str) -> String {
    absolute_url(url)
}

// ─────────────────────────────────────────────────────────────────────
// 8. 导演脑 `/brain/*` 与节点画布 `/canvas/*`
// ─────────────────────────────────────────────────────────────────────

/// `GET /brain/health`：导演脑引擎状态。
pub fn brain_health() -> Result<Value, String> {
    get("/brain/health")
}

/// `GET /brain/styles`：内置 3 个导演风格档案。
pub fn brain_styles() -> Result<Value, String> {
    get("/brain/styles")
}

/// `GET /canvas/health`：节点画布引擎 / 节点类型 / 模板数量。
pub fn canvas_health() -> Result<Value, String> {
    get("/canvas/health")
}

/// `GET /canvas/templates`：内置画布模板。
pub fn canvas_templates() -> Result<Value, String> {
    get("/canvas/templates")
}

// ─────────────────────────────────────────────────────────────────────
// 本地文件上传：`POST /upload`（multipart，平台公开接口）
// ─────────────────────────────────────────────────────────────────────

/// 上传一个本地视频 / 图片，换平台地址（给反推 / 审美 / 自愈 / 补机位用）。
///
/// 与 `ai_studio::upload` 同一个平台接口，但**不要求登录**：上传本身是公开
/// 的（返回 24 小时有效的 `studio/files/uploads/` 地址），需要登录的是后面
/// 的生成类接口。读等待给足，200MB 以内。
pub fn upload(
    path: &Path,
    progress: &mut dyn FnMut(f32, String),
    cancel: &AtomicBool,
) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|error| format!("本地文件读不出来：{error}"))?;
    if data.len() > 200 * 1024 * 1024 {
        return Err("文件超过 200MB，请先裁剪再试".to_owned());
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "upload.mp4".to_owned());
    let mime = match path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("m4v") => "video/x-m4v",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => "video/mp4",
    };
    let boundary = format!(
        "----XingLaiPro3{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let mut body = Vec::with_capacity(data.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    progress(
        0.1,
        format!("上传到星籁（{:.1} MB）…", data.len() as f64 / (1024.0 * 1024.0)),
    );
    if cancel.load(Ordering::Relaxed) {
        return Err("已取消".to_owned());
    }
    let token = crate::pro::token();
    let url = absolute_url("/upload");
    let (status, reply) = crate::account::api_call(
        "POST",
        &url,
        &format!("multipart/form-data; boundary={boundary}"),
        &body,
        token.as_deref(),
        Duration::from_secs(300),
    )
    .map_err(|error| format!("连不上星籁平台：{error}"))?;
    let value = serde_json::from_str::<Value>(&reply).unwrap_or_else(|_| {
        Value::Object(
            [(
                "hint".to_owned(),
                Value::String(reply.chars().take(200).collect()),
            )]
            .into_iter()
            .collect(),
        )
    });
    if !(200..300).contains(&status) {
        return Err(format!(
            "上传失败（HTTP {status}）：{}",
            crate::pro::api_hint(&value)
        ));
    }
    let url = value
        .get("url")
        .or_else(|| value.get("file_url"))
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| "上传成功，但服务器没有给出文件地址".to_owned())?;
    progress(1.0, "上传完成".to_owned());
    Ok(absolute_url(url))
}

/// 公开地址（HTML 报告 / 成片 / 截图）换完整 URL。
pub fn file_url(path: &str) -> String {
    absolute_url(path)
}
