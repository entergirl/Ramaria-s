//! crates/ramaria-eval/src/sandbox/mod.rs - Ramaria 评测沙盒入口
//!
//! 设计特点:
//! - 沙盒自包含：每次运行清空并重建根目录，库、配置快照与人格文件都落在根内
//! - 输入与执行分离：行文本先解析为结构化输入，运行器只消费结构化输入
//! - 后端来源可选：继承用户配置或独立本地 / 云端模型，解析与降级在 backend 模块
//! - 分阶段上报：每个阶段推一条耗时与摘要，界面只读上报，不感知服务层类型
//! - 失败分级：装配 / 人格 / 写入 / 召回失败中止，封存 / 级联 / 索引 / 回答失败降级为告警
//! - 进度写标准错误、最终报告写标准输出，管道取标准输出即得报告

use std::path::PathBuf;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::ChatTurn;
use tokio::sync::mpsc::UnboundedSender;

pub mod backend;
pub mod parse;
pub mod report;

mod line;
mod pipeline;
mod root;

#[cfg(test)]
mod tests;

pub use backend::{
    BackendReport, SandboxBackendChoice, SandboxBackendMode, SandboxModelKind, resolve_backend,
};
pub use parse::{ParsedLineInput, parse_dialogue, parse_line_input};
pub use report::{SandboxReport, StageKind, StageReport, StageStatus, render_text, trace_lines};
pub use root::prepare;

// =========================================================
// 运行选项与输入
// =========================================================

/// 沙盒运行选项。
///
/// 字段约定:
/// - `root`: 沙盒根目录；运行前清空并重建，库与配置快照只写在该目录下；
/// - `backend`: 后端来源选择（继承用户配置或独立配置）；
/// - `user_db`: 用户库路径；仅继承来源使用，`None` 表示无宿主库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxOptions {
    pub root: PathBuf,
    pub backend: SandboxBackendChoice,
    pub user_db: Option<PathBuf>,
}

impl SandboxOptions {
    /// 默认沙盒根（当前工作目录下的 `.eval/sandbox`）。
    ///
    /// 返回:
    /// - 成功时返回默认根目录；
    /// - 读取当前工作目录失败返回 `Io` 错误。
    pub fn default_root() -> RamariaResult<PathBuf> {
        let cwd = std::env::current_dir()
            .map_err(|error| RamariaError::io("读取当前工作目录失败", Some(error)))?;
        Ok(cwd.join(".eval").join("sandbox"))
    }
}

impl Default for SandboxOptions {
    /// 默认沙盒根 + 继承后端；当前工作目录不可读时退回相对路径，不在此处报错。
    fn default() -> Self {
        let root = Self::default_root().unwrap_or_else(|_| PathBuf::from(".eval").join("sandbox"));
        Self {
            root,
            backend: SandboxBackendChoice::Inherit,
            user_db: None,
        }
    }
}

/// 一次沙盒运行的输入。
///
/// 字段约定:
/// - `persona_path`: 可选人格预设文件（TOML / JSON / PNG）；`None` 用内置沙盒人格；
/// - `turns`: 写入沙盒库的对话轮次（user / assistant 片段）；
/// - `question`: 召回与回答使用的问题文本。
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxInput {
    pub persona_path: Option<PathBuf>,
    pub turns: Vec<ChatTurn>,
    pub question: String,
}

// =========================================================
// 运行入口
// =========================================================

/// 执行一次沙盒运行。
///
/// 参数:
/// - `options`: 沙盒运行选项（根目录、后端来源、用户库路径）；
/// - `input`: 人格预设、对话轮次与问题；
/// - `progress`: 阶段上报通道；`None` 表示不订阅。
///
/// 返回:
/// - 成功时返回完整报告（含召回轨迹与答案）；
/// - 目录准备 / 引擎装配 / 后端解析 / 人格创建 / 写入 / 召回失败返回对应错误。
pub async fn run(
    options: &SandboxOptions,
    input: SandboxInput,
    progress: Option<UnboundedSender<StageReport>>,
) -> RamariaResult<SandboxReport> {
    pipeline::run(options, input, progress).await
}

/// 行模式：从标准输入读一次完整输入并执行沙盒。
///
/// 参数:
/// - `user_db`: 用户库路径；继承后端来源时读取其配置，`None` 表示无宿主库；
/// - `default_backend`: 输入未指定后端来源时使用的来源选择。
///
/// 说明:
/// - 输入格式：可选头部指令行（`@backend` / `@base-url` / `@model` /
///   `@embedding` / `@persona`），随后 `user:` / `assistant:` 对话，
///   单独一行 `---` 之后写问题；
/// - 输入中出现后端指令时以输入为准，否则用 `default_backend`；
/// - 阶段进度写标准错误，最终报告写标准输出；
/// - 读取失败、解析失败与运行失败都返回对应错误。
pub async fn run_line_mode(
    user_db: Option<PathBuf>,
    default_backend: SandboxBackendChoice,
) -> RamariaResult<()> {
    let text = line::read_stdin().await?;
    let parsed = parse_line_input(&text).map_err(RamariaError::validation)?;
    let backend = if parsed.backend_explicit {
        parsed.backend
    } else {
        default_backend
    };
    let options = SandboxOptions {
        backend,
        user_db,
        ..SandboxOptions::default()
    };
    let input = parsed.input;

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        while let Some(stage) = receiver.recv().await {
            eprintln!("{}", line::format_stage(&stage));
        }
    });

    let outcome = run(&options, input, Some(sender)).await;
    let _ = printer.await;
    let report = outcome?;
    println!("{}", render_text(&report));
    Ok(())
}
