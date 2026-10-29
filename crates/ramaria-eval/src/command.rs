//! crates/ramaria-eval/src/command.rs - 评测命令与占位执行
//!
//! 设计特点:
//! - 命令枚举只承载参数，不持有引擎与宿主句柄
//! - 无参进入沙盒界面（TTY 为 TUI，非 TTY 为行模式），子命令未实现时返回 unsupported
//! - 异步实现：沙盒读写库与标准输入都需要运行时
//! - 错误走 core 结构化错误，由入口层统一映射 exit code

use std::path::PathBuf;

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::EvalContext;

/// 评测子命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalCommand {
    /// 按语料与矩阵批量运行
    Run {
        /// 语料名（缺省用基线登记的主语料）
        corpus: Option<String>,
        /// 矩阵定义文件
        matrix: Option<PathBuf>,
        /// 产物输出路径
        output: Option<PathBuf>,
    },
    /// 下载语料并校验哈希
    CorpusFetch {
        /// 语料名
        name: String,
    },
    /// 校验本地语料完整性
    CorpusVerify {
        /// 语料名
        name: String,
    },
    /// 生成基线库与指纹
    Baseline {
        /// 语料名
        corpus: Option<String>,
        /// 产物输出路径
        output: Option<PathBuf>,
    },
    /// 对已有运行结果判分
    Score {
        /// 运行 id
        run_id: String,
    },
    /// 生成运行报告
    Report {
        /// 运行 id
        run_id: String,
        /// 报告输出路径
        output: Option<PathBuf>,
    },
}

impl EvalCommand {
    /// 子命令名（用于错误文案）。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Run { .. } => "run",
            Self::CorpusFetch { .. } => "corpus fetch",
            Self::CorpusVerify { .. } => "corpus verify",
            Self::Baseline { .. } => "baseline",
            Self::Score { .. } => "score",
            Self::Report { .. } => "report",
        }
    }
}

/// 执行评测命令。
///
/// 参数:
/// - `command`: `None` 表示未给子命令（进入沙盒界面）；
/// - `context`: 宿主上下文（用户库路径等），透传给沙盒界面。
///
/// 返回:
/// - 无参：TTY 下进入沙盒界面，非 TTY 读取标准输入并运行沙盒（报告写标准输出）；
/// - 子命令：`Unsupported`（尚未实现）。
pub async fn run(command: Option<EvalCommand>, context: EvalContext) -> RamariaResult<()> {
    match command {
        None => crate::tui::run(context.user_db, context.backend).await,
        Some(command) => Err(RamariaError::unsupported(format!(
            "eval {} 尚未实现",
            command.name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 子命令统一返回 unsupported，文案带子命令名。
    #[tokio::test]
    async fn run_with_command_reports_unsupported() {
        let err = run(
            Some(EvalCommand::CorpusFetch {
                name: "lifebench".into(),
            }),
            EvalContext::default(),
        )
        .await
        .expect_err("子命令应返回 unsupported");
        assert!(
            matches!(&err, RamariaError::Unsupported { .. }),
            "应为 unsupported，实际 {err:?}"
        );
        assert!(
            err.to_string().contains("corpus fetch"),
            "文案应含子命令名，实际 {err}"
        );
    }

    /// 每个子命令都有稳定名称（提示与错误文案共用）。
    #[test]
    fn name_covers_all_subcommands() {
        let cases = [
            (
                EvalCommand::Run {
                    corpus: None,
                    matrix: None,
                    output: None,
                },
                "run",
            ),
            (
                EvalCommand::CorpusFetch { name: "x".into() },
                "corpus fetch",
            ),
            (
                EvalCommand::CorpusVerify { name: "x".into() },
                "corpus verify",
            ),
            (
                EvalCommand::Baseline {
                    corpus: None,
                    output: None,
                },
                "baseline",
            ),
            (EvalCommand::Score { run_id: "r".into() }, "score"),
            (
                EvalCommand::Report {
                    run_id: "r".into(),
                    output: None,
                },
                "report",
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(command.name(), expected);
        }
    }
}
