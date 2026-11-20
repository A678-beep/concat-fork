// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai AI and Concat contributors

//! The built-in effects catalogue's words, in Chinese.
//!
//! Effects, filters, transitions and sound packages are data: their names,
//! categories and knob labels come from manifests written in English, and
//! the interface draws them as they are. This module is the one place those
//! words are turned into the interface's language for the built-in
//! catalogue - the panels a person actually works in (Adjust, Effects,
//! Filters, Transitions, the sound shelf). A package someone writes keeps
//! its own words, as a text preset keeps its name: this table answers only
//! for the words it knows, and anything else passes through untouched.
//!
//! It is a table rather than locale entries on purpose: a `.slint` file
//! cannot look a translation up by a string that came from data, and the
//! catalogue is shared with the API and the MCP server, which speak English.

/// The Chinese for a built-in package's name, or the name itself.
pub fn name(english: &str) -> String {
    lookup(NAMES, english).unwrap_or(english).to_owned()
}

/// The Chinese for a built-in knob's label, or the label itself.
pub fn label(english: &str) -> String {
    lookup(LABELS, english).unwrap_or(english).to_owned()
}

/// The Chinese for a built-in package category.
pub fn category(english: &str) -> String {
    lookup(CATEGORIES, english).unwrap_or(english).to_owned()
}

fn lookup<'a>(table: &'a [(&'a str, &'a str)], english: &str) -> Option<&'a str> {
    table
        .iter()
        .find(|(held, _)| held.eq_ignore_ascii_case(english.trim()))
        .map(|(_, chinese)| *chinese)
}

/// The package names the built-in catalogue ships, and their Chinese.
const NAMES: &[(&str, &str)] = &[
    ("Adjust", "调整"),
    ("After Hours", "夜归"),
    ("Aquamarine", "碧海"),
    ("Arabic", "阿拉伯语"),
    ("Atomic", "原子"),
    ("Autumn", "秋意"),
    ("Bass", "低音"),
    ("Beat Shake", "节拍震动"),
    ("Bengali", "孟加拉语"),
    ("Blackletter", "哥特体"),
    ("Bleach Bypass", "漂白旁路"),
    ("Block", "方块"),
    ("Bloom Pulse", "泛光脉冲"),
    ("Blur Dissolve", "模糊溶解"),
    ("Blush", "胭红"),
    ("Bold caption", "粗体字幕"),
    ("Bright", "明亮"),
    ("Bronze", "古铜"),
    ("Candy", "糖果"),
    ("Cartoon", "卡通"),
    ("Cathedral", "教堂混响"),
    ("Chinese", "中文"),
    ("Chinese (Traditional)", "繁体中文"),
    ("Chipmunk", "花栗鼠"),
    ("Chroma Key", "抠像"),
    ("Chromatic Aberration", "色差"),
    ("City Lights", "城市灯光"),
    ("Classic Chrome", "经典胶片"),
    ("Clean", "干净"),
    ("Clock Wipe", "时钟擦除"),
    ("Cloudless Sky", "晴空"),
    ("Code", "代码"),
    ("Color Wheels", "色轮"),
    ("Comic", "漫画"),
    ("Contrast", "对比度"),
    ("Contrast Pop", "对比冲压"),
    ("Cool", "冷调"),
    ("Cool Blue", "冷蓝"),
    ("Corona", "日冕"),
    ("Cube", "立方体"),
    ("Curves", "曲线"),
    ("Cyberpunk", "赛博朋克"),
    ("Deep", "深沉"),
    ("Delicious", "甜腻"),
    ("Diner", "餐厅"),
    ("Dissolve", "溶解"),
    ("Distorted", "失真"),
    ("Dreamy", "梦幻"),
    ("Echo", "回声"),
    ("Editorial", "杂志"),
    ("Enhance Voice", "人声增强"),
    ("Exposure", "曝光"),
    ("Fade to Black", "淡入黑场"),
    ("Fade to White", "淡入白场"),
    ("Falling Dust", "飘尘"),
    ("Fashion", "时尚"),
    ("Film", "胶片"),
    ("Film Burn", "胶片灼烧"),
    ("Film Grain", "胶片颗粒"),
    ("Fisheye", "鱼眼"),
    ("Fresh", "清新"),
    ("Friendly", "亲和"),
    ("Gaussian Blur", "高斯模糊"),
    ("Glitch", "故障"),
    ("Glitch Warp", "故障扭曲"),
    ("Glow", "发光"),
    ("Golden Hour", "黄金时刻"),
    ("Greek", "希腊语"),
    ("Hairline", "细线"),
    ("Halation", "光晕"),
    ("Halftone", "半调"),
    ("Hall", "大厅混响"),
    ("Handwritten", "手写"),
    ("Hebrew", "希伯来语"),
    ("High-Contrast Mono", "高反差黑白"),
    ("Hindi", "印地语"),
    ("Impact", "冲击"),
    ("Indie", "独立"),
    ("Instant Print", "拍立得"),
    ("Iris", "光圈"),
    ("Japanese", "日语"),
    ("Korean", "韩语"),
    ("Light Leak", "漏光"),
    ("Light Leak Flash", "漏光闪"),
    ("Linear Wipe", "线性擦除"),
    ("Linen", "亚麻"),
    ("Lo-fi", "低保真"),
    ("Lower third", "底部字幕条"),
    ("Luma Key", "亮度键"),
    ("Luminous", "发光"),
    ("Marker", "马克笔"),
    ("Megaphone", "扩音器"),
    ("Mint Haze", "薄荷雾"),
    ("Mission", "使命"),
    ("Mono", "黑白"),
    ("Moody", "沉郁"),
    ("Motion Blur", "动感模糊"),
    ("Mystery", "神秘"),
    ("Natural", "自然"),
    ("Neon", "霓虹"),
    ("Night", "夜色"),
    ("Noir", "黑色电影"),
    ("Noise Reduction", "降噪"),
    ("Normalize Loudness", "响度标准化"),
    ("Notes", "笔记"),
    ("Persian", "波斯语"),
    ("Pink Fuzz", "粉噪"),
    ("Pitch", "变调"),
    ("Pixel", "像素"),
    ("Pixelate", "像素化"),
    ("Plate", "板式混响"),
    ("Point and Shoot", "卡片机"),
    ("Poster", "海报"),
    ("Posterize", "色调分离"),
    ("Prism Shake", "棱镜震动"),
    ("Push", "推挤"),
    ("RGB Split", "RGB 分离"),
    ("Radio", "收音机"),
    ("Remove Black", "去黑底"),
    ("Retro 70s", "七十年代"),
    ("Retro Cartoon", "复古卡通"),
    ("Retro", "复古"),
    ("Robot", "机器人"),
    ("Room", "房间混响"),
    ("Rosy", "玫瑰"),
    ("Russian", "俄语"),
    ("Saturation", "饱和度"),
    ("Sci-Fi", "科幻"),
    ("Scribble", "涂鸦"),
    ("Shake", "震动"),
    ("Shape Wipe", "形状擦除"),
    ("Sharpen", "锐化"),
    ("Silver", "银盐"),
    ("Slide", "滑动"),
    ("Soft Light", "柔光"),
    ("Spin", "旋转"),
    ("Spooky", "阴森"),
    ("Stadium", "体育场"),
    ("Stencil", "模板字"),
    ("Super 8", "超八毫米"),
    ("Sweet", "甜美"),
    ("Swirl", "漩涡"),
    ("Teal & Orange", "青橙"),
    ("Tech", "科技"),
    ("Telephone", "电话"),
    ("Thai", "泰语"),
    ("Title", "标题"),
    ("Treble", "高音"),
    ("Tropical", "热带"),
    ("Ukrainian", "乌克兰语"),
    ("Underwater", "水下"),
    ("Urdu", "乌尔都语"),
    ("VHS", "录像带"),
    ("Vaporwave", "蒸汽波"),
    ("Vibrance", "鲜艳度"),
    ("Vietnamese", "越南语"),
    ("Vignette", "暗角"),
    ("Vintage", "复古"),
    ("Vinyl", "黑胶"),
    ("Vivid", "艳丽"),
    ("Vivid Landscape", "风光艳丽"),
    ("Walnut", "胡桃木"),
    ("Warm", "暖调"),
    ("Western", "西部"),
    ("Wet Street", "雨后街道"),
    ("Whip Pan", "甩镜"),
    ("Whisper", "低语"),
    ("White Balance", "白平衡"),
    ("Voice Clean", "人声降噪"),
    ("Hum Remove", "去工频"),
    ("Whitewash", "白洗"),
    ("Window Seat", "靠窗座位"),
    ("Winter Cool", "冬日冷调"),
    ("Zoo", "动物园"),
    ("Zoom", "变焦"),
    ("Zoom Blur", "变焦模糊"),
];

/// The knob labels the built-in catalogue ships, and their Chinese.
const LABELS: &[(&str, &str)] = &[
    ("Age", "年代"),
    ("Air", "空气感"),
    ("Amount", "强度"),
    ("Angle", "角度"),
    ("Any", "全部"),
    ("Balance", "平衡"),
    ("Bits", "位深"),
    ("Black level", "黑场"),
    ("Blacks", "黑色"),
    ("Bleed", "溢色"),
    ("Blue", "蓝"),
    ("Blush", "腮红"),
    ("Body", "厚度"),
    ("Bottom to Top", "从下到上"),
    ("Brighten", "提亮"),
    ("Brightness", "明度"),
    ("Centre X", "中心 X"),
    ("Centre Y", "中心 Y"),
    ("Chill", "寒气"),
    ("Circle", "圆形"),
    ("Clarity", "清晰度"),
    ("Clip black", "压黑"),
    ("Clip white", "压白"),
    ("Colour", "颜色"),
    ("Colour filter", "颜色滤镜"),
    ("Color", "色彩"),
    ("Curves", "曲线"),
    ("Effects", "效果"),
    ("Lightness", "明暗"),
    ("Wheels", "色轮"),
    ("Contrast", "对比度"),
    ("Cool", "冷"),
    ("Coolness", "冷度"),
    ("Decay", "衰减"),
    ("Delay", "延迟"),
    ("Depth", "深度"),
    ("Direction", "方向"),
    ("Dot size", "点大小"),
    ("Down", "下"),
    ("Dread", "恐惧"),
    ("Drive", "推力"),
    ("Dust", "尘埃"),
    ("Edge softness", "边缘柔度"),
    ("Even", "均匀"),
    ("Exposure", "曝光"),
    ("Fade", "褪色"),
    ("Flash", "闪光"),
    ("Gain", "增益"),
    ("Gamma", "伽马"),
    ("Glow", "发光"),
    ("Grain", "颗粒"),
    ("Green", "绿"),
    ("Greens", "绿色"),
    ("Handheld", "手持"),
    ("Haze", "雾感"),
    ("Heart", "心形"),
    ("Heat", "热度"),
    ("Highlights", "高光"),
    ("Hit Length", "冲击长度"),
    ("In", "入"),
    ("Ink", "墨色"),
    ("Invert", "反转"),
    ("Jitter", "抖动"),
    ("Keep brightness", "保留亮度"),
    ("Key colour", "键控颜色"),
    ("Left", "左"),
    ("Light", "轻"),
    ("Medium", "中"),
    ("Strong", "强"),
    ("Left to Right", "从左到右"),
    ("Length", "时长"),
    ("Levels", "色阶"),
    ("Lift", "提升"),
    ("Light leak", "漏光"),
    ("Lights", "亮部"),
    ("Lines", "线条"),
    ("Luma", "亮度通道"),
    ("Matte", "遮罩"),
    ("Motion Blur", "动感模糊"),
    ("Noise", "噪点"),
    ("Out", "出"),
    ("Pitch", "音高"),
    ("Pop", "弹出"),
    ("Protect skin", "保护肤色"),
    ("Punch", "冲击"),
    ("Quake", "震颤"),
    ("Radius", "半径"),
    ("Red", "红"),
    ("Result", "结果"),
    ("Right", "右"),
    ("Right to Left", "从右到左"),
    ("Rotation", "旋转"),
    ("Saturation", "饱和度"),
    ("Semitones", "半音"),
    ("Shadows", "阴影"),
    ("Shape", "形状"),
    ("Sharpen", "锐化"),
    ("Shift", "位移"),
    ("Shrink / grow", "收缩 / 扩张"),
    ("Sideways", "横向"),
    ("Size", "大小"),
    ("Sky", "天空"),
    ("Soften", "柔化"),
    ("Softness", "柔和度"),
    ("Speed", "速度"),
    ("Spill", "溢色"),
    ("Split", "分离"),
    ("Spread", "扩散"),
    ("Star", "星形"),
    ("Status", "状态"),
    ("Strength", "强度"),
    ("Style", "风格"),
    ("Target", "目标"),
    ("Temperature", "色温"),
    ("Tempo", "节奏"),
    ("Threshold", "阈值"),
    ("Tint", "色调"),
    ("Tone", "影调"),
    ("Top to Bottom", "从上到下"),
    ("Turns", "圈数"),
    ("Twist", "扭转"),
    ("Up", "上"),
    ("Up and Down", "上下"),
    ("View", "视图"),
    ("Vignette", "暗角"),
    ("Warmth", "暖度"),
    ("Water", "水感"),
    ("Whites", "白色"),
    ("Zoom Punch", "变焦冲击"),
];

/// The categories the built-in catalogue ships, and their Chinese. The
/// library's shelves are these: a package files itself under one.
const CATEGORIES: &[(&str, &str)] = &[
    ("Adjust", "调整"),
    ("Basic", "基础"),
    ("Black & White", "黑白"),
    ("Blur", "模糊"),
    ("Cinematic", "电影感"),
    ("Clean", "修复"),
    ("Color", "色彩"),
    ("Cutout", "抠像"),
    ("Dissolve", "溶解"),
    ("Distort", "变形"),
    ("Everyday", "日常"),
    ("Film", "胶片"),
    ("Food", "美食"),
    ("Glitch", "故障"),
    ("Light", "光影"),
    ("Portrait", "人像"),
    ("Retro", "复古"),
    ("Scenery", "风光"),
    ("Shake", "震动"),
    ("Slide", "滑动"),
    ("Space", "空间"),
    ("Spin", "旋转"),
    ("Stylised", "风格化"),
    ("Stylize", "风格化"),
    ("Tone", "影调"),
    ("Vibe", "氛围"),
    ("Voice", "人声"),
    ("Wipe", "擦除"),
    ("Zoom", "变焦"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every built-in package's words have a Chinese entry: the panels a
    /// person works in must not fall back to English for a word this build
    /// ships itself. The test reads the same manifests the app loads, so a
    /// package added next release is caught here rather than in a
    /// screenshot.
    #[test]
    fn every_built_in_word_is_translated() {
        let mut missing = Vec::new();
        for package in concat_effects::Catalogue::builtin().packages() {
            let meta = &package.manifest.effect;
            if lookup(NAMES, &meta.name).is_none() {
                missing.push(format!("name {:?} ({})", meta.name, meta.id));
            }
            if !meta.category.is_empty() && lookup(CATEGORIES, &meta.category).is_none() {
                missing.push(format!("category {:?}", meta.category));
            }
            for param in &package.manifest.params {
                if lookup(LABELS, &param.label).is_none() {
                    missing.push(format!("label {:?} ({})", param.label, param.key));
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "no Chinese for:\n{}", missing.join("\n"));
    }

    /// A word the table does not know comes back as it was: a user's own
    /// package and a text preset keep the names their authors gave them.
    #[test]
    fn an_unknown_word_is_left_alone() {
        assert_eq!(name("My Look"), "My Look");
        assert_eq!(label("Колор"), "Колор");
        assert_eq!(category("Someone's shelf"), "Someone's shelf");
        // Case does not matter for the words it does know.
        assert_eq!(name("chroma key"), "抠像");
        assert_eq!(label("TEMPERATURE"), "色温");
    }
}
