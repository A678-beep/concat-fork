// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 本地 AI 模型目录：有哪些可以装在本机跑的模型、效果多好、要多少显存和
//! 内存、这台机器适不适合。
//!
//! 目录是**静态表**：每个模型的名称、效果星级、大小、显存内存要求、预计
//! 耗时都写在这里，客户端不联网也能把整张表画出来。哪一项现在真的能下载
//! 由插件清单（[`crate::plugins::remote_manifest`]）与 [`crate::plugins::OFFICIAL`]
//! 决定——能下的给下载按钮，没上线的灰着写「即将上线」，点了也不发请求。
//!
//! 「适配本机」按 [`crate::hardware::Hardware`] 现算：要求 CUDA 的模型在
//! AMD 机器上直接说不适用（DirectML 不是 CUDA），显存/内存不够的说差多少。
//! 不适配的模型仍然列出来，但按钮灰着——不硬塞跑不动的半成品。
//!
//! 选型依据见 `docs/本地模型选型.md`。

use crate::hardware::Hardware;

/// 模型属于哪一类，界面上分组说。
pub const KIND_VOICE: &str = "本地配音";
/// 零样本语音克隆。
pub const KIND_CLONE: &str = "语音克隆";
/// 照片/音频 → 说话视频。
pub const KIND_AVATAR: &str = "数字人";
/// 人像抠像（换背景的上半步）。
pub const KIND_CUTOUT: &str = "抠像";
/// 背景生成（云端）。
pub const KIND_BACKGROUND: &str = "背景生成";

/// 目录里的一个模型。
#[derive(Clone, Copy, Debug)]
pub struct ModelSpec {
    /// 与插件包 id、清单键一致：`piper`、`voice-clone`……
    pub id: &'static str,
    /// 界面上叫它什么。
    pub name: &'static str,
    /// 一句话卖点。
    pub summary: &'static str,
    /// 分类，[`KIND_VOICE`] / [`KIND_CLONE`] / [`KIND_AVATAR`]。
    pub kind: &'static str,
    /// 插件 `plugin.json` 里的引擎名。
    pub engine: &'static str,
    /// 效果星级 1–5（同类别里比）。
    pub effect: u8,
    /// 安装包大约多大，字节。
    pub size_bytes: u64,
    /// 需要多少专用显存，MB；0 表示不需要独显。
    pub vram_mb: u64,
    /// 需要多少内存，MB。
    pub ram_mb: u64,
    /// 需要 NVIDIA 显卡（CUDA）。
    pub cuda: bool,
    /// 支持 DirectML（AMD 显卡也能加速）。
    pub directml: bool,
    /// 纯 CPU 也能跑。
    pub cpu_ok: bool,
    /// 预计生成耗时，本机口径。
    pub speed: &'static str,
    /// 一句限制或补充。
    pub note: &'static str,
}

/// 整个目录。顺序就是界面上从上到下的顺序：先能马上用的，再数字人。
pub const MODELS: [ModelSpec; 11] = [
    ModelSpec {
        id: "piper",
        name: "Piper 本地配音",
        summary: "完全离线的中文语音合成，CPU 实时，最稳的一条路",
        kind: KIND_VOICE,
        engine: "piper",
        effect: 2,
        size_bytes: 100_003_538,
        vram_mb: 0,
        ram_mb: 1024,
        cuda: false,
        directml: false,
        cpu_ok: true,
        speed: "约 0.3–0.6× 实时：10 秒音频约 3–6 秒",
        note: "音色固定（华言两个），不支持克隆；胜在便宜、不挑机器",
    },
    ModelSpec {
        id: "voice-clone",
        name: "ZipVoice 零样本克隆",
        summary: "给一段参考录音就能克隆音色，中文英文都行，CPU 可跑",
        kind: KIND_CLONE,
        engine: "zipvoice",
        effect: 4,
        size_bytes: 177_134_608,
        vram_mb: 0,
        ram_mb: 4096,
        cuda: false,
        directml: false,
        cpu_ok: true,
        speed: "约 1–2× 实时：10 秒音频约 6–15 秒（16 线程）",
        note: "开箱即用、文件小；效果接近 CosyVoice2，弱于 GPT-SoVITS 精调",
    },
    ModelSpec {
        id: "gpt-sovits",
        name: "GPT-SoVITS v4",
        summary: "少样本克隆效果最好之一，中文拟真度高",
        kind: KIND_CLONE,
        engine: "gpt-sovits",
        effect: 5,
        size_bytes: 3_200_000_000,
        vram_mb: 6144,
        ram_mb: 12288,
        cuda: true,
        directml: false,
        cpu_ok: true,
        speed: "CPU 约 5–20× 实时；AMD 显卡用不上 CUDA 加速",
        note: "官方整包是 PyTorch + Python 环境，0.2.13 还没插件化",
    },
    ModelSpec {
        id: "cosyvoice2",
        name: "CosyVoice2-0.5B",
        summary: "中文 SOTA 级克隆与情感控制，0.5B 规模",
        kind: KIND_CLONE,
        engine: "cosyvoice",
        effect: 5,
        size_bytes: 4_500_000_000,
        vram_mb: 4096,
        ram_mb: 12288,
        cuda: true,
        directml: false,
        cpu_ok: true,
        speed: "CPU 约 3–8× 实时；AMD 显卡用不上 CUDA 加速",
        note: "PyTorch 整包大、装环境复杂；0.2.13 还没插件化",
    },
    ModelSpec {
        id: "wav2lip",
        name: "Wav2Lip 口型同步",
        summary: "照片 + 配音 → 对口型说话视频，本机离线，DirectML 或 CPU 都能跑",
        kind: KIND_AVATAR,
        engine: "wav2lip",
        effect: 3,
        size_bytes: 258_790_891,
        vram_mb: 2048,
        ram_mb: 8192,
        cuda: false,
        directml: true,
        cpu_ok: true,
        speed: "本机实测：10 秒片段约 6 秒（DirectML）/ 约 38 秒（纯 CPU）",
        note: "只动嘴、不摇头；清晰度取决于照片，效果不如云端数字人（Hedra 级）；装一次以后完全离线",
    },
    ModelSpec {
        id: "sadtalker",
        name: "SadTalker",
        summary: "头部动作 + 眨眼，比 Wav2Lip 自然",
        kind: KIND_AVATAR,
        engine: "sadtalker",
        effect: 4,
        size_bytes: 1_500_000_000,
        vram_mb: 4096,
        ram_mb: 8192,
        cuda: true,
        directml: false,
        cpu_ok: true,
        speed: "CPU 约 10–30 分钟/10 秒；AMD 用不上 CUDA",
        note: "PyTorch 环境；CPU 慢，AMD 显卡不加速",
    },
    ModelSpec {
        id: "musetalk",
        name: "MuseTalk",
        summary: "高质量实时口型，数字人直播级",
        kind: KIND_AVATAR,
        engine: "musetalk",
        effect: 4,
        size_bytes: 3_000_000_000,
        vram_mb: 6144,
        ram_mb: 12288,
        cuda: true,
        directml: false,
        cpu_ok: false,
        speed: "需 CUDA：10 秒片段约 1–3 分钟",
        note: "PyTorch + CUDA 必需；本机 AMD 不能加速",
    },
    ModelSpec {
        id: "echomimicv2",
        name: "EchoMimicV2",
        summary: "半身数字人，效果最好，也最吃显存",
        kind: KIND_AVATAR,
        engine: "echomimic",
        effect: 5,
        size_bytes: 5_000_000_000,
        vram_mb: 12288,
        ram_mb: 16384,
        cuda: true,
        directml: false,
        cpu_ok: false,
        speed: "需 12GB+ 显存，本机 8GB 跑不动",
        note: "本机显存不足，不建议；用云端数字人代替",
    },
    ModelSpec {
        id: "liveportrait",
        name: "LivePortrait 表情迁移",
        summary: "表情/头部动作迁移，效果最自然，但要驱动视频",
        kind: KIND_AVATAR,
        engine: "liveportrait",
        effect: 5,
        size_bytes: 800_000_000,
        vram_mb: 4096,
        ram_mb: 8192,
        cuda: true,
        directml: false,
        cpu_ok: true,
        speed: "需驱动视频；CPU 数十倍实时，CUDA 才实用",
        note: "不是「照片+音频直接出片」，得有一段真人驱动视频",
    },
    ModelSpec {
        id: "cutout",
        name: "人像抠像（rembg isnet）",
        summary: "一键把人像抠成透明背景，离线、CPU 可跑，1080p 单图 1–3 秒",
        kind: KIND_CUTOUT,
        engine: "isnet",
        effect: 4,
        size_bytes: 176_000_000,
        vram_mb: 0,
        ram_mb: 2048,
        cuda: false,
        directml: false,
        cpu_ok: true,
        speed: "1080p 单图约 1–3 秒；视频按帧，1080p 每秒约 3–8 秒",
        note: "桌面端用内置「抠像」功能：首次使用时自动下载 isnet-general-use.onnx（约 176MB），不用另装插件",
    },
    ModelSpec {
        id: "ai-background",
        name: "AI 背景生成（云端）",
        summary: "一句话生成新背景，和抠像合成「换背景」",
        kind: KIND_BACKGROUND,
        engine: "cloud",
        effect: 3,
        size_bytes: 0,
        vram_mb: 0,
        ram_mb: 0,
        cuda: false,
        directml: false,
        cpu_ok: false,
        speed: "云端生成约 10–30 秒/张",
        note: "走网页端同一个接口（POST /api/autoedit/bg/generate）：需登录星籁账号，默认关闭，每次生成前确认；也可自带 302.AI Key",
    },
];

/// 找一条目录项。
pub fn spec(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|model| model.id == id)
}

/// 这台机器能不能跑：`true` 是能（可能慢），`false` 是跑不动。
pub fn fits(spec: &ModelSpec, hardware: &Hardware) -> bool {
    fit_reason(spec, hardware).0
}

/// 能不能跑，以及界面上那句为什么。
///
/// 先看最硬的门槛：CUDA、内存——这两样过不去就是跑不动。显存只对
/// **不能退 CPU** 的模型是硬门槛；能走 CPU 的模型（比如 Wav2Lip）显存
/// 不够只是「慢一些」，照样不适配成红色。
pub fn fit_reason(spec: &ModelSpec, hardware: &Hardware) -> (bool, String) {
    let gpu = &hardware.gpu;
    let discrete = gpu.discrete && gpu.vram_mb > 0;
    if spec.cuda && gpu.vendor != "NVIDIA" {
        let what = if gpu.vendor == "AMD" {
            "AMD 显卡走不了 CUDA"
        } else if !discrete {
            "没有 NVIDIA 独显"
        } else {
            "不是 NVIDIA 显卡"
        };
        return (false, format!("需要 NVIDIA 显卡（{what}）"));
    }
    // 显存门槛：不能退 CPU 的模型、以及目录里按 CUDA 显卡设计的模型，
    // 显存不够就是跑不动；能退 CPU 的模型（Wav2Lip）不够也只是慢一些。
    if spec.vram_mb > 0 && gpu.vram_mb < spec.vram_mb && (!spec.cpu_ok || spec.cuda) {
        return (
            false,
            format!(
                "显存不足：需要 {}GB，本机 {}GB",
                gb(spec.vram_mb),
                gb(gpu.vram_mb)
            ),
        );
    }
    if spec.ram_mb > 0 && hardware.ram_mb > 0 && hardware.ram_mb < spec.ram_mb {
        return (
            false,
            format!(
                "内存不足：需要 {}GB，本机 {}GB",
                gb(spec.ram_mb),
                gb(hardware.ram_mb)
            ),
        );
    }
    // 有显卡加速的：不是纯 CPU，也不是「能跑但慢」。
    if spec.directml && discrete && gpu.vram_mb >= spec.vram_mb.max(1024) {
        return (true, "适配本机（DirectML 显卡加速）".to_owned());
    }
    if spec.cpu_ok && spec.vram_mb > 0 && gpu.vram_mb < spec.vram_mb {
        return (true, "适配本机（纯 CPU，慢一些）".to_owned());
    }
    if spec.vram_mb == 0 && spec.cpu_ok {
        return (true, "适配本机（纯 CPU）".to_owned());
    }
    if spec.directml && gpu.vendor == "AMD" {
        return (true, "适配本机（DirectML）".to_owned());
    }
    (true, "适配本机".to_owned())
}

/// 效果星级画成星：`4` → `★★★★☆`。
pub fn stars(effect: u8) -> String {
    let filled = effect.clamp(0, 5) as usize;
    let mut text = String::new();
    for index in 0..5 {
        text.push(if index < filled { '★' } else { '☆' });
    }
    text
}

/// MB 按整数 GB 说。
fn gb(mb: u64) -> u64 {
    if mb >= 1024 {
        (mb as f64 / 1024.0).round() as u64
    } else {
        mb
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(vendor: &str, vram_mb: u64, ram_mb: u64, discrete: bool) -> Hardware {
        Hardware {
            cpu_name: "test".to_owned(),
            cpu_threads: 16,
            ram_mb,
            gpu: crate::hardware::Gpu {
                name: "test".to_owned(),
                vendor: vendor.to_owned(),
                vram_mb,
                discrete,
            },
            disk_free_gb: 100,
            tier: crate::hardware::Tier::LocalFast,
        }
    }

    /// 本机（RX 6600 8GB / 32GB）上：ZipVoice 能跑，CUDA 模型不能。
    #[test]
    fn an_amd_machine_can_run_cpu_and_directml_but_not_cuda() {
        let amd = machine("AMD", 8176, 32558, true);
        assert!(fits(spec("voice-clone").unwrap(), &amd));
        assert!(fits(spec("piper").unwrap(), &amd));
        assert!(!fits(spec("gpt-sovits").unwrap(), &amd));
        assert!(!fits(spec("musetalk").unwrap(), &amd));
        assert!(!fits(spec("echomimicv2").unwrap(), &amd));
    }

    /// 一张 4GB 的卡跑不动 6GB 的模型，理由里说差多少。
    #[test]
    fn a_small_card_is_told_what_it_lacks() {
        let small = machine("NVIDIA", 4096, 32768, true);
        let (ok, why) = fit_reason(spec("gpt-sovits").unwrap(), &small);
        assert!(!ok);
        assert!(why.contains("6GB"), "{why}");
        assert!(why.contains("4GB"), "{why}");
    }

    /// 核显机器上 Wav2Lip 仍按 CPU 算适配。
    #[test]
    fn an_integrated_machine_still_fits_cpu_models() {
        let integrated = machine("Intel", 0, 16384, false);
        assert!(fits(spec("voice-clone").unwrap(), &integrated));
        let (ok, why) = fit_reason(spec("wav2lip").unwrap(), &integrated);
        assert!(ok, "{why}");
        assert!(why.contains("CPU"), "{why}");
    }

    /// 这台 AMD 机器上 Wav2Lip 走 DirectML，说清楚用的是显卡加速。
    #[test]
    fn an_amd_machine_gets_directml_for_the_avatar() {
        let amd = machine("AMD", 8176, 32558, true);
        let (ok, why) = fit_reason(spec("wav2lip").unwrap(), &amd);
        assert!(ok, "{why}");
        assert!(why.contains("DirectML"), "{why}");
    }

    #[test]
    fn stars_fill_from_the_left() {
        assert_eq!(stars(5), "★★★★★");
        assert_eq!(stars(4), "★★★★☆");
        assert_eq!(stars(0), "☆☆☆☆☆");
    }
}
