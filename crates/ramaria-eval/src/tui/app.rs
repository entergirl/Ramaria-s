//! crates/ramaria-eval/src/tui/app.rs - Ramaria 评测沙盒界面状态
//!
//! 设计特点:
//! - 界面状态集中一处：屏幕、焦点、输入缓冲、阶段与结果，渲染只读
//! - 按键单向转动作：`handle_key` 只改状态并返回 `AppAction`，不直接启动任务
//! - 后端屏先行：来源与独立配置在对话前确定，返回后端不丢已填内容
//! - 对话屏先校验对话格式，通过后进提问屏；校验错误留在状态行，不打断编辑
//! - 粘贴是独立事件入口：按当前焦点缓冲插入，单行字段过滤换行

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::render;
use super::text::TextBuffer;
use crate::sandbox::backend::LOCAL_DEFAULT_BASE_URL;
use crate::sandbox::{
    SandboxBackendChoice, SandboxBackendMode, SandboxInput, SandboxModelKind, SandboxReport,
    StageReport, parse_dialogue,
};

// =========================================================
// 屏幕与焦点
// =========================================================

/// 当前屏幕。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    /// 后端来源选择
    Backend,
    /// 对话编辑
    Dialogue,
    /// 问题输入
    Question,
    /// 运行中
    Running,
    /// 结果查看
    Result,
    /// 运行失败
    Error,
}

/// 后端屏的输入焦点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendFocus {
    /// 来源选择
    Mode,
    /// 模型服务地址
    BaseUrl,
    /// 模型标识
    Model,
    /// 向量模型目录
    Embedding,
}

/// 对话屏的输入焦点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    /// 人格预设（单行）
    Persona,
    /// 对话块（多行）
    Dialogue,
}

/// 结果屏的三个分页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultTab {
    /// 概览
    Overview,
    /// 召回轨迹
    Trace,
    /// 答案
    Answer,
}

/// 状态机产出的动作，由事件循环执行。
pub(crate) enum AppAction {
    /// 仅更新状态
    None,
    /// 进入运行屏并启动沙盒
    Run(SandboxInput),
    /// 退出界面
    Quit,
}

// =========================================================
// 界面状态
// =========================================================

/// 评测沙盒界面状态。
///
/// 字段约定:
/// - `screen` / `focus` / `result_tab`: 当前屏幕、对话屏焦点与结果分页；
/// - `persona` / `dialogue` / `question`: 三个输入缓冲；
/// - `backend_mode` / `backend_focus`: 后端屏的来源与焦点；
/// - `backend_base_url` / `backend_model` / `backend_embedding`: 后端屏单行缓冲；
/// - `status`: 各屏的校验提示；`error`: 失败屏的错误全文；
/// - `stages`: 运行阶段上报序列；`result`: 完成后的报告；
/// - `scroll`: 结果页纵向滚动行数；`running`: 后台运行是否在进行。
pub(crate) struct App {
    pub(crate) screen: Screen,
    pub(crate) focus: Focus,
    pub(crate) persona: TextBuffer,
    pub(crate) dialogue: TextBuffer,
    pub(crate) question: TextBuffer,
    pub(crate) backend_mode: SandboxBackendMode,
    pub(crate) backend_focus: BackendFocus,
    pub(crate) backend_base_url: TextBuffer,
    pub(crate) backend_model: TextBuffer,
    pub(crate) backend_embedding: TextBuffer,
    pub(crate) status: Option<String>,
    pub(crate) stages: Vec<StageReport>,
    pub(crate) result: Option<SandboxReport>,
    pub(crate) error: Option<String>,
    pub(crate) result_tab: ResultTab,
    pub(crate) scroll: u16,
    pub(crate) running: bool,
}

impl App {
    /// 初始状态：停在后端屏，来源与独立配置按入口注入的后端选择预填。
    ///
    /// 参数:
    /// - `default`: 入口注入的后端选择；继承来源时三个配置字段留空。
    pub(crate) fn new(default: SandboxBackendChoice) -> Self {
        let (backend_mode, backend_base_url, backend_model, backend_embedding) = match default {
            SandboxBackendChoice::Inherit => (
                SandboxBackendMode::Inherit,
                TextBuffer::new(),
                TextBuffer::new(),
                TextBuffer::new(),
            ),
            SandboxBackendChoice::Independent {
                kind,
                base_url,
                model_id,
                embedding_model_path,
            } => {
                let mode = match kind {
                    SandboxModelKind::Local => SandboxBackendMode::Local,
                    SandboxModelKind::Cloud => SandboxBackendMode::Cloud,
                };
                let embedding = embedding_model_path
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (
                    mode,
                    text_buffer(&base_url),
                    text_buffer(&model_id),
                    text_buffer(&embedding),
                )
            }
        };

        Self {
            screen: Screen::Backend,
            focus: Focus::Dialogue,
            persona: TextBuffer::new(),
            dialogue: TextBuffer::new(),
            question: TextBuffer::new(),
            backend_mode,
            backend_focus: BackendFocus::Mode,
            backend_base_url,
            backend_model,
            backend_embedding,
            status: None,
            stages: Vec::new(),
            result: None,
            error: None,
            result_tab: ResultTab::Overview,
            scroll: 0,
            running: false,
        }
    }

    // ===== 按键入口 =====

    /// 处理一个按键事件。
    ///
    /// 参数:
    /// - `key`: 终端按键；只响应按下事件（释放与重复事件直接忽略）。
    ///
    /// 返回:
    /// - 需要退出或启动运行时的动作；其余情况返回 `AppAction::None`。
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        if key.kind != KeyEventKind::Press {
            return AppAction::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return AppAction::Quit;
        }
        match self.screen {
            Screen::Backend => self.handle_backend_key(key),
            Screen::Dialogue => self.handle_dialogue_key(key),
            Screen::Question => self.handle_question_key(key),
            // 运行屏只允许 Ctrl+C（已在上面处理）
            Screen::Running => AppAction::None,
            Screen::Result => self.handle_result_key(key),
            Screen::Error => self.handle_error_key(key),
        }
    }

    /// 处理一段粘贴文本：按当前屏幕与焦点插入对应缓冲。
    pub(crate) fn handle_paste(&mut self, text: &str) {
        match self.screen {
            Screen::Backend => match self.backend_focus {
                BackendFocus::Mode => {}
                BackendFocus::BaseUrl => self.backend_base_url.insert_str(&single_line(text)),
                BackendFocus::Model => self.backend_model.insert_str(&single_line(text)),
                BackendFocus::Embedding => self.backend_embedding.insert_str(&single_line(text)),
            },
            Screen::Dialogue => match self.focus {
                Focus::Persona => self.persona.insert_str(&single_line(text)),
                Focus::Dialogue => self.dialogue.insert_str(text),
            },
            Screen::Question => self.question.insert_str(text),
            Screen::Running | Screen::Result | Screen::Error => {}
        }
    }

    // ===== 后端屏 =====

    /// 处理后端屏按键：方向键改来源，Tab / Enter 换字段，Ctrl+Enter 或 F5 进入对话。
    fn handle_backend_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => return AppAction::Quit,
            KeyCode::F(5) => return self.advance_to_dialogue(),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.advance_to_dialogue();
            }
            KeyCode::Tab => {
                // 继承来源没有独立配置字段，Tab 直接进入下一步
                if self.backend_focus == BackendFocus::Mode
                    && self.backend_mode == SandboxBackendMode::Inherit
                {
                    return self.advance_to_dialogue();
                }
                self.backend_focus = next_backend_focus(self.backend_focus);
                return AppAction::None;
            }
            KeyCode::Left | KeyCode::Right if self.backend_focus == BackendFocus::Mode => {
                self.switch_backend_mode(key.code == KeyCode::Right);
                return AppAction::None;
            }
            _ => {}
        }

        if self.backend_focus == BackendFocus::Mode {
            return AppAction::None;
        }
        if key.code == KeyCode::Enter {
            self.backend_focus = next_backend_focus(self.backend_focus);
            return AppAction::None;
        }
        match self.backend_focus {
            BackendFocus::Mode => {}
            BackendFocus::BaseUrl => edit_multiline(&mut self.backend_base_url, key),
            BackendFocus::Model => edit_multiline(&mut self.backend_model, key),
            BackendFocus::Embedding => edit_multiline(&mut self.backend_embedding, key),
        }
        AppAction::None
    }

    /// 切换来源；本地默认地址按来源自动填入或清空，已手填的内容保留。
    fn switch_backend_mode(&mut self, forward: bool) {
        let next = match (self.backend_mode, forward) {
            (SandboxBackendMode::Inherit, true) => SandboxBackendMode::Local,
            (SandboxBackendMode::Local, true) => SandboxBackendMode::Cloud,
            (SandboxBackendMode::Cloud, true) => SandboxBackendMode::Inherit,
            (SandboxBackendMode::Inherit, false) => SandboxBackendMode::Cloud,
            (SandboxBackendMode::Cloud, false) => SandboxBackendMode::Local,
            (SandboxBackendMode::Local, false) => SandboxBackendMode::Inherit,
        };
        // 本地默认地址是本地来源的占位值，切到云端时清掉以免误用
        if self.backend_mode == SandboxBackendMode::Local
            && next == SandboxBackendMode::Cloud
            && self.backend_base_url.text().trim() == LOCAL_DEFAULT_BASE_URL
        {
            self.backend_base_url = TextBuffer::new();
        }
        self.backend_mode = next;
        if next == SandboxBackendMode::Local && self.backend_base_url.text().trim().is_empty() {
            self.backend_base_url = text_buffer(LOCAL_DEFAULT_BASE_URL);
        }
        self.status = None;
    }

    /// 校验后端来源并进入对话屏；云端来源缺地址或模型标识时留在本屏。
    fn advance_to_dialogue(&mut self) -> AppAction {
        if self.backend_mode == SandboxBackendMode::Cloud {
            if self.backend_base_url.text().trim().is_empty() {
                self.status = Some("云端模型必须填写地址".to_string());
                return AppAction::None;
            }
            if self.backend_model.text().trim().is_empty() {
                self.status = Some("云端模型必须填写模型标识".to_string());
                return AppAction::None;
            }
        }
        self.status = None;
        self.backend_focus = BackendFocus::Mode;
        self.screen = Screen::Dialogue;
        AppAction::None
    }

    /// 本次运行的后端来源选择；独立配置的空字段按语义留空。
    pub(crate) fn backend_choice(&self) -> SandboxBackendChoice {
        let kind = match self.backend_mode {
            SandboxBackendMode::Inherit => return SandboxBackendChoice::Inherit,
            SandboxBackendMode::Local => SandboxModelKind::Local,
            SandboxBackendMode::Cloud => SandboxModelKind::Cloud,
        };
        let embedding = self.backend_embedding.text().trim().to_string();
        SandboxBackendChoice::Independent {
            kind,
            base_url: self.backend_base_url.text().trim().to_string(),
            model_id: self.backend_model.text().trim().to_string(),
            embedding_model_path: if embedding.is_empty() {
                None
            } else {
                Some(PathBuf::from(embedding))
            },
        }
    }

    // ===== 对话屏 =====

    fn handle_dialogue_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Persona => Focus::Dialogue,
                    Focus::Dialogue => Focus::Persona,
                };
                AppAction::None
            }
            KeyCode::Esc => {
                // 离开对话屏时清掉对话校验提示，返回后端屏不显示过期信息
                self.status = None;
                self.screen = Screen::Backend;
                AppAction::None
            }
            KeyCode::F(5) => self.advance_to_question(),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.advance_to_question()
            }
            KeyCode::Enter => {
                match self.focus {
                    Focus::Persona => self.focus = Focus::Dialogue,
                    Focus::Dialogue => self.dialogue.newline(),
                }
                AppAction::None
            }
            _ => {
                match self.focus {
                    // 单行字段同样只走编辑分发：Enter 在上面的分支被截住
                    Focus::Persona => edit_multiline(&mut self.persona, key),
                    Focus::Dialogue => edit_multiline(&mut self.dialogue, key),
                }
                AppAction::None
            }
        }
    }

    /// 校验对话块并进入提问屏。
    fn advance_to_question(&mut self) -> AppAction {
        let text = self.dialogue.text();
        match parse_dialogue(&text) {
            Ok(turns) if turns.is_empty() => {
                self.status = Some(EMPTY_DIALOGUE.to_string());
                AppAction::None
            }
            Ok(_) => {
                self.status = None;
                self.screen = Screen::Question;
                AppAction::None
            }
            Err(error) => {
                self.status = Some(error);
                AppAction::None
            }
        }
    }

    // ===== 提问屏 =====

    fn handle_question_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Enter => self.start_run(),
            KeyCode::Esc => {
                self.screen = Screen::Dialogue;
                AppAction::None
            }
            KeyCode::Tab => AppAction::None,
            _ => {
                edit_multiline(&mut self.question, key);
                AppAction::None
            }
        }
    }

    /// 组装运行输入；对话或问题不合法时留在本屏并给状态提示。
    fn start_run(&mut self) -> AppAction {
        let turns = match parse_dialogue(&self.dialogue.text()) {
            Ok(turns) if turns.is_empty() => {
                self.status = Some(EMPTY_DIALOGUE.to_string());
                return AppAction::None;
            }
            Ok(turns) => turns,
            Err(error) => {
                self.status = Some(error);
                return AppAction::None;
            }
        };

        let question = self.question.text().trim().to_string();
        if question.is_empty() {
            self.status = Some("问题为空：先写要提问的内容".to_string());
            return AppAction::None;
        }

        let persona = self.persona.text().trim().to_string();
        let persona_path = if persona.is_empty() {
            None
        } else {
            Some(PathBuf::from(persona))
        };

        self.status = None;
        AppAction::Run(SandboxInput {
            persona_path,
            turns,
            question,
        })
    }

    // ===== 结果屏 =====

    fn handle_result_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Tab | KeyCode::Right => self.switch_tab(true),
            KeyCode::Left => self.switch_tab(false),
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = self.max_scroll(),
            KeyCode::Char('1') => self.set_tab(ResultTab::Overview),
            KeyCode::Char('2') => self.set_tab(ResultTab::Trace),
            KeyCode::Char('3') => self.set_tab(ResultTab::Answer),
            KeyCode::Char('q') | KeyCode::Esc => return AppAction::Quit,
            _ => {}
        }
        AppAction::None
    }

    /// 按方向切页并回到页首。
    fn switch_tab(&mut self, forward: bool) {
        let next = match (self.result_tab, forward) {
            (ResultTab::Overview, true) => ResultTab::Trace,
            (ResultTab::Trace, true) => ResultTab::Answer,
            (ResultTab::Answer, true) => ResultTab::Overview,
            (ResultTab::Overview, false) => ResultTab::Answer,
            (ResultTab::Trace, false) => ResultTab::Overview,
            (ResultTab::Answer, false) => ResultTab::Trace,
        };
        self.set_tab(next);
    }

    fn set_tab(&mut self, tab: ResultTab) {
        self.result_tab = tab;
        self.scroll = 0;
    }

    /// 当前页最后一行可停留的滚动位置。
    fn max_scroll(&self) -> u16 {
        let lines = render::result_lines(self).len().saturating_sub(1);
        u16::try_from(lines).unwrap_or(u16::MAX)
    }

    // ===== 失败屏 =====

    fn handle_error_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => {
                self.screen = Screen::Backend;
                AppAction::None
            }
            KeyCode::Char('q') => AppAction::Quit,
            _ => AppAction::None,
        }
    }

    // ===== 结果落盘 =====

    /// 记录报告并切到结果屏概览页。
    pub(crate) fn set_result(&mut self, report: SandboxReport) {
        self.result = Some(report);
        self.screen = Screen::Result;
        self.result_tab = ResultTab::Overview;
        self.scroll = 0;
        self.running = false;
    }
}

impl Default for App {
    /// 缺省后端选择：继承当前用户配置。
    fn default() -> Self {
        Self::new(SandboxBackendChoice::Inherit)
    }
}

/// 对话为空时的统一提示。
const EMPTY_DIALOGUE: &str = "对话为空：先写至少一条 user:/assistant: 消息";

// =========================================================
// 编辑按键分发
// =========================================================

/// 把编辑类按键应用到多行缓冲；组合键不参与编辑（避免 Alt 组合误插入）。
fn edit_multiline(buffer: &mut TextBuffer, key: KeyEvent) {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return;
    }
    match key.code {
        KeyCode::Char(value) => buffer.insert_char(value),
        KeyCode::Backspace => buffer.backspace(),
        KeyCode::Delete => buffer.delete(),
        KeyCode::Left => buffer.move_left(),
        KeyCode::Right => buffer.move_right(),
        KeyCode::Up => buffer.move_up(),
        KeyCode::Down => buffer.move_down(),
        KeyCode::Home => buffer.home(),
        KeyCode::End => buffer.end(),
        _ => {}
    }
}

/// 后端屏焦点顺序：来源 → 地址 → 模型标识 → 向量目录 → 来源。
fn next_backend_focus(focus: BackendFocus) -> BackendFocus {
    match focus {
        BackendFocus::Mode => BackendFocus::BaseUrl,
        BackendFocus::BaseUrl => BackendFocus::Model,
        BackendFocus::Model => BackendFocus::Embedding,
        BackendFocus::Embedding => BackendFocus::Mode,
    }
}

/// 由初始文本构造缓冲（用于后端屏的预填与默认地址）。
fn text_buffer(value: &str) -> TextBuffer {
    let mut buffer = TextBuffer::new();
    buffer.insert_str(value);
    buffer
}

/// 去掉所有换行，用于单行字段的粘贴。
fn single_line(text: &str) -> String {
    text.chars()
        .filter(|ch| *ch != '\r' && *ch != '\n')
        .collect()
}
