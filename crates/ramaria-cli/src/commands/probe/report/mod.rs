//! crates/ramaria-cli/src/commands/probe/report/mod.rs - 探针 report 模块入口
//!
//! 设计特点:
//! - 档位对比报告（`probe report`）：汇总各档位评分生成对比表，给出每维最佳档位与综合定稿建议
//! - 消融对比（--ablation）：F 组（移除）/ S 组（替代）/ I 组（净增量）三类对照分别配对
//!   做 Wilcoxon 符号秩 + Cohen's d + 95% CI + BH-FDR 判定，并按对照类型分栏表述
//! - 等效性检验：并行做 TOST，补上显著性框架无法证明零净增量盲区，边界取 |d_av|=0.3
//! - 辅助指标四件套（产物可复算）/ 人工抽检校准 / 知识层质量（双口径 × 三判据）
//! - 风格形态指标（客观口径）与外部效度局限声明必出；markdown / JSON 双形态输出
//! - 子模块按域拆分，对外名称在此逐项 re-export（`probe::report::X` 路径不变）

mod ablation;
mod aux_metrics;
mod calibration;
mod knowledge_quality;
mod markdown;
mod render;
mod stats;
mod style_metrics;
mod tost;

// 对外 re-export：`probe::report::X` 与 `ramaria_cli::commands::probe::X` 路径保持不变。
pub use aux_metrics::VariantAuxMetrics;
pub use calibration::{CalibrationResult, ManualScore};
pub use knowledge_quality::KnowledgeQualityReport;
pub(crate) use render::run_report;
pub use render::{
    AblationComparisonRow, AblationReport, DimensionRecommendation, ProbeReport, Recommendation,
    VariantReportRow,
};

// 测试经 `super::report::X` 引用（仅测试构建需要，故门控在 `#[cfg(test)]`）。
#[cfg(test)]
pub(crate) use ablation::build_ablation_report;
#[cfg(test)]
pub(crate) use aux_metrics::{AuxiliaryMetrics, compute_auxiliary_metrics};
#[cfg(test)]
pub(crate) use calibration::read_manual_scores;
#[cfg(test)]
pub(crate) use knowledge_quality::{
    KnowledgeJudgeRates, KnowledgeQualityScope, assess_knowledge_quality,
};
#[cfg(test)]
pub(crate) use markdown::render_report_markdown;
#[cfg(test)]
pub(crate) use render::build_limitations;
#[cfg(test)]
pub(crate) use stats::{
    bh_fdr_adjust, cohens_d_paired, cohens_d_pooled, erf_approx, normal_cdf, wilcoxon_signed_rank_p,
};
#[cfg(test)]
pub(crate) use style_metrics::{
    EMOTION_DESCRIPTIVE_NOTE, VariantStyleMetrics, compute_style_metrics,
};
#[cfg(test)]
pub(crate) use tost::{student_t_cdf, tost_equivalence};
