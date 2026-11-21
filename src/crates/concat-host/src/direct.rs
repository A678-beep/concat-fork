// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 平台自己的 HTTPS 直连下载。
//!
//! 和登录（[`crate::account`]）一样，Windows 上自己开 TCP、用 OpenSSL 谈
//! TLS、不问系统代理：平台的 Aliyun WAF 会重置 rustls/Schannel 形状的
//! ClientHello，只放行 OpenSSL 形状的。下载的音频、插件包都走这里。
//!
//! 别的平台没有这条约束，用 ureq 的默认 TLS；苹果/手机上这些功能本来也
//! 用不到。
//!
//! 下载边写边报进度（字节），可取消：取消时删掉半截文件。

use std::io::{Read, Write};
use std::path::Path;

/// 下载一个 https 地址到 `dest`，进度按已收字节/总字节（总长未知时按 0）。
///
/// 失败时半截文件会被删掉，`dest` 要么是完整的一份，要么不存在。
pub fn download(
    url: &str,
    dest: &Path,
    progress: &mut dyn FnMut(f32, &str),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<u64, String> {
    match fetch(url, dest, progress, cancel, 0, None) {
        Ok(bytes) => Ok(bytes),
        Err(error) => {
            let _ = std::fs::remove_file(dest);
            Err(error)
        }
    }
}

/// 只取文件开头 `max_bytes` 字节的下载，给素材库悬停预览用：HTTP Range
/// （平台 302 到 OSS 后同样带 Range），不整档下载。服务器不认 Range 时也
/// 只读到上限就断开，同样不会把整个文件收下来。失败时半截文件删掉。
pub fn download_range(
    url: &str,
    dest: &Path,
    max_bytes: u64,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<u64, String> {
    let max = max_bytes.max(1);
    match fetch(url, dest, &mut |_, _| {}, cancel, 0, Some(max)) {
        Ok(bytes) => Ok(bytes),
        Err(error) => {
            let _ = std::fs::remove_file(dest);
            Err(error)
        }
    }
}

#[cfg(windows)]
fn fetch(
    url: &str,
    dest: &Path,
    progress: &mut dyn FnMut(f32, &str),
    cancel: &std::sync::atomic::AtomicBool,
    redirects: u8,
    cap: Option<u64>,
) -> Result<u64, String> {
    use std::net::ToSocketAddrs;

    use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode};

    if redirects > 5 {
        return Err("下载地址跳转太多次，停下来了".to_owned());
    }
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
        .map_err(|error| format!("地址解析失败（{host}）：{error}"))?;
    let mut socket = None;
    let mut refusal = String::new();
    for address in addresses {
        match std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(15)) {
            Ok(stream) => {
                socket = Some(stream);
                break;
            }
            Err(error) => refusal = error.to_string(),
        }
    }
    let socket = socket.ok_or_else(|| format!("连不上 {host}：{refusal}"))?;
    socket.set_read_timeout(Some(std::time::Duration::from_secs(120))).ok();
    let mut stream = connector
        .connect(host.as_str(), socket)
        .map_err(|error| format!("TLS 握手失败（{host}）：{error}"))?;
    let range = cap
        .map(|max| format!("Range: bytes=0-{}\r\n", max.saturating_sub(1)))
        .unwrap_or_default();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nUser-Agent: XingLaiCut/{}\r\n{range}Connection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION"),
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("请求发不出去：{error}"))?;
    // 先读到空行为止：状态、头。
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while head.len() < 64 * 1024 {
        match stream.read(&mut byte) {
            Ok(1) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Ok(_) => return Err("服务器的回复不完整".to_owned()),
            Err(error) => return Err(format!("服务器的回复收不全：{error}")),
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let header = |name: &str| {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    if (300..400).contains(&status) {
        let location = header("location").ok_or_else(|| "下载地址跳转没有给出新地址".to_owned())?;
        let next = if location.starts_with("https://") {
            location
        } else if location.starts_with("http://") {
            return Err("下载地址跳到了 http，拒绝".to_owned());
        } else {
            format!("https://{host}{location}")
        };
        return fetch(&next, dest, progress, cancel, redirects + 1, cap);
    }
    if !(200..300).contains(&status) {
        return Err(format!("下载失败（HTTP {status}）"));
    }
    let total = header("content-length")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let mut out = std::fs::File::create(dest)
        .map_err(|error| format!("下载文件写不进去（{}）：{error}", dest.display()))?;
    let mut got = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("已取消".to_owned());
        }
        let read = match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => return Err(format!("下载中途断了：{error}")),
        };
        out.write_all(&buffer[..read])
            .map_err(|error| format!("下载文件写不进去：{error}"))?;
        got += read as u64;
        let fraction = if total > 0 {
            got as f32 / total as f32
        } else {
            0.0
        };
        let detail = if total > 0 {
            format!(
                "正在下载 {:.1} MB / {:.1} MB",
                got as f64 / (1024.0 * 1024.0),
                total as f64 / (1024.0 * 1024.0)
            )
        } else {
            format!("正在下载 {:.1} MB", got as f64 / (1024.0 * 1024.0))
        };
        progress(fraction.clamp(0.0, 1.0), &detail);
    }
    out.flush().map_err(|error| format!("下载文件写不进去：{error}"))?;
    if cap.is_none() && total > 0 && got != total {
        return Err(format!("下载不完整：收到 {got}/{total} 字节"));
    }
    Ok(got)
}

/// `https://host[:port]/path` 拆成三样。
#[cfg(windows)]
fn split_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("下载地址必须从 https:// 开始：{url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host.to_owned(),
            port.parse::<u16>()
                .map_err(|_| format!("下载地址的端口不对：{authority}"))?,
        ),
        None => (authority.to_owned(), 443),
    };
    if host.is_empty() {
        return Err(format!("下载地址里没有主机名：{url}"));
    }
    Ok((host, port, path.to_owned()))
}

/// 别的平台：ureq 的默认 TLS，逐块读出来写盘。
#[cfg(not(windows))]
fn fetch(
    url: &str,
    dest: &Path,
    progress: &mut dyn FnMut(f32, &str),
    cancel: &std::sync::atomic::AtomicBool,
    redirects: u8,
    cap: Option<u64>,
) -> Result<u64, String> {
    if redirects > 5 {
        return Err("下载地址跳转太多次，停下来了".to_owned());
    }
    let range = cap.map(|max| format!("bytes=0-{}", max.saturating_sub(1)));
    let response = match &range {
        Some(range) => ureq::get(url)
            .set("Range", range)
            .timeout(std::time::Duration::from_secs(120))
            .call(),
        None => ureq::get(url).timeout(std::time::Duration::from_secs(120)).call(),
    }
    .map_err(|error| format!("下载失败：{error}"))?;
    let total = response
        .header("content-length")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let mut reader = response.into_reader();
    let mut out = std::fs::File::create(dest)
        .map_err(|error| format!("下载文件写不进去（{}）：{error}", dest.display()))?;
    let mut got = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("已取消".to_owned());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("下载中途断了：{error}"))?;
        if read == 0 {
            break;
        }
        out.write_all(&buffer[..read])
            .map_err(|error| format!("下载文件写不进去：{error}"))?;
        got += read as u64;
        if cap.is_some_and(|max| got >= max) {
            break;
        }
        let fraction = if total > 0 {
            got as f32 / total as f32
        } else {
            0.0
        };
        progress(fraction.clamp(0.0, 1.0), &format!("正在下载 {}", human(got)));
    }
    Ok(got)
}

#[allow(dead_code)]
fn human(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn an_address_splits_into_host_port_and_path() {
        let (host, port, path) = super::split_url("https://example.test:8443/a/b.zip").unwrap();
        assert_eq!((host.as_str(), port, path.as_str()), ("example.test", 8443, "/a/b.zip"));
        assert!(super::split_url("http://example.test/a").is_err());
    }
}
