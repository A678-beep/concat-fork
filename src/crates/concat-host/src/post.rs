// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 专业后期底座（0.3.6）：平台 `/api/post/*` 与本地兜底。
//!
//! 面向影视 / 短剧公司交付：EDL（CMX3600）、OpenTimelineIO、交付 QC、
//! 媒体校验清单、LUT / CDL 与一键交付包。网络这一层复用登录同一条
//! OpenSSL 直连（见 [`crate::account::api_call`]），平台不可用时由窗口
//! 那一半落成本地文件：
//!
//! - EDL / OTIO 在本地按工程文档直接写，任何窗口都能产出；
//! - QC 用随包的 ffmpeg/ffprobe 单遍解码跑 ebur128 / blackdetect /
//!   silencedetect / freezedetect，输出与平台 `qc_report.json` 同形的
//!   JSON（status / summary / issues），时间码可跳转；
//! - 交付包是一个 zip：project.json + EDL + OTIO + QC + 媒体清单 +
//!   LUT/CDL 侧车 + README + checksums.sha256。
//!
//! 本模块只做「取数、算数、写文件」，不做任何编辑决定。

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use concat_project::model::{ClipKind, MediaItem, Project, Timeline};

/// 星籁平台网页入口。
pub const SITE: &str = "https://xinglaiai.com";

/// QC 目标的默认阈值：与平台 EBU R128 默认一致。
pub const TARGET_LUFS: f64 = -23.0;
/// 响度容差，LU。
pub const LOUDNESS_TOLERANCE: f64 = 1.0;
/// 真峰上限，dBTP。
pub const TRUE_PEAK_MAX: f64 = -1.0;
/// 黑帧最短时长，秒。
pub const BLACK_MIN: f64 = 0.2;
/// 静音最短时长，秒。
pub const SILENCE_MIN: f64 = 2.0;
/// 冻结帧最短时长，秒。
pub const FREEZE_MIN: f64 = 0.5;

fn endpoint(path: &str) -> String {
    let base = std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::account::API_BASE.to_owned());
    format!("{base}{path}")
}

/// 一次 JSON 调用。平台不通时返回中文原因，调用方决定本地兜底。
pub fn call_json(method: &str, path: &str, body: Option<&Value>, read: Duration) -> Result<Value, String> {
    let token = crate::movie::token();
    let payload = match body {
        Some(value) => serde_json::to_vec(value).map_err(|error| format!("请求编码失败：{error}"))?,
        None => Vec::new(),
    };
    let (status, text) = crate::account::api_call(
        method,
        &endpoint(path),
        "application/json",
        &payload,
        token.as_deref(),
        read,
    )
    .map_err(|error| format!("连不上星籁后期接口（{path}）：{error}"))?;
    let value = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| {
        json!({"ok": false, "hint": text.chars().take(200).collect::<String>()})
    });
    if !(200..300).contains(&status) {
        return Err(format!(
            "平台拒绝了这次后期请求（HTTP {status}）：{}",
            api_hint(&value)
        ));
    }
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(format!("平台后期接口返回失败：{}", api_hint(&value)));
    }
    Ok(value)
}

fn api_hint(value: &Value) -> String {
    value
        .get("hint")
        .and_then(Value::as_str)
        .or_else(|| value.get("error_cn").and_then(Value::as_str))
        .or_else(|| value.get("error").and_then(Value::as_str))
        .map(str::to_owned)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "没有给出原因".to_owned())
}

/// 平台返回的相对下载地址换成完整 URL（nginx 前缀 `/ai-nav`）。
pub fn absolute_url(path: &str) -> String {
    let path = path.trim();
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_owned();
    }
    if path.starts_with("/api/") {
        return format!("{SITE}/ai-nav{path}");
    }
    if path.starts_with('/') {
        return format!("{SITE}{path}");
    }
    format!("{SITE}/{path}")
}

/// `GET /api/post/health`：平台后期模块在不在。
pub fn health() -> Result<Value, String> {
    call_json("GET", "/post/health", None, Duration::from_secs(12))
}

/// `GET /api/post/standards`：色域 / 帧率 / 编码 / QC 阈值。
pub fn standards() -> Result<Value, String> {
    call_json("GET", "/post/standards", None, Duration::from_secs(15))
}

/// `POST /api/post/edl`：平台生成 CMX3600。
pub fn edl(project: &Value) -> Result<Value, String> {
    call_json("POST", "/post/edl", Some(project), Duration::from_secs(60))
}

/// `POST /api/post/otio`：平台生成 OTIO。
pub fn otio(project: &Value) -> Result<Value, String> {
    call_json("POST", "/post/otio", Some(project), Duration::from_secs(60))
}

/// `POST /api/post/handoff`：平台打交付包，返回 zip 的下载地址。
pub fn handoff(project: &Value) -> Result<Value, String> {
    call_json("POST", "/post/handoff", Some(project), Duration::from_secs(180))
}

/// `POST /api/post/qc` 的多媒体上传：把本地文件交给平台质检。
pub fn qc_upload(path: &Path, fps: f64) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("读不了质检文件：{error}"))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "media".to_owned());
    let boundary = format!(
        "----xinglai{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let mut body = Vec::with_capacity(bytes.len() + 1024);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"project_fps\"\r\n\r\n{fps:.3}\
             \r\n--{boundary}\r\nContent-Disposition: form-data; name=\"label\"\r\n\r\n桌面端质检\
             \r\n--{boundary}--\r\n"
        )
        .as_bytes(),
    );
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let token = crate::movie::token();
    let (status, text) = crate::account::api_call(
        "POST",
        &endpoint("/post/qc"),
        &content_type,
        &body,
        token.as_deref(),
        Duration::from_secs(300),
    )
    .map_err(|error| format!("连不上平台质检接口：{error}"))?;
    let value = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| {
        json!({"ok": false, "hint": text.chars().take(200).collect::<String>()})
    });
    if !(200..300).contains(&status) || value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(format!("平台质检失败（HTTP {status}）：{}", api_hint(&value)));
    }
    Ok(value)
}

/// 把平台导出的 zip 下到本地。
pub fn download(url: &str, dest: &Path) -> Result<u64, String> {
    if url.trim().is_empty() {
        return Err("平台没有给出下载地址".to_owned());
    }
    let token = crate::movie::token();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("建不了下载目录：{error}"))?;
    }
    crate::account::api_download(url, token.as_deref(), dest, Duration::from_secs(300), &std::sync::atomic::AtomicBool::new(false))
        .map_err(|error| format!("下载交付包失败：{error}"))
}

// ── 本地：文件指纹 / 探测 / 质检 ─────────────────────────────────────

/// 随包 / 系统里的 ffprobe。
pub fn ffprobe() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for name in ["ffprobe.exe", "ffprobe"] {
            let beside = dir.join(name);
            if beside.is_file() {
                return beside;
            }
        }
    }
    PathBuf::from("ffprobe.exe")
}

/// 一个文件的 sha256（流式读，不整档进内存）。
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|error| format!("打不开 {}：{error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).map_err(|error| format!("读不了 {}：{error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

/// ffprobe 的一份 JSON；文件不是媒体或读不了时是 `None`。
pub fn probe(path: &Path) -> Option<Value> {
    let output = std::process::Command::new(ffprobe())
        .args(["-v", "error", "-show_format", "-show_streams", "-of", "json"])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn probe_seconds(probe: &Value) -> Option<f64> {
    probe
        .get("format")
        .and_then(|format| format.get("duration"))
        .and_then(|value| value.as_str())
        .and_then(|text| text.parse::<f64>().ok())
        .or_else(|| {
            probe
                .get("streams")
                .and_then(Value::as_array)
                .and_then(|streams| {
                    streams.iter().find_map(|stream| {
                        stream
                            .get("duration")
                            .and_then(Value::as_str)
                            .and_then(|text| text.parse::<f64>().ok())
                    })
                })
        })
}

/// 一个媒体的校验行：与平台 `/post/checksum` 同形的字段。
pub fn fingerprint(path: &Path) -> Value {
    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let sha256 = sha256_file(path).unwrap_or_default();
    let probe = probe(path);
    let duration = probe.as_ref().and_then(probe_seconds).unwrap_or(0.0);
    let (width, height, fps, codec) = probe
        .as_ref()
        .and_then(|probe| probe.get("streams").and_then(Value::as_array))
        .and_then(|streams| {
            streams
                .iter()
                .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"))
        })
        .map(|video| {
            let width = video.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
            let height = video.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
            let fps = video
                .get("avg_frame_rate")
                .and_then(Value::as_str)
                .and_then(parse_rate)
                .unwrap_or(0.0);
            let codec = video
                .get("codec_name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            (width, height, fps, codec)
        })
        .unwrap_or((0, 0, 0.0, String::new()));
    json!({
        "name": path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
        "path": path.to_string_lossy(),
        "size": size,
        "sha256": sha256,
        "duration": duration,
        "width": width,
        "height": height,
        "fps": fps,
        "codec": codec,
    })
}

/// "30000/1001" → 29.97。
pub fn parse_rate(text: &str) -> Option<f64> {
    let (num, den) = text.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    (den > 0.0 && num > 0.0).then_some(num / den)
}

/// 时间码：`HH:MM:SS:FF`，从 `start_seconds` 起（默认 `01:00:00:00` 以外由调用方给）。
pub fn timecode(seconds: f64, fps: f64, start_seconds: f64) -> String {
    let fps = if fps.is_finite() && fps > 0.0 { fps } else { 25.0 };
    let total = ((seconds - start_seconds) * fps).round().max(0.0) as u64;
    let frames = total % fps.round().max(1.0) as u64;
    let total_seconds = total / fps.round().max(1.0) as u64;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        total_seconds / 3600,
        (total_seconds / 60) % 60,
        total_seconds % 60,
        frames
    )
}

/// 缺省起算时间码。
pub const START_TIMECODE: f64 = 3600.0;

/// 本地质检：一遍解码同时跑 ebur128（含真峰）/ 黑帧 / 静音 / 冻结帧。
///
/// 返回与平台 `qc_report.json` 同形的 JSON：`status` 为 `PASS` / `WARN` /
/// `FAIL`，`summary` 带时长、时间码、分辨率、帧率、编码、响度，`issues`
/// 每项带可跳转的 `start_s` / `start_tc`。
pub fn qc_local(media: &Path, project_fps: f64) -> Result<Value, String> {
    let probe = probe(media).ok_or_else(|| "ffprobe 读不了这个文件（不是媒体或文件损坏）".to_owned())?;
    let duration = probe_seconds(&probe).unwrap_or(0.0);
    let video = probe
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| {
            streams
                .iter()
                .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"))
        });
    let has_video = video.is_some();
    let width = video.and_then(|video| video.get("width")).and_then(Value::as_u64).unwrap_or(0);
    let height = video.and_then(|video| video.get("height")).and_then(Value::as_u64).unwrap_or(0);
    let fps = video
        .and_then(|video| video.get("avg_frame_rate"))
        .and_then(Value::as_str)
        .and_then(parse_rate)
        .filter(|fps| *fps > 0.0)
        .unwrap_or(if project_fps > 0.0 { project_fps } else { 25.0 });
    let codec = video
        .and_then(|video| video.get("codec_name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let has_audio = probe
        .get("streams")
        .and_then(Value::as_array)
        .is_some_and(|streams| streams.iter().any(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio")));

    let mut command = std::process::Command::new(crate::video_preview::ffmpeg());
    command
        .args(["-hide_banner", "-nostats", "-v", "info", "-i"])
        .arg(media);
    if has_video {
        command
            .arg("-vf")
            .arg(format!(
                "blackdetect=d={BLACK_MIN}:pix_th=0.10,freezedetect=n=0.001:d={FREEZE_MIN}"
            ));
    }
    if has_audio {
        command
            .arg("-af")
            .arg(format!("ebur128=peak=true,silencedetect=n=-50dB:d={SILENCE_MIN}"));
    }
    let output = command
        .args(["-f", "null", "-"])
        .output()
        .map_err(|error| format!("起不了 ffmpeg 质检：{error}"))?;
    let log = String::from_utf8_lossy(&output.stderr).into_owned();
    let mut integrated = None;
    let mut lra = None;
    let mut peak = None;
    let mut blacks: Vec<(f64, f64)> = Vec::new();
    let mut silences: Vec<(f64, f64)> = Vec::new();
    let mut freezes: Vec<(f64, f64)> = Vec::new();
    let mut open_black: Option<f64> = None;
    let mut open_silence: Option<f64> = None;
    let mut open_freeze: Option<f64> = None;
    for line in log.lines() {
        let line = line.trim();
        if line.contains(" I:") && line.contains("LUFS") && !line.contains("Threshold") {
            integrated = learn_metric(line, "I:", "LUFS").or(integrated);
        }
        if line.contains("LRA:") && line.contains(" LU") {
            lra = learn_metric(line, "LRA:", "LU").or(lra);
        }
        if line.contains("Peak:") && line.contains("dBFS") {
            peak = learn_metric(line, "Peak:", "dBFS").or(peak);
        }
        if line.contains("black_start:") {
            open_black = field(line, "black_start:");
        }
        if line.contains("black_end:") {
            if let (Some(start), Some(end)) = (open_black.take(), field(line, "black_end:")) {
                blacks.push((start, end));
            }
        }
        if line.contains("silence_start:") {
            open_silence = field(line, "silence_start:");
        }
        if line.contains("silence_end:") {
            if let (Some(start), Some(end)) = (open_silence.take(), field(line, "silence_end:")) {
                silences.push((start, end));
            }
        }
        if line.contains("freeze_start:") {
            open_freeze = field(line, "freeze_start:");
        }
        if line.contains("freeze_end:") {
            if let (Some(start), Some(end)) = (open_freeze.take(), field(line, "freeze_end:")) {
                freezes.push((start, end));
            }
        }
        if line.contains("freeze_duration:") && open_freeze.is_none() {
            // freezedetect 只给起点与时长时，用它算终点；没有起点就跳过。
        }
    }
    // 冻结帧的日志只保证 freeze_start/freeze_duration/freeze_end 三行；
    // 用 duration 补齐终点。
    let mut ended: Vec<(f64, f64)> = Vec::new();
    for line in log.lines() {
        if let Some(start) = field(line, "freeze_start:") {
            let duration = field(line, "freeze_duration:").unwrap_or(0.0);
            ended.push((start, start + duration));
        }
    }
    if !ended.is_empty() {
        freezes = ended;
    }
    if let Some(start) = open_silence {
        // 静音一直到片尾：终点取时长。
        silences.push((start, duration));
    }
    if let Some(start) = open_black {
        blacks.push((start, duration));
    }
    if let Some(start) = open_freeze {
        freezes.push((start, duration));
    }

    let integrated = integrated.unwrap_or(0.0);
    let peak = peak.unwrap_or(0.0);
    let mut issues: Vec<Value> = Vec::new();
    let mut fail = false;
    let mut warn = false;
    if has_audio {
        if integrated == 0.0 && peak == 0.0 {
            issues.push(issue(
                "loudness",
                "响度",
                "warn",
                0.0,
                duration,
                "ffmpeg 没给出响度汇总（可能整段无声）",
                fps,
            ));
            warn = true;
        } else {
            if (integrated - TARGET_LUFS).abs() > LOUDNESS_TOLERANCE {
                issues.push(issue(
                    "loudness",
                    "整体响度",
                    "fail",
                    0.0,
                    duration,
                    &format!(
                        "整体响度 {integrated:.1} LUFS，偏离目标 {TARGET_LUFS:.0} LUFS 超过 ±{LOUDNESS_TOLERANCE:.0} LU"
                    ),
                    fps,
                ));
                fail = true;
            }
            if peak > TRUE_PEAK_MAX {
                issues.push(issue(
                    "true_peak",
                    "真峰",
                    "fail",
                    0.0,
                    duration,
                    &format!("真峰 {peak:.1} dBTP，超过上限 {TRUE_PEAK_MAX:.1} dBTP"),
                    fps,
                ));
                fail = true;
            }
        }
    }
    if has_video {
        for (start, end) in &blacks {
            if end - start >= BLACK_MIN {
                issues.push(issue(
                    "black",
                    "黑场",
                    "warn",
                    *start,
                    *end,
                    &format!("黑场 {:.2} 秒", end - start),
                    fps,
                ));
                warn = true;
            }
        }
        for (start, end) in &freezes {
            if end - start >= FREEZE_MIN {
                issues.push(issue(
                    "freeze",
                    "冻结帧",
                    "warn",
                    *start,
                    *end,
                    &format!("冻结帧 {:.2} 秒", end - start),
                    fps,
                ));
                warn = true;
            }
        }
    }
    for (start, end) in &silences {
        if end - start >= SILENCE_MIN {
            issues.push(issue(
                "silence",
                "静音",
                "warn",
                *start,
                *end,
                &format!("静音 {:.2} 秒", end - start),
                fps,
            ));
            warn = true;
        }
    }
    issues.sort_by(|a, b| {
        a.get("start_s").and_then(Value::as_f64).unwrap_or(0.0).partial_cmp(
            &b.get("start_s").and_then(Value::as_f64).unwrap_or(0.0),
        ).unwrap_or(std::cmp::Ordering::Equal)
    });
    let status = if fail {
        "FAIL"
    } else if warn {
        "WARN"
    } else {
        "PASS"
    };
    let duration_tc = timecode(duration, fps, START_TIMECODE);
    let file_name = media
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(json!({
        "ok": true,
        "source": "local",
        "label": file_name,
        "file": media.to_string_lossy(),
        "status": status,
        "summary": {
            "duration_s": duration,
            "duration_tc": duration_tc,
            "width": width,
            "height": height,
            "fps": fps,
            "codec": codec,
            "integrated_lufs": integrated,
            "lra": lra.unwrap_or(0.0),
            "true_peak_dbtp": peak,
            "black_count": blacks.len(),
            "silence_count": silences.len(),
            "freeze_count": freezes.len(),
            "target_lufs": TARGET_LUFS,
            "tolerance_lu": LOUDNESS_TOLERANCE,
            "true_peak_max": TRUE_PEAK_MAX,
        },
        "issues": issues,
        "thresholds": {
            "target_lufs": TARGET_LUFS,
            "tolerance_lu": LOUDNESS_TOLERANCE,
            "true_peak_max": TRUE_PEAK_MAX,
            "black_min_s": BLACK_MIN,
            "silence_min_s": SILENCE_MIN,
            "freeze_min_s": FREEZE_MIN,
        },
        "warnings": [],
        "limitations": ["桌面端本地质检：随包 ffmpeg 单遍解码；VFR / 多音轨只按主音轨计。".to_owned()],
    }))
}

fn issue(kind: &str, label: &str, level: &str, start: f64, end: f64, message: &str, fps: f64) -> Value {
    json!({
        "kind": kind,
        "label": label,
        "level": level,
        "start_s": start,
        "end_s": end,
        "duration_s": (end - start).max(0.0),
        "start_tc": timecode(start, fps, START_TIMECODE),
        "end_tc": timecode(end, fps, START_TIMECODE),
        "message": message,
        "source": "local",
    })
}

fn field(line: &str, key: &str) -> Option<f64> {
    let at = line.find(key)? + key.len();
    let rest = &line[at..];
    let token: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '-' || *ch == '+')
        .collect();
    token.parse().ok()
}

fn learn_metric(line: &str, key: &str, unit: &str) -> Option<f64> {
    let at = line.find(key)? + key.len();
    let mut words = line[at..].split_whitespace();
    let value: f64 = words.next()?.parse().ok()?;
    let unit_word = words.next()?;
    if unit_word.starts_with(unit) {
        Some(value)
    } else {
        None
    }
}

// ── 工程 → 平台 JSON / 本地 EDL / OTIO ─────────────────────────────

fn media_of<'a>(project: &'a Project, media_id: &str) -> Option<&'a MediaItem> {
    project.media.iter().find(|item| item.id == media_id)
}

fn color_space_label(space: concat_project::model::ColorSpace) -> &'static str {
    use concat_project::model::ColorSpace;
    match space {
        ColorSpace::Sdr => "Rec.709",
        ColorSpace::Hlg => "HLG",
        ColorSpace::Pq => "PQ",
    }
}

fn fps_of(timeline: &Timeline) -> f64 {
    let num = timeline.video.rate_num as f64;
    let den = timeline.video.rate_den as f64;
    if num > 0.0 && den > 0.0 {
        num / den
    } else {
        25.0
    }
}

/// 工程文档 → 平台 `xinglai.post.project.v1`（桌面端推荐接法）。
///
/// 时间单位是帧的字段是 `src_in/src_out/record_in`，秒的字段是
/// `start/duration/source_start`；两条轨道按主视频 / 主音频两条铺，
/// 素材都写本机绝对路径（平台读不到时会标 `media_exists=false`）。
pub fn project_json(project: &Project) -> Value {
    let timeline = project.active();
    let fps = fps_of(timeline);
    let rate = if fps > 0.0 { fps } else { 25.0 };
    let mut video: Vec<Value> = Vec::new();
    let mut audio: Vec<Value> = Vec::new();
    let mut clips: Vec<&concat_project::model::Clip> = timeline.clips.iter().map(|clip| clip.as_ref()).collect();
    clips.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    for clip in clips {
        let media = media_of(project, &clip.media_id);
        let is_audio = clip.kind == ClipKind::Audio;
        let entry = json!({
            "clip_id": clip.id,
            "name": clip.name,
            "media": media.map(|item| item.path.clone()).unwrap_or_default(),
            "media_name": media.map(|item| item.name.clone()).unwrap_or_default(),
            "src_in": (clip.source_start * rate).round() as i64,
            "src_out": ((clip.source_start + clip.duration * clip.speed) * rate).round() as i64,
            "record_in": (clip.start * rate).round() as i64,
            "start": clip.start,
            "duration": clip.duration,
            "source_start": clip.source_start,
            "speed": clip.speed,
            "color_space": media.map(|item| color_space_label(item.color_space)).unwrap_or("Rec.709"),
            "media_exists": media.map(|item| Path::new(&item.path).is_file()).unwrap_or(false),
            "note": clip.name,
        });
        if is_audio {
            audio.push(entry);
        } else {
            video.push(entry);
        }
    }
    json!({
        "schema": "xinglai.post.project.v1",
        "name": timeline.name,
        "fps": rate,
        "start_timecode": "01:00:00:00",
        "color": {
            "working_space": color_space_label(timeline.video.color_space),
            "output_space": "Rec.709",
        },
        "canvas": {"width": timeline.video.width, "height": timeline.video.height},
        "tracks": [
            {"kind": "Video", "name": "V1", "clips": video},
            {"kind": "Audio", "name": "A1", "clips": audio},
        ],
    })
}

/// 本地 CMX3600 EDL：主视频轨按时间排序，逐条事件；转场降级为切点并注释。
pub fn edl_text(project: &Project, title: &str) -> String {
    let timeline = project.active();
    let fps = fps_of(timeline);
    let rate = if fps > 0.0 { fps } else { 25.0 };
    let mut clips: Vec<&concat_project::model::Clip> = timeline.clips.iter().map(|clip| clip.as_ref()).collect();
    clips.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    clips.retain(|clip| !matches!(clip.kind, ClipKind::Audio));
    let mut out = String::new();
    out.push_str(&format!("TITLE: {title}\n"));
    out.push_str("FCM: NON-DROP FRAME\n\n");
    for (index, clip) in clips.iter().enumerate() {
        let number = index + 1;
        let reel = reel_name(&clip.name);
        let src_in = timecode(clip.source_start, rate, START_TIMECODE);
        let src_out = timecode(clip.source_start + clip.duration * clip.speed, rate, START_TIMECODE);
        let rec_in = timecode(clip.start, rate, START_TIMECODE);
        let rec_out = timecode(clip.start + clip.duration, rate, START_TIMECODE);
        out.push_str(&format!(
            "{number:03}  {reel:<8} V     C        {src_in} {src_out} {rec_in} {rec_out}\n"
        ));
        out.push_str(&format!("* FROM CLIP NAME: {}\n", clip.name));
        if let Some(media) = media_of(project, &clip.media_id) {
            out.push_str(&format!("* FROM FILE: {}\n", media.path));
            out.push_str(&format!(
                "* FROM COLOR SPACE: {}\n",
                color_space_label(media.color_space)
            ));
        }
        if clip.transition_in.is_some() {
            out.push_str("* NOTE: 转场已按切点导出（本地 EDL 不含转场时长）\n");
        }
        out.push('\n');
    }
    out
}

fn reel_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase();
    if cleaned.is_empty() {
        "AX".to_owned()
    } else {
        cleaned
    }
}

/// 本地 OpenTimelineIO JSON（schema 0.17），Resolve / Avid / Nuke 可读。
pub fn otio_json(project: &Project) -> Value {
    let timeline = project.active();
    let fps = fps_of(timeline);
    let rate = if fps > 0.0 { fps } else { 25.0 };
    let mut picture: Vec<&concat_project::model::Clip> = timeline.clips.iter().map(|clip| clip.as_ref()).collect();
    picture.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    let mut video_children: Vec<Value> = Vec::new();
    let mut audio_children: Vec<Value> = Vec::new();
    for clip in picture {
        let media = media_of(project, &clip.media_id);
        let start_frames = (clip.source_start * rate).round().max(0.0);
        let duration_frames = (clip.duration * clip.speed * rate).round().max(1.0);
        let media_ref = match media {
            Some(media) => json!({
                "OTIO_SCHEMA": "ExternalReference.1",
                "name": media.name,
                "target_url": media.path,
                "available_range": {
                    "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": {"OTIO_SCHEMA": "RationalTime.1", "rate": rate, "value": 0},
                    "duration": {"OTIO_SCHEMA": "RationalTime.1", "rate": rate, "value": (media.duration.unwrap_or(0.0) * rate).round()},
                },
            }),
            None => json!({"OTIO_SCHEMA": "MissingReference.1", "name": clip.name}),
        };
        let item = json!({
            "OTIO_SCHEMA": "Clip.2",
            "name": clip.name,
            "source_range": {
                "OTIO_SCHEMA": "TimeRange.1",
                "start_time": {"OTIO_SCHEMA": "RationalTime.1", "rate": rate, "value": start_frames},
                "duration": {"OTIO_SCHEMA": "RationalTime.1", "rate": rate, "value": duration_frames},
            },
            "media_reference": media_ref,
            "metadata": {
                "xinglai": {
                    "clip_id": clip.id,
                    "media_id": clip.media_id,
                    "color_space": media.map(|item| color_space_label(item.color_space)).unwrap_or("Rec.709"),
                    "reel": reel_name(&clip.name),
                    "media_exists": media.map(|item| Path::new(&item.path).is_file()).unwrap_or(false),
                }
            },
        });
        if clip.kind == ClipKind::Audio {
            audio_children.push(item);
        } else {
            video_children.push(item);
        }
    }
    json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": timeline.name,
        "metadata": {
            "xinglai": {
                "fps": rate,
                "drop_frame": false,
                "start_timecode": timecode(0.0, rate, 0.0),
                "color": color_space_label(timeline.video.color_space),
                "canvas": {"width": timeline.video.width, "height": timeline.video.height},
                "source": "xinglai-cut-desktop",
            }
        },
        "global_start_time": {"OTIO_SCHEMA": "RationalTime.1", "rate": rate, "value": 0},
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "name": "tracks",
            "children": [
                {"OTIO_SCHEMA": "Track.1", "name": "V1", "kind": "Video", "metadata": {}, "children": video_children},
                {"OTIO_SCHEMA": "Track.1", "name": "A1", "kind": "Audio", "metadata": {}, "children": audio_children},
            ],
            "metadata": {},
        },
    })
}

// ── 本地交付包 ────────────────────────────────────────────────────

/// 本地交付包的内容：路径名 → 字节。
pub type HandoffFiles = Vec<(String, Vec<u8>)>;

/// 把一个交付包写成 zip；文件名一律 `/` 分隔。返回字节数与作用域外空。
pub fn write_zip(dest: &Path, files: HandoffFiles) -> Result<u64, String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("建不了交付包目录：{error}"))?;
    }
    let file = std::fs::File::create(dest).map_err(|error| format!("建不了交付包：{error}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for (name, bytes) in files {
        zip.start_file(name, options)
            .map_err(|error| format!("zip 写入失败（{error}）"))?;
        zip.write_all(&bytes)
            .map_err(|error| format!("zip 写入失败（{error}）"))?;
    }
    zip.finish().map_err(|error| format!("交付包收尾失败（{error}）"))?;
    let size = std::fs::metadata(dest).map(|meta| meta.len()).unwrap_or(0);
    Ok(size)
}

/// 交付包的 manifest.json 与 checksums.sha256。
pub fn handoff_manifest(files: &HandoffFiles) -> (String, String) {
    let mut rows: Vec<Value> = Vec::new();
    let mut checksums = String::new();
    for (name, bytes) in files {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let sha = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        rows.push(json!({"path": name, "size": bytes.len(), "sha256": sha}));
        checksums.push_str(&format!("{sha}  {name}\n"));
    }
    let manifest = json!({
        "schema": "xinglai.handoff.manifest.v1",
        "generated_by": "星籁剪辑 0.3.6 桌面端（本地兜底）",
        "file_count": files.len(),
        "files": rows,
    });
    (serde_json::to_string_pretty(&manifest).unwrap_or_default(), checksums)
}

/// 媒体清单 JSON：每条路径、大小、sha256、时长、分辨率、色彩空间。
pub fn media_manifest(project: &Project, hashes: &BTreeMap<String, String>) -> Value {
    let rows: Vec<Value> = project
        .media
        .iter()
        .map(|item| {
            let path = Path::new(&item.path);
            let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
            json!({
                "media_id": item.id,
                "name": item.name,
                "path": item.path,
                "size": size,
                "sha256": hashes.get(&item.path).cloned().unwrap_or_default(),
                "exists": path.is_file(),
                "duration": item.duration.unwrap_or(0.0),
                "width": item.width.unwrap_or(0),
                "height": item.height.unwrap_or(0),
                "fps": item.frame_rate.unwrap_or(0.0),
                "color_space": color_space_label(item.color_space),
            })
        })
        .collect();
    json!({
        "schema": "xinglai.media_manifest.v1",
        "project": project.active().name,
        "fps": fps_of(project.active()),
        "media": rows,
    })
}
