// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 智能粗剪（0.3.2，顶部 tab 的那一页）。
//!
//! 平台 `/api/smartcut/*` 已上线，这里把页面接到完整闭环：
//! 选本地视频 → 上传（`/upload`）→ 出计划（`/smartcut/plan`）→ 渲染
//! （`/smartcut/render` + 轮询 `/smartcut/task/{id}`）→ 下载并插入时间轴。
//!
//! 不在这里假装成功：登录态、视频、平台报错都如实写进 `smartcut_status`，
//! 404/5xx 翻成「接口联调中」，429 说高峰限流，并保留「用 AI 工作室成片」
//! 的替代路径（在页面上，不在这个模块里）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_project::Command;

use crate::host;
use crate::studio::Studio;

impl Studio {
    /// 选一条本地视频。选完只记路径，上传与拆解在「上传并出粗剪计划」里做。
    pub fn smartcut_pick(&mut self) {
        if self.smartcut_busy {
            self.notify("智能粗剪正在跑，稍等再换视频", false);
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("选择要粗剪的视频（3 秒–15 分钟，≤300MB，要有音轨）")
            .add_filter("视频", &["mp4", "mov", "webm", "m4v", "mkv"])
            .pick_file()
        else {
            return;
        };
        let size = std::fs::metadata(&path).map_or(0, |meta| meta.len());
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "video.mp4".to_owned());
        if size > 300 * 1024 * 1024 {
            self.smartcut_status =
                format!("视频 {:.0} MB 超过平台 300MB 上限：请先裁剪再试", size as f64 / (1024.0 * 1024.0));
            return;
        }
        self.smartcut_video = Some(PathBuf::from(&path));
        self.smartcut_video_name = format!("{name}（{:.1} MB）", size as f64 / (1024.0 * 1024.0));
        self.smartcut_plan_id.clear();
        self.smartcut_plan_title.clear();
        self.smartcut_plan_lines.clear();
        self.smartcut_progress = 0.0;
        self.smartcut_stage.clear();
        self.smartcut_output_url.clear();
        self.smartcut_output_ready = false;
        self.smartcut_status =
            "已选好视频：点「上传并出粗剪计划」，平台会拆镜头、去废话、顺节奏".to_owned();
    }

    /// 上传 + 出计划。两段都在 worker 上跑，进度通过 `on_ui` 回报。
    pub fn smartcut_submit(
        &mut self,
        mode: String,
        precision: String,
        requirement: String,
    ) {
        if self.smartcut_busy {
            self.notify("智能粗剪上一次提交还没回音，先等它一下", false);
            return;
        }
        let Some(path) = self.smartcut_video.clone() else {
            self.smartcut_status = "先选一条本地视频，再提交".to_owned();
            return;
        };
        let Some(token) = concat_host::account::load().map(|account| account.token) else {
            self.smartcut_status = "请先登录星籁账号：登录后再提交智能粗剪".to_owned();
            return;
        };
        let preset = preset_for(&mode).to_owned();
        let precision = precision.clone();
        self.smartcut_busy = true;
        self.smartcut_progress = 0.05;
        self.smartcut_stage = "上传视频…".to_owned();
        self.smartcut_output_ready = false;
        self.smartcut_status = format!("已提交：{mode} · {}…", grade(&precision));
        let cancel = Arc::new(AtomicBool::new(false));
        self.smartcut_cancel = Some(Arc::clone(&cancel));
        let mode_line = mode.clone();
        host::spawn(
            {
                let cancel = Arc::clone(&cancel);
                let token = token.clone();
                move || -> Result<concat_host::smartcut::Plan, String> {
                    let mut upload_report = |fraction: f32, stage: String| {
                        host::on_ui(move |studio, _, _| {
                            studio.smartcut_progress = fraction * 0.4;
                            studio.smartcut_stage = stage;
                        });
                    };
                    let url = concat_host::smartcut::upload(
                        &token,
                        &path,
                        &mut upload_report,
                        &cancel,
                    )?;
                    host::on_ui(|studio, _, _| {
                        studio.smartcut_progress = 0.45;
                        studio.smartcut_stage = "平台拆镜头、挑片段…".to_owned();
                    });
                    concat_host::smartcut::plan(
                        &token,
                        &url,
                        &requirement,
                        &preset,
                        &precision,
                    )
                }
            },
            move |studio, _, _, result| {
                studio.smartcut_busy = false;
                studio.smartcut_progress = 1.0;
                studio.smartcut_cancel = None;
                match result {
                    Ok(plan) => {
                        if plan.plan_id.is_empty() {
                            studio.smartcut_status =
                                "平台返回了片段，但没有 plan_id：无法继续渲染，请重试".to_owned();
                            return;
                        }
                        studio.smartcut_plan_id = plan.plan_id.clone();
                        studio.smartcut_plan_title = if plan.title.is_empty() {
                            format!("{mode_line} · {} 段", plan.segments.len())
                        } else {
                            plan.title.clone()
                        };
                        let mut lines = Vec::new();
                        for (index, segment) in plan.segments.iter().enumerate() {
                            let role = if segment.role.is_empty() {
                                String::new()
                            } else {
                                format!("【{}】", segment.role)
                            };
                            lines.push(format!(
                                "{:02}  {:.1}s–{:.1}s  {}{}",
                                index + 1,
                                segment.start,
                                segment.end,
                                role,
                                if segment.reason.is_empty() {
                                    String::new()
                                } else {
                                    format!(" {}", segment.reason)
                                }
                            ));
                        }
                        studio.smartcut_plan_lines = lines;
                        studio.smartcut_stage.clear();
                        studio.smartcut_status =
                            format!("计划已出：{}。满意就点「渲染成片」。", plan.stats_line);
                    }
                    Err(error) => {
                        studio.smartcut_stage.clear();
                        studio.smartcut_status =
                            format!("{error}——可重试，或直接用下方「用 AI 工作室成片」。");
                    }
                }
            },
        );
    }

    /// 渲染计划，轮询到成片。`ratio` 用工程当前的输出尺寸。
    pub fn smartcut_render(&mut self) {
        if self.smartcut_busy {
            self.notify("智能粗剪正在跑，等它回音", false);
            return;
        }
        if self.smartcut_plan_id.is_empty() {
            self.smartcut_status = "还没有计划：先「上传并出粗剪计划」".to_owned();
            return;
        }
        let Some(token) = concat_host::account::load().map(|account| account.token) else {
            self.smartcut_status = "请先登录星籁账号再渲染".to_owned();
            return;
        };
        let plan_id = self.smartcut_plan_id.clone();
        let ratio = if self.output_size().0 >= self.output_size().1 {
            "16:9"
        } else {
            "9:16"
        }
        .to_owned();
        let cancel = Arc::new(AtomicBool::new(false));
        self.smartcut_cancel = Some(Arc::clone(&cancel));
        self.smartcut_busy = true;
        self.smartcut_progress = 0.05;
        self.smartcut_stage = "提交渲染…".to_owned();
        self.smartcut_output_ready = false;
        self.smartcut_status = "渲染已提交，等平台出片（约 8–30 秒）…".to_owned();
        host::spawn(
            {
                let cancel = Arc::clone(&cancel);
                move || -> Result<concat_host::smartcut::TaskStatus, String> {
                    let started = concat_host::smartcut::render(&token, &plan_id, &ratio)?;
                    if started.task_id.is_empty() {
                        return Err("平台受理了渲染，但没给任务 id：稍后到成片库看看".to_owned());
                    }
                    let mut last = None::<std::time::Instant>;
                    concat_host::smartcut::wait_render(
                        &token,
                        &started.task_id,
                        move |fraction, stage| {
                            let due = last.is_none_or(|at: std::time::Instant| {
                                at.elapsed() >= std::time::Duration::from_millis(400)
                            });
                            if !due {
                                return;
                            }
                            last = Some(std::time::Instant::now());
                            // 0.05..0.95 给渲染过程，剩下的给下载。
                            let shown = 0.05 + fraction * 0.9;
                            host::on_ui(move |studio, _, _| {
                                studio.smartcut_progress = shown;
                                studio.smartcut_stage = stage;
                            });
                        },
                        &cancel,
                    )
                }
            },
            |studio, _, _, result| {
                studio.smartcut_busy = false;
                studio.smartcut_cancel = None;
                studio.smartcut_progress = 1.0;
                match result {
                    Ok(done) => {
                        studio.smartcut_output_url = done.video_url.clone();
                        studio.smartcut_output_ready = true;
                        studio.smartcut_stage.clear();
                        studio.smartcut_status = format!(
                            "成片好了：{:.1} 秒{}。点「下载并插入时间轴」继续精剪。",
                            done.duration,
                            if done.size > 0.0 {
                                format!("、{:.1} MB", done.size / (1024.0 * 1024.0))
                            } else {
                                String::new()
                            }
                        );
                    }
                    Err(error) => {
                        studio.smartcut_stage.clear();
                        studio.smartcut_status = format!("{error}——可重试，或改用 AI 工作室成片。");
                    }
                }
            },
        );
    }

    /// 下载成片并插到时间线（播放头起）。
    pub fn smartcut_insert(&mut self) {
        if self.smartcut_busy {
            self.notify("还有任务在跑，等它回音", false);
            return;
        }
        if !self.smartcut_output_ready || self.smartcut_output_url.is_empty() {
            self.smartcut_status = "还没有渲染好的成片：先「渲染成片」".to_owned();
            return;
        }
        let Some(token) = concat_host::account::load().map(|account| account.token) else {
            self.smartcut_status = "请先登录星籁账号再下载".to_owned();
            return;
        };
        let dirs = self.host.dirs.clone();
        let url = self.smartcut_output_url.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.smartcut_cancel = Some(Arc::clone(&cancel));
        self.smartcut_busy = true;
        self.smartcut_progress = 0.1;
        self.smartcut_stage = "下载成片…".to_owned();
        self.smartcut_status = "下载中…".to_owned();
        host::spawn(
            {
                let cancel = Arc::clone(&cancel);
                move || concat_host::smartcut::download(&dirs, &token, &url, &cancel)
            },
            |studio, _, _, result| {
                studio.smartcut_busy = false;
                studio.smartcut_cancel = None;
                studio.smartcut_progress = 1.0;
                studio.smartcut_stage.clear();
                match result {
                    Ok(path) => {
                        let inserted = insert_output(studio, &path);
                        studio.smartcut_status = if inserted {
                            format!(
                                "已下载并插入时间轴：{}",
                                path.file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_default()
                            )
                        } else {
                            format!("成片已下载到 {}，但插入失败（先把工程打开）", path.display())
                        };
                    }
                    Err(error) => {
                        studio.smartcut_status = format!("{error}——可重试。");
                    }
                }
            },
        );
    }
}

/// 页面上的六种方式 → 平台约定的 preset 值。
fn preset_for(mode: &str) -> &'static str {
    match mode {
        "口播片" => "口播片",
        "配音解说" => "配音解说",
        "长视频切片" => "长视频切片",
        "VLOG记录" => "VLOG记录",
        "混剪" => "混剪",
        _ => "通用",
    }
}

fn grade(precision: &str) -> &'static str {
    if precision == "high" {
        "高精度"
    } else {
        "标准"
    }
}

/// 把成片加进素材库并从播放头插到时间线，返回是否插入。
fn insert_output(studio: &mut Studio, path: &std::path::Path) -> bool {
    let Ok(summary) = concat_host::media::probe(&path.to_string_lossy()) else {
        return false;
    };
    let mut item = summary.to_new_media();
    item.name = path
        .file_stem()
        .map(|stem| format!("智能粗剪 · {}", stem.to_string_lossy()))
        .unwrap_or_else(|| "智能粗剪成片".to_owned());
    if let Some(existing) = studio
        .project()
        .media
        .iter()
        .find(|media| media.path == summary.path)
        .map(|media| media.id.clone())
        .or_else(|| studio.apply(Command::AddMedia { item }))
    {
        let start = f64::from(studio.playhead.max(0.0));
        return studio
            .apply(Command::AddClipAtFirstFree {
                media_id: existing,
                start,
            })
            .is_some();
    }
    false
}
