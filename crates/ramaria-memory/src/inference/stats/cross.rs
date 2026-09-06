//! crates/ramaria-memory/src/inference/stats/cross.rs - Ramaria A6 跨分类高阶指标模块
//!
//! 设计特点:
//! - 情绪稳定性: 全局 valence 加权标准差（值越小越平稳）。
//! - 叙事一致性: 跨分类 presentation 分布余弦相似度均值（仅 1 个分类时返回 1.0）。
//! - share 偏度/峰度: 基于加权矩的分布形状指标。
//! - compute_cross_category_metrics: 收口上述指标为 CrossCategoryMetrics。
//!
//! 可见性说明:
//! - compute_weights_for_events 为模块私有，按增强数据是否存在选择校准/简单权重。
//!
//! 安全约束:
//! - 纯数值计算，零 I/O；不记录任何事件原文或隐私数据。

use super::config::{CalibratedWeightConfig, CategoryStats, CrossCategoryMetrics, EventEnrichment};
use super::weighted::{weighted_mean, weighted_variance};
use super::weights::{compute_calibrated_weights_batch, compute_simple_weights_batch};
use ramaria_core::types::MemoryEvent;

// =========================================================
// A6: 跨分类高阶指标
// =========================================================

/// 计算事件列表的权重向量（根据配置选择校准或简单权重）。
fn compute_weights_for_events(
    events: &[MemoryEvent],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> Vec<f64> {
    match enrichments {
        Some(enr) => compute_calibrated_weights_batch(events, enr, config),
        None => compute_simple_weights_batch(events),
    }
}

/// 计算情绪稳定性（全局 valence 加权标准差）。
///
/// 说明:
/// - 不按分类分组，直接对全部事件的 valence 做加权标准差。
/// - 标准差小 → 情绪平稳；标准差大 → 情绪波动剧烈。
///
/// 参数:
/// - `events`: 事件列表。
/// - `enrichments`: 可选的增强数据（None 时使用简单权重）。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 全局加权 valence 标准差。
pub fn compute_emotional_stability(
    events: &[MemoryEvent],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> f64 {
    let valences: Vec<f64> = events.iter().map(|e| e.valence).collect();
    let weights = compute_weights_for_events(events, enrichments, config);
    let mean = weighted_mean(&valences, &weights);
    weighted_variance(&valences, &weights, mean).sqrt()
}

/// 计算叙事一致性（跨分类 presentation 分布的相似度）。
///
/// 策略:
/// - 对每对分类计算其 presentation 分布（三元素向量）的余弦相似度。
/// - 取所有分类对的均值作为一致性指标。
/// - 仅 1 个分类时返回 1.0（完全一致）。
///
/// 参数:
/// - `categories`: 所有分类的统计摘要。
///
/// 返回:
/// - 归一化一致性指标 0.0..1.0。值越高表示跨分类表达风格越一致。
pub fn compute_narrative_consistency(categories: &[CategoryStats]) -> f64 {
    if categories.len() <= 1 {
        return 1.0;
    }

    let mut similarities = Vec::new();
    for i in 0..categories.len() {
        for j in (i + 1)..categories.len() {
            let a = &categories[i];
            let b = &categories[j];
            let dot = a.presentation_objective_ratio * b.presentation_objective_ratio
                + a.presentation_subjective_ratio * b.presentation_subjective_ratio
                + a.presentation_mixed_ratio * b.presentation_mixed_ratio;
            let norm_a = (a.presentation_objective_ratio.powi(2)
                + a.presentation_subjective_ratio.powi(2)
                + a.presentation_mixed_ratio.powi(2))
            .sqrt();
            let norm_b = (b.presentation_objective_ratio.powi(2)
                + b.presentation_subjective_ratio.powi(2)
                + b.presentation_mixed_ratio.powi(2))
            .sqrt();
            if norm_a > 0.0 && norm_b > 0.0 {
                similarities.push((dot / (norm_a * norm_b)).clamp(0.0, 1.0));
            }
        }
    }

    if similarities.is_empty() {
        0.0
    } else {
        similarities.iter().sum::<f64>() / similarities.len() as f64
    }
}

/// 计算 share 分布的偏度（基于加权）。
///
/// 公式: skew = Σ(w_i · (x_i - x̄)³) / (σ³ · Σ w_i)
///
/// 参数:
/// - `events`: 事件列表。
/// - `enrichments`: 可选的增强数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 偏度系数。正值=右偏（少数事件 share 很高），负值=左偏。
pub fn compute_share_skewness(
    events: &[MemoryEvent],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> f64 {
    let shares: Vec<f64> = events.iter().map(|e| e.share).collect();
    let weights = compute_weights_for_events(events, enrichments, config);
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let mean = weighted_mean(&shares, &weights);
    let variance = weighted_variance(&shares, &weights, mean);
    let std = variance.sqrt();
    if std < 1e-10 {
        return 0.0;
    }
    let m3: f64 = shares
        .iter()
        .zip(&weights)
        .map(|(s, w)| w * (s - mean).powi(3))
        .sum::<f64>()
        / total_weight;
    m3 / std.powi(3)
}

/// 计算 share 分布的峰度（基于加权）。
///
/// 公式: kurt = Σ(w_i · (x_i - x̄)⁴) / (σ⁴ · Σ w_i)
///
/// 参数:
/// - `events`: 事件列表。
/// - `enrichments`: 可选的增强数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 峰度系数。正值=尖峰分布，负值=扁平分布。
pub fn compute_share_kurtosis(
    events: &[MemoryEvent],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> f64 {
    let shares: Vec<f64> = events.iter().map(|e| e.share).collect();
    let weights = compute_weights_for_events(events, enrichments, config);
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let mean = weighted_mean(&shares, &weights);
    let variance = weighted_variance(&shares, &weights, mean);
    let std = variance.sqrt();
    if std < 1e-10 {
        return 0.0;
    }
    let m4: f64 = shares
        .iter()
        .zip(&weights)
        .map(|(s, w)| w * (s - mean).powi(4))
        .sum::<f64>()
        / total_weight;
    m4 / std.powi(4)
}

/// 计算完整的跨分类高阶指标。
///
/// 参数:
/// - `events`: 事件列表。
/// - `categories`: 所有分类的统计摘要。
/// - `enrichments`: 可选的增强数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - CrossCategoryMetrics 结构体。
pub fn compute_cross_category_metrics(
    events: &[MemoryEvent],
    categories: &[CategoryStats],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> CrossCategoryMetrics {
    let emotional_stability = compute_emotional_stability(events, enrichments, config);
    let narrative_consistency = compute_narrative_consistency(categories);
    // 态度矛盾检测在 LLM 推断阶段基于分类对做标记，具体计数由语义判断
    // 此处预留基础指标：分类数 >= 2 时标记可能存在矛盾
    let attitude_contradiction_count = if categories.len() >= 2 { 1 } else { 0 };
    let share_skewness = compute_share_skewness(events, enrichments, config);
    let share_kurtosis = compute_share_kurtosis(events, enrichments, config);

    CrossCategoryMetrics {
        emotional_stability,
        narrative_consistency,
        attitude_contradiction_count,
        share_skewness,
        share_kurtosis,
    }
}
