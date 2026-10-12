//! crates/ramaria-cli/src/cli/probe.rs - 探针子命令参数定义
//!
//! 设计特点:
//! - 探针命令族的 clap 参数定义单独成文件（顶层定义文件按命令族拆分，控制单文件规模）
//! - 默认值引用探针模块常量（`ramaria_cli::commands::probe::*`），常量单一来源

use std::path::PathBuf;

use clap::Subcommand;

/// 探针子命令。
#[derive(Subcommand)]
pub(crate) enum ProbeArgs {
    /// 构建测试集（`dataset` 保留为 alias）
    #[command(visible_alias = "dataset")]
    Build {
        /// 目标 persona_uid（默认自动选择白名单内角色类 persona，兜底 char-0001）
        #[arg(long)]
        persona: Option<String>,

        /// 每维题数（默认 10，3 维共 30 题；正式评估可扩大至 ≥30 题）
        #[arg(long, default_value_t = ramaria_cli::commands::probe::DEFAULT_QUESTIONS_PER_DIM)]
        questions_per_dim: usize,

        /// 抽样 seed（固定可复跑；同 seed 输出相同测试集）
        #[arg(long, default_value_t = ramaria_cli::commands::probe::DEFAULT_SEED)]
        seed: u64,

        /// 显式数据源文件（JSON；不指定则从数据库构建，无真实数据时夹具兜底）
        #[arg(long)]
        source: Option<PathBuf>,

        /// 数据集输出文件（`-` = stdout；不指定时 --json 输出完整数据集）
        #[arg(long)]
        output: Option<String>,

        /// 追加 15 档消融 Profile（B0/B1/F0/F1~F4/S_*/I_*），用于消融数据集构建
        #[arg(long)]
        ablation: bool,
    },

    /// 按参数档位批量跑对话管线，结构化输出（档位 → 输出 → 指标）
    Run {
        /// 数据集文件（`ramaria probe build` 的产物）
        #[arg(long)]
        dataset: PathBuf,

        /// 只跑指定档位（逗号分隔 id，默认全部；无效 id 跳过）
        #[arg(long)]
        variants: Option<String>,

        /// 每档位最多跑题数（默认全部）
        #[arg(long)]
        limit: Option<usize>,

        /// 不按档位参数重建 utt 块（复用库中已建块；θ_gap/条数档位仍需重建才生效，
        /// 非切分档位——如仅 top_k 变化——可跳过重建复用已建块）
        #[arg(long)]
        no_rebuild_utt: bool,

        /// 统计法重复次数 N（多次运行取均值 ± 置信区间；默认 1 即不聚合）
        #[arg(long, default_value_t = 1)]
        repeat: usize,

        /// 结果输出文件（`-` = stdout 输出原始结果 JSON）
        #[arg(long)]
        output: Option<String>,
    },

    /// 对 `probe run` 实验结果自动评分（事实维 golden + 语气维 LLM-as-judge）
    Evaluate {
        /// 实验结果文件（`ramaria probe run --output` 的产物）
        #[arg(long)]
        results: PathBuf,

        /// 数据集文件（`ramaria probe build --output` 的产物；提供时按 golden reference 精确评分）
        #[arg(long)]
        dataset: Option<PathBuf>,

        /// 只评指定档位（逗号分隔 id，默认全部）
        #[arg(long)]
        variants: Option<String>,

        /// 评分数值输出文件（`-` = stdout 输出评分 JSON）
        #[arg(long)]
        output: Option<String>,

        /// 跳过语气维 LLM-as-judge（仅评事实维，节省 LLM 调用）
        #[arg(long)]
        no_tone_judge: bool,
    },

    /// 统计主动对话数值基线（只读：投递 / 回应 / 判据计数）
    Baseline {
        /// 回应判定窗口（小时；0 = 不设上界——投递后任意时间的首条本地用户消息均算回应）
        #[arg(long, default_value_t = 24)]
        window_hours: u32,
    },

    /// 生成档位对比报告与定稿建议（markdown/JSON 双形态；支持人工抽检校准）
    Report {
        /// 实验结果文件（`ramaria probe run --output` 的产物）
        #[arg(long)]
        results: PathBuf,

        /// 评分数值文件（`ramaria probe evaluate --output` 的产物）
        #[arg(long)]
        evaluation: Option<PathBuf>,

        /// 人工抽检校准文件（JSON: 数组 of {item_id, score}）
        #[arg(long)]
        calibration: Option<PathBuf>,

        /// 报告输出文件（`-` = stdout；.md 为 markdown、.json 为 JSON）
        #[arg(long)]
        output: Option<String>,

        /// 消融对比报告模式：自动识别 F0 基线与 F1~F4（及 S 组 vs B1），
        /// 按题目配对 Wilcoxon + Cohen's d + 95% CI + FDR 校正输出统计判定；
        /// 需要 --evaluation 评分数值文件。
        #[arg(long)]
        ablation: bool,
    },
}
