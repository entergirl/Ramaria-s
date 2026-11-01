//! crates/ramaria-eval/src/lib.rs - Ramaria 评测工具层入口
//!
//! 设计特点:
//! - dev-only：不随安装包发布；产品能力层（core / service 等）不依赖本 crate，仅 CLI 入口接入
//! - 依赖面仅 core / service：需要存储或检索句柄时经服务层取用
//! - 按统计、测量、命令、语料、沙盒、界面六域分模块，统计与测量为纯计算零 I/O
//! - 命令层只做参数承载与结果返回，无子命令进入沙盒界面（TTY 为 TUI，非 TTY 为行模式）
//! - 评测装配使用独立配置快照、不拉起后台循环
//! - 评测只写评测根下的库与产物，不触碰用户库

use std::path::PathBuf;

pub mod assembly;
pub mod baseline;
pub mod command;
pub mod corpus;
pub mod measure;
pub mod run;
pub mod sandbox;
pub mod stats;
pub mod tui;

pub use assembly::{EVAL_CONFIG_FILE, EVAL_DB_FILE, open_engine, write_config_snapshot};
pub use baseline::{BaselineArtifact, BaselineReport};
pub use command::{EvalCommand, run};
pub use run::{
    BaselineTier, Condition, Pipeline, RunItem, RunItemRecord, RunMatrix, RunReport, RunState,
    RunWorkspace, build_full_context, plan_items,
};
pub use sandbox::{
    SandboxBackendChoice, SandboxBackendMode, SandboxInput, SandboxModelKind, SandboxOptions,
    SandboxReport, StageKind, StageReport, StageStatus, run_line_mode,
};

/// 评测运行上下文（入口层注入的宿主信息）。
///
/// 字段约定:
/// - `user_db`: 用户库路径；继承后端来源时读取其配置，`None` 表示无宿主库
///   （沙盒回退产品默认后端）；
/// - `backend`: 本次运行的后端来源选择，缺省继承当前用户配置。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvalContext {
    pub user_db: Option<PathBuf>,
    pub backend: SandboxBackendChoice,
}
