// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 用一份现有工程生成 0.2.28 的互操作样例：剪映草稿与 FCPXML / PR XML。
//!
//! ```text
//! cargo run -p concat-project --example interop_sample -- <工程文件夹> <输出文件夹>
//! ```
//!
//! 工程文件夹里有 `concat.json` 就行（星籁剪辑的工程格式）。样例写出后
//! 由 `work/interop-028/validate.py` 用 Python 的 json / xml.etree 再做一遍
//! 独立校验：这里的工作只是把文件摆出来，校验不靠自己的测试。

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let project_folder = args
        .next()
        .ok_or_else(|| "用法：interop_sample <工程文件夹> <输出文件夹>".to_owned())?;
    let out_folder = args
        .next()
        .ok_or_else(|| "用法：interop_sample <工程文件夹> <输出文件夹>".to_owned())?;
    let project_folder = std::path::PathBuf::from(project_folder);
    let out_folder = std::path::PathBuf::from(out_folder);

    let text = std::fs::read_to_string(project_folder.join("concat.json"))
        .map_err(|error| format!("读不到 {}\\concat.json：{error}", project_folder.display()))?;
    let document: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("concat.json 不是 JSON：{error}"))?;
    let project =
        concat_project::from_document(&document).ok_or_else(|| "这份工程读不出来".to_owned())?;
    let name = project_folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "星籁剪辑".to_owned());
    let timeline = project.active();
    println!(
        "工程：{} · {} 条时间线 · {} 个片段 · {} 个素材 · 时长 {:.2}s · {}x{} @ {:.3}fps",
        name,
        project.timelines.len(),
        timeline.clips.len(),
        project.media.len(),
        timeline.duration(),
        timeline.video.width,
        timeline.video.height,
        timeline.video.rate()
    );

    let draft_folder = concat_project::capcut_export::free_folder(
        &out_folder,
        &format!("{name}-剪映草稿"),
    );
    let draft = concat_project::capcut_export::write(&project, &name, &draft_folder)?;
    println!(
        "剪映草稿：{} · {} 个文件 · {} 个片段 · {} 条字幕 · 降级 {} 条",
        draft_folder.display(),
        draft.files.len(),
        draft.clips,
        draft.titles,
        draft.notes.len()
    );
    for note in &draft.notes {
        println!("  降级：{note}");
    }

    let xml_folder = out_folder.join(format!("{name}-XML"));
    let xml = concat_project::fcpxml::write(&project, &name, &xml_folder)?;
    println!(
        "XML：{} · {} 个文件 · {} 个片段 · {} 帧",
        xml_folder.display(),
        xml.files.len(),
        xml.clips,
        xml.frames
    );
    for note in &xml.notes {
        println!("  说明：{note}");
    }
    println!("\n样例已写出：{}", out_folder.display());
    Ok(())
}
