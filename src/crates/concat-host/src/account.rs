// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 星籁 AI 账号：用手机号和密码登录平台，把会话加密留在本机。
//!
//! 只和 [`API_BASE`] 说话（可以用环境变量 `XINGLAI_API_BASE` 换掉）。
//! 令牌用 Windows 的 DPAPI（`CryptProtectData`，当前用户、本机）加密后
//! 写进配置目录的 `account.bin`；别的账号、别的机器都解不开。退出登录
//! 删掉它。
//!
//! 两个环境事实决定了这一块的写法：
//!
//! 1. **直连，不走系统代理**。用户机器上常驻着 Clash 之类的 HTTP 代理
//!    （127.0.0.1:7897），平台的域名经它转发会 TLS 握手失败；这里自己
//!    开 TCP 直连，系统的代理设置一概不问。
//! 2. **Windows 上用 OpenSSL（自己的 TLS），不用 rustls**。平台的
//!    Aliyun WAF 会重置 rustls 与 Schannel 的 ClientHello，只放行
//!    OpenSSL 形状的（实测见 docs/REBRAND.md 第 4 节）。别的平台上没有
//!    DPAPI，登录本来也做不了，那里仍走 Concat 自己的网络层。
//!
//! 登录的回复是 `{ok, token, user{...}}`，密码错了是 401 加一句
//! `hint`。两样都在 [`sign_in`] 里翻成人话。

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::dirs::AppDirs;
#[cfg(not(windows))]
use crate::net::{self, Trust};

/// 平台接口的根地址，除非环境变量 `XINGLAI_API_BASE` 说别的。
pub const API_BASE: &str = "https://xinglaiai.com/ai-nav/api";

/// 连接与读取的等待：一个登录不该让人等上好几秒，也不该在网络不好时
/// 三十秒后才说失败。
const CONNECT: Duration = Duration::from_secs(8);
const READ: Duration = Duration::from_secs(15);

/// 登录接口的完整地址。
fn login_url() -> String {
    let base = std::env::var("XINGLAI_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| API_BASE.to_owned());
    format!("{base}/auth/login")
}

/// 本机保存的那次登录：谁，什么套餐，以及那枚令牌。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Account {
    /// 账号显示名，平台给的。
    pub name: String,
    /// 登录用的手机号。
    pub phone: String,
    /// 套餐的键，`basic` 之类。
    pub plan: String,
    /// 套餐的名字，平台给的中文名。
    pub plan_name: String,
    /// 套餐到期日，平台给的 `2026-12-17 19:02:38`。
    pub plan_expire_at: String,
    /// 平台角色：普通用户是空或 `user`，内部账号是 `admin`。
    #[serde(default)]
    pub role: String,
    /// 内部账号：AI 工作室等云端能力免扣积分（平台只记成本）。
    #[serde(default)]
    pub is_admin: bool,
    /// 请求带着走的令牌。
    pub token: String,
}

impl Account {
    /// 界面上叫这个人什么：平台给的名字，没有就是手机号。
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            name.to_owned()
        } else if !self.phone.trim().is_empty() {
            self.phone.trim().to_owned()
        } else {
            "星籁用户".to_owned()
        }
    }

    /// 界面上叫这个套餐什么：中文名，退而求其次是键。
    pub fn plan_label(&self) -> String {
        let name = self.plan_name.trim();
        if !name.is_empty() {
            name.to_owned()
        } else if !self.plan.trim().is_empty() {
            self.plan.trim().to_owned()
        } else {
            "免费版".to_owned()
        }
    }
}

/// 登录：`POST /auth/login`，成功就把会话加密存下。
///
/// 返回的 [`Account`] 已经落盘，重启后 [`load`] 直接拿得到。
pub fn sign_in(phone: &str, password: &str) -> Result<Account, String> {
    let phone = phone.trim();
    if phone.is_empty() {
        return Err("请输入手机号".to_owned());
    }
    if password.is_empty() {
        return Err("请输入密码".to_owned());
    }
    let body = serde_json::json!({ "phone": phone, "password": password }).to_string();
    let (status, text) = login(&body)?;
    if status == 0 {
        return Err("服务器没有给出回复（连接被中途关闭）".to_owned());
    }
    if !(200..300).contains(&status) {
        return Err(refusal(status, &text));
    }
    let account = account_from_json(&text)?;
    save(&account)?;
    Ok(account)
}

/// 一个不是 2xx 的答复：状态、服务器自己的说法、以及它在 HTTP 里的位置。
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
        401 | 403 => hint.unwrap_or_else(|| "手机号或密码不对，请重试".to_owned()),
        429 => "登录太频繁了，请稍后再试".to_owned(),
        500..=599 => format!("星籁账号服务暂时不可用（HTTP {status}），请稍后再试"),
        _ => match hint {
            Some(hint) => format!("登录失败（HTTP {status}）：{hint}"),
            None => format!("登录失败（HTTP {status}）"),
        },
    }
}

/// Windows：自己开一条直连的 TCP，用 OpenSSL 谈 TLS。
///
/// 不用系统的代理设置，也不碰 rustls，理由见模块注释。
#[cfg(windows)]
fn login(body: &str) -> Result<(u16, String), String> {
    api_call(
        "POST",
        &login_url(),
        "application/json",
        body.as_bytes(),
        None,
        READ,
    )
}

/// 平台 API 的一次直连请求：方法、完整 URL、正文与可选的 Bearer 令牌。
///
/// 工程云同步（[`crate::cloud`]）复用它：同样是 Windows 上 OpenSSL 直连，
/// 不走系统代理，不碰 rustls。`read` 是读等待，上传成片要给它足够的时间。
#[cfg(windows)]
pub fn api_call(
    method: &str,
    url: &str,
    content_type: &str,
    body: &[u8],
    token: Option<&str>,
    read: Duration,
) -> Result<(u16, String), String> {
    use std::io::{Read, Write};
    use std::net::ToSocketAddrs;

    use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode};

    let (host, port, path) = split_url(url)?;

    let mut builder = SslConnector::builder(SslMethod::tls())
        .map_err(|error| format!("TLS 初始化失败：{error}"))?;
    builder.set_verify(SslVerifyMode::PEER);
    let mut store = openssl::x509::store::X509StoreBuilder::new()
        .map_err(|error| format!("证书库初始化失败：{error}"))?;
    // 系统信任的根，和下载走的是同一份；OpenSSL 自己不读 Windows 的
    // 证书库，所以由 rustls-native-certs 读出来再交给它。
    let system = rustls_native_certs::load_native_certs();
    for cert in system.certs {
        if let Ok(x509) = openssl::x509::X509::from_der(&cert) {
            let _ = store.add_cert(x509);
        }
    }
    builder.set_cert_store(store.build());
    let connector = builder.build();

    // 名字可能解析出好几个地址（IPv4 与 IPv6），第一个能连上的算。
    let addresses = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| format!("星籁账号服务的地址解析失败（网络或 DNS）：{error}"))?;
    let mut refusal = String::new();
    let mut socket = None;
    for address in addresses {
        match std::net::TcpStream::connect_timeout(&address, CONNECT) {
            Ok(stream) => {
                socket = Some(stream);
                break;
            }
            Err(error) => refusal = error.to_string(),
        }
    }
    let socket = socket.ok_or_else(|| {
        format!("连不上星籁服务（{host}:{port}）：{refusal}；请检查网络，或代理是否拦住了直连")
    })?;
    socket.set_read_timeout(Some(read)).ok();
    socket.set_write_timeout(Some(read)).ok();
    let mut stream = connector
        .connect(host.as_str(), socket)
        .map_err(|error| format!("与星籁服务的 TLS 握手失败（{host}）：{error}"))?;

    let mut request = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: {content_type}\r\n\
         Accept: application/json\r\n\
         Content-Length: {}\r\n\
         User-Agent: XingLaiCut/{}\r\n",
        body.len(),
        env!("CARGO_PKG_VERSION"),
    );
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    let mut wire = request.into_bytes();
    wire.extend_from_slice(body);
    stream
        .write_all(&wire)
        .map_err(|error| format!("请求发不出去：{error}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|error| format!("服务器的回复收不全（连接被中途关闭）：{error}"))?;
    read_response(&raw)
}

/// 二进制下载：同一条 OpenSSL 直连，GET 一个接口把正文写进 `dest`。给整剧
/// zip（`/aidrama/download/{id}`，带 Bearer）用。返回落盘字节数。
#[cfg(windows)]
pub fn api_download(
    url: &str,
    token: Option<&str>,
    dest: &std::path::Path,
    read: Duration,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<u64, String> {
    use std::io::{Read, Write};
    use std::net::ToSocketAddrs;

    use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode};

    let (host, port, path) = split_url(url)?;

    let mut builder = SslConnector::builder(SslMethod::tls())
        .map_err(|error| format!("TLS 初始化失败：{error}"))?;
    builder.set_verify(SslVerifyMode::PEER);
    let mut store = openssl::x509::store::X509StoreBuilder::new()
        .map_err(|error| format!("证书库初始化失败：{error}"))?;
    let system = rustls_native_certs::load_native_certs();
    for cert in system.certs {
        if let Ok(x509) = openssl::x509::X509::from_der(&cert) {
            let _ = store.add_cert(x509);
        }
    }
    builder.set_cert_store(store.build());
    let connector = builder.build();

    let addresses = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| format!("地址解析失败（网络或 DNS）：{error}"))?;
    let mut refusal = String::new();
    let mut socket = None;
    for address in addresses {
        match std::net::TcpStream::connect_timeout(&address, CONNECT) {
            Ok(stream) => {
                socket = Some(stream);
                break;
            }
            Err(error) => refusal = error.to_string(),
        }
    }
    let socket = socket.ok_or_else(|| {
        format!("连不上星籁服务（{host}:{port}）：{refusal}；请检查网络")
    })?;
    socket.set_read_timeout(Some(read)).ok();
    socket.set_write_timeout(Some(read)).ok();
    let mut stream = connector
        .connect(host.as_str(), socket)
        .map_err(|error| format!("与星籁服务的 TLS 握手失败（{host}）：{error}"))?;

    let mut request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Accept: */*\r\n\
         User-Agent: XingLaiCut/{}\r\n",
        env!("CARGO_PKG_VERSION"),
    );
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("请求发不出去：{error}"))?;

    let mut raw = Vec::new();
    let mut buffer = [0u8; 65536];
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("已取消下载".to_owned());
        }
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                raw.extend_from_slice(&buffer[..read]);
                if raw.len() > 512 * 1024 * 1024 {
                    return Err("下载的整剧包超过 512MB，已停止".to_owned());
                }
            }
            Err(error) => return Err(format!("下载收不全（连接被中途关闭）：{error}")),
        }
    }
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "服务器的回复不完整".to_owned())?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = &raw[split + 4..];
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let chunked = head.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    if !(200..300).contains(&status) {
        let text = String::from_utf8_lossy(body);
        let value = serde_json::from_str::<serde_json::Value>(&text).ok();
        let message = value
            .as_ref()
            .and_then(|value| value.get("hint"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                value
                    .as_ref()
                    .and_then(|value| value.get("error"))
                    .and_then(serde_json::Value::as_str)
            })
            .map(str::to_owned)
            .unwrap_or_else(|| format!("整剧下载失败（HTTP {status}）"));
        return Err(message);
    }
    let bytes = if chunked {
        decode_chunk_bytes(body)
    } else {
        body.to_vec()
    };
    std::fs::write(dest, &bytes).map_err(|error| format!("存不下整剧包：{error}"))?;
    Ok(bytes.len() as u64)
}

/// 别的平台：短剧整剧下载只在 Windows 桌面版上做（平台的 TLS 形状要求）。
#[cfg(not(windows))]
pub fn api_download(
    _url: &str,
    _token: Option<&str>,
    _dest: &std::path::Path,
    _read: Duration,
    _cancel: &std::sync::atomic::AtomicBool,
) -> Result<u64, String> {
    Err("短剧整剧下载只在 Windows 桌面版可用".to_owned())
}

/// 二进制正文的 chunked 解码。
#[cfg(windows)]
fn decode_chunk_bytes(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < body.len() {
        let Some(line_end) = body[at..].windows(2).position(|window| window == b"\r\n") else {
            break;
        };
        let size = std::str::from_utf8(&body[at..at + line_end])
            .ok()
            .and_then(|line| usize::from_str_radix(line.trim(), 16).ok())
            .unwrap_or(0);
        at += line_end + 2;
        if size == 0 || at + size > body.len() {
            break;
        }
        out.extend_from_slice(&body[at..at + size]);
        at += size;
        if body[at..].starts_with(b"\r\n") {
            at += 2;
        }
    }
    out
}

/// 别的平台：工程云同步只在 Windows 桌面版上做（平台的 TLS 形状要求）。
#[cfg(not(windows))]
pub fn api_call(
    method: &str,
    _url: &str,
    _content_type: &str,
    _body: &[u8],
    _token: Option<&str>,
    _read: Duration,
) -> Result<(u16, String), String> {
    Err(format!("工程云同步只在 Windows 桌面版可用（{method}）"))
}

/// `https://host[:port]/path` 拆成三样。
#[cfg(windows)]
fn split_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("API 地址必须从 https:// 开始：{url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host.to_owned(),
            port.parse::<u16>()
                .map_err(|_| format!("API 地址的端口不对：{authority}"))?,
        ),
        None => (authority.to_owned(), 443),
    };
    if host.is_empty() {
        return Err(format!("API 地址里没有主机名：{url}"));
    }
    Ok((host, port, path.to_owned()))
}

/// 把一份 HTTP/1.1 回复切成状态与正文，chunked 也认。
#[cfg(windows)]
fn read_response(raw: &[u8]) -> Result<(u16, String), String> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "服务器的回复不完整".to_owned())?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = &raw[split + 4..];
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let chunked = head.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    let text = if chunked {
        decode_chunks(body)
    } else {
        String::from_utf8_lossy(body).into_owned()
    };
    Ok((status, text))
}

/// chunked 的正文：每段一块，块前是十六进制的长度。
#[cfg(windows)]
fn decode_chunks(body: &[u8]) -> String {
    let mut out = Vec::new();
    let mut at = 0;
    while at < body.len() {
        let Some(line_end) = body[at..].windows(2).position(|window| window == b"\r\n") else {
            break;
        };
        let size = std::str::from_utf8(&body[at..at + line_end])
            .ok()
            .and_then(|line| usize::from_str_radix(line.trim(), 16).ok())
            .unwrap_or(0);
        at += line_end + 2;
        if size == 0 {
            break;
        }
        let end = (at + size).min(body.len());
        out.extend_from_slice(&body[at..end]);
        at = end;
        if body.get(at..at + 2) == Some(b"\r\n") {
            at += 2;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 别的平台：走 Concat 自己的网络层。
#[cfg(not(windows))]
fn login(body: &str) -> Result<(u16, String), String> {
    let response = net::post(
        &login_url(),
        body,
        CONNECT,
        READ,
        Trust::System,
        &|request| request,
    )
    .map_err(|failure| format!("连不上星籁账号服务：{}", failure.error))?;
    let status = response.status();
    let text = response
        .into_string()
        .map_err(|error| format!("服务器的回复读不出来：{error}"))?;
    Ok((status, text))
}

/// 读登录接口的回复。
pub fn account_from_json(text: &str) -> Result<Account, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("登录回复不是 JSON：{error}"))?;
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
        return Err(value
            .get("hint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("登录失败")
            .to_owned());
    }
    let token = value
        .get("token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "登录回复里没有令牌".to_owned())?;
    let user = value.get("user").cloned().unwrap_or_default();
    let field = |name: &str| {
        user.get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Ok(Account {
        name: field("name"),
        phone: field("phone"),
        plan: field("plan"),
        plan_name: field("plan_name"),
        plan_expire_at: field("plan_expire_at"),
        role: field("role"),
        is_admin: user
            .get("is_admin")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            || field("role").eq_ignore_ascii_case("admin"),
        token: token.to_owned(),
    })
}

/// 会话文件：配置目录里的 `account.bin`，DPAPI 加密过。
pub fn session_file() -> Result<PathBuf, String> {
    Ok(AppDirs::locate()?.config.join("account.bin"))
}

/// 加密后写进配置目录。旧的先写新的再改名，写到一半断电不会留下半个文件。
fn save(account: &Account) -> Result<(), String> {
    let path = session_file()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("账号目录建不出来：{error}"))?;
    }
    let json = serde_json::to_vec(account).map_err(|error| format!("会话序列化失败：{error}"))?;
    let sealed = dpapi::protect(&json)?;
    let temporary = path.with_extension("bin.tmp");
    std::fs::write(&temporary, sealed).map_err(|error| format!("登录信息写不进去：{error}"))?;
    std::fs::rename(&temporary, &path).map_err(|error| format!("登录信息换不进去：{error}"))
}

/// 上次登录如果还在，就拿回来；解不开当作没登录。
pub fn load() -> Option<Account> {
    let path = session_file().ok()?;
    let sealed = std::fs::read(path).ok()?;
    let json = dpapi::unprotect(&sealed).ok()?;
    serde_json::from_slice(&json).ok()
}

/// 退出登录：把本机那份会话删掉。
pub fn sign_out() {
    if let Ok(path) = session_file() {
        let _ = std::fs::remove_file(path);
    }
}

/// DPAPI：Windows 让当前用户自己的加密，别的用户解不开。
#[cfg(windows)]
mod dpapi {
    use std::ffi::c_void;
    use std::ptr;

    /// `DATA_BLOB`。
    #[repr(C)]
    struct Blob {
        size: u32,
        data: *mut u8,
    }

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            input: *const Blob,
            description: *const u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
        fn CryptUnprotectData(
            input: *const Blob,
            description: *mut *mut u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    /// 不弹 UI：后台线程里没有人按「确定」。
    const UI_FORBIDDEN: u32 = 0x1;

    /// 加密一段字节，返回密文。
    pub fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
        // CryptoAPI 不写输入，只读它；可变切片只是为了凑一个非 const 指针。
        let mut input = plain.to_vec();
        let input_blob = Blob {
            size: u32::try_from(input.len()).map_err(|_| "要保存的令牌太长".to_owned())?,
            data: input.as_mut_ptr(),
        };
        let mut output = Blob {
            size: 0,
            data: ptr::null_mut(),
        };
        // SAFETY: 两个 BLOB 都指向活到调用之后的缓冲；输出的指针由
        // CryptoAPI 分配，下面复制走再 LocalFree。
        let ok = unsafe {
            CryptProtectData(
                &input_blob,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(format!("DPAPI 加密失败（{}）", last_error()));
        }
        Ok(take(output))
    }

    /// 解一段密文回来；别处加密的直接失败。
    pub fn unprotect(sealed: &[u8]) -> Result<Vec<u8>, String> {
        let mut input = sealed.to_vec();
        let input_blob = Blob {
            size: u32::try_from(input.len()).map_err(|_| "保存的登录信息太长".to_owned())?,
            data: input.as_mut_ptr(),
        };
        let mut output = Blob {
            size: 0,
            data: ptr::null_mut(),
        };
        // SAFETY: 同 protect。
        let ok = unsafe {
            CryptUnprotectData(
                &input_blob,
                ptr::null_mut(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(format!("DPAPI 解密失败（{}）", last_error()));
        }
        Ok(take(output))
    }

    /// 把 CryptoAPI 分配的 BLOB 复制成 `Vec`，然后还回去。
    fn take(output: Blob) -> Vec<u8> {
        if output.data.is_null() || output.size == 0 {
            return Vec::new();
        }
        // SAFETY: 成功的调用给出 size 个有效字节。
        let bytes =
            unsafe { std::slice::from_raw_parts(output.data, output.size as usize) }.to_vec();
        // SAFETY: 这块内存是 CryptProtectData 用 LocalAlloc 给的。
        unsafe { LocalFree(output.data.cast::<c_void>()) };
        bytes
    }

    /// 系统给的错误码。
    fn last_error() -> u32 {
        std::io::Error::last_os_error()
            .raw_os_error()
            .map_or(0, |code| u32::try_from(code).unwrap_or(0))
    }
}

/// 别处：没有 DPAPI，就明说不能登录，而不是把令牌明文写下来。
#[cfg(not(windows))]
mod dpapi {
    /// 加密：别的平台上没有 DPAPI。
    pub fn protect(_plain: &[u8]) -> Result<Vec<u8>, String> {
        Err("星籁账号加密（DPAPI）只在 Windows 上可用".to_owned())
    }

    /// 解密：别的平台上没有 DPAPI。
    pub fn unprotect(_sealed: &[u8]) -> Result<Vec<u8>, String> {
        Err("星籁账号加密（DPAPI）只在 Windows 上可用".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{Account, account_from_json, dpapi};

    #[test]
    fn a_sealed_secret_opens_again() {
        let sealed = dpapi::protect(b"a token nobody else may read").unwrap();
        assert_ne!(sealed, b"a token nobody else may read");
        assert_eq!(
            dpapi::unprotect(&sealed).unwrap(),
            b"a token nobody else may read"
        );
    }

    #[test]
    fn the_reply_is_read_as_the_platform_sends_it() {
        let reply = r#"{"ok":true,"token":"abc.def","expires_at":"2026-11-25 10:01:56",
            "user":{"id":3,"name":"测试商家A店","phone":"13900000002",
            "plan":"basic","plan_name":"基础版","plan_expire_at":"2026-12-17 19:02:38"}}"#;
        let account = account_from_json(reply).unwrap();
        assert_eq!(account.token, "abc.def");
        assert_eq!(account.display_name(), "测试商家A店");
        assert_eq!(account.plan_label(), "基础版");
    }

    #[test]
    fn a_refusal_is_read_as_a_sentence() {
        let reply = r#"{"ok":false,"error":"bad_credentials","hint":"账号或密码错误"}"#;
        assert_eq!(account_from_json(reply).unwrap_err(), "账号或密码错误");
    }

    #[test]
    fn a_password_refusal_is_said_in_chinese() {
        assert_eq!(
            super::refusal(401, r#"{"ok":false,"error":"bad_credentials"}"#),
            "手机号或密码不对，请重试"
        );
        assert_eq!(
            super::refusal(
                401,
                r#"{"ok":false,"error":"bad_credentials","hint":"账号或密码错误"}"#
            ),
            "账号或密码错误"
        );
        assert!(super::refusal(503, "busy").contains("503"));
    }

    #[test]
    fn a_name_falls_back_to_the_phone() {
        let account = Account {
            phone: "13900000002".to_owned(),
            ..Account::default()
        };
        assert_eq!(account.display_name(), "13900000002");
        assert_eq!(Account::default().plan_label(), "免费版");
    }

    #[cfg(windows)]
    #[test]
    fn a_chunked_reply_is_put_back_together() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"ok\r\n9\r\n\":true}\r\n0\r\n\r\n";
        let (status, body) = super::read_response(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "{\"ok\":true}");
    }

    #[cfg(windows)]
    #[test]
    fn an_api_address_splits_into_host_port_and_path() {
        let (host, port, path) =
            super::split_url("https://xinglaiai.com/ai-nav/api/auth/login").unwrap();
        assert_eq!(host, "xinglaiai.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/ai-nav/api/auth/login");
        let (host, port, _) = super::split_url("https://example.test:8443/api").unwrap();
        assert_eq!((host.as_str(), port), ("example.test", 8443));
        assert!(super::split_url("http://xinglaiai.com/api").is_err());
    }

    /// 真的连一次平台；要网络，测试用 `--ignored` 点名跑。
    #[test]
    #[ignore = "hits the platform's network"]
    fn live_sign_in() {
        let account = super::sign_in("13900000002", "Test@12345").unwrap();
        assert!(!account.token.is_empty());
        assert_eq!(account.phone, "13900000002");
        let wrong = super::sign_in("13900000002", "not-the-password").unwrap_err();
        assert!(wrong.contains("密码"), "{wrong}");
    }
}
