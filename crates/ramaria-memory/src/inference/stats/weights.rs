//! crates/ramaria-memory/src/inference/stats/weights.rs - Ramaria 情境强度加权与校准权重链核心模块
//!
//! 设计特点:
//! - 情境强度加权: 弱情境(1-2) → ×1.5，中性(3)/None → ×1.0，强情境(4-5) → ×0.5。
//! - 校准权重链: w_i = salience_cal × confidence_factor × situation_multiplier × source_support。
//! - calibrate_salience: 基于复现次数/情绪强度/提及频率的指数校准，结果 clamp 到 [0.01, 1.0]。
//! - 兼容路径: compute_simple_weight(s) 保留旧公式 w_i = salience × situation_multiplier。
//! - compute_calibrated_weight 依赖 classify_event 判定准入轨道，故引用 admission 模块。
//!
//! 安全约束:
//! - 纯数值计算，零 I/O；不记录任何事件原文或隐私数据。

use super::admission::classify_event;
use super::config::{CalibratedWeightConfig, EventEnrichment};
use ramaria_core::types::MemoryEvent;

// =========================================================
// 情境强度加权
// =========================================================

/// 根据情境强度计算 salience 乘数。
///
/// 公式（对齐决策列表 §5）:
/// - 弱情境（1-2）: ×1.5 — 日常琐事中流露的性格信号更强
/// - 中性（3 或 None）: ×1.0 — 常规权重
/// - 强情境（4-5）: ×0.5 — 强情境中行为更多由环境驱动，非性格
///
/// 参数:
/// - `strength`: 情境强度 1-5 或 None（等效 3）。
///
/// 返回:
/// - salience 权重乘数（0.5 / 1.0 / 1.5）。
pub fn situation_multiplier(strength: Option<i32>) -> f64 {
    match strength {
        Some(1) | Some(2) => 1.5,
        Some(4) | Some(5) => 0.5,
        _ => 1.0, // None 或 3 均为中性
    }
}

// =========================================================
// 校准权重链核心函数
// =========================================================

/// salience 校准函数。
///
/// 公式: `salience_cal = raw_salience^exp × (1 + α_rec × recurrence + α_int × intensity + α_men × mention)`
///
/// 其中 α_rec/int/men 分别是复现次数、情绪强度、提及频率的最大加成比例。
///
/// 说明:
/// - 原始 salience 通过指数变换调整凸性（exp=1.0 时线性，exp<1.0 时压缩高值差异）。
/// - 三个加成因子独立叠加，上限由配置控制。
/// - 结果 clamp 到 [0.01, 1.0] 以避免零权重。
///
/// 参数:
/// - `raw_salience`: 事件的原始显著性 [0.0, 1.0]。
/// - `recurrence_count`: 同主题复现次数归一化值 [0.0, 1.0]。
/// - `emotional_intensity`: 情绪强度 [0.0, 1.0]。
/// - `mention_frequency`: 用户提及频率归一化值 [0.0, 1.0]。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 校准后的 salience 值 [0.01, 1.0]。
pub fn calibrate_salience(
    raw_salience: f64,
    recurrence_count: f64,
    emotional_intensity: f64,
    mention_frequency: f64,
    config: &CalibratedWeightConfig,
) -> f64 {
    // 指数变换: 调整 salience 的凸性
    let base = raw_salience.clamp(0.0, 1.0).powf(config.salience_exponent);

    // 三个加成因子，各自归一化后乘以最大加成比例
    let rec_boost = recurrence_count.clamp(0.0, 1.0) * config.recurrence_boost_max;
    let intensity_boost = emotional_intensity.clamp(0.0, 1.0) * config.intensity_boost_max;
    let men_boost = mention_frequency.clamp(0.0, 1.0) * config.mention_boost_max;

    let calibrated = base * (1.0 + rec_boost + intensity_boost + men_boost);
    // 保底 0.01，避免零权重导致事件完全消失
    calibrated.clamp(0.01, 1.0)
}

/// 计算四因子校准权重。
///
/// 公式: `w_i = salience_cal × confidence_factor × situation_multiplier × source_support`
///
/// 其中:
/// - `salience_cal`: 由 `calibrate_salience` 计算的校准后显著性。
/// - `confidence_factor`: 由事件置信度决定（Confirmed→1.0, Tentative→半权重, Discarded→0.0）。
/// - `situation_multiplier`: 情境强度乘数（1.5 / 1.0 / 0.5）。
/// - `source_support`: 多源互证因子 = min(1.0, source_count / min_sources)。
///
/// 说明:
/// - 四因子相乘意味着任一因子为零则整体权重为零。
/// - 这是相对于 `salience × situation_multiplier` 的核心升级。
///
/// 参数:
/// - `event`: 待计算权重的事件。
/// - `enrichment`: 事件的增强统计数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 校准后的综合权重 [0.0, 1.0]。
pub fn compute_calibrated_weight(
    event: &MemoryEvent,
    enrichment: &EventEnrichment,
    config: &CalibratedWeightConfig,
) -> f64 {
    // Step 1: salience 校准
    let salience_cal = calibrate_salience(
        event.salience,
        enrichment.topic_recurrence_count,
        enrichment.emotional_intensity,
        enrichment.mention_frequency,
        config,
    );

    // Step 2: confidence_factor（基于三轨分类）
    let track = classify_event(event);
    let confidence_factor = track.confidence_factor(config.tentative_weight_factor);

    // Step 3: situation_multiplier
    let sit_mult = situation_multiplier(event.situation_strength);

    // Step 4: source_support（多源互证）
    let source_support = if enrichment.source_count == 0 {
        0.5 // 无来源时给半权重（防御性处理）
    } else {
        let ratio = enrichment.source_count as f64 / config.min_sources_for_full_support as f64;
        ratio.min(1.0)
    };

    salience_cal * confidence_factor * sit_mult * source_support
}

/// 为事件列表批量计算校准权重。
///
/// 参数:
/// - `events`: 事件列表。
/// - `enrichments`: 与 events 一一对应的增强数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 与 events 一一对应的校准权重向量。
///
/// # Panics
/// - 当 events 和 enrichments 长度不一致时 panic。
pub fn compute_calibrated_weights_batch(
    events: &[MemoryEvent],
    enrichments: &[EventEnrichment],
    config: &CalibratedWeightConfig,
) -> Vec<f64> {
    assert_eq!(
        events.len(),
        enrichments.len(),
        "events 与 enrichments 长度必须一致"
    );
    events
        .iter()
        .zip(enrichments)
        .map(|(event, enrichment)| compute_calibrated_weight(event, enrichment, config))
        .collect()
}

/// 使用简单权重（兼容路径）。
///
/// 公式: `w_i = salience × situation_multiplier(situation_strength)`
///
/// 说明:
/// - 这是旧权重公式，保留以支持 `use_calibrated_weights=false`。
///
/// 参数:
/// - `event`: 待计算权重的事件。
///
/// 返回:
/// - 简单权重 [0.0, 1.5]。
pub fn compute_simple_weight(event: &MemoryEvent) -> f64 {
    event.salience * situation_multiplier(event.situation_strength)
}

/// 为事件列表批量计算简单权重。
pub fn compute_simple_weights_batch(events: &[MemoryEvent]) -> Vec<f64> {
    events.iter().map(compute_simple_weight).collect()
}
