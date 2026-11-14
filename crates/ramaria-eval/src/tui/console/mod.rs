//! crates/ramaria-eval/src/tui/console/mod.rs - Ramaria 评测台终端入口
//!
//! 设计特点:
//! - 终端守卫自包含：进入时逐步推进，任一步失败按已进入的部分回退再报错
//! - 恢复路径唯一：正常退出、错误返回与提前退出都经 Drop 还原原始模式与屏幕
//! - 事件分两路：界面事件（沙盒 / 运行完成）走 UI 通道，逐题进度走运行通道
//! - 运行中屏 500ms 定时刷新：长 LLM 调用期间已用时间与剩余估计持续更新
//! - 启动动作集中在此：状态机只返回动作，任务派生与通道接线由入口完成

mod matrix;
mod render;
mod review;
mod running;
mod state;

#[cfg(test)]
mod tests;

use std::io::Stdout;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::cursor;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ramaria_core::error::{RamariaError, RamariaResult};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::MissedTickBehavior;

use crate::corpus::Sample;
use crate::run::{RunControl, RunEvent, RunReport};
use crate::sandbox::{
    self, SandboxBackendChoice, SandboxInput, SandboxOptions, SandboxReport, StageReport,
};
use review::{CorpusRequest, ReviewIndex};
use state::{Console, ConsoleAction};

/// 运行中屏的刷新间隔。
const TICK_INTERVAL: Duration = Duration::from_millis(500);

// =========================================================
// 入口
// =========================================================

/// 进入评测台界面。
///
/// 参数:
/// - `user_db`: 用户库路径；沙盒继承后端来源时读取其配置，`None` 表示无宿主库；
/// - `default_backend`: 沙盒初始后端来源（由入口参数决定，界面内可改）。
///
/// 返回:
/// - 成功进入并退出时返回成功；
/// - 终端进入、绘制或事件读取失败返回 Io 错误（终端已先恢复）。
pub(crate) async fn run(
    user_db: Option<PathBuf>,
    default_backend: SandboxBackendChoice,
) -> RamariaResult<()> {
    let mut session = TerminalSession::enter()?;
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut console = Console::new(root, default_backend);
    // 返回值由 session 的 Drop 恢复终端之后再上浮
    event_loop(session.terminal(), &mut console, user_db).await
}

// =========================================================
// 终端守卫
// =========================================================

/// 终端会话守卫：持有备用屏幕上的终端，离开作用域即恢复宿主状态。
struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    /// 进入原始模式与备用屏幕并清屏。
    ///
    /// 返回:
    /// - 成功时返回可用终端；
    /// - 任一步失败时先把已进入的状态恢复，再返回 Io 错误。
    fn enter() -> RamariaResult<Self> {
        enable_raw_mode().map_err(|error| RamariaError::io("切换终端原始模式失败", Some(error)))?;

        let mut stdout = std::io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(RamariaError::io("切换终端备用屏幕失败", Some(error)));
        }
        // 括号粘贴在部分宿主（conhost）不支持，失败时降级为逐字符输入
        let _ = execute!(stdout, EnableBracketedPaste);

        let mut terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                restore_terminal();
                return Err(RamariaError::io("初始化终端失败", Some(error)));
            }
        };
        if let Err(error) = terminal.clear() {
            drop(terminal);
            restore_terminal();
            return Err(RamariaError::io("清空终端失败", Some(error)));
        }

        Ok(Self { terminal })
    }

    fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TerminalSession {
    /// 恢复终端：关闭原始模式、括号粘贴与备用屏幕，并显示光标。
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            cursor::Show
        );
    }
}

/// 恢复终端到进入前的样子；用于尚未构造出守卫的失败路径。
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
}

// =========================================================
// 事件循环
// =========================================================

/// 后台任务回报给界面的完成事件。
enum UiEvent {
    /// 沙盒单个阶段完成
    Stage(StageReport),
    /// 沙盒整次运行结束；失败携带可直接展示的错误文本
    Done(Result<Box<SandboxReport>, String>),
    /// 矩阵运行结束；失败携带可直接展示的错误文本
    RunDone(Result<Box<RunReport>, String>),
    /// 审查索引构建结束；`root` 用于丢弃换题后到达的结果
    ReviewLoaded {
        /// 发起时的运行根
        root: PathBuf,
        /// 索引或可直接展示的错误文本
        result: Result<Box<ReviewIndex>, String>,
    },
    /// 原对话语料加载结束；`key` 为语料名 / 版本 / 语言
    CorpusLoaded {
        /// 缓存键
        key: String,
        /// 全部样本或可直接展示的错误文本
        result: Result<Vec<Sample>, String>,
    },
}

/// 本轮 `select!` 的唤醒来源。
enum Wake {
    /// 终端按键或粘贴
    Key(ConsoleAction),
    /// 沙盒阶段或完成
    Ui(UiEvent),
    /// 运行进度；通道关闭时为 `None`
    Run(Option<RunEvent>),
    /// 定时刷新
    Tick,
}

/// 绘制 → 等事件 → 更新的主循环。
async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    console: &mut Console,
    user_db: Option<PathBuf>,
) -> RamariaResult<()> {
    let mut events = Box::pin(EventStream::new());
    let (sender, mut receiver) = unbounded_channel::<UiEvent>();
    let mut run_events: Option<UnboundedReceiver<RunEvent>> = None;
    let mut ticker = tokio::time::interval(TICK_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        terminal
            .draw(|frame| render::draw(frame, console))
            .map_err(|error| RamariaError::io("绘制终端界面失败", Some(error)))?;

        let mut pending_run: Option<(PathBuf, PathBuf)> = None;
        let wake = tokio::select! {
            event = events.next() => {
                let Some(event) = event else {
                    return Ok(());
                };
                let event = event
                    .map_err(|error| RamariaError::io("读取终端事件失败", Some(error)))?;
                let action = match event {
                    Event::Key(key) => console.handle_key(key),
                    Event::Paste(text) => {
                        console.handle_paste(&text);
                        ConsoleAction::None
                    }
                    _ => ConsoleAction::None,
                };
                Wake::Key(action)
            }
            Some(event) = receiver.recv() => Wake::Ui(event),
            event = next_run_event(run_events.as_mut()) => Wake::Run(event),
            _ = ticker.tick(), if console.needs_tick() => Wake::Tick,
        };

        match wake {
            Wake::Key(ConsoleAction::None) => {}
            Wake::Key(ConsoleAction::Exit) => return Ok(()),
            Wake::Key(ConsoleAction::StartSandbox(input)) => {
                start_sandbox(console, input, &sender, user_db.clone());
            }
            Wake::Key(ConsoleAction::StartRun { matrix, output }) => {
                pending_run = Some((matrix, output));
            }
            Wake::Key(ConsoleAction::LoadReview { root }) => {
                start_review_load(&sender, root);
            }
            Wake::Key(ConsoleAction::LoadCorpus(request)) => {
                start_corpus_load(&sender, request);
            }
            Wake::Ui(event) => console.apply_ui_event(event),
            Wake::Run(Some(event)) => console.apply_run_event(event),
            Wake::Run(None) => run_events = None,
            Wake::Tick => {}
        }

        // 通道接线放在 select 之后：运行中屏的接收端借用已结束
        if let Some((matrix, output)) = pending_run {
            run_events = Some(start_run(console, matrix, output, &sender));
        }
    }
}

/// 等待一条运行事件；未接入运行通道时永不就绪。
async fn next_run_event(receiver: Option<&mut UnboundedReceiver<RunEvent>>) -> Option<RunEvent> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// 切到沙盒运行屏并在后台执行；阶段与结果回投同一通道。
fn start_sandbox(
    console: &mut Console,
    input: SandboxInput,
    sender: &UnboundedSender<UiEvent>,
    user_db: Option<PathBuf>,
) {
    // 后端来源在切屏前取定：运行期间界面不再改动这份选择
    let backend = console.sandbox.backend_choice();
    console.sandbox.stages.clear();
    console.sandbox.status = None;
    console.sandbox.error = None;
    console.sandbox.screen = crate::tui::app::Screen::Running;
    console.sandbox.running = true;

    let (stage_sender, mut stage_receiver) = unbounded_channel::<StageReport>();
    let event_sender = sender.clone();
    let done_sender = sender.clone();
    tokio::spawn(async move {
        // 沙盒只认阶段通道，转发一次把阶段并入界面事件流
        let forward = tokio::spawn(async move {
            while let Some(stage) = stage_receiver.recv().await {
                if event_sender.send(UiEvent::Stage(stage)).is_err() {
                    break;
                }
            }
        });

        let options = SandboxOptions {
            backend,
            user_db,
            ..SandboxOptions::default()
        };
        let outcome = sandbox::run(&options, input, Some(stage_sender)).await;
        // 先等阶段转发收尾，保证阶段事件都排在结束事件之前
        let _ = forward.await;

        let event = match outcome {
            Ok(report) => UiEvent::Done(Ok(Box::new(report))),
            Err(error) => UiEvent::Done(Err(error.to_string())),
        };
        let _ = done_sender.send(event);
    });
}

/// 切到运行屏并在后台执行矩阵；返回逐题进度通道。
fn start_run(
    console: &mut Console,
    matrix: PathBuf,
    output: PathBuf,
    sender: &UnboundedSender<UiEvent>,
) -> UnboundedReceiver<RunEvent> {
    let (event_sender, event_receiver) = unbounded_channel::<RunEvent>();
    let control = RunControl::new(event_sender);
    console.begin_run(matrix.clone(), output.clone(), control.clone());

    let sender = sender.clone();
    tokio::spawn(async move {
        let outcome = crate::run::run_with_control(None, Some(matrix), Some(output), control).await;
        let event = match outcome {
            Ok(report) => UiEvent::RunDone(Ok(Box::new(report))),
            Err(error) => UiEvent::RunDone(Err(error.to_string())),
        };
        let _ = sender.send(event);
    });
    event_receiver
}

/// 后台建立审查索引并回投界面。
fn start_review_load(sender: &UnboundedSender<UiEvent>, root: PathBuf) {
    let sender = sender.clone();
    drop(tokio::task::spawn_blocking(move || {
        let result = match ReviewIndex::build(&root) {
            Ok(index) => Ok(Box::new(index)),
            Err(error) => Err(error.to_string()),
        };
        let _ = sender.send(UiEvent::ReviewLoaded { root, result });
    }));
}

/// 后台读取原始语料并回投界面。
fn start_corpus_load(sender: &UnboundedSender<UiEvent>, request: CorpusRequest) {
    let sender = sender.clone();
    let key = request.key();
    drop(tokio::task::spawn_blocking(move || {
        let result = match request.path().and_then(load_corpus_file) {
            Ok(samples) => Ok(samples),
            Err(error) => Err(error.to_string()),
        };
        let _ = sender.send(UiEvent::CorpusLoaded { key, result });
    }));
}

/// 读取语料文件；缺失时给出期望路径。
fn load_corpus_file(path: std::path::PathBuf) -> RamariaResult<Vec<Sample>> {
    if !path.is_file() {
        return Err(RamariaError::validation(format!(
            "语料文件不存在: {}（请在 .eval/corpus 下放置本地副本）",
            path.display()
        )));
    }
    crate::corpus::load(&path)
}
