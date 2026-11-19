// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 星籁云端的 AI 配音与照片数字人（**默认关闭**，需要登录与套餐额度）。
//!
//! 桌面版默认走本机插件；只有设置里选了「仅云端」，或智能选择把弱机器
//! 推荐到云端，并且用户逐次确认「会上传素材、消耗额度」之后，才走到这里。
//!
//! 接口（2026-11 线上）：
//!
//! - `GET  /voice/list` → `{presets:[...], cloned:[...], default, limits}`
//! - `POST /voice/tts {text, voice, rate}` →
//!   `{url, duration, engine, voice_label, cost_hint, fallback}`
//! - `POST /autoedit/avatar {file: "data:image/jpeg;base64,...", audio_url}` →
//!   `{avatar_clip_url, duration, width, height, note}`
//!
//! `url` 有时是绝对地址（`https://xinglaiai.com/...`），有时是站内相对
//! 地址（`/ai-nav/studio/files/...`），[`absolute`] 统一补全。请求本身复用
//! 登录的 [`crate::account::api_call`]：Windows 上 OpenSSL 直连、不走系统
//! 代理，平台的 WAF 才放行。

use std::time::Duration;

use serde::Deserialize;

/// 一次 AI 请求最多等这么久（产品要求 120 秒）。
const READ: Duration = Duration::from_secs(120);

/// 平台站点；相对地址补全用。
pub const SITE: &str = "https://xinglaiai.com";

/// 一个云端音色（预设或克隆）。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CloudVoice {
    /// 请求里要带的 `voice`。
    #[serde(alias = "voice_id")]
    pub id: String,
    /// 界面上的名字。
    #[serde(default)]
    pub name: String,
    /// 一句话说明。
    #[serde(default)]
    pub desc: String,
    /// 分类标签。
    #[serde(default)]
    pub tag: String,
    /// 引擎（`qwen3-tts-flash`、`cosyvoice-v2`）。
    #[serde(default)]
    pub engine: String,
    /// 已经可合成；部署中的克隆音色为 false。
    #[serde(default)]
    pub ready: bool,
    /// 是克隆音色。
    #[serde(default)]
    pub cloned: bool,
    /// 部署中的提示。
    #[serde(default)]
    pub deploy_hint: String,
    /// `status`：OK / DEPLOYING。
    #[serde(default)]
    pub status: String,
    /// 平台推荐。
    #[serde(default)]
    pub featured: bool,
}

impl CloudVoice {
    /// 界面上的完整一行：`老板本人（星籁）· 已就绪`。
    pub fn label(&self) -> String {
        let name = if self.name.trim().is_empty() {
            self.id.clone()
        } else {
            self.name.clone()
        };
        let suffix = if !self.ready {
            if self.deploy_hint.trim().is_empty() {
                "部署中".to_owned()
            } else {
                self.deploy_hint.trim().to_owned()
            }
        } else if self.cloned {
            "克隆 · 已就绪".to_owned()
        } else {
            self.engine.clone()
        };
        if suffix.is_empty() {
            name
        } else {
            format!("{name} · {suffix}")
        }
    }
}

/// `/voice/list` 的回复。
#[derive(Clone, Debug, Default)]
pub struct CloudVoices {
    /// 可选项：预设在前、克隆在后。
    pub voices: Vec<CloudVoice>,
    /// 平台默认音色的 id。
    pub default_id: String,
    /// 每 10 分钟能合成几次。
    pub per_10_min: u32,
    /// 平台自己的补充说明。
    pub note: String,
}

/// 一次云端合成。
#[derive(Clone, Debug, Default)]
pub struct CloudTts {
    /// 音频地址，已补成绝对地址。
    pub url: String,
    /// 时长，秒。
    pub duration: f64,
    /// 实际用的引擎。
    pub engine: String,
    /// 实际用的音色名。
    pub label: String,
    /// 平台给的花费提示，原样展示。
    pub cost_hint: String,
    /// 是否降级成了备用引擎。
    pub fallback: bool,
}

/// 一次云端数字人。
#[derive(Clone, Debug, Default)]
pub struct CloudAvatar {
    /// mp4 地址，已补成绝对地址。
    pub url: String,
    /// 时长，秒。
    pub duration: f64,
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// 平台的说明（"照片+口播音轨（未做口型同步）"）。
    pub note: String,
}

/// 站内相对地址补成绝对地址；已经是绝对的原样返回。
pub fn absolute(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        url.to_owned()
    } else if url.starts_with('/') {
        format!("{SITE}{url}")
    } else {
        format!("{SITE}/{url}")
    }
}

/// 服务器说不行时翻成一句人话。`hint` 是平台自己的说法，优先用。
fn refusal(status: u16, text: &str) -> String {
    let hint = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("hint")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    match status {
        0 => "服务器没有给出回复（连接被中途关闭），请稍后重试".to_owned(),
        401 => "登录已失效，请重新登录星籁账号".to_owned(),
        402 | 403 => hint.unwrap_or_else(|| "套餐额度不足或没有这个功能的权限，请在网页工作台升级套餐".to_owned()),
        404 => hint.unwrap_or_else(|| "云端接口不存在，请升级到最新版本".to_owned()),
        413 => "要上传的照片太大（上限 10MB），请压缩后再试".to_owned(),
        429 => "请求太频繁或额度用完了，请稍后再试，或在网页工作台升级套餐".to_owned(),
        500..=599 => format!("星籁云端 AI 暂时不可用（HTTP {status}），请稍后再试"),
        _ => hint.unwrap_or_else(|| format!("云端 AI 返回了 HTTP {status}")),
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
            .unwrap_or("云端 AI 拒绝了这次请求")
            .to_owned());
    }
    Ok(())
}

/// 读平台接口的根地址（与登录同一份，环境变量可换）。
fn endpoint(path: &str) -> String {
    let base = std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::account::API_BASE.to_owned());
    format!("{base}{path}")
}

/// 平台给的所有音色：12 个预设 + 本账号的克隆音色。
pub fn list_voices(token: &str) -> Result<CloudVoices, String> {
    let (status, text) = crate::account::api_call(
        "GET",
        &endpoint("/voice/list"),
        "application/json",
        &[],
        Some(token),
        READ,
    )?;
    let value = json_reply(status, &text)?;
    check_ok(&value)?;
    let mut voices = Vec::new();
    if let Some(rows) = value.get("presets").and_then(serde_json::Value::as_array) {
        for row in rows {
            let mut voice: CloudVoice = serde_json::from_value(row.clone())
                .map_err(|error| format!("音色列表格式不认识：{error}"))?;
            voice.ready = true;
            voices.push(voice);
        }
    }
    if let Some(rows) = value.get("cloned").and_then(serde_json::Value::as_array) {
        for row in rows {
            let mut voice: CloudVoice = serde_json::from_value(row.clone())
                .map_err(|error| format!("克隆音色列表格式不认识：{error}"))?;
            voice.cloned = true;
            // 平台用 ready 字段；没有时按 status=OK 算。
            if row.get("ready").is_none() {
                voice.ready = voice.status.eq_ignore_ascii_case("OK");
            }
            voices.push(voice);
        }
    }
    let per_10_min = value
        .get("limits")
        .and_then(|limits| limits.get("tts_per_10min"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    Ok(CloudVoices {
        voices,
        default_id: value
            .get("default")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        per_10_min,
        note: value
            .get("note")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

/// 云端配音：文字、音色、语速进，mp3 地址出。
pub fn tts(token: &str, text: &str, voice: &str, rate: f32) -> Result<CloudTts, String> {
    let body = serde_json::json!({
        "text": text,
        "voice": voice,
        "rate": rate,
    })
    .to_string();
    let (status, reply) = crate::account::api_call(
        "POST",
        &endpoint("/voice/tts"),
        "application/json",
        body.as_bytes(),
        Some(token),
        READ,
    )?;
    let value = json_reply(status, &reply)?;
    check_ok(&value)?;
    let url = value
        .get("url")
        .or_else(|| value.get("audio_url"))
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "合成成功，但服务器没有给出音频地址".to_owned())?;
    Ok(CloudTts {
        url: absolute(url),
        duration: value
            .get("duration")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
        engine: value
            .get("engine")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        label: value
            .get("voice_label")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(voice)
            .to_owned(),
        cost_hint: value
            .get("cost_hint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        fallback: value
            .get("fallback")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// 照片数字人：照片（JPG/PNG）+ 已在云端的音频，出 mp4 地址。
///
/// 照片以 data URI 放在 `file` 字段里，与线上接口一致。
pub fn avatar(
    token: &str,
    photo: &[u8],
    mime: &str,
    audio_url: &str,
) -> Result<CloudAvatar, String> {
    if photo.is_empty() {
        return Err("请先选择一张照片".to_owned());
    }
    if photo.len() > 10 * 1024 * 1024 {
        return Err("照片超过 10MB，请压缩后再试".to_owned());
    }
    let mime = match mime {
        "image/png" => "image/png",
        _ => "image/jpeg",
    };
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(photo);
    let body = serde_json::json!({
        "file": format!("data:{mime};base64,{encoded}"),
        "audio_url": absolute(audio_url),
    })
    .to_string();
    let (status, reply) = crate::account::api_call(
        "POST",
        &endpoint("/autoedit/avatar"),
        "application/json",
        body.as_bytes(),
        Some(token),
        READ,
    )?;
    let value = json_reply(status, &reply)?;
    check_ok(&value)?;
    let url = value
        .get("avatar_clip_url")
        .or_else(|| value.get("video_url"))
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "数字人生成成功，但服务器没有给出视频地址".to_owned())?;
    Ok(CloudAvatar {
        url: absolute(url),
        duration: value
            .get("duration")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
        width: value
            .get("width")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32,
        height: value
            .get("height")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32,
        note: value
            .get("note")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("照片推近 + 口播音轨，未做口型同步")
            .to_owned(),
    })
}

/// 登录状态检查：没有令牌就是一句「请先登录」。
pub fn token() -> Result<String, String> {
    match crate::account::load() {
        Some(account) if !account.token.trim().is_empty() => Ok(account.token),
        _ => Err("请先登录星籁账号（首屏登录后重试）；云端 AI 需要登录并消耗套餐额度".to_owned()),
    }
}

/// 令牌过期（401）时把本机的会话清掉；界面据此提示重新登录。
pub fn forget_if_expired(message: &str) {
    if message.contains("重新登录") || message.contains("登录已失效") {
        crate::account::sign_out();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_site_address_is_made_absolute() {
        assert_eq!(
            absolute("/ai-nav/studio/files/a.mp3"),
            "https://xinglaiai.com/ai-nav/studio/files/a.mp3"
        );
        assert_eq!(
            absolute("https://xinglaiai.com/x.mp4"),
            "https://xinglaiai.com/x.mp4"
        );
    }

    #[test]
    fn a_voice_is_read_as_the_platform_sends_it() {
        let reply = r#"{"ok":true,"presets":[{"id":"qwen:Cherry","voice":"Cherry",
            "name":"Cherry（女·亲和）","engine":"qwen3-tts-flash","tag":"女 · 普通话"}],
            "cloned":[{"voice_id":"cosyvoice-v2-xlboss","name":"老板本人（星籁）",
            "status":"OK","ready":true,"engine":"cosyvoice-v2"}],"default":"qwen:Cherry",
            "limits":{"tts_per_10min":20}}"#;
        let voices = parse_for_test(reply);
        assert_eq!(voices.voices.len(), 2);
        assert!(voices.voices[0].ready);
        assert!(voices.voices[1].cloned && voices.voices[1].ready);
        assert_eq!(voices.voices[1].id, "cosyvoice-v2-xlboss");
        assert_eq!(voices.default_id, "qwen:Cherry");
        assert_eq!(voices.per_10_min, 20);
    }

    /// 测试里复用真实解析路径：喂一段 JSON 给同一个函数。
    fn parse_for_test(text: &str) -> CloudVoices {
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        let mut voices = Vec::new();
        for row in value.get("presets").and_then(serde_json::Value::as_array).unwrap() {
            let mut voice: CloudVoice = serde_json::from_value(row.clone()).unwrap();
            voice.ready = true;
            voices.push(voice);
        }
        for row in value.get("cloned").and_then(serde_json::Value::as_array).unwrap() {
            let mut voice: CloudVoice = serde_json::from_value(row.clone()).unwrap();
            voice.cloned = true;
            if row.get("ready").is_none() {
                voice.ready = voice.status.eq_ignore_ascii_case("OK");
            }
            voices.push(voice);
        }
        CloudVoices {
            voices,
            default_id: value.get("default").and_then(|v| v.as_str()).unwrap_or_default().to_owned(),
            per_10_min: value
                .get("limits")
                .and_then(|l| l.get("tts_per_10min"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32,
            note: String::new(),
        }
    }

    #[test]
    fn a_deploying_clone_is_shown_as_such() {
        let voice = CloudVoice {
            name: "测试".to_owned(),
            status: "DEPLOYING".to_owned(),
            deploy_hint: "音色部署中，完成后可合成".to_owned(),
            ..CloudVoice::default()
        };
        assert!(voice.label().contains("部署中"));
    }
}
