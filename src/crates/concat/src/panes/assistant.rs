// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 XingLai contributors

//! 左侧「AI 助手」常驻栏（0.3.1，简版）。
//!
//! 剪映 11.9 把「剪映助手」提升成与素材同级的常驻入口；这里给一个桌面简版：
//! 一句话一问（平台 `/xiaolai/chat` 文本模式）→ 回复 + 三条快捷指令。
//!
//! 定位说清楚：它是**建议类助手**，不会自动改时间线、不会替人花积分。
//! 回复里平台若顺手建了任务，面板如实提示「去 AI 工作室看成片」。快捷指令
//! 只做三件确定的事：打开 AI 工作室并预填需求、打开 AI 短剧、打开导出面板。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use concat_host::assistant;

use crate::host;
use crate::panes::Msg;
use crate::studio::Studio;
use crate::ui::AssistantData;

/// 工作模式：剪映助手同类的「模式」下拉（简版，只影响建议口径）。
const MODES: &[&str] = &["图片生成", "视频生成", "视频编辑", "智能粗剪", "视频包装"];

/// 技能 chips：点一下就是问一句（不直接执行）。
const SKILLS: &[&str] = &[
    "开场三秒怎么设计更抓人？",
    "字幕怎么排更清楚？",
    "配什么 BGM 更贴？",
    "节奏怎么剪更紧凑？",
];

/// 面板消息。
#[derive(Debug)]
pub enum AssistantMsg {
    /// 打开 / 收起。
    Toggle,
    /// 明确设成打开或收起。
    Set(bool),
    /// 用户发了一句话。
    Send(String),
    /// 快捷指令。
    Quick(String),
    /// 输入框里的字变了。
    Input(String),
    /// 工作模式换了。
    Mode(i32),
    /// 小来的回复（或失败原因）。
    Reply(Box<Result<assistant::Reply, String>>),
}

/// AI 助手栏的状态。
pub struct AssistantPane {
    /// 展开着。
    pub open: bool,
    /// 正在等回复。
    busy: bool,
    /// 输入框里的字。
    input: String,
    /// 工作模式下标（[`MODES`]）。
    mode: i32,
    /// 上下文提示（当前草稿、选中片段），发布时由 Studio 给。
    context: String,
    /// 对话：`(是不是用户说的, 文字)`。
    messages: Vec<(bool, String)>,
    /// 常驻一句提示。
    note: String,
    /// 平台会话 id。
    session: String,
    /// 最近一次提示，避免同一句反复弹。
    last_alert: String,
    /// 当前等待的取消标志（关掉面板也让它自然结束，回复仍然落进历史）。
    cancel: Option<Arc<AtomicBool>>,
}

impl Default for AssistantPane {
    fn default() -> Self {
        Self {
            open: false,
            busy: false,
            input: String::new(),
            mode: 2,
            context: String::new(),
            messages: Vec::new(),
            note: "建议类助手：只给建议、不自动改时间线。".to_owned(),
            session: format!("desktop-{}", concat_host::ai_studio::stamp_ms()),
            last_alert: String::new(),
            cancel: None,
        }
    }
}

impl AssistantPane {
    /// 处理一条消息。
    pub fn update(&mut self, msg: AssistantMsg, studio: &mut Studio) {
        match msg {
            AssistantMsg::Toggle => {
                self.open = !self.open;
                if self.open && self.messages.is_empty() {
                    self.greet();
                }
            }
            AssistantMsg::Set(open) => {
                self.open = open;
                if self.open && self.messages.is_empty() {
                    self.greet();
                }
            }
            AssistantMsg::Send(text) => self.send(text, studio),
            AssistantMsg::Quick(command) => self.quick(&command, studio),
            AssistantMsg::Input(text) => self.input = text.chars().take(500).collect(),
            AssistantMsg::Mode(index) => {
                self.mode = index.clamp(0, (MODES.len() - 1) as i32);
                self.note = format!(
                    "模式：{}（只影响建议口径；不自动改时间线、不替你花积分）。",
                    MODES[self.mode as usize]
                );
            }
            AssistantMsg::Reply(result) => {
                self.busy = false;
                self.cancel = None;
                match *result {
                    Ok(reply) => {
                        if !reply.session.trim().is_empty() {
                            self.session.clone_from(&reply.session);
                        }
                        let mut line = format!("小来：{}", reply.text);
                        if !reply.cost.trim().is_empty() {
                            line.push_str(&format!("\n（{}）", reply.cost));
                        }
                        if !reply.tool.trim().is_empty() {
                            line.push_str(&format!("\n工具：{}", reply.tool));
                        }
                        self.messages.push((false, line));
                        self.note = if reply.note.trim().is_empty() {
                            "建议类助手：只给建议、不自动改时间线；生成类任务请到 AI 工作室确认。"
                                .to_owned()
                        } else {
                            reply.note
                        };
                        self.trim();
                    }
                    Err(error) => {
                        self.messages
                            .push((false, format!("小来：{error}")));
                        self.note = "追问不了的时候，也可以自己动手：右侧面板 / 时间线都能直接改。".to_owned();
                        self.trim();
                    }
                }
            }
        }
    }

    /// 第一次展开时的一句引导（空态即引导）。
    fn greet(&mut self) {
        if self.messages.is_empty() {
            self.messages.push((
                false,
                "小来：我是剪辑助手。可以问我「这个片子怎么剪更抓人」「开场三秒怎么设计」；\
要直接出片就到 AI 工作室，要写短剧就到 AI 短剧。我只给建议，不会替你改时间线。"
                    .to_owned(),
            ));
        }
    }

    /// 发一句话。
    fn send(&mut self, text: String, studio: &mut Studio) {
        let text = text.trim().to_owned();
        if text.is_empty() {
            studio.notify("先写一句要问的话", true);
            return;
        }
        if self.busy {
            studio.notify("小来还在回上一句，等一下", true);
            return;
        }
        if self.input.trim() == text {
            self.input.clear();
        }
        self.messages.push((true, format!("你：{text}")));
        self.busy = true;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        let token = concat_host::account::load().map(|account| account.token);
        let session = self.session.clone();
        host::spawn(
            move || assistant::chat(token.as_deref(), &text, &session),
            |studio, _, _, result| {
                studio.handle(Msg::Assistant(AssistantMsg::Reply(Box::new(result))));
            },
        );
    }

    /// 四条快捷指令：三条确定动作 + 一句常用问题。
    fn quick(&mut self, command: &str, studio: &mut Studio) {
        match command {
            "生成 15 秒视频" => {
                self.open = true;
                self.greet();
                self.messages.push((
                    false,
                    "小来：帮你在 AI 工作室打开「AI 一键成片」，需求已预填成一条 15 秒门店宣传片；\
提交前可以自己改文案，确认计费再生成。".to_owned(),
                ));
                self.note = "已把需求预填到 AI 工作室：确认文案与积分后再点生成。".to_owned();
                studio.handle(Msg::AiStudio(crate::panes::ai_studio::AiStudioMsg::Action {
                    action: "ae-text".to_owned(),
                    index: 0,
                    text: "帮我做一条 15 秒的门店宣传片：主打「当天上门、明码标价」，语气亲切，结尾引导到店咨询。".to_owned(),
                }));
                studio.handle(Msg::AiStudio(crate::panes::ai_studio::AiStudioMsg::Open(0)));
            }
            "打开 AI 短剧" => {
                self.open = true;
                self.greet();
                self.messages.push((
                    false,
                    "小来：已经帮你打开 AI 短剧。设定题材和集数 → 生成分集剧本（可逐集改）→ \
配分镜素材 → 逐集或整剧出片，成片自动下载并可以一键插入时间轴。".to_owned(),
                ));
                self.note = "AI 短剧已在桌面打开：完整流程都在软件里，网页版只是备用。".to_owned();
                studio.handle(Msg::AiDrama(crate::panes::aidrama::AiDramaMsg::Open(0)));
            }
            "导出当前工程" => {
                self.messages.push((
                    false,
                    "小来：导出面板打开了。分辨率/码率/字幕这些都按你的选择走；\
空时间线会被拦下（和剪映一样先提示「时间线为空」）。".to_owned(),
                ));
                self.note = "导出面板已打开；参数按需改，导出前确认保存工程。".to_owned();
                studio.handle(Msg::Export(crate::panes::export::ExportMsg::Open));
            }
            // 其余当一句话问出去。
            other => self.send(other.to_owned(), studio),
        }
    }

    /// 只留最近 24 条，免得越聊越占内存。
    fn trim(&mut self) {
        if self.messages.len() > 24 {
            let cut = self.messages.len() - 24;
            self.messages.drain(0..cut);
        }
    }

    /// 发布给 Slint 的数据。`context` 由 Studio 给：当前草稿 + 选中片段。
    pub fn data(&self, signed_in: bool, account_line: &str, context: &str) -> AssistantData {
        AssistantData {
            open: self.open,
            busy: self.busy,
            input: self.input.as_str().into(),
            note: self.note.as_str().into(),
            account_line: account_line.into(),
            signed_in,
            context: if context.trim().is_empty() {
                "上下文：当前草稿".into()
            } else {
                context.into()
            },
            mode: self.mode,
            modes: slint::ModelRc::new(slint::VecModel::from(
                MODES.iter().map(|mode| slint::SharedString::from(*mode)).collect::<Vec<_>>(),
            )),
            skills: slint::ModelRc::new(slint::VecModel::from(
                SKILLS
                    .iter()
                    .map(|skill| slint::SharedString::from(*skill))
                    .collect::<Vec<_>>(),
            )),
            quick: slint::ModelRc::new(slint::VecModel::from({
                let mut rows: Vec<slint::SharedString> = vec![
                    "生成 15 秒视频".into(),
                    "打开 AI 短剧".into(),
                    "导出当前工程".into(),
                ];
                rows.push("开场三秒怎么设计更抓人？".into());
                rows
            })),
        }
    }

    /// 对话行，给面板。
    pub fn message_rows(&self) -> Vec<slint::SharedString> {
        self.messages
            .iter()
            .map(|(_, line)| line.as_str().into())
            .collect()
    }
}
