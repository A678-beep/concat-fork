// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! How long the last launches took, phase by phase, for Settings > About.
//!
//! The log already says it once per run - the `start: done in ...` line,
//! with every step on it - but the person who has to decide whether an
//! update made the window slower is looking at the window, not at a file.
//! So the last few launches are kept in `<config>/startup.json` and drawn
//! as rows in the About page, next to the machine's own facts. Nothing here
//! is sent anywhere; the file is the person's, like the log.
//!
//! Written on a worker after the window is up. A failed write is a warning,
//! never a reason not to have started.

use serde::{Deserialize, Serialize};

/// One launch's report.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Launch {
    /// Unix milliseconds at the window coming up, UTC.
    pub at: u64,
    /// Log opened to first publish, milliseconds.
    pub total_ms: u64,
    /// Each phase that ended, and how long it took.
    pub steps: Vec<(String, u64)>,
}

const FILE: &str = "startup.json";

/// How many launches are kept; five fills the About page without becoming
/// a log of its own.
const KEEP: usize = 5;

fn path(dirs: &concat_host::AppDirs) -> std::path::PathBuf {
    dirs.config.join(FILE)
}

/// The launches on file, oldest first. An unreadable or older file is an
/// empty list: this is a report, not state the app needs.
pub fn read(dirs: &concat_host::AppDirs) -> Vec<Launch> {
    std::fs::read_to_string(path(dirs))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Appends this launch, keeps the last [`KEEP`], and returns the file as it
/// now stands.
pub fn record(
    dirs: &concat_host::AppDirs,
    steps: Vec<(String, u64)>,
    total_ms: u64,
) -> Vec<Launch> {
    let mut launches = read(dirs);
    launches.push(Launch {
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as u64),
        total_ms,
        steps,
    });
    while launches.len() > KEEP {
        launches.remove(0);
    }
    if let Ok(text) = serde_json::to_string(&launches) {
        if let Err(error) = std::fs::write(path(dirs), text) {
            log::warn!(
                "startup: wanted to keep this launch's report and could not: {error}"
            );
        }
    }
    launches
}

/// The rows Settings > About shows: the newest launch first, one row each.
pub fn facts(launches: &[Launch]) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for (index, launch) in launches.iter().rev().enumerate() {
        let which = if index == 0 {
            "最近启动 · 本次".to_owned()
        } else {
            format!("最近启动 · {}", stamp(launch.at))
        };
        let mut value = format!("{:.2} s", launch.total_ms as f64 / 1000.0);
        for (step, ms) in &launch.steps {
            value.push_str(&format!(" · {step} {ms} ms"));
        }
        rows.push((which, value));
    }
    if rows.is_empty() {
        rows.push((
            "最近启动".to_owned(),
            "还没有记录：本次启动完成后会写在这里".to_owned(),
        ));
    }
    rows
}

/// `2026-11-22 09:30` in UTC, from unix milliseconds. The log calls its
/// times UTC too; a support report can compare the two.
fn stamp(ms: u64) -> String {
    let seconds = ms / 1000;
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3600,
        (rest % 3600) / 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date; Howard Hinnant's
/// `civil_from_days`, the short version.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_instant_is_written_as_its_utc_date() {
        // 2026-11-22 09:30:00 UTC.
        assert_eq!(stamp(1_795_339_800_000), "2026-11-22 09:30");
        // The epoch itself.
        assert_eq!(stamp(0), "1970-01-01 00:00");
    }

    #[test]
    fn facts_put_the_newest_first_and_carry_each_phase() {
        let launches = vec![
            Launch {
                at: 0,
                total_ms: 3_400,
                steps: vec![("studio".to_owned(), 2_600)],
            },
            Launch {
                at: 0,
                total_ms: 640,
                steps: vec![("studio".to_owned(), 40)],
            },
        ];
        let rows = facts(&launches);
        assert!(rows[0].0.contains("本次"));
        assert!(rows[1].0.contains("1970-01-01"));
        assert!(rows[1].1.starts_with("3.40 s"));
        assert!(rows[1].1.contains("studio 2600 ms"));
    }
}
