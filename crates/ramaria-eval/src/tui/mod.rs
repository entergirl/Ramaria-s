//! crates/ramaria-eval/src/tui/mod.rs - Ramaria 评测台终端入口
//!
//! 设计特点:
//! - 本文件只做入口分流：终端检测、非 TTY 回退与界面模块挂载
//! - 非 TTY 不报错：直接回退沙盒行模式，管道与 CI 取报告的方式保持不变
//! - TTY 进入评测台首页，默认选中沙盒；沙盒流程与键位保持原样
//! - 终端守卫、事件循环与各屏状态在 console 子模块，沙盒状态机与渲染留在本层
//! - 界面只做状态机与渲染，沙盒与矩阵运行复用同一执行入口

mod app;
mod console;
mod render;
mod text;

#[cfg(test)]
mod tests;

use std::io::IsTerminal;
use std::path::PathBuf;

use ramaria_core::error::RamariaResult;

use crate::sandbox::{self, SandboxBackendChoice};

/// 进入评测台界面。
///
/// 参数:
/// - `user_db`: 用户库路径；沙盒继承后端来源时读取其配置，`None` 表示无宿主库；
/// - `default_backend`: 沙盒初始后端来源（由入口参数决定，界面内可改）。
///
/// 返回:
/// - 标准输入与标准输出都是终端时进入评测台，退出后返回成功；
/// - 任一不是终端时回退沙盒行模式，输入未指定来源时沿用 `default_backend`；
/// - 终端进入、绘制或事件读取失败返回 Io 错误（终端已先恢复）。
pub async fn run(
    user_db: Option<PathBuf>,
    default_backend: SandboxBackendChoice,
) -> RamariaResult<()> {
    if !is_terminal() {
        return sandbox::run_line_mode(user_db, default_backend).await;
    }
    console::run(user_db, default_backend).await
}

/// 标准输入与标准输出是否都是终端。
fn is_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}
