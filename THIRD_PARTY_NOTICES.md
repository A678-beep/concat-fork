# Third-party notices

## FFmpeg

Concat links FFmpeg's libraries - libavformat, libavcodec, libavfilter,
libavutil, libswscale and libswresample - through the `ffmpeg-the-third`
crate, as shared libraries that ship beside it and can be replaced. The
Slint app spawns no `ffmpeg` or `ffprobe` process.

Every FFmpeg Concat ships is LGPL, with nothing GPL or non-free in it: no
x264, x265 or fdk-aac. H.264, HEVC and AAC are encoded by the platform's own
encoders (VideoToolbox and AudioToolbox, Media Foundation and the GPUs'
encoders, MediaCodec). Which build a binary carries:

- macOS: FFmpeg 8.1 compiled from source by `src/scripts/ffmpeg-macos.sh`,
  LGPL-2.1-or-later, linking libvpx (BSD-3-Clause,
  https://chromium.googlesource.com/webm/libvpx), SVT-AV1 (BSD-3-Clause-Clear
  with the Alliance for Open Media patent licence,
  https://gitlab.com/AOMediaCodec/SVT-AV1), Opus (BSD-3-Clause,
  https://opus-codec.org) and dav1d (BSD-2-Clause,
  https://code.videolan.org/videolan/dav1d), as Homebrew builds them.
- Windows and Linux: BtbN's `lgpl-shared` build
  (https://github.com/BtbN/FFmpeg-Builds), LGPL-3.0-or-later, which links
  the same libraries and libaom (BSD-2-Clause), the GPUs' encoder headers,
  OpenH264 (BSD-2-Clause, https://github.com/cisco/openh264) and Kvazaar
  (BSD-3-Clause, https://github.com/ultravideo/kvazaar). Concat encodes
  H.264 with OpenH264 only where nothing of the platform's does - Linux
  without a GPU encoder, Windows without Media Foundation's - and HEVC with
  Kvazaar only where no GPU encoder or Media Foundation's takes it.
  OpenH264 compiled from source, as BtbN's is, does not carry Cisco's
  patent licence, which covers Cisco's own binary only; neither carries
  any HEVC patent licence.
- Android and iOS: an LGPL-2.1-or-later build compiled from FFmpeg's source
  by `src/scripts/ffmpeg-mobile.sh`, with the platform's codecs and nothing
  else, linked statically.

FFmpeg source code: https://ffmpeg.org/download.html

## whisper.cpp

Transcription compiles whisper.cpp (https://github.com/ggml-org/whisper.cpp,
MIT) and ggml into the app through the `whisper-rs` crate. Whisper models
originate at https://huggingface.co/ggerganov/whisper.cpp (MIT); Concat
mirrors them and downloads them on demand from that mirror, and never
bundles them. See "The model mirror" below.

Concat bundles two models:

- Silero VAD v6.2.0 in whisper.cpp's format (`ggml-silero-v6.2.0.bin`, from
  https://huggingface.co/ggml-org/whisper-vad, MIT; Silero VAD itself is MIT,
  https://github.com/snakers4/silero-vad, Copyright (c) 2020-present Silero
  Team). It finds where the speech is, so captions are timed from it.
- DeepFilterNet 3's ONNX export (`enc.onnx`, `erb_dec.onnx`, `df_dec.onnx`
  from `models/DeepFilterNet3_onnx.tar.gz` at
  https://github.com/Rikorose/DeepFilterNet, Copyright (c) 2021 Hendrik
  Schröter, code and weights dual-licensed MIT or Apache-2.0; Concat takes
  it under MIT). It is voice isolation's model. The licence texts are in
  `src/crates/concat-speech/assets/deepfilternet3/`.

## Slint (GPL-3.0-only, or Royalty-free for commercial builds)

The `concat` crate (`src/crates/concat`) builds against
[Slint](https://github.com/slint-ui/slint), which its authors offer under
**any one** of three licences, at the user's choice: a Royalty-free licence, a
paid commercial licence, or **GNU GPL-3.0-only**.

**Concat as published here, under AGPL-3.0-or-later, uses Slint under the
GPL-3.0-only option.** Builds of Concat distributed under a commercial licence
from its owner use Slint under the Slint Royalty-free Desktop, Mobile, and Web
Applications License 2.0, which the owner accepted on 3 November 2026, with
Slint's attribution badge on the page Concat is downloaded from.

For the GPL-3.0-only option, section 13 of the GPL-3.0 exists for exactly this
combination:

> Notwithstanding any other provision of this License, you have permission to
> link or combine any covered work with a work licensed under version 3 of the
> GNU Affero General Public License into a single combined work, and to convey
> the resulting work.

So the combined binary is conveyable. Slint's portion remains GPL-3.0-only,
Concat's portion remains AGPL-3.0-or-later, and the AGPL's section 13 network
requirement applies to the combination as a whole. Anyone forking Concat takes
Slint under the GPL-3.0 with it; the Royalty-free licence is the owner's, and
does not pass to a fork.

Slint pulls in its Skia renderer (see `src/crates/concat/Cargo.toml`) along
with winit and their transitive crates, which are
predominantly MIT/Apache-2.0/BSD licensed. The Skia renderer crate,
`i-slint-renderer-skia`, is built from a patched copy in
`src/patches/i-slint-renderer-skia`; its changes are listed in
`src/patches/README.md` and marked `Concat:` in the source.
`cargo tree -p concat` gives the resolved set of any given build.

## Fonts

The window and the title painter embed the interface's font into the binary
(`src/crates/concat/build.rs`, `EmbedResourcesKind::EmbedFiles`, and
`include_bytes!` in `concat-text`), so a distributed binary carries it and its
licence travels with it. The files and the full licence text are in
`src/crates/concat-text/fonts/`.

- **Hanken Grotesk**: Copyright 2021 The Hanken Grotesk Project Authors
  (https://github.com/marcologous/hanken-grotesk), SIL Open Font License
  1.1. The Regular, Medium, SemiBold, Bold and Italic static instances are
  embedded. See `fonts/LICENSE-HankenGrotesk.txt`.

These are the base fonts every title can use, embedded by `concat-text`
(`BASE_FONTS`) and registered with the window for the text presets. Each
file's licence is beside it in `src/crates/concat-text/fonts/`, as
`LICENSE-<Family>.txt`; OFL is the SIL Open Font License 1.1.

- **Inter**: Copyright 2016 The Inter Project Authors (https://github.com/rsms/inter). OFL. See `fonts/LICENSE-Inter.txt`.
- **Montserrat**: Copyright 2024 The Montserrat Project Authors (https://github.com/JulietaUla/Montserrat). OFL. See `fonts/LICENSE-Montserrat.txt`.
- **Space Grotesk**: Copyright Florian Karsten. OFL. See `fonts/LICENSE-SpaceGrotesk.txt`.
- **Archivo Black**: Copyright 2017 The Archivo Black Project Authors (https://github.com/Omnibus-Type/ArchivoBlack). OFL. See `fonts/LICENSE-ArchivoBlack.txt`.
- **Bebas Neue**: Copyright 2010 Dharma Type. OFL. See `fonts/LICENSE-BebasNeue.txt`.
- **Anton**: Copyright 2020 The Anton Project Authors (https://github.com/googlefonts/AntonFont). OFL. See `fonts/LICENSE-Anton.txt`.
- **DM Serif Display**: Copyright 2014-2018 Adobe, with Reserved Font Name 'Source'; Copyright 2019 Colophon Foundry. OFL. See `fonts/LICENSE-DMSerifDisplay.txt`.
- **Abril Fatface**: Copyright 2011 TypeTogether. OFL. See `fonts/LICENSE-AbrilFatface.txt`.
- **Bangers**: Copyright 2010 The Bangers Project Authors. OFL. See `fonts/LICENSE-Bangers.txt`.
- **Permanent Marker**: Copyright Font Diner. Apache License 2.0. See `fonts/LICENSE-PermanentMarker.txt`.
- **Pacifico**: Copyright 2018 The Pacifico Project Authors. OFL. See `fonts/LICENSE-Pacifico.txt`.
- **Lobster**: Copyright 2010 The Lobster Project Authors, with Reserved Font Name "Lobster". OFL. See `fonts/LICENSE-Lobster.txt`.
- **Caveat Brush**: Copyright 2015 Google Inc. OFL. See `fonts/LICENSE-CaveatBrush.txt`.
- **Staatliches**: The Staatliches Project Authors. OFL. See `fonts/LICENSE-Staatliches.txt`.
- **Black Ops One**: The Black Ops One Project Authors. OFL. See `fonts/LICENSE-BlackOpsOne.txt`.
- **Germania One**: The Germania One Project Authors. OFL. See `fonts/LICENSE-GermaniaOne.txt`.
- **Atomic Age**: The Atomic Age Project Authors. OFL. See `fonts/LICENSE-AtomicAge.txt`.
- **Hammersmith One**: The Hammersmith One Project Authors. OFL. See `fonts/LICENSE-HammersmithOne.txt`.
- **Secular One**: The Secular One Project Authors. OFL. See `fonts/LICENSE-SecularOne.txt`.
- **Londrina Solid**: The Londrina Solid Project Authors. OFL. See `fonts/LICENSE-LondrinaSolid.txt`.
- **Bungee Hairline**: The Bungee Project Authors. OFL. See `fonts/LICENSE-BungeeHairline.txt`.
- **Rubik Scribble**: The Rubik Scribble Project Authors. OFL. See `fonts/LICENSE-RubikScribble.txt`.
- **Shadows Into Light Two**: The Shadows Into Light Two Project Authors. OFL. See `fonts/LICENSE-ShadowsIntoLightTwo.txt`.
- **Pixelify Sans**: The Pixelify Sans Project Authors. OFL. See `fonts/LICENSE-PixelifySans.txt`.
- **Google Sans Code**: The Google Sans Code Project Authors. OFL. See `fonts/LICENSE-GoogleSansCode.txt`.
- **Kosugi Maru**: Copyright MOTOYA Co., Ltd. Apache License 2.0. See `fonts/LICENSE-KosugiMaru.txt`.
- **Frijole**: The Frijole Project Authors. OFL. See `fonts/LICENSE-Frijole.txt`.
- **Emilys Candy**: The Emilys Candy Project Authors. OFL. See `fonts/LICENSE-EmilysCandy.txt`.
- **Mystery Quest**: The Mystery Quest Project Authors. OFL. See `fonts/LICENSE-MysteryQuest.txt`.
- **Zen Tokyo Zoo**: The Zen Tokyo Zoo Project Authors. OFL. See `fonts/LICENSE-ZenTokyoZoo.txt`.

The Noto faces in `src/crates/concat-text/fonts/` are the fallback for the
scripts the other faces do not draw, in the interface and in titles and
captions alike, compiled into the binary once
(`src/crates/concat-text/src/lib.rs`, `SCRIPT_FALLBACKS`). Each is the
Regular face from the notofonts project's own release, cut down with
fonttools' `pyftsubset`; the release, the SHA-256 of the archive it came
in and the command that cut it are recorded beside each face there. A subset is a Modified
Version under the OFL, and keeps the family name the release gave it, as
the OFL allows for Noto, which reserves no font name.

- **Noto Sans CJK SC** (a subset: GB 2312, Big5 level 1, JIS X 0208, the Hangul of KS X 1001 and the kana): Copyright 2014-2021 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'. OFL. See `src/crates/concat-text/fonts/LICENSE-NotoSansCJK.txt`.
- **Noto Sans Arabic UI** 2.011 (a subset: the Arabic block and the characters the Arabic, Persian and Urdu interface uses): Copyright 2022 The Noto Project Authors (https://github.com/notofonts/arabic). OFL. See `src/crates/concat-text/fonts/LICENSE-NotoSansArabicUI.txt`.
- **Noto Sans Hebrew** 3.001 (a subset: the Hebrew block): Copyright 2022 The Noto Project Authors (https://github.com/notofonts/hebrew). OFL. See `src/crates/concat-text/fonts/LICENSE-NotoSansHebrew.txt`.
- **Noto Sans Devanagari** 2.007 (a subset: the Devanagari block): Copyright 2022 The Noto Project Authors (https://github.com/notofonts/devanagari). OFL. See `src/crates/concat-text/fonts/LICENSE-NotoSansDevanagari.txt`.
- **Noto Sans Bengali** 3.011 (a subset: the Bengali block): Copyright 2022 The Noto Project Authors (https://github.com/notofonts/bengali). OFL. See `src/crates/concat-text/fonts/LICENSE-NotoSansBengali.txt`.
- **Noto Sans Thai** 2.002 (a subset: the Thai block): Copyright 2022 The Noto Project Authors (https://github.com/notofonts/thai). OFL. See `src/crates/concat-text/fonts/LICENSE-NotoSansThai.txt`.

The licence does not permit selling the font on its own; shipping the
`fonts/` directory as it stands satisfies it.

## Text to speech

Text to speech on the desktop is Chatterbox Turbo (MIT,
https://huggingface.co/ResembleAI/chatterbox-turbo-ONNX, Resemble AI), driven
through ONNX Runtime directly, from Resemble's own ONNX export, downloaded on
demand from the model mirror. Its GPT-2 tokenizer is read by the tokenizers
crate (Apache-2.0). Resemble's Python package stamps a Perth watermark on what
it speaks; the ONNX pipeline carries no such step and Concat adds none, which
MIT permits and which anyone passing the sound off as a person's should know
is not there. On a phone, text to speech is the phone's own system voice, which
Concat neither ships nor downloads.

Chatterbox reads in the voice of a short recording. Its four stock voices
ship inside the app in `src/crates/concat-speech/voices/`: ten seconds each
from LibriVox readings by Elizabeth Klett, Mark F. Smith, Ruth Golding and
Bob Neufeld (https://librivox.org). LibriVox readers dedicate their
recordings to the public domain, and archive.org files each of these under
a public-domain licence; `voices/README.md` names each source file. Concat
credits the readers though nothing requires it.
Reading in the voice of any other recording is a use the person doing it is
answerable for: Concat offers it for the narrator's own voice, and no one's
voice should be cloned without their consent.

Releases up to 0.2.8 also used the sherpa-onnx runtime (Apache-2.0), which
carried espeak-ng (GPL-3.0-or-later), with the Kokoro (Apache-2.0) and Pocket
TTS (CC-BY-4.0) voices; those releases' own notices describe them. No current
build contains any of them.

## ONNX Runtime

The cutout models run on Microsoft's ONNX Runtime
(https://github.com/microsoft/onnxruntime, MIT), through the `ort` crate
(https://github.com/pykeio/ort, MIT OR Apache-2.0). On Apple silicon and
the phones it is linked into the app from pyke's builds of it; an Intel
Mac's bundle carries Microsoft's last shared build for that machine in
`Contents/Frameworks`. The Linux and Windows bundles ship Microsoft's own
shared build of ONNX Runtime 1.28.0 beside the binary - `lib/libonnxruntime.so.1.28.0` on Linux,
`onnxruntime.dll` beside `concat.exe` on Windows - unmodified, as Microsoft publishes it
at https://github.com/microsoft/onnxruntime/releases/tag/v1.28.0. Its
licence (MIT, Copyright (c) Microsoft Corporation) and third-party
notices are in that release's archive.

## Microsoft Visual C++ runtime (Windows)

The Windows bundles and installers carry the Microsoft Visual C++ runtime
DLLs the program and its DLLs import - `vcruntime140.dll`,
`vcruntime140_1.dll`, `msvcp140.dll` and their kin, whichever the build
finds imported - copied unmodified from the redistributable folder of the
Visual Studio that built the release (`VC\Redist\MSVC\<version>\<arch>\
Microsoft.VC14x.CRT`). They are Microsoft's, not Concat's, and are not
covered by Concat's licence: they are redistributed under the terms
Microsoft sets for them, the Microsoft Software License Terms for Visual
Studio and its list of distributable code (for Visual Studio 2022,
https://learn.microsoft.com/visualstudio/releases/2022/redistribution),
which permit shipping them alongside an application that uses them.

## The cutout models

Remove background runs four models, none of which ship inside the app
except the first:

- Google's MediaPipe Selfie Segmentation (Apache-2.0), in the ONNX
  conversion published by the ONNX Community
  (https://huggingface.co/onnx-community/mediapipe_selfie_segmentation,
  Apache-2.0), compiled into the `concat-vision` crate; see
  `src/crates/concat-vision/models/NOTICE.md`. The answer when nothing
  has been downloaded.
- Robust Video Matting, the MobileNetV3 variant, by Peter Lin and others
  (https://github.com/PeterL1n/RobustVideoMatting, GPL-3.0), from that
  repository's releases, downloaded on first use. The person model.
- IS-Net from "Highly Accurate Dichotomous Image Segmentation" by Qin and
  others (https://github.com/xuebinqin/DIS, Apache-2.0), in the ONNX
  export the rembg project publishes
  (https://github.com/danielgatis/rembg, MIT), downloaded on first use.
  The object model.
- SlimSAM (https://github.com/czg1225/SlimSAM, Apache-2.0), in the ONNX
  export published at https://huggingface.co/Xenova/slimsam-77-uniform
  (Apache-2.0), downloaded on first use. The brushes' model.

Downloaded models live in the app's data directory under `cutout-models`
and are never bundled; the three that are downloaded come from Concat's own
mirror of the sources named above, described below. They are all run by
ONNX Runtime
(https://github.com/microsoft/onnxruntime, MIT) through the `ort` crate
(https://github.com/pykeio/ort, MIT OR Apache-2.0), with the platform's
own accelerator behind it where the runtime has one: CoreML on macOS and
iOS, NNAPI on Android. How each platform carries the runtime is under
"ONNX Runtime" above.

## The model mirror

Every downloaded model named above is mirrored onto a release of Concat's own
repository, and the app fetches it from there; the upstream each was
obtained from is the fallback and is named above in every case. The table of
what is mirrored, and the digest each download is checked against, is
`models/manifest.toml`; `.github/workflows/models.yml` is what fills the
mirror.

Mirroring is redistribution, and each model keeps the licence it arrived
under - Apache-2.0 for SlimSAM and IS-Net, BSD-3-Clause for Real-ESRGAN,
MIT for Chatterbox Turbo and the whisper conversions, GPL-3.0 for Robust
Video Matting. The Kokoro (Apache-2.0) and Pocket TTS (CC-BY-4.0) bundles
stay on the mirror for releases up to 0.2.8, which download them. Those terms are met
by the attributions above and by the licence files each mirrored archive
carries; a mirrored file is a verbatim copy, never a modification. Nothing
in the mirror is bundled with the app, and Concat claims no rights over any
of it.

## Effect preview photograph

The effect catalogue cards are drawn from a photograph by Sou Jest on
Unsplash (https://unsplash.com/@soujest,
https://unsplash.com/photos/2v921tlqdXE), used under the Unsplash License. The still lives at `src/crates/concat-host/assets/card-still.jpg`
and is embedded in the app; each card is the effect's own shader run over
it on the GPU when the app first needs the card (`concat-host`'s
`cards.rs`).

---

# 星籁剪辑（XingLai Cut）补充声明

本文件由星籁（XingLai）为 0.2.11–0.2.25 Windows 桌面版补充，用于说明上游
Concat 文本未覆盖的随包组件、AI 插件与模型、素材与许可义务。与上游文本
冲突时，以义务更严格者为准。审计日期：2026-11-22。许可证全文见本目录旁
`LICENSES/`；源码要约见 `SOURCE_OFFER.md`；AI 插件清单见 `AI_PLUGINS_NOTICES.md`。

## 1. FFmpeg（Windows）

随包 FFmpeg 8.1 共享库（`avcodec-62.dll`、`avdevice-62.dll`、
`avfilter-11.dll`、`avformat-62.dll`、`avutil-60.dll`、`swresample-6.dll`、
`swscale-9.dll`；0.2.14–0.2.24 另含 `ffmpeg.exe`、`ffprobe.exe`）来自 BtbN 的
`ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-shared-8.1` 构建，许可
**LGPL-3.0-or-later**（`--enable-version3`，未启用 `--enable-gpl` /
`--enable-nonfree`，`--disable-libx264 --disable-libx265`）。许可证全文：
`LICENSES/LGPL-3.0.txt`；对应源码与构建脚本见 `SOURCE_OFFER.md` 第 2 节。
DLL 位于 exe 旁、可被用户替换。H.264/HEVC/AAC 等编解码专利许可不在上述
开源许可范围内，由分发/使用方自行处理。

## 2. Slint（界面）

`slint 1.18.1` 等 14 个 crate 以 **GPL-3.0-only** 选项使用（Slint 同时提供
GPLv3 / Royalty-free / 商业许可三种选择；本产品选择 GPLv3）。GPLv3 第 13 条
明确允许与 AGPLv3 结合，故与主程序 AGPL-3.0-or-later 一并分发合规。
GPLv3 全文：`LICENSES/GPL-3.0-only.txt`。

## 3. ONNX Runtime

`onnxruntime.dll`、`onnxruntime_providers_shared.dll` 为 Microsoft ONNX
Runtime 1.28.0（MIT）。文本：`LICENSES/MIT-ONNXRuntime.txt`；微软第三方
声明：`LICENSES/ONNXRuntime-ThirdPartyNotices.txt`。

## 4. Rust 依赖（Cargo.lock 全量）

主程序由 Cargo 构建，`Cargo.lock` 共 **826** 个包：806 个 crates.io 包 +
20 个本地 workspace/补丁包（19 个 concat-* + `i-slint-renderer-skia` +
`whisper-rs-sys`）。许可分布（2026-11-22 本地审计）：

- 约 760 个 MIT / Apache-2.0 / BSD / ISC / Zlib / Unicode 等宽松许可；
- 14 个 Slint crate：GPL-3.0-only 选项（见上）；
- 9 个 MPL-2.0：`uniffi*`（移动端绑定，Windows 构建不链接）与
  `option-ext`——MPL 为文件级弱 copyleft，不影响作为更大作品分发，
  源码可从 crates.io 获取；
- 2 个 r-efi：MIT / Apache-2.0 / LGPL-2.1-or-later 三选一（本产品取宽松项）；
- 其余为 Unlicense / BSL-1.0 / CDLA-Permissive / WTFPL 等宽松许可；
- 2 个包（`arbitrary`、`derive_arbitrary`）本地缓存缺 license 字段，
  crates.io 标称 MIT OR Apache-2.0（待复核）。
- 完整清单：`LICENSES/cargo-licenses.csv`。

未发现 AGPL/SSPL/自定义非自由许可；未发现要求版税的依赖。

## 5. 字体

界面与标题渲染嵌入的字体均为 SIL OFL 1.1（Permanent Marker 与 Kosugi Maru
为 Apache-2.0），包括 Hanken Grotesk、Inter、Montserrat、Space Grotesk、
Archivo Black、Bebas Neue、Anton、DM Serif Display、Abril Fatface、Bangers、
Permanent Marker、Pacifico、Lobster、Caveat Brush、Staatliches、Black Ops One、
Germania One、Atomic Age、Hammersmith One、Secular One、Londrina Solid、
Bungee Hairline、Rubik Scribble、Shadows Into Light Two、Pixelify Sans、
Google Sans Code、Kosugi Maru、Frijole、Emilys Candy、Mystery Quest、
Zen Tokyo Zoo，以及 Noto Sans CJK SC / Arabic UI / Hebrew / Devanagari /
Bengali / Thai 的 OFL 子集。OFL 全文：`LICENSES/OFL-1.1.txt`；各字体版权行在
源码 `src/crates/concat-text/fonts/LICENSE-*.txt`。OFL 允许随软件分发，
但不得单独售卖字体文件。

## 6. 素材库（云端，不随安装包）

桌面端 0.2.20 起「素材库」按需从平台接口下载图片/视频/音乐/音效/字体，
不随安装包分发。接口为每个素材返回授权元数据：Pixabay Content License、
CC0 1.0、CC BY 3.0、SIL OFL 1.1，并标注是否需要署名。CC BY 素材需在成片
中署名，客户端卡片显示 attribution；建议导出时同时给出署名清单（待办）。
Pixabay 素材由平台重新托管并供下载，是否符合其「不得单独再分发」条款，
待法务确认。

## 7. AI 插件与本地模型（按需下载，不随安装包）

详细清单、证据与处置见 `AI_PLUGINS_NOTICES.md`。摘要：

- **Piper 本地配音**：引擎 MIT；`espeak-ng` 运行时与数据 GPL-3.0-or-later
  （源码 https://github.com/espeak-ng/espeak-ng）；中文音色 `zh_CN-huayan`
  的数据集许可上游记载为 **Unknown**，商用授权待确认。
- **Wav2Lip 本地数字人**：上游无许可证文件、README 明确仅个人/研究/
  非商业使用，权重为第三方转换。**已停止分发**（0.2.26 起从下载清单与
  客户端默认流程移除）。
- **SadTalker 本地数字人**：代码 Apache-2.0（含腾讯补充声明）；权重由
  上游发布，其 3D 人脸重建基于 Basel Face Model (BFM)，权重商用授权
  待法务确认。
- **ZipVoice 语音克隆**：sherpa-onnx 与 ZipVoice 模型均 Apache-2.0；
  随包 `espeak-ng-data` 为 GPL-3.0-or-later；内置参考音色
  `news-female*.wav` 与 ZipVoice 发布包 `test_wavs` 字节一致（新闻播报
  音频，版权/声音权未确认，正在替换）。
- 本地字幕：whisper.cpp（MIT）+ Whisper ggml 模型（MIT，按需下载）；
  Silero VAD（MIT）；DeepFilterNet3（MIT/Apache-2.0）。
- 抠像模型：MediaPipe Selfie Segmentation（Apache-2.0，内嵌）；
  IS-Net/rembg（Apache-2.0/MIT）、SlimSAM（Apache-2.0）、Robust Video
  Matting（GPL-3.0，按需下载）。RVM 的 GPL-3.0 仅与 AGPL 分发兼容；
  若将来闭源发行必须移除。

## 8. 其他

Microsoft Visual C++ 运行库按 Visual Studio 可再分发条款随包；语音示例
来自 LibriVox 公有领域录音；效果预览图为 Unsplash License。均见上方上游
文本。

## 9. 无额外限制

星籁剪辑不限制用户复制、修改、再分发本程序（AGPL 允许的行为）；账号登录
仅用于星籁云端 AI、云同步等平台服务，本地剪辑、本地配音、本地数字人
不需要登录，也不含激活/DRM 限制。若未来以修改版 Concat 提供网络交互
服务，将按 AGPL 第 13 条向网络用户提供对应源码。
