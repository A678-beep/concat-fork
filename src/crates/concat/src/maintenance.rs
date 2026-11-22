// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! Cache cleanup for Settings > About's diagnostics block.
//!
//! Only what the app can make again is ever deleted here: the asset cache
//! it downloaded, the effect cards and title pictures it drew, and the
//! plugin packages it downloaded. A project, a sound the person imported, a
//! voice they cloned and a plugin they installed are never touched - those
//! are in `dirs.data`'s `audio` and `voices`, in `dirs.config`'s
//! `ai-plugins/<id>`, and in the project folders themselves.
//!
//! Runs on a worker from the Settings sheet; the caller says what it freed.

use std::path::Path;

use concat_host::AppDirs;

/// What a cleanup removed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cleared {
    /// Files removed.
    pub files: u64,
    /// Bytes they took.
    pub bytes: u64,
}

impl Cleared {
    fn add(&mut self, other: Cleared) {
        self.files += other.files;
        self.bytes += other.bytes;
    }
}

/// The downloaded asset cache, the drawn effect cards and title pictures.
/// The next time one is needed it is downloaded or drawn again.
pub fn clear_asset_cache(dirs: &AppDirs) -> Cleared {
    let mut total = Cleared::default();
    for path in [
        dirs.config.join("asset-cache"),
        // Both are redrawn from the catalogue and the projects.
        dirs.data.join("cards"),
        dirs.data.join("titles"),
    ] {
        total.add(remove(&path));
    }
    log::info!(
        "maintenance: asset cache cleared · {} files · {}",
        total.files,
        concat_log::bytes(total.bytes)
    );
    total
}

/// The plugin download cache (`.downloads` holds the zips a plugin was
/// installed from, two gigabytes of them here), and what is left of an
/// interrupted import. Installed plugin folders are untouched: those are
/// the person's.
pub fn clear_plugin_cache(dirs: &AppDirs) -> Cleared {
    let root = concat_host::plugins::root(dirs);
    let mut total = remove(&root.join(".downloads"));
    total.add(remove(&root.join(".sizes.json")));
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".import-") {
                total.add(remove(&entry.path()));
            }
        }
    }
    log::info!(
        "maintenance: plugin cache cleared · {} files · {}",
        total.files,
        concat_log::bytes(total.bytes)
    );
    total
}

/// Removes one path and counts what it held first.
fn remove(path: &Path) -> Cleared {
    let mut cleared = Cleared::default();
    count(path, &mut cleared);
    let removed = if path.is_dir() {
        std::fs::remove_dir_all(path).is_ok()
    } else if path.exists() {
        std::fs::remove_file(path).is_ok()
    } else {
        true
    };
    if !removed {
        log::warn!("maintenance: could not remove {}", path.display());
    }
    cleared
}

/// Counts a tree, without following links: a plugin folder can hold a
/// junction to somewhere else on the disk and this must not walk it.
fn count(path: &Path, out: &mut Cleared) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                count(&entry.path(), out);
            }
        }
    } else {
        out.files += 1;
        out.bytes += meta.len();
    }
}
