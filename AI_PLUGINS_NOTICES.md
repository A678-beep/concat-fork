# 星籁剪辑本地 AI 插件与模型 —— 许可与来源清单

适用版本：0.2.11–0.2.25 桌面版。插件不随安装包分发，由「AI 面板 → 本地模型
中心」从 `https://xinglaiai.com/ai-nav/api/ai-plugins/` 按需下载，装在
`%APPDATA%\app.concat.editor\ai-plugins\<id>\`。审计日期：2026-11-22。

> 结论先行：**Wav2Lip 插件为非商用许可，已停止分发**；**Piper 中文音色
> 的数据集许可未知，商用前必须替换或取得授权**；其余插件为宽松/弱 copyleft
> 许可，需随包带许可文本与源码地址（见 `LICENSES/` 与 `SOURCE_OFFER.md`）。

## 1. 插件清单

| 插件 | 版本/包 | 引擎许可 | 模型/音色许可 | 商用 | 处置 |
| --- | --- | --- | --- | --- | --- |
| Piper 本地配音 | 1.0.0，`piper-windows-zh_CN.zip` (100,003,538 B) | Piper 2023.11.14-2：MIT | 音色 `zh_CN-huayan` medium/x_low：**上游 MODEL_CARD 记载数据集 License: Unknown** | 引擎可；音色待授权 | 音色替换/取得书面授权，见第 3 节 |
| Wav2Lip 本地数字人 | 1.0.0，`wav2lip.zip` (258,790,891 B) | Wav2Lip 上游**无 LICENSE 文件**，README 声明仅个人/研究/非商业 | 权重 `wav2lip_256.onnx` 由第三方从 Linly-Talker/Wav2Lip 转换，无授权声明；`scrfd_2.5g_bnkps.onnx`（InsightFace，MIT） | **否** | **停止分发/下架**，客户端默认流程移除 |
| SadTalker 本地数字人 | 1.0.0，`sadtalker.zip` (2,123,080,397 B) | SadTalker 代码 Apache-2.0（含腾讯补充声明；上游称已取消非商用限制） | 官方权重 `SadTalker_V0.0.2_512.safetensors`、`mapping_00229-model.pth.tar`；3D 重建基于 Basel Face Model (BFM)，GFPGAN/facexlib 权重另有许可 | 代码可；权重待法务 | 法务确认权重/BFM 后再默认推荐，否则标注"试验" |
| 语音克隆 ZipVoice | 1.0.0，`voice-clone.zip` (177,134,608 B) | sherpa-onnx v1.12.14：Apache-2.0 | ZipVoice distill zh-en：Apache-2.0（k2-fsa） | 引擎/模型可 | 参考音色 wav 需替换，见第 4 节 |

## 2. 插件内随包运行时

| 组件 | 许可 | 义务 |
| --- | --- | --- |
| espeak-ng 1.5x（`espeak-ng.dll`、`espeak-ng-data/`，Piper 与 voice-clone 各约 17.2 MB） | GPL-3.0-or-later | 随包提供许可证全文（`LICENSES/GPL-3.0-only.txt` 同文本）与对应源码地址 <https://github.com/espeak-ng/espeak-ng>；本程序整体以 AGPL-3.0 分发，GPLv3 与 AGPLv3 可依 GPLv3 §13 结合 |
| ONNX Runtime（各插件自带 `onnxruntime.dll`，1.20–1.28 区间） | MIT | 附 MIT 文本（`LICENSES/MIT-ONNXRuntime.txt`） |
| 嵌入版 Python 3.12 / 3.10 | PSF-2.0 | 附 Python 许可证（python 目录内 `LICENSE.txt` 已含） |
| PyTorch 2.4.1 CPU（SadTalker） | BSD-3-Clause（含 Intel MKL/OpenMP 等第三方组件，见 `torch-2.4.1+cpu.dist-info/LICENSE` 与 `NOTICE`） | 保留声明 |
| 其他 Python 依赖 | 见各 `dist-info` 内 LICENSE（numpy/BSD-3、OpenCV/Apache-2.0、SciPy/BSD-3、Librosa/ISC 等） | 保留声明 |
| PyAV（SadTalker 环境） | BSD-3-Clause 封装 + FFmpeg 共享库 libx264/libx265（**GPL-2.0**）等 | 只随 SadTalker 插件分发；须附 GPL 文本与源码地址（`libx264`、`libx265`），或将 PyAV/FFmpeg 从插件中移除 |

> 注：SadTalker 插件内含 `av.libs\libx264-164*.dll`、`libx265-*.dll`（GPL-2.0）。
> 这是随包分发的 GPL 二进制，需在插件目录/清单中附 GPL-2.0 文本与
> <https://code.videolan.org/videolan/x264>、<https://bitbucket.org/multicoreware/x265_git>
> 源码地址；否则应从插件中剔除（本地合成本可由主程序自带 ffmpeg 完成）。

## 3. Piper 中文音色（华言）—— 高风险项

- 文件：`voices/zh_CN-huayan-medium.onnx`（63,201,294 B）、
  `voices/zh_CN-huayan-x_low.onnx`（20,628,813 B），下载自
  `rhasspy/piper-voices`（`zh/zh_CN/huayan/{medium,x_low}`）。
- 上游 `MODEL_CARD` 原文：`Dataset URL: https://github.com/PlayVoice/HuaYan_TTS`、
  `License: Unknown`。插件内 `README-插件说明.txt` 写的「CC BY 4.0」**没有
  上游依据，应删除/更正**。
- 处置建议（按优先级）：① 换用有明确商用许可的中文音色（自录/委托录制后
  自行训练，版权归本机构）；② 向数据集/音色作者取得书面商用授权并留档；
  ③ 未取得授权前，在插件说明与界面标注「仅限个人/测试」，不随商业版分发。

## 4. 语音克隆参考音色

- 插件内 `voices/news-female.wav`、`voices/news-female-2.wav` 与
  `sherpa-onnx-zipvoice-distill-int8-zh-en-emilia` 发布包 `test_wavs/` 中的
  文件 **SHA-256 完全一致**（已核对）；`prompt.wav` 不在该发布包内，来源不明。
- 这些参考音频内容是新闻播报（CCTV 新闻文本），版权与播音员声音权未确认；
  在商业产品中随包分发有版权/人格权风险。
- 处置：替换为自录或已取得书面授权的参考录音（同时更新 `promptText`），
  在授权到位前从插件包中移除这三个 wav。

## 5. 模型权重来源与校验

- Wav2Lip：`wav2lip_256.onnx` ← instant-high/wav2lip-onnx-256（该仓库无
  license，已 archived）← 256×256 权重源自 Linly-Talker（代码 MIT，但其
  Wav2Lip 权重继承上游非商用限制）。
- SadTalker：`sadtalker.zip` 内 checkpoints 为上游发布的
  `SadTalker_V0.0.2_512.safetensors` + `mapping_00229-model.pth.tar`；
  安装包内未包含 BFM 的 `.mat` 3DMM 数据文件（若未来加入，必须单独评估
  BFM 非商用许可）。
- ZipVoice：`text_encoder.int8.onnx`、`fm_decoder.int8.onnx`、
  `vocos_24khz.onnx`、`tokens.txt`、`pinyin.raw` 来自 k2-fsa/sherpa-onnx
  的 ZipVoice distill INT8 发布包（Apache-2.0）。
- 每个插件包在服务端的 SHA-256 记录于 `stage/ai-plugins/SHA256SUMS.txt`；
  客户端按清单里的 sha256 校验后才安装。

## 6. 平台侧必须同步完成的动作

1. 在 `https://xinglaiai.com/ai-nav/api/ai-plugins/manifest.json` 中把
   `wav2lip` 改为 `{"version":"0.1.0","url":"...wav2lip.zip","size":0,"sha256":""}`
   （客户端会显示「即将上线」且不再请求下载），并从 CDN/`/var/www/ai-plugins/`
   删除 `wav2lip.zip`。
2. 从分发目录（`desktop/dist/plugins/`）删除 `wav2lip.zip`；下一版客户端
   已把 Wav2Lip 从官方表移除。
3. 为 piper / voice-clone / sadtalker 各补一个 `LICENSE-插件.txt`（内容可
   直接引用本文件第 1、2 节并附 `LICENSES/` 中对应全文），重新打包上传。
