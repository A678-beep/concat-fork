// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai AI and Concat contributors

//! 外部命令行工具（ffmpeg / ffprobe）在这台机器上的位置。
//!
//! 悬停预览、素材指纹和本地 AI 的成片拼接都要起一个 ffmpeg / ffprobe，
//! 而安装包把它放在哪里是平台自己的事：Windows 的 setup 放在 `concat.exe`
//! 旁边，macOS 的 `.app` 放在 `Contents/Resources/bin`（见
//! `scripts/macos/package.sh`）。开发机上两处都没有时，最后按裸名字交给
//! `Command`，让系统去 PATH 里找；再找不到，错误由起进程的那一层报出来。
//!
//! 平台的差别只有三样：名字带不带 `.exe`、随包的文件夹在哪、裸名字是什么。
//! 调用方只用 [`ffmpeg`] 和 [`ffprobe`]。

use std::path::{Path, PathBuf};

/// 一个工具在这台机器上的文件名：Windows 带 `.exe`，别的平台是裸名字。
fn exe_name(tool: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{tool}.exe")
    } else {
        tool.to_owned()
    }
}

/// 安装包放工具的几个文件夹，第一个命中的就是它；没有安装包时是空的。
fn shipped_dirs() -> Vec<PathBuf> {
    let Some(bin) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    // Only the macOS arm below pushes; Windows keeps the one folder.
    #[allow(unused_mut)]
    let mut dirs = vec![bin.clone()];
    // macOS：`X.app/Contents/MacOS/concat` 的同伴在 `Contents/Resources/bin`。
    #[cfg(target_os = "macos")]
    {
        if let Some(contents) = bin.parent() {
            dirs.push(contents.join("Resources/bin"));
        }
    }
    dirs
}

/// 在 PATH 里找一个工具。
fn in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// 找这个工具在这台机器上的路径，交给 `Command` 用。
///
/// 先看环境变量（`XINGLAI_FFMPEG` / `XINGLAI_FFPROBE`，开发机用它指定自己
/// 编的 LGPL 一份），其次随包的文件夹，其次 PATH；都没有时返回平台惯用的
/// 裸名字，让 `Command` 去找。
pub fn find(tool: &str) -> PathBuf {
    let name = exe_name(tool);
    if let Some(given) = std::env::var_os(format!("XINGLAI_{}", tool.to_ascii_uppercase())) {
        let given = PathBuf::from(given);
        if given.is_file() {
            return given;
        }
    }
    if let Some(shipped) = shipped_dirs()
        .into_iter()
        .map(|dir| dir.join(&name))
        .find(|candidate| candidate.is_file())
    {
        return shipped;
    }
    in_path(&name).unwrap_or_else(|| PathBuf::from(name))
}

/// 随包 / 系统里的 ffmpeg。
pub fn ffmpeg() -> PathBuf {
    find("ffmpeg")
}

/// 随包 / 系统里的 ffprobe。
pub fn ffprobe() -> PathBuf {
    find("ffprobe")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_is_the_platforms() {
        let expected = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        assert_eq!(exe_name("ffmpeg"), expected);
    }

    #[test]
    fn a_tool_that_is_nowhere_comes_back_as_its_bare_name() {
        let name = "xinglai-tool-no-machine-has";
        let found = find(name);
        // 万一开发机的 PATH 里真有同名程序，那也还是这个名字。
        assert_eq!(found.file_stem().and_then(|stem| stem.to_str()), Some(name));
    }
}
