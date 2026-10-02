//! crates/ramaria-cli/src/commands/probe/evaluate/mod.rs - 探针 evaluate 自动评分模块入口
//!
//! 设计特点:
//! - `run_evaluate`：读取 probe run 实验结果，按档位逐题评分并输出评分数值 / JSON / 文本摘要
//! - 事实维：embedding 余弦 + 关键词项加权（旧 2-gram / 长度归一 / 子句级事实点三口径）
//! - 语气维：LLM-as-judge（rubric 1~5、温度 0、few-shot 锚定）；仅本地后端可用
//! - 情感维：确定性 rubric（0/0.5/1 回应恰当性），标记词表驱动，零 LLM 依赖
//! - 单题失败不中断批量；judge / embedding 缺失静默降级并标注；输出不含完整原文
//! - 子模块按域拆分，对外名称在此逐项 re-export（`probe::evaluate::X` 路径不变）

mod model;
mod pipeline;
mod scoring;

pub(crate) use model::{FactItemScore, ItemEvaluation, ProbeEvaluation, VariantEvaluation};
pub(crate) use pipeline::{read_experiment, run_evaluate};

#[cfg(test)]
pub(crate) use model::DimensionScoreAgg;
#[cfg(test)]
pub(crate) use model::EmotionItemScore;
#[cfg(test)]
pub(crate) use model::ToneItemScore;
#[cfg(test)]
pub(crate) use pipeline::load_golden_references;
#[cfg(test)]
pub(crate) use scoring::aggregate_round_dimension_scores;
#[cfg(test)]
pub(crate) use scoring::content_bigrams;
#[cfg(test)]
pub(crate) use scoring::fact_point_score;
#[cfg(test)]
pub(crate) use scoring::is_local_backend;
#[cfg(test)]
pub(crate) use scoring::keyword_hit_norm_score;
#[cfg(test)]
pub(crate) use scoring::reference_clauses;
#[cfg(test)]
pub(crate) use scoring::score_emotion_item;
#[cfg(test)]
pub(crate) use scoring::tone_judge_system_prompt;
