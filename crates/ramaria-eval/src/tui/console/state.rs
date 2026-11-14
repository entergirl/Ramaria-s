//! crates/ramaria-eval/src/tui/console/state.rs - Ramaria 评测台状态机
//!
//! 设计特点:
//! - 屏幕集中一处：首页 / 沙盒 / 矩阵 / 运行中 / 错误，渲染只读状态
//! - 按键单向转动作：状态机只改自身字段并返回动作，不直接启动任务
//! - 沙盒复用原状态机：退出语义映射为返回首页，内部流程与键位不变
//! - 运行中屏只应用事件：取消只置位开关，等待运行器写完在飞题
//! - 结束态保留本次矩阵与输出根：续跑沿用同一路径，不重新扫描

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::run::{RunControl, RunEvent, RunReport};
use crate::sandbox::{SandboxBackendChoice, SandboxInput};
use crate::tui::app::{App, AppAction, Screen as SandboxScreen};

use super::UiEvent;
use super::matrix::MatrixState;
use super::review::{CorpusRequest, ReviewFocus, ReviewIndex, ReviewState, RunsState, SourceOpen};
use super::running::RunningState;

// =========================================================
// 屏幕与动作
// =========================================================

/// 评测台当前屏幕。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    /// 首页（沙盒 / 矩阵运行）
    Home,
    /// 沙盒（内部屏幕由沙盒状态机决定）
    Sandbox,
    /// 矩阵选择与预览
    Matrix,
    /// 矩阵运行中 / 已结束
    Running,
    /// 审查运行列表
    Runs,
    /// 审查：题单 + 详情
    Review,
    /// 审查：原对话片段
    Source,
    /// 运行入口失败
    Error,
}

/// 首页条目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeEntry {
    /// 沙盒
    Sandbox,
    /// 矩阵运行
    Matrix,
    /// 审查
    Review,
}

impl HomeEntry {
    /// 候选顺序里的下一项（三项循环）。
    fn next(self) -> Self {
        match self {
            Self::Sandbox => Self::Matrix,
            Self::Matrix => Self::Review,
            Self::Review => Self::Sandbox,
        }
    }

    /// 候选顺序里的上一项（三项循环）。
    fn previous(self) -> Self {
        match self {
            Self::Sandbox => Self::Review,
            Self::Review => Self::Matrix,
            Self::Matrix => Self::Sandbox,
        }
    }
}

/// 状态机产出的动作，由事件循环执行。
pub(crate) enum ConsoleAction {
    /// 仅更新状态
    None,
    /// 退出进程
    Exit,
    /// 切到沙盒运行屏并启动
    StartSandbox(SandboxInput),
    /// 切到运行屏并启动矩阵
    StartRun {
        /// 矩阵原文路径
        matrix: PathBuf,
        /// 输出根（续跑沿用同一路径）
        output: PathBuf,
    },
    /// 切到审查屏并在后台建立索引
    LoadReview {
        /// 运行根
        root: PathBuf,
    },
    /// 后台加载原对话语料
    LoadCorpus(CorpusRequest),
}

// =========================================================
// 界面状态
// =========================================================

/// 本次运行的固定路径（续跑复用）。
#[derive(Debug, Clone)]
pub(crate) struct ActiveRun {
    pub(crate) matrix: PathBuf,
    pub(crate) output: PathBuf,
}

/// 评测台状态。
///
/// 字段约定:
/// - `screen` / `home`: 当前屏幕与首页选中项；
/// - `root`: 评测根（当前工作目录），矩阵扫描与输出根都相对它解析；
/// - `sandbox`: 沙盒状态机（内部屏幕独立）；
/// - `matrix`: 矩阵扫描与预览；
/// - `running`: 运行中屏状态；
/// - `error`: 运行入口失败全文；
/// - `control`: 本次运行的取消开关；
/// - `active`: 本次矩阵与输出根；
/// - `runs` / `review`: 审查运行列表与审查屏状态。
pub(crate) struct Console {
    pub(crate) screen: Screen,
    pub(crate) root: PathBuf,
    pub(crate) home: HomeEntry,
    pub(crate) sandbox: App,
    pub(crate) matrix: MatrixState,
    pub(crate) running: RunningState,
    pub(crate) error: Option<String>,
    pub(crate) control: RunControl,
    pub(crate) active: Option<ActiveRun>,
    pub(crate) runs: RunsState,
    pub(crate) review: ReviewState,
}

impl Console {
    /// 初始状态：停在首页，默认选中沙盒；沙盒后端按入口注入预填。
    pub(crate) fn new(root: PathBuf, default_backend: SandboxBackendChoice) -> Self {
        Self {
            screen: Screen::Home,
            root,
            home: HomeEntry::Sandbox,
            sandbox: App::new(default_backend),
            matrix: MatrixState::default(),
            running: RunningState::default(),
            error: None,
            control: RunControl::default(),
            active: None,
            runs: RunsState::default(),
            review: ReviewState::default(),
        }
    }

    // ===== 按键入口 =====

    /// 处理一个按键事件。
    ///
    /// 参数:
    /// - `key`: 终端按键；只响应按下事件（释放与重复事件直接忽略）。
    ///
    /// 返回:
    /// - 退出、启动沙盒或启动矩阵时的动作；其余情况返回 [`ConsoleAction::None`]。
    ///
    /// 说明:
    /// - 沙盒屏把 Ctrl+C 交给沙盒状态机，其退出动作映射为返回首页；
    /// - 其余屏幕的 Ctrl+C 直接退出进程。
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ConsoleAction {
        if key.kind != KeyEventKind::Press {
            return ConsoleAction::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return match self.screen {
                Screen::Sandbox => self.handle_sandbox_key(key),
                _ => ConsoleAction::Exit,
            };
        }
        match self.screen {
            Screen::Home => self.handle_home_key(key),
            Screen::Sandbox => self.handle_sandbox_key(key),
            Screen::Matrix => self.handle_matrix_key(key),
            Screen::Running => self.handle_running_key(key),
            Screen::Runs => self.handle_runs_key(key),
            Screen::Review => self.handle_review_key(key),
            Screen::Source => self.handle_source_key(key),
            Screen::Error => self.handle_error_key(key),
        }
    }

    /// 处理粘贴：只在沙盒屏转给沙盒状态机。
    pub(crate) fn handle_paste(&mut self, text: &str) {
        if self.screen == Screen::Sandbox {
            self.sandbox.handle_paste(text);
        }
    }

    // ===== 首页 =====

    fn handle_home_key(&mut self, key: KeyEvent) -> ConsoleAction {
        match key.code {
            KeyCode::Up => {
                self.home = self.home.previous();
                ConsoleAction::None
            }
            KeyCode::Down => {
                self.home = self.home.next();
                ConsoleAction::None
            }
            KeyCode::Enter => match self.home {
                HomeEntry::Sandbox => {
                    self.screen = Screen::Sandbox;
                    ConsoleAction::None
                }
                HomeEntry::Matrix => {
                    self.open_matrix();
                    ConsoleAction::None
                }
                HomeEntry::Review => {
                    self.runs.refresh(&self.root);
                    self.screen = Screen::Runs;
                    ConsoleAction::None
                }
            },
            KeyCode::Char('q') | KeyCode::Esc => ConsoleAction::Exit,
            _ => ConsoleAction::None,
        }
    }

    /// 进入矩阵屏：每次进入都重新扫描，避免展示过期文件列表。
    fn open_matrix(&mut self) {
        self.matrix.refresh(&self.root);
        self.screen = Screen::Matrix;
    }

    // ===== 沙盒 =====

    fn handle_sandbox_key(&mut self, key: KeyEvent) -> ConsoleAction {
        match self.sandbox.handle_key(key) {
            AppAction::None => ConsoleAction::None,
            AppAction::Quit => {
                self.screen = Screen::Home;
                ConsoleAction::None
            }
            AppAction::Run(input) => ConsoleAction::StartSandbox(input),
        }
    }

    // ===== 矩阵 =====

    fn handle_matrix_key(&mut self, key: KeyEvent) -> ConsoleAction {
        match key.code {
            KeyCode::Up => {
                self.matrix.select(-1, &self.root);
                ConsoleAction::None
            }
            KeyCode::Down => {
                self.matrix.select(1, &self.root);
                ConsoleAction::None
            }
            KeyCode::Char('r' | 'R') => {
                self.matrix.refresh(&self.root);
                ConsoleAction::None
            }
            KeyCode::Enter => match self.matrix.start_request(&self.root) {
                Some((matrix, output)) => ConsoleAction::StartRun { matrix, output },
                None => ConsoleAction::None,
            },
            KeyCode::Esc => {
                self.screen = Screen::Home;
                ConsoleAction::None
            }
            _ => ConsoleAction::None,
        }
    }

    // ===== 运行中 =====

    fn handle_running_key(&mut self, key: KeyEvent) -> ConsoleAction {
        if !self.running.is_finished() {
            if key.code == KeyCode::Esc && !self.running.is_cancel_requested() {
                self.control.cancel();
                self.running.request_cancel();
            }
            return ConsoleAction::None;
        }
        match key.code {
            KeyCode::Char('r' | 'R') => match &self.active {
                Some(active) => ConsoleAction::StartRun {
                    matrix: active.matrix.clone(),
                    output: active.output.clone(),
                },
                None => ConsoleAction::None,
            },
            KeyCode::Enter => match self.running.root().map(Path::to_path_buf) {
                Some(root) => {
                    self.review.begin(root.clone(), self.root.clone());
                    self.screen = Screen::Review;
                    ConsoleAction::LoadReview { root }
                }
                None => {
                    self.screen = Screen::Home;
                    ConsoleAction::None
                }
            },
            KeyCode::Esc => {
                self.screen = Screen::Home;
                ConsoleAction::None
            }
            _ => ConsoleAction::None,
        }
    }

    // ===== 审查列表 =====

    fn handle_runs_key(&mut self, key: KeyEvent) -> ConsoleAction {
        match key.code {
            KeyCode::Up => {
                self.runs.select(-1);
                ConsoleAction::None
            }
            KeyCode::Down => {
                self.runs.select(1);
                ConsoleAction::None
            }
            KeyCode::Char('r' | 'R') => {
                self.runs.refresh(&self.root);
                ConsoleAction::None
            }
            KeyCode::Enter => match self.runs.selected().map(|run| run.root.clone()) {
                Some(root) => {
                    self.review.begin(root.clone(), self.root.clone());
                    self.screen = Screen::Review;
                    ConsoleAction::LoadReview { root }
                }
                None => ConsoleAction::None,
            },
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = Screen::Home;
                ConsoleAction::None
            }
            _ => ConsoleAction::None,
        }
    }

    // ===== 审查 =====

    fn handle_review_key(&mut self, key: KeyEvent) -> ConsoleAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = Screen::Runs;
                ConsoleAction::None
            }
            KeyCode::Tab => {
                self.review.toggle_focus();
                ConsoleAction::None
            }
            KeyCode::Up => {
                match self.review.focus {
                    ReviewFocus::List => self.review.select_question(-1),
                    ReviewFocus::Detail => self.review.scroll_detail(-1),
                }
                ConsoleAction::None
            }
            KeyCode::Down => {
                match self.review.focus {
                    ReviewFocus::List => self.review.select_question(1),
                    ReviewFocus::Detail => self.review.scroll_detail(1),
                }
                ConsoleAction::None
            }
            KeyCode::Left => {
                self.review.select_condition(-1);
                ConsoleAction::None
            }
            KeyCode::Right => {
                self.review.select_condition(1);
                ConsoleAction::None
            }
            KeyCode::PageUp => {
                match self.review.focus {
                    ReviewFocus::List => self.review.select_question(-10),
                    ReviewFocus::Detail => self.review.scroll_detail(-10),
                }
                ConsoleAction::None
            }
            KeyCode::PageDown => {
                match self.review.focus {
                    ReviewFocus::List => self.review.select_question(10),
                    ReviewFocus::Detail => self.review.scroll_detail(10),
                }
                ConsoleAction::None
            }
            KeyCode::Home => {
                match self.review.focus {
                    ReviewFocus::List => self.review.first_question(),
                    ReviewFocus::Detail => self.review.detail_scroll = 0,
                }
                ConsoleAction::None
            }
            KeyCode::End => {
                match self.review.focus {
                    ReviewFocus::List => self.review.last_question(),
                    ReviewFocus::Detail => self.review.scroll_detail_to_end(),
                }
                ConsoleAction::None
            }
            KeyCode::Char('g') => match self.review.open_source() {
                SourceOpen::Ready => {
                    self.screen = Screen::Source;
                    ConsoleAction::None
                }
                SourceOpen::Load(request) => {
                    self.screen = Screen::Source;
                    ConsoleAction::LoadCorpus(request)
                }
                SourceOpen::None => ConsoleAction::None,
            },
            KeyCode::Char('r' | 'R') => {
                let root = self.review.root.clone();
                if root.as_os_str().is_empty() {
                    return ConsoleAction::None;
                }
                self.review.begin(root.clone(), self.root.clone());
                ConsoleAction::LoadReview { root }
            }
            _ => ConsoleAction::None,
        }
    }

    // ===== 原对话 =====

    fn handle_source_key(&mut self, key: KeyEvent) -> ConsoleAction {
        let Some(view) = self.review.source.as_mut() else {
            self.screen = Screen::Review;
            return ConsoleAction::None;
        };
        let max = view.max_scroll.get();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = Screen::Review;
            }
            KeyCode::Up => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Down => view.scroll = view.scroll.saturating_add(1).min(max),
            KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(10),
            KeyCode::PageDown => view.scroll = view.scroll.saturating_add(10).min(max),
            KeyCode::Home => view.scroll = 0,
            KeyCode::End => view.scroll = max,
            _ => {}
        }
        ConsoleAction::None
    }

    // ===== 错误 =====

    fn handle_error_key(&mut self, key: KeyEvent) -> ConsoleAction {
        match key.code {
            KeyCode::Esc => {
                self.screen = Screen::Home;
                ConsoleAction::None
            }
            KeyCode::Char('q') => ConsoleAction::Exit,
            _ => ConsoleAction::None,
        }
    }

    // ===== 运行接入 =====

    /// 切到运行屏并登记本次取消开关与路径。
    pub(crate) fn begin_run(&mut self, matrix: PathBuf, output: PathBuf, control: RunControl) {
        self.running.prepare(&output);
        self.control = control;
        self.active = Some(ActiveRun { matrix, output });
        self.error = None;
        self.screen = Screen::Running;
    }

    /// 应用运行事件。
    pub(crate) fn apply_run_event(&mut self, event: RunEvent) {
        match event {
            RunEvent::Started(started) => self.running.started(started),
            RunEvent::ConditionStarted {
                index,
                total,
                id,
                items,
            } => self.running.condition_started(index, total, id, items),
            RunEvent::ItemDone(item) => self.running.item_done(item),
            RunEvent::ConditionDone {
                id,
                completed,
                failed,
            } => self.running.condition_done(&id, completed, failed),
            RunEvent::Warning { condition, message } => self.running.warning(condition, message),
        }
    }

    /// 应用沙盒与运行完成事件。
    pub(crate) fn apply_ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Stage(stage) => self.sandbox.stages.push(stage),
            UiEvent::Done(Ok(report)) => self.sandbox.set_result(*report),
            UiEvent::Done(Err(message)) => {
                self.sandbox.error = Some(message);
                self.sandbox.screen = SandboxScreen::Error;
                self.sandbox.running = false;
            }
            UiEvent::RunDone(Ok(report)) => self.finish_run(*report),
            UiEvent::RunDone(Err(message)) => {
                self.running.fail(&message);
                self.error = Some(message);
                self.screen = Screen::Error;
            }
            UiEvent::ReviewLoaded { root, result } => self.apply_review_loaded(root, result),
            UiEvent::CorpusLoaded { key, result } => {
                self.review.apply_corpus(key, result);
            }
        }
    }

    /// 应用索引构建结果：只接受当前运行且仍在加载的回投。
    fn apply_review_loaded(&mut self, root: PathBuf, result: Result<Box<ReviewIndex>, String>) {
        if !self.review.loading || self.review.root != root {
            return;
        }
        match result {
            Ok(index) => self.review.apply_loaded(*index),
            Err(message) => self.review.fail(message),
        }
    }

    /// 记录运行报告并停在运行屏的结束态。
    fn finish_run(&mut self, report: RunReport) {
        self.running.finish(&report);
    }

    /// 运行屏是否需要定时刷新（只在运行中且未结束时）。
    pub(crate) fn needs_tick(&self) -> bool {
        self.screen == Screen::Running && !self.running.is_finished()
    }
}
