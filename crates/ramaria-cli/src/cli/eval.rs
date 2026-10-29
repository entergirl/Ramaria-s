//! crates/ramaria-cli/src/cli/eval.rs - 评测子命令参数定义
//!
//! 设计特点:
//! - 评测命令族的 clap 参数定义单独成文件，控制顶层定义文件规模
//! - `corpus` 的二级动词（fetch / verify）在此展开，语料接入前即可解析
//! - 参数只表达类型与缺省，语义校验与文件存在性检查留给评测层

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// 评测子命令组（无子命令时进入沙盒界面）。
#[derive(Args)]
pub(crate) struct EvalArgs {
    /// 评测子命令（缺省进入沙盒界面）
    #[command(subcommand)]
    pub(crate) command: Option<EvalCmd>,
}

/// 评测子命令。
#[derive(Subcommand)]
pub(crate) enum EvalCmd {
    /// 按语料与矩阵批量运行
    Run {
        /// 语料名（缺省用基线登记的主语料）
        #[arg(long)]
        corpus: Option<String>,

        /// 矩阵定义文件
        #[arg(long)]
        matrix: Option<PathBuf>,

        /// 产物输出路径
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// 语料管理（fetch: 下载并校验 / verify: 校验本地副本）
    #[command(subcommand)]
    Corpus(CorpusCmd),

    /// 生成基线库与指纹
    Baseline {
        /// 语料名（缺省用基线登记的主语料）
        #[arg(long)]
        corpus: Option<String>,

        /// 产物输出路径
        #[arg(long)]
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
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

/// 语料管理子命令。
#[derive(Subcommand)]
pub(crate) enum CorpusCmd {
    /// 下载语料并校验哈希
    Fetch {
        /// 语料名（如 lifebench）
        name: String,
    },
    /// 校验本地语料的完整性
    Verify {
        /// 语料名（如 lifebench）
        name: String,
    },
}
