//! crates/ramaria-eval/src/lib.rs - Ramaria 评测工具层入口
//!
//! 设计特点:
//! - dev-only：不随安装包发布；产品能力层（core / service 等）不依赖本 crate，仅 CLI 入口接入
//! - 依赖面仅 core / service：需要存储或检索句柄时经服务层取用
//! - 按统计、测量、命令、语料、沙盒、界面、运行、判分与 oracle 分模块，统计与测量为纯计算零 I/O
//! - 命令层只做参数承载与结果返回，无子命令进入评测台（TTY 为 TUI，非 TTY 回退沙盒行模式）
//! - 评测装配使用独立配置快照、不拉起后台循环
//! - 评测只写评测根下的库与产物，不触碰用户库

use std::path::PathBuf;

pub mod assembly;
pub mod baseline;
pub mod command;
pub mod corpus;
pub mod measure;
pub mod oracle;
pub mod report;
pub mod run;
pub mod sandbox;
pub mod score;
pub mod stats;
pub mod tui;

pub use assembly::{EVAL_CONFIG_FILE, EVAL_DB_FILE, open_engine, write_config_snapshot};
pub use baseline::{ArtifactBackfill, ArtifactBackfillReport, BaselineArtifact, BaselineReport};
pub use command::{EvalCommand, run};
pub use report::{ReportArtifact, ReportOptions, ReportOutcome};
pub use run::{
    BaselineTier, Condition, Pipeline, RunItem, RunItemRecord, RunMatrix, RunReport, RunState,
    RunWorkspace, build_full_context, plan_items,
};
pub use sandbox::{
    SandboxBackendChoice, SandboxBackendMode, SandboxInput, SandboxModelKind, SandboxOptions,
    SandboxReport, StageKind, StageReport, StageStatus, run_line_mode,
};
pub use score::{
    ConditionScore, ConditionUsage, ConditionUsageEntry, JudgeSource, JudgeStatus, RubricSummary,
    ScoreOptions, ScoreReport, ScoredItem, TemplateHash, UsageReport, UsageSummary,
};

// =========================================================
// 评测专用密钥
// =========================================================

/// 评测专用密钥的环境变量名。
pub const EVAL_API_KEY_ENV: &str = "RAMARIA_EVAL_API_KEY";

/// 读取评测专用密钥。
///
/// 返回:
/// - `Some(key)`: 环境变量已设置且非空白（值已去除首尾空白）；
/// - `None`: 未设置或为空白——调用方按"沿用系统 keychain"处理，行为与默认一致。
///
/// 说明:
/// - 密钥只进进程内存（provider 构造时注入），不落库、不写配置文件、不进日志；
/// - 日志里只允许出现 `has_eval_key` 布尔。
pub fn eval_api_key() -> Option<String> {
    std::env::var(EVAL_API_KEY_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

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
