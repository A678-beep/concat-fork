// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 本机算力：显卡、显存、CPU、内存、磁盘，以及由它们得出的「智能选择」档位。
//!
//! 桌面版默认按这台机器的能力决定 AI 跑在哪里：独显够大就本地跑（快），
//! 独显偏小或内存刚好够就本地跑但提示慢，核显/小显存/小内存则默认推荐
//! 云端。判档规则由产品定下，写死在 [`tier`] 里，界面上把理由一并说清。
//!
//! 显卡信息在 Windows 上从注册表的显示适配器类键读：`DriverDesc` 是名字，
//! `HardwareInformation.qwMemorySize` 是显存字节数（比 WMI 的 AdapterRAM
//! 可靠，后者对 6GB 以上的卡会截成 4GB 以下）。核显的 qwMemorySize 报的是
//! 共享内存，不可当显存用，所以 Intel 非 Arc 与 AMD 的 "Radeon(TM) Graphics"
//! 一律按核显算。
//!
//! 别处（macOS/Linux/手机）没有这条注册表路径：显卡未知、按云端推荐算，
//! 桌面版的实际发行只有 Windows。

use std::path::Path;

/// 本机跑 AI 的档位。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tier {
    /// 独立显卡够大、内存也够：默认本地，快。
    LocalFast,
    /// 独显 4–6GB 或内存刚好 16GB：本地能跑，数字人偏慢。
    LocalSlow,
    /// 核显、显存不足 4GB 或内存不足 16GB：默认推荐云端。
    #[default]
    CloudRec,
}

impl Tier {
    /// 存档与界面用的键：`local-fast` / `local-slow` / `cloud-rec`。
    pub fn key(self) -> &'static str {
        match self {
            Tier::LocalFast => "local-fast",
            Tier::LocalSlow => "local-slow",
            Tier::CloudRec => "cloud-rec",
        }
    }

    /// 这一档在界面上的短名字。
    pub fn label(self) -> &'static str {
        match self {
            Tier::LocalFast => "本地可用（快）",
            Tier::LocalSlow => "本地可用（较慢）",
            Tier::CloudRec => "建议用云端",
        }
    }

    /// 从存档里的键读回来；认不出的一律当作智能（由硬件重新判）。
    pub fn from_key(key: &str) -> Option<Tier> {
        match key.trim() {
            "local-fast" => Some(Tier::LocalFast),
            "local-slow" => Some(Tier::LocalSlow),
            "cloud-rec" => Some(Tier::CloudRec),
            _ => None,
        }
    }
}

/// 一张显卡。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gpu {
    /// 驱动给的名字，`AMD Radeon RX 6600`。
    pub name: String,
    /// `NVIDIA` / `AMD` / `Intel` / `其他`。
    pub vendor: String,
    /// 专用显存，MB。核显为 0。
    pub vram_mb: u64,
    /// 独立显卡。
    pub discrete: bool,
}

/// 这台机器的一次检测结果。
#[derive(Clone, Debug, Default)]
pub struct Hardware {
    /// CPU 的商品名。
    pub cpu_name: String,
    /// 逻辑线程数。
    pub cpu_threads: usize,
    /// 物理内存，MB。
    pub ram_mb: u64,
    /// 挑出来的主显卡。
    pub gpu: Gpu,
    /// 系统盘剩余空间，GB。
    pub disk_free_gb: u64,
    /// 由上面几样判出来的档位。
    pub tier: Tier,
}

impl Hardware {
    /// 现在检测一次。进程活着的这段时间里硬件不会变，调用方自己缓存。
    pub fn detect() -> Hardware {
        let cpu_name = cpu_name().unwrap_or_else(|| std::env::consts::ARCH.to_owned());
        let cpu_threads = std::thread::available_parallelism().map_or(0, |n| n.get());
        let ram_mb = crate::memory::total().map_or(0, |bytes| bytes / (1024 * 1024));
        let gpu = primary_gpu();
        let disk_free_gb = disk_free_gb(&config_root());
        let machine = Hardware {
            cpu_name,
            cpu_threads,
            ram_mb,
            gpu,
            disk_free_gb,
            tier: Tier::CloudRec,
        };
        let tier = tier(&machine);
        Hardware { tier, ..machine }
    }

    /// 界面上的一句话：CPU、内存、显卡、显存、磁盘。
    pub fn summary(&self) -> String {
        let gpu = if self.gpu.name.trim().is_empty() {
            "未识别显卡".to_owned()
        } else if self.gpu.vram_mb > 0 {
            format!("{} · {}GB 显存", self.gpu.name, mb_to_gb(self.gpu.vram_mb))
        } else {
            self.gpu.name.clone()
        };
        format!(
            "{} · {} 线程 · {}GB 内存 · {} · 系统盘剩 {}GB",
            self.cpu_name,
            self.cpu_threads,
            mb_to_gb(self.ram_mb),
            gpu,
            self.disk_free_gb
        )
    }

    /// 判档的理由，一句中文。
    pub fn reason(&self) -> String {
        let gpu = if self.gpu.name.trim().is_empty() {
            "未识别显卡".to_owned()
        } else if self.gpu.vram_mb > 0 {
            format!("{} · {}GB 显存", self.gpu.name, mb_to_gb(self.gpu.vram_mb))
        } else {
            self.gpu.name.clone()
        };
        match self.tier {
            Tier::LocalFast => format!(
                "检测到 {gpu}、{}GB 内存，本地推理够快：默认本地生成，素材不出本机",
                mb_to_gb(self.ram_mb)
            ),
            Tier::LocalSlow => format!(
                "检测到 {gpu}、{}GB 内存：本地配音很快；本地数字人可跑但较慢（约 {} 分钟/条），默认本地，也可在设置里切到云端",
                mb_to_gb(self.ram_mb),
                avatar_minutes(self)
            ),
            Tier::CloudRec => format!(
                "检测到 {gpu}、{}GB 内存：本地推理偏吃力，默认推荐云端（需登录星籁账号，按套餐额度计费）",
                mb_to_gb(self.ram_mb)
            ),
        }
    }

    /// 本地配音的预估：Piper 在 CPU 上是实时的几倍，一条 15 秒音频约几秒。
    pub fn tts_estimate(&self) -> String {
        if self.tier == Tier::CloudRec {
            "本地配音仍可运行（约 2–6 秒/条），只是数字人不建议本地".to_owned()
        } else {
            "本地配音约 2–6 秒/条（是音频时长的 0.3–0.6 倍）".to_owned()
        }
    }

    /// 本地数字人的预估耗时档。
    pub fn avatar_minutes(&self) -> (u64, u64) {
        match self.tier {
            Tier::LocalFast => (2, 6),
            Tier::LocalSlow => (8, 20),
            Tier::CloudRec => (20, 60),
        }
    }

    /// 本地数字人估时的中文说法。
    pub fn avatar_estimate(&self) -> String {
        let (low, high) = self.avatar_minutes();
        format!("本地数字人约 {low}–{high} 分钟/条（10 秒片段，CPU 回退更慢）")
    }
}

/// 由硬件判档。规则是产品定的，改这里就是改规则。
fn tier(machine: &Hardware) -> Tier {
    let ram_gb = machine.ram_mb / 1024;
    let gpu = &machine.gpu;
    // 显卡厂商给的显存字节数比标称的少一点（RX 6600 的 8GB 报 8,573,157,376
    // 字节，合 8176MB），所以边界留 128MB 的余量：真正的 8GB 卡算 8GB。
    const SLACK: u64 = 128;
    let vram = gpu.vram_mb;
    if !gpu.discrete || ram_gb < 16 || vram + SLACK < 4096 {
        return Tier::CloudRec;
    }
    let fast = match gpu.vendor.as_str() {
        "NVIDIA" => vram + SLACK >= 6144,
        "AMD" => vram + SLACK >= 8192,
        _ => false,
    };
    if fast && ram_gb >= 16 {
        Tier::LocalFast
    } else {
        Tier::LocalSlow
    }
}

fn mb_to_gb(mb: u64) -> String {
    if mb >= 1024 {
        // 显卡厂商标的「8GB」在注册表里常是 8,573,157,376 字节（8176MB，
        // 驱动留了一点），内存也常是 31.8GB 而不是 32；界面上按整数 GB 说。
        format!("{}", (mb as f64 / 1024.0).round() as u64)
    } else {
        format!("{mb}MB")
    }
}

fn avatar_minutes(machine: &Hardware) -> String {
    let (low, high) = machine.avatar_minutes();
    format!("{low}–{high}")
}

/// 配置目录所在的盘。Windows 上就是系统盘。
fn config_root() -> std::path::PathBuf {
    crate::dirs::AppDirs::locate()
        .map(|dirs: crate::dirs::AppDirs| dirs.config)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// 磁盘剩余空间，GB。拿不到就是 0。
#[cfg(windows)]
fn disk_free_gb(path: &Path) -> u64 {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let text = path.to_string_lossy();
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    // SAFETY: the path is NUL-terminated; the two optional outputs are
    // null and the one we ask for is a valid u64.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        0
    } else {
        free / (1024 * 1024 * 1024)
    }
}

/// 别处：磁盘剩余空间用 `statvfs` 读根目录。
#[cfg(unix)]
fn disk_free_gb(path: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    let Ok(cpath) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return 0;
    };
    // SAFETY: `statvfs` fills the struct it is given and writes nothing else.
    unsafe {
        let mut info: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(cpath.as_ptr(), &mut info) != 0 {
            return 0;
        }
        (info.f_bavail as u64)
            .saturating_mul(info.f_frsize)
            .saturating_div(1024 * 1024 * 1024)
    }
}

#[cfg(not(any(windows, unix)))]
fn disk_free_gb(_path: &Path) -> u64 {
    0
}

/// CPU 的商品名：Windows 从注册表，别的平台尽力而为。
#[cfg(windows)]
fn cpu_name() -> Option<String> {
    registry_string(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString")
        .map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(target_vendor = "apple")]
fn cpu_name() -> Option<String> {
    let mut value = [0u8; 256];
    let mut len = value.len();
    // SAFETY: the name is NUL-terminated and the buffer is ours.
    let status = unsafe {
        libc::sysctlbyname(
            c"machdep.cpu.brand_string".as_ptr(),
            value.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&value[..len.saturating_sub(1)]).into_owned();
    (!text.trim().is_empty()).then(|| text.trim().to_owned())
}

#[cfg(all(not(windows), not(target_vendor = "apple")))]
fn cpu_name() -> Option<String> {
    let text = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, name)| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// 主显卡：显存最大的那张独显。没有独显就报第一张认出来的显卡。
fn primary_gpu() -> Gpu {
    let gpus = gpu_list();
    let mut discrete: Vec<Gpu> = gpus.iter().filter(|gpu| gpu.discrete).cloned().collect();
    discrete.sort_by_key(|gpu| std::cmp::Reverse(gpu.vram_mb));
    if let Some(best) = discrete.into_iter().next() {
        return best;
    }
    gpus.into_iter().next().unwrap_or_default()
}

#[cfg(windows)]
fn gpu_list() -> Vec<Gpu> {
    let mut out = Vec::new();
    let root = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    for name in registry_subkeys(root) {
        let key = format!(r"{root}\{name}");
        let Some(desc) = registry_string(&key, "DriverDesc") else {
            continue;
        };
        let desc = desc.trim().to_owned();
        if desc.is_empty() {
            continue;
        }
        let vram = registry_u64(&key, "HardwareInformation.qwMemorySize").unwrap_or(0)
            / (1024 * 1024);
        out.push(classify_gpu(desc, vram));
    }
    out
}

#[cfg(not(windows))]
fn gpu_list() -> Vec<Gpu> {
    Vec::new()
}

/// 由名字认厂商、分独显核显，并把核显的共享显存按 0 计。
fn classify_gpu(name: String, vram_mb: u64) -> Gpu {
    let lower = name.to_ascii_lowercase();
    let (vendor, discrete) = if lower.contains("nvidia")
        || lower.contains("geforce")
        || lower.contains("quadro")
        || lower.contains("rtx ")
    {
        ("NVIDIA", true)
    } else if lower.contains("amd") || lower.contains("radeon") || lower.contains("ati ") {
        // AMD 的 APU 也报 "Radeon(TM) Graphics"；有 RX / Pro 字样或显存
        // 达到 4GB 才是真独显。
        let discrete = lower.contains("rx ")
            || lower.contains("rx6")
            || lower.contains("pro ")
            || lower.contains("instinct")
            || vram_mb >= 4096;
        ("AMD", discrete)
    } else if lower.contains("intel") || lower.contains("iris") || lower.contains("uhd") {
        let discrete = lower.contains("arc");
        ("Intel", discrete)
    } else if lower.contains("microsoft") || lower.contains("basic") || lower.contains("remote") {
        ("其他", false)
    } else {
        ("其他", vram_mb >= 4096)
    };
    Gpu {
        name,
        vendor: vendor.to_owned(),
        vram_mb: if discrete { vram_mb } else { 0 },
        discrete,
    }
}

/// Windows：读 HKEY_LOCAL_MACHINE 下一个字符串值。
#[cfg(windows)]
fn registry_string(key: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    let key = wide(key);
    let value = wide(value);
    let mut buf = [0u16; 512];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: the buffers are NUL-terminated and `size` is the output
    // buffer's length in bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buf.len());
    let text = String::from_utf16_lossy(&buf[..len]);
    let text = text.trim_matches(char::from(0)).trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Windows：读 HKEY_LOCAL_MACHINE 下一个 QWORD 值。
#[cfg(windows)]
fn registry_u64(key: &str, value: &str) -> Option<u64> {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_QWORD, RegGetValueW};
    let key = wide(key);
    let value = wide(value);
    let mut out = 0u64;
    let mut size = 8u32;
    // SAFETY: as in `registry_string`, with an eight-byte output.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_QWORD,
            std::ptr::null_mut(),
            (&mut out as *mut u64).cast(),
            &mut size,
        )
    };
    (status == 0 && out > 0).then_some(out)
}

/// Windows：HKEY_LOCAL_MACHINE 下一个键的子键名。
#[cfg(windows)]
fn registry_subkeys(key: &str) -> Vec<String> {
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    };
    let path = wide(key);
    let mut handle = std::ptr::null_mut();
    // SAFETY: the path is NUL-terminated, the handle is ours to close.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            path.as_ptr(),
            0,
            KEY_READ | KEY_WOW64_64KEY,
            &mut handle,
        )
    };
    if opened != 0 || handle.is_null() {
        return Vec::new();
    }
    let mut names = Vec::new();
    let mut index = 0u32;
    loop {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        // SAFETY: the name buffer is ours; the optional class/time arguments
        // are null, which the API allows.
        let status = unsafe {
            RegEnumKeyExW(
                handle,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..len as usize]));
        index += 1;
        if index > 64 {
            break;
        }
    }
    // SAFETY: the handle came from RegOpenKeyExW and is closed once.
    unsafe { RegCloseKey(handle) };
    names
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(vendor: &str, vram_mb: u64, ram_mb: u64, discrete: bool) -> Hardware {
        Hardware {
            cpu_name: "test".to_owned(),
            cpu_threads: 8,
            ram_mb,
            gpu: Gpu {
                name: "test gpu".to_owned(),
                vendor: vendor.to_owned(),
                vram_mb,
                discrete,
            },
            disk_free_gb: 100,
            tier: Tier::CloudRec,
        }
    }

    /// 本机（AMD RX 6600 4GB / 32GB 内存）按产品规则应判 local-slow。
    #[test]
    fn a_small_discrete_card_is_local_slow() {
        let machine = machine("AMD", 4096, 32 * 1024, true);
        assert_eq!(tier(&machine), Tier::LocalSlow);
    }

    /// 本机（AMD RX 6600 8GB / 32GB 内存）按产品规则应判 local-fast：
    /// 驱动报 8,573,157,376 字节，合 8176MB，边界要留余量。
    #[test]
    fn a_reported_eight_gigabyte_amd_card_is_local_fast() {
        assert_eq!(
            tier(&machine("AMD", 8176, 32 * 1024, true)),
            Tier::LocalFast
        );
        assert_eq!(mb_to_gb(8176), "8");
        assert_eq!(mb_to_gb(32558), "32");
    }

    #[test]
    fn a_big_card_is_local_fast() {
        assert_eq!(
            tier(&machine("NVIDIA", 8192, 32 * 1024, true)),
            Tier::LocalFast
        );
        assert_eq!(
            tier(&machine("AMD", 8192, 16 * 1024, true)),
            Tier::LocalFast
        );
        // NVIDIA 6GB 就够快，AMD 要 8GB。
        assert_eq!(
            tier(&machine("NVIDIA", 6144, 16 * 1024, true)),
            Tier::LocalFast
        );
        assert_eq!(
            tier(&machine("NVIDIA", 4096, 32 * 1024, true)),
            Tier::LocalSlow
        );
    }

    #[test]
    fn an_integrated_gpu_or_small_memory_recommends_cloud() {
        assert_eq!(
            tier(&machine("Intel", 0, 32 * 1024, false)),
            Tier::CloudRec
        );
        assert_eq!(
            tier(&machine("NVIDIA", 8192, 8 * 1024, true)),
            Tier::CloudRec
        );
        assert_eq!(
            tier(&machine("AMD", 2048, 32 * 1024, true)),
            Tier::CloudRec
        );
    }

    #[test]
    fn a_gpu_is_read_by_its_name() {
        let nvidia = classify_gpu("NVIDIA GeForce RTX 4070".to_owned(), 12288);
        assert_eq!((nvidia.vendor.as_str(), nvidia.discrete), ("NVIDIA", true));
        let apu = classify_gpu("AMD Radeon(TM) Graphics".to_owned(), 512);
        assert_eq!((apu.vendor.as_str(), apu.discrete), ("AMD", false));
        let rx = classify_gpu("AMD Radeon RX 6600".to_owned(), 4096);
        assert_eq!((rx.vendor.as_str(), rx.discrete), ("AMD", true));
        assert_eq!(rx.vram_mb, 4096);
        let arc = classify_gpu("Intel(R) Arc(TM) A770".to_owned(), 8192);
        assert_eq!((arc.vendor.as_str(), arc.discrete), ("Intel", true));
        assert!(!classify_gpu("Intel(R) UHD Graphics 770".to_owned(), 0).discrete);
    }
}
