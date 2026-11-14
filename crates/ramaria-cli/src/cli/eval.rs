//! crates/ramaria-cli/src/cli/eval.rs - 评测子命令参数定义
//!
//! 设计特点:
//! - 评测命令族的 clap 参数定义单独成文件，控制顶层定义文件规模
//! - `corpus` 的二级动词（verify）在此展开，参数只表达类型与缺省
//! - 沙盒后端来源与独立配置参数只对无子命令调用生效，取值域由 clap 收敛
//! - 语义校验（语言标签、语料名、文件存在性）留给评测层

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

/// 沙盒后端来源参数。
///
/// 字段约定:
/// - `Inherit`: 继承当前用户配置（缺省）；
/// - `Local` / `Cloud`: 本次运行使用独立配置，配套 `--base-url` / `--model` / `--embedding-path`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum BackendModeArg {
    /// 继承当前用户配置
    Inherit,
    /// 本地模型
    Local,
    /// 云端模型
    Cloud,
}

impl std::fmt::Display for BackendModeArg {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Inherit => "inherit",
            Self::Local => "local",
            Self::Cloud => "cloud",
        };
        formatter.write_str(value)
    }
}

/// 评测子命令组（无子命令时进入评测台，默认沙盒）。
#[derive(Args)]
pub(crate) struct EvalArgs {
    /// 评测台沙盒的后端来源（只对无子命令调用生效）
    #[arg(long, value_enum, default_value_t = BackendModeArg::Inherit)]
    pub(crate) backend: BackendModeArg,

    /// 独立后端的模型服务地址（本地留空用默认地址）
    #[arg(long)]
    pub(crate) base_url: Option<String>,

    /// 独立后端的模型标识（本地留空由服务端选择）
    #[arg(long)]
    pub(crate) model: Option<String>,

    /// 独立后端的向量模型目录（留空则向量通道降级）
    #[arg(long)]
    pub(crate) embedding_path: Option<PathBuf>,

    /// 评测子命令（缺省进入评测台，默认沙盒）
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

    /// 语料管理（verify: 校验本地副本）
    #[command(subcommand)]
    Corpus(CorpusCmd),

    /// 生成基线库与指纹
    Baseline {
        /// 语料名（缺省用基线登记的主语料）
        #[arg(long)]
        corpus: Option<String>,

        /// 规模与跨度档位
        #[arg(long)]
        tier: Option<String>,

        /// 语料语言（zh / en）
        #[arg(long)]
        language: Option<String>,

        /// 产物输出路径
        #[arg(long)]
        output: Option<PathBuf>,

        /// 只回填 artifact 映射（需同时给 --tier）
        #[arg(long)]
        artifacts: bool,
    },

    /// 对已有运行结果判分
    Score {
        /// 运行 id（也可以是运行根目录路径）
        run_id: String,

        /// 跳过 judge 判分，只产出确定性指标
        #[arg(long)]
        no_judge: bool,
    },

    /// 生成运行报告
    Report {
        /// 运行 id（也可以是运行根目录路径）
        run_id: String,

        /// 压缩层 oracle 的运行 id（缺省尝试运行根下的 oracle）
        #[arg(long)]
        oracle: Option<String>,

        /// 额外对照运行（可重复），用于预算轴或其它对照
        #[arg(long = "compare")]
        compare: Vec<String>,

        /// 报告输出目录（缺省为运行根下 report）
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// 以预置上下文跑压缩层 oracle
    Oracle {
        /// 被引用的运行 id（也可以是运行根目录路径）
        run_id: String,

        /// 产物输出路径（缺省为被引用运行根下的 oracle）
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

/// 语料管理子命令。
#[derive(Subcommand)]
pub(crate) enum CorpusCmd {
    /// 校验本地语料的完整性
    Verify {
        /// 语料名（如 lifebench）
        name: String,

        /// 语料语言（zh / en；留空校验注册表内全部文件）
        #[arg(long)]
        language: Option<String>,
    },
}
