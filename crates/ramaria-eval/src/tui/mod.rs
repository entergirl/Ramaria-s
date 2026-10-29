//! crates/ramaria-eval/src/tui/mod.rs - Ramaria 评测沙盒终端界面
//!
//! 设计特点:
//! - 终端守卫自包含：进入时逐步推进，任一步失败按已进入的部分回退再报错
//! - 恢复路径唯一：正常退出、错误返回与提前退出都经 Drop 还原原始模式与屏幕
//! - 事件单入口：键盘与粘贴来自终端事件流，阶段与结果来自运行任务的上报通道
//! - 非 TTY 不报错：直接回退行模式，管道与 CI 取报告的方式保持不变
//! - 界面只做状态机与渲染，沙盒执行复用同一运行入口

mod app;
mod render;
mod text;

#[cfg(test)]
mod tests;

use std::io::{IsTerminal, Stdout};
use std::path::PathBuf;

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
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::sandbox::{
    self, SandboxBackendChoice, SandboxInput, SandboxOptions, SandboxReport, StageReport,
};
use app::{App, AppAction, Screen};

// =========================================================
// 入口
// =========================================================

/// 进入评测沙盒界面。
///
/// 参数:
/// - `user_db`: 用户库路径；沙盒继承后端来源时读取其配置，`None` 表示无宿主库；
/// - `default_backend`: 界面初始后端来源（由入口参数决定，界面内可改）。
///
/// 返回:
/// - 标准输入与标准输出都是终端时进入 TUI，退出后返回成功；
/// - 任一不是终端时回退行模式，输入未指定来源时沿用 `default_backend`；
/// - 终端进入、绘制或事件读取失败返回 Io 错误（终端已先恢复）。
pub async fn run(
    user_db: Option<PathBuf>,
    default_backend: SandboxBackendChoice,
) -> RamariaResult<()> {
    if !is_terminal() {
        return sandbox::run_line_mode(user_db, default_backend).await;
    }

    let mut session = TerminalSession::enter()?;
    let mut app = App::new(default_backend);
    // 返回值由 session 的 Drop 恢复终端之后再上浮
    event_loop(session.terminal(), &mut app, user_db).await
}

/// 标准输入与标准输出是否都是终端。
fn is_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
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

/// 运行任务回报给界面的事件。
enum UiEvent {
    /// 单个阶段完成
    Stage(StageReport),
    /// 整次运行结束；失败携带可直接展示的错误文本。
    ///
    /// 报告装箱：整次运行只投递一次，避免让事件枚举按报告体积对齐。
    Done(Result<Box<SandboxReport>, String>),
}

/// 绘制 → 等事件 → 更新的主循环。
async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    user_db: Option<PathBuf>,
) -> RamariaResult<()> {
    let mut events = Box::pin(EventStream::new());
    let (sender, mut receiver) = unbounded_channel::<UiEvent>();

    loop {
        terminal
            .draw(|frame| render::draw(frame, app))
            .map_err(|error| RamariaError::io("绘制终端界面失败", Some(error)))?;

        tokio::select! {
            event = events.next() => {
                let Some(event) = event else {
                    return Ok(());
                };
                let event = event
                    .map_err(|error| RamariaError::io("读取终端事件失败", Some(error)))?;
                let action = match event {
                    Event::Key(key) => app.handle_key(key),
                    Event::Paste(text) => {
                        app.handle_paste(&text);
                        AppAction::None
                    }
                    _ => AppAction::None,
                };
                match action {
                    AppAction::None => {}
                    AppAction::Quit => return Ok(()),
                    AppAction::Run(input) => start_run(app, input, &sender, user_db.clone()),
                }
            }
            Some(event) = receiver.recv() => apply_ui_event(app, event),
        }
    }
}

/// 切到运行屏并在后台执行沙盒；阶段与结果回投同一通道。
fn start_run(
    app: &mut App,
    input: SandboxInput,
    sender: &UnboundedSender<UiEvent>,
    user_db: Option<PathBuf>,
) {
    // 后端来源在切屏前取定：运行期间界面不再改动这份选择
    let backend = app.backend_choice();
    app.stages.clear();
    app.status = None;
    app.error = None;
    app.screen = Screen::Running;
    app.running = true;

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

/// 把运行事件写回界面状态。
fn apply_ui_event(app: &mut App, event: UiEvent) {
    match event {
        UiEvent::Stage(stage) => app.stages.push(stage),
        UiEvent::Done(Ok(report)) => app.set_result(*report),
        UiEvent::Done(Err(message)) => {
            app.error = Some(message);
            app.screen = Screen::Error;
            app.running = false;
        }
    }
}
