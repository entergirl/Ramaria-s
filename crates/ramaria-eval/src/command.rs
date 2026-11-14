//! crates/ramaria-eval/src/command.rs - 评测命令与执行分发
//!
//! 设计特点:
//! - 命令枚举只承载参数，不持有引擎与宿主句柄
//! - 无参进入沙盒界面（TTY 为 TUI，非 TTY 为行模式）
//! - 语料校验、基线建库与批量运行已落地，报告与产物摘要直出标准输出
//! - 未实现子命令统一返回 unsupported，文案带子命令名
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
    /// 校验本地语料完整性
    CorpusVerify {
        /// 语料名
        name: String,
        /// 语言标签（zh / en；缺省校验注册表内全部文件）
        language: Option<String>,
    },
    /// 生成基线库与指纹
    Baseline {
        /// 语料名
        corpus: Option<String>,
        /// 规模与跨度档位
        tier: Option<String>,
        /// 语言标签（zh / en）
        language: Option<String>,
        /// 产物输出路径
        output: Option<PathBuf>,
        /// 只回填 artifact 映射（必须显式给档位）
        artifacts: bool,
    },
    /// 对已有运行结果判分
    Score {
        /// 运行 id（也可以是运行根目录路径）
        run_id: String,
        /// 跳过 judge 判分，只产出确定性指标
        no_judge: bool,
    },
    /// 生成运行报告
    Report {
        /// 运行 id（也可以是运行根目录路径）
        run_id: String,
        /// 压缩层 oracle 的运行 id（缺省尝试运行根下的 oracle）
        oracle: Option<String>,
        /// 额外对照运行（可重复）
        compare: Vec<String>,
        /// 报告输出目录（缺省为运行根下 report）
        output: Option<PathBuf>,
    },
    /// 以预置上下文跑压缩层 oracle
    Oracle {
        /// 被引用的运行 id（也可以是运行根目录路径）
        run_id: String,
        /// 产物输出路径（缺省为被引用运行根下的 oracle）
        output: Option<PathBuf>,
    },
}

impl EvalCommand {
    /// 子命令名（用于错误文案）。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Run { .. } => "run",
            Self::CorpusVerify { .. } => "corpus verify",
            Self::Baseline { .. } => "baseline",
            Self::Score { .. } => "score",
            Self::Report { .. } => "report",
            Self::Oracle { .. } => "oracle",
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
/// - `corpus verify`：校验通过时报告写标准输出，失败返回结构化错误；
/// - `baseline`：逐档建库并导出产物，摘要写标准输出；带 `artifacts` 时只回填 artifact 映射；
/// - `run`：按矩阵批量运行，摘要写标准输出（已存在运行根则续跑）；
/// - `score`：对已有运行结果判分，产物写运行根下 `score/`，摘要写标准输出；
/// - `oracle`：以预置上下文复用被引用运行的矩阵跑一遍，摘要写标准输出；
/// - 其余子命令：`Unsupported`（尚未实现）。
pub async fn run(command: Option<EvalCommand>, context: EvalContext) -> RamariaResult<()> {
    match command {
        None => crate::tui::run(context.user_db, context.backend).await,
        Some(EvalCommand::CorpusVerify { name, language }) => {
            crate::corpus::verify_command(&name, language.as_deref())
        }
        Some(EvalCommand::Baseline {
            corpus,
            tier,
            language,
            output,
            artifacts: true,
        }) => {
            let report = crate::baseline::backfill(corpus, tier, language, output).await?;
            println!("{}", report.render());
            Ok(())
        }
        Some(EvalCommand::Baseline {
            corpus,
            tier,
            language,
            output,
            artifacts: false,
        }) => {
            let report = crate::baseline::run(corpus, tier, language, output).await?;
            println!("{}", report.render());
            Ok(())
        }
        Some(EvalCommand::Run {
            corpus,
            matrix,
            output,
        }) => {
            let report = crate::run::run(corpus, matrix, output).await?;
            println!("{}", report.render());
            Ok(())
        }
        Some(EvalCommand::Score { run_id, no_judge }) => {
            let cwd = std::env::current_dir()
                .map_err(|error| RamariaError::io("读取当前工作目录失败", Some(error)))?;
            let root = crate::score::resolve_run_root(&cwd, &run_id);
            let (report, usage) =
                crate::score::run(&root, crate::score::ScoreOptions { no_judge }).await?;
            println!(
                "{}",
                report.render(&crate::score::ScoreReport::dir_of(&root), &usage.total)
            );
            Ok(())
        }
        Some(EvalCommand::Report {
            run_id,
            oracle,
            compare,
            output,
        }) => {
            let cwd = std::env::current_dir()
                .map_err(|error| RamariaError::io("读取当前工作目录失败", Some(error)))?;
            let outcome = crate::report::run(
                &cwd,
                &crate::report::ReportOptions {
                    run_id,
                    oracle,
                    compare,
                    output,
                },
            )?;
            print!("{}", outcome.render());
            Ok(())
        }
        Some(EvalCommand::Oracle { run_id, output }) => {
            let report = crate::oracle::run(&run_id, output).await?;
            println!("{}", report.render());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 报告子命令对不存在的运行在读取产物前失败。
    #[tokio::test]
    async fn report_rejects_unknown_run() {
        let missing = std::env::temp_dir().join("ramaria-eval-report-missing-run");
        let err = run(
            Some(EvalCommand::Report {
                run_id: missing.to_string_lossy().to_string(),
                oracle: None,
                compare: Vec::new(),
                output: None,
            }),
            EvalContext::default(),
        )
        .await
        .expect_err("未知运行应报错");
        assert!(
            matches!(&err, RamariaError::Validation { .. }),
            "应为 Validation，实际 {err:?}"
        );
        assert!(
            err.to_string().contains("运行目录不存在"),
            "文案应提示运行目录不存在，实际 {err}"
        );
    }

    /// 未知语料在校验前即报错，不触碰文件系统。
    #[tokio::test]
    async fn corpus_verify_rejects_unknown_corpus() {
        let err = run(
            Some(EvalCommand::CorpusVerify {
                name: "nope".into(),
                language: None,
            }),
            EvalContext::default(),
        )
        .await
        .expect_err("未知语料应报错");
        assert!(
            matches!(&err, RamariaError::Validation { .. }),
            "应为 Validation，实际 {err:?}"
        );
        assert!(
            err.to_string().contains("未知语料"),
            "文案应含未知语料，实际 {err}"
        );
    }

    /// 语言标签非法时在读取文件前失败。
    #[tokio::test]
    async fn corpus_verify_rejects_unknown_language() {
        let err = run(
            Some(EvalCommand::CorpusVerify {
                name: "lifebench".into(),
                language: Some("fr".into()),
            }),
            EvalContext::default(),
        )
        .await
        .expect_err("非法语言应报错");
        assert!(
            matches!(&err, RamariaError::Validation { .. }),
            "应为 Validation，实际 {err:?}"
        );
        assert!(
            err.to_string().contains("未知语料语言"),
            "文案应含语言取值，实际 {err}"
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
                EvalCommand::CorpusVerify {
                    name: "x".into(),
                    language: None,
                },
                "corpus verify",
            ),
            (
                EvalCommand::Baseline {
                    corpus: None,
                    tier: None,
                    language: None,
                    output: None,
                    artifacts: false,
                },
                "baseline",
            ),
            (
                EvalCommand::Score {
                    run_id: "r".into(),
                    no_judge: false,
                },
                "score",
            ),
            (
                EvalCommand::Report {
                    run_id: "r".into(),
                    oracle: None,
                    compare: Vec::new(),
                    output: None,
                },
                "report",
            ),
            (
                EvalCommand::Oracle {
                    run_id: "r".into(),
                    output: None,
                },
                "oracle",
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(command.name(), expected);
        }
    }
}
