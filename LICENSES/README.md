# 星籁剪辑（XingLai Cut）随包许可证文本

本目录随星籁剪辑 Windows 桌面版一起分发，收集程序实际携带的第三方许可证
全文与清单。上游 Concat 的说明见上级目录 `THIRD_PARTY_NOTICES.md`（本目录
是其补充，冲突时以义务更严格者为准）。

| 文件 | 覆盖内容 | 来源/版本 |
| --- | --- | --- |
| `AGPL-3.0.txt` | 星籁剪辑主程序（基于 Concat） | AGPL-3.0-or-later |
| `GPL-3.0-only.txt` | Slint 1.18.1（界面，GPLv3 选项）；espeak-ng 运行时/数据 | Slint 1.18.1；espeak-ng |
| `LGPL-3.0.txt` | 随包 FFmpeg 8.1 共享库（av*.dll 等） | BtbN `ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-shared-8.1` |
| `MIT-ONNXRuntime.txt` | ONNX Runtime 1.28.0 | Microsoft ONNX Runtime |
| `ONNXRuntime-ThirdPartyNotices.txt` | ONNX Runtime 内附第三方声明 | Microsoft ONNX Runtime 1.28.0 发布包 |
| `Apache-2.0.txt` | SadTalker 代码、MediaPipe Selfie Segmentation 等 | Apache License 2.0 |
| `OFL-1.1.txt` | 界面与标题内嵌的全部 OFL 字体、Noto 子集 | SIL Open Font License 1.1 |
| `cargo-licenses.csv` | 主程序 Cargo.lock 全部 826 个包的名称/版本/许可（2026-11-22 审计） | 本地 cargo registry 元数据 |

源码获取、FFmpeg 对应源码与书面要约见 `../SOURCE_OFFER.md`；
AI 插件与模型许可见 `../AI_PLUGINS_NOTICES.md`；素材库授权见主程序
`THIRD_PARTY_NOTICES.md` 第 6 节。
