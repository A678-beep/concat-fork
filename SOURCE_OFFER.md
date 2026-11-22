# 星籁剪辑（XingLai Cut）源码获取与第三方源码要约（SOURCE OFFER）

版本适用范围：星籁剪辑 0.2.11–0.2.25 Windows 桌面版（基于 Concat 0.2.11 的
AGPL-3.0-or-later 分支）。本文件随安装目录分发，是 AGPLv3 §6 与
LGPLv3 §4 所要求的源码提供方式说明。审计日期：2026-11-22。

## 1. 主程序源码（AGPL-3.0-or-later）

星籁剪辑是 [Concat](https://github.com/jub0t/Concat)（Copyright (C) 2026
Jareer Abdullah and Concat contributors，AGPL-3.0-or-later）的分支。本机构
对它的修改同样以 AGPL-3.0-or-later 授权。

对应源码的获取方式（任选其一，均免费）：

1. **源码仓库**：<https://github.com/A678-beep/concat-fork>，与本机版本
   对应的分支或标签（例如 `v0.2.25`）。仓库包含全部修改、`LICENSES/`、
   构建说明（`docs/REBRAND.md` 第 5 节）、`patches/`（对 Slint Skia 渲染器
   与 whisper.cpp 的修改）以及本文件。
2. **书面要约（Written Offer）**：发邮件到 <dev@xinglaiai.com>，主题写
   「索取源码 + 版本号」，我们将在收到请求后 30 日内免费提供对应版本的
   完整源代码（可下载地址或只读介质，二选一）。本要约自交付之日起
   三年内有效，适用于任何从本机构或授权渠道取得本软件的自然人/组织。
3. 本机安装目录中的 `LICENSE`（AGPL-3.0 全文）与 `THIRD_PARTY_NOTICES.md`
   说明了许可证与第三方组件。

「完整源代码」包括：全部 Rust/Slint 源码、Cargo.lock、构建配置、字体与
内嵌资源、AI 插件的打包/下载清单（数量级：约 20 个 crate + 1 个 Slint
补丁 + 1 个 whisper.cpp 补丁）、以及生成安装包所用的 Inno Setup 脚本。
AI 插件包（Piper/SadTalker/ZipVoice）内含可读的 Python 推理脚本与
`plugin.json`，这些脚本即为对应组件的源码；其打包脚本与下载地址记录在
本文件与 `AI_PLUGINS_NOTICES.md`。

## 2. FFmpeg 对应源码（LGPL-3.0-or-later）

随包的 FFmpeg 8.1 共享库来自 BtbN 的官方构建：

- 构建名：`ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-shared-8.1`
- 版本：`n8.1.3-9-g29e619e767`（2026-09-30 构建）
- 许可：LGPL-3.0-or-later（许可证全文见 `LICENSES/LGPL-3.0.txt`）
- 构建参数（节选，完整串由 `ffmpeg.exe -buildconf` 打印）：
  `--enable-version3 --disable-static --enable-shared --disable-libx264
  --disable-libx265 --disable-libfdk-aac --disable-libxvid --enable-libopenh264
  --enable-libkvazaar --enable-libaom --enable-libdav1d --enable-libvpx
  --enable-libsvtav1 --enable-librav1e --enable-libvvenc`（未启用
  `--enable-gpl`、`--enable-nonfree`）。

FFmpeg 对应源码（含用于本次构建的构建脚本与补丁）：

1. 构建脚本/补丁：<https://github.com/BtbN/FFmpeg-Builds>（`master` 分支，
   对应 2026-09-30 构建；如需精确 commit 可邮件索取，见下）。
2. FFmpeg 源码：<https://ffmpeg.org/download.html> 与
   <https://github.com/FFmpeg/FFmpeg>，版本 `n8.1.3-9-g29e619e767`
   （即 tag `n8.1.3` 之后 9 个提交）。编译配置同上。
3. 静态链接进上述 DLL/exe 的第三方库（如 libaom、dav1d、libvpx、SVT-AV1、
   rav1e、OpenH264、Kvazaar、Opus、Vorbis、libxml2、libzmq 等）均以
   宽松许可或 LGPL 许可发布，其源码地址列于上游 `THIRD_PARTY_NOTICES.md`
   的 "FFmpeg" 一节；需要时亦可邮件索取。
4. **书面要约**：同样可按第 1 节第 2 条的邮箱免费索取上述对应源码
   （本要约自交付之日起三年内有效）。
5. 这些 DLL 位于 `concat.exe` 旁，用户可以用 ABI 兼容的 FFmpeg 构建替换它们；
   这是 LGPLv3 要求的「可重新链接」能力。

## 3. 其他第三方源码

- ONNX Runtime 1.28.0（MIT）：<https://github.com/microsoft/onnxruntime/releases/tag/v1.28.0>
  （随包 DLL 为 Microsoft 原版未修改）。
- Slint 1.18.1（GPL-3.0-only 选项）：<https://github.com/slint-ui/slint>；
  我们对 Skia 渲染器的一处补丁在 <https://github.com/A678-beep/concat-fork>
  的 `src/patches/i-slint-renderer-skia`。
- whisper.cpp（MIT）与 whisper-rs：<https://github.com/ggml-org/whisper.cpp>，
  <https://github.com/tazz4843/whisper-rs>。
- 字体：OFL/Apache 字体文件与版权行在源码
  `src/crates/concat-text/fonts/`；许可全文见 `LICENSES/OFL-1.1.txt`。
- 其余 crates.io 依赖：名称/版本/许可清单见 `LICENSES/cargo-licenses.csv`，
  源码均可从 <https://crates.io> 对应版本免费下载。

## 4. 网络使用（AGPL §13）

本桌面程序本身不是网络服务。星籁账号登录、云端 AI、云同步使用本机构
平台服务，该平台并非由本程序源码派生的网络服务；若未来以修改版 Concat
提供网络交互服务，我们将按 AGPL §13 向该服务的网络用户提供对应源码。
