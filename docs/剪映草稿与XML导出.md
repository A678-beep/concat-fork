# 剪映草稿与 XML 导出（0.2.28）

把一条时间线交给别的剪辑软件继续改：剪映专业版、Final Cut Pro / 达芬奇、
Premiere Pro。实现分两层：

- `concat-project::capcut_export`：工程 → 剪映草稿（`draft_content.json` +
  `draft_meta_info.json` + `materials/`）；
- `concat-project::fcpxml`：工程 → FCPXML 1.9（`.fcpxml`）、Premiere 兼容
  XML（FCP 7 `xmeml` version 4，`.xml`）与字幕（`.srt`）。

窗口这一层在 `crates/concat/src/interop.rs`：文件菜单的三行、导出面板、
选择文件夹、打开导出文件夹、一键同步到剪映草稿目录、导入草稿。

## 剪映草稿格式

剪映把一份草稿存成一个文件夹，5.9 及更早版本里 `draft_content.json` 是明文
JSON（`"version": 360000`）。本实现按公开的字段集生成：

```
<工程名>-剪映草稿/
├── draft_content.json
├── draft_meta_info.json
├── README.txt
└── materials/{videos,audios,images,texts,effects}/
```

- 时间一律是整数**微秒**；画幅、帧率取活动时间线的 `video` 设置。
- `materials.videos/audios/texts/speeds/transitions` 是素材；`tracks` 里
  按类型分 video / audio / text，每条轨的 `segments` 按开始时间排序。
- 转场挂在**前一段**片段的 `extra_material_refs` 上（剪映的存法），
  `materials.transitions` 里是它自己的对象：名称、`resource_id`、
  `effect_id`、时长、`is_overlap`。剪映的转场资源 id 是它自带的，本实现只
  映射**免费**转场（VIP 的会显示锁定），映射不上的一律降级为「叠化」并在
  导出面板逐条说明。
- 字幕是 text 轨的片段，`materials.texts[].content` 是「JSON 套 JSON」：
  `{"text": "...", "styles": [{"range": [0, n], "size": …}]}`。字号换算用
  `crate::capcut` 读取器的同一常数：剪映的 15 号 ≈ 画幅高度的 0.09。
- 媒体路径是**绝对路径**，与剪映自己的存量方式一致，不复制媒体文件：
  `materials/` 目录只是结构占位。

### 剪映 6.x

剪映 6 起对 `draft_content.json` 加密，外部工具写不进去。本版生成的是 5.x
明文格式；在 6.x 里会读不出来。面板与 README 都写明这一点。

### 映射表（转场）

| 星籁剪辑 | 剪映 |
| --- | --- |
| `concat.dissolve` | 叠化 |
| `concat.blur-dissolve` | 模糊 |
| `concat.fade-black` / `fade-white` | 闪黑 / 闪白 |
| `concat.glitch-transition` / `glitch-warp` | 故障 |
| `concat.zoom-transition` / `zoom-blur-transition` | 推近 |
| `concat.push` / `whip-pan` | 左移 |
| `concat.slide` | 滑动 |
| `concat.linear-wipe` | 向右擦除 |
| `concat.clock-wipe` | 圆形扫描 |
| `concat.iris` | 圆形遮罩 |
| `concat.spin` | 顺时针旋转 |
| `concat.swirl` | 漩涡 |
| `concat.rgb-split-wipe` | 色差顺时针 |
| `concat.cube` | 立方体 |
| `concat.shape-wipe` | 横向分割 |
| 其它 | 叠化（并在面板说明） |

## FCPXML 1.9

- `resources` 里一个 `format`（帧率用有理数 `frameDuration`）、每个用到的
  媒体一个 `asset`（`media-rep src=file:///...`，路径做百分号转义），字幕用
  一个 `Basic Title` effect。
- 最底层**画面轨**是主故事线（`spine`），没有画面的时间是 `gap`；其余画面
  轨与字幕按 `lane` 挂在主故事线片段上（字幕是 `title` 元素，文字可编辑）；
  声音轨是负 `lane` 的 `asset-clip`（`audioRole="dialogue"`）。
- 跨越主故事线切点的覆盖片段按切点拆成多段，`offset` 相对父片段、`start`
  继续源素材的入点。

## Premiere 兼容 XML（xmeml v4）

- `sequence` 有宽高与帧率；每条画面轨 / 声音轨一个 `<track>`，`clipitem` 有
  时间、入出点、`file`（`pathurl`）与 Basic Motion / Opacity 滤镜（缩放、
  旋转、位置、不透明度）。
- 变速：`out - in` 与 `end - start` 的差就是速度，Premiere 导入时按速度处
  理。
- Premiere 的 xmeml 没有「不依赖外部标题文件」的字幕元素，所以字幕单独写
  `<工程名>.srt`，导入 PR 后可当字幕轨用。

## 不是 100% 兼容：没写过去的东西

特效、滤镜、蒙版、抠像、防抖、关键帧、变速曲线、形状/图层片段、独立转场
片段、自定义字体、超过 100% 的音量。导出面板会按类逐条列出来，剪映里需
手动重做。

## 验收与样例

- 结构单测：`cargo test -p concat-project --lib capcut_export` 与
  `cargo test -p concat-project --lib fcpxml`（含「导出→用自己的读取器读回」
  的往返用例）。
- 独立校验：`work/interop-028/validate.py` 用 Python 标准库的 `json` 与
  `xml.etree` 再读一遍样例，逐项断言结构（不用项目自己的代码）。
- 样例由 `cargo run -p concat-project --example interop_sample -- <工程文件夹>
  <输出文件夹>` 用现有测试工程生成。
- **尚未在装了剪映的机器上实机导入验证**：本机没有剪映，按公开格式生成，
  验收记录里如实注明。
