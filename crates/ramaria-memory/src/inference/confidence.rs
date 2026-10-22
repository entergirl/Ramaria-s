//! crates/ramaria-memory/src/inference/confidence.rs - 证据累积式置信度更新
//!
//! 设计特点:
//! - C2: 有效证据量 E_total + 一致度 C → conf = C × (1 - 1/(1 + E_total))
//! - 时间衰减权重对接 Ebbinghaus 遗忘曲线: w(t) = e^(-t/S), L2 层 S=60
//! - 每条事件的证据贡献 = confidence × w(t)
//! - 增量权重随总证据量增长自然衰减（近因事件不会异常放大）
//! - 新旧 C 的融合使用 n_eff 加权平滑
//! - 纯数值计算，零 I/O，不依赖数据库
//!
//! 决策 2 标注（保留+标注）:
//! - calibrated 族（`compute_e_total_calibrated` / `compute_consistency_calibrated` /
//!   `update_trait_confidence_calibrated` + `OldTraitState`）当前零生产调用：
//!   Phase C 现用路径走未校准版（`run_confidence_update` → `update_trait_confidence`）。
//! - 按决策 2 保留并标注，预留给校准权重链路径；v1.6 接线时核查是否并入现用管线。

use ramaria_core::types::TraitEvidence;

use crate::utils::MS_PER_DAY;

// =========================================================
// 配置类型
// =========================================================

/// 置信度更新配置。
///
/// 职责:
/// - 管理证据时间衰减和一致度融合参数。
///
/// 字段约定:
/// - `stability_s`: L2 层稳定性系数，默认 60（对接 Ebbinghaus 遗忘曲线）。
/// - `min_decay`: 时间衰减保底值，防止极旧证据权重为 0（默认 0.01）。
#[derive(Debug, Clone)]
pub struct ConfidenceConfig {
    /// L2 层稳定性系数 S（衰减公式 w = e^(-t/S)）
    pub stability_s: f64,
    /// 时间衰减保底值
    pub min_decay: f64,
}

impl Default for ConfidenceConfig {
    fn default() -> Self {
        Self {
            stability_s: 60.0,
            min_decay: 0.01,
        }
    }
}

impl From<ramaria_core::config::ConfidenceConf> for ConfidenceConfig {
    fn from(conf: ramaria_core::config::ConfidenceConf) -> Self {
        Self {
            stability_s: conf.stability_s,
            min_decay: conf.min_decay,
        }
    }
}

// =========================================================
// 输出类型
// =========================================================

/// 单条性格标签的置信度更新结果。
///
/// 职责:
/// - 记录更新前后的 E_total、C、conf，供日志和 UI 展示。
#[derive(Debug, Clone)]
pub struct TraitConfidenceUpdate {
    /// 性格标签 ID（对应 personality_traits.id）
    pub trait_id: i64,
    /// 更新前的置信度
    pub conf_before: f64,
    /// 更新后的置信度
    pub conf_after: f64,
    /// 更新前的有效证据量
    pub e_total_before: f64,
    /// 更新后的有效证据量
    pub e_total_after: f64,
    /// 更新前的一致度
    pub consistency_before: f64,
    /// 更新后的一致度
    pub consistency_after: f64,
    /// 本次新增的证据条数
    pub new_evidence_count: usize,
}

/// 全局置信度更新汇总。
#[derive(Debug, Clone)]
pub struct ConfidenceSummary {
    /// 逐 trait 更新结果
    pub updates: Vec<TraitConfidenceUpdate>,
    /// 是否有任何 trait 发生了显著变化（conf 变化 ≥ 0.05）
    pub has_significant_change: bool,
}

// =========================================================
// 时间衰减权重
// =========================================================

/// 计算单条证据的时间衰减权重。
///
/// 公式: w(t) = max(e^(-t/S), min_decay)
///
/// 参数:
/// - `created_at_ms`: 时间基准（源事件发生时间，缺失回退登记时刻；Unix 毫秒）。
/// - `now_ms`: 当前时间（Unix 毫秒）。
/// - `config`: 置信度配置。
///
/// 返回:
/// - 衰减权重 0.01..1.0。
pub fn time_decay_weight(created_at_ms: i64, now_ms: i64, config: &ConfidenceConfig) -> f64 {
    let t_days = (now_ms.saturating_sub(created_at_ms)) as f64 / MS_PER_DAY;
    let weight = (-t_days / config.stability_s).exp();
    weight.max(config.min_decay)
}

// =========================================================
// 有效证据量 E_total
// =========================================================

/// 从已有证据记录计算有效证据总量 E_total。
///
/// 每条证据的贡献 = event_confidence × time_decay_weight(t)。
/// 其中 event_confidence 为证据的 score 绝对值。
///
/// 参数:
/// - `evidence_records`: 该 trait 的所有历史证据记录。
/// - `now_ms`: 当前时间（用于衰减计算）。
/// - `config`: 置信度配置。
///
/// 返回:
/// - E_total（有效证据量）。
pub fn compute_e_total(
    evidence_records: &[TraitEvidence],
    now: i64,
    config: &ConfidenceConfig,
) -> f64 {
    evidence_records
        .iter()
        .map(|ev| {
            // 证据贡献 = |score| × decay_weight
            // score 的范围 -1.0..1.0，取绝对值为证据强度
            let strength = ev.score.abs();
            let decay_w = time_decay_weight(ev.occurred_at_or_created(), now, config);
            strength * decay_w
        })
        .sum()
}

/// 使用校准权重链计算 E_total。
///
/// 每条证据的贡献 = `calibrated_weight × |score| × decay_weight`。
/// 与 `compute_e_total` 的区别：额外乘以校准权重 `calibrated_weight`，
/// 反映事件本身的重要性（salience × confidence × situation × source）。
///
/// 参数:
/// - `evidence_records`: 历史证据记录。
/// - `calibrated_weights`: 每个证据对应的校准权重（需与 evidence_records 一一对应）。
/// - `now`: 当前时间。
/// - `config`: 置信度配置。
///
/// 返回:
/// - 校准后的 E_total。
pub fn compute_e_total_calibrated(
    evidence_records: &[TraitEvidence],
    calibrated_weights: &[f64],
    now: i64,
    config: &ConfidenceConfig,
) -> f64 {
    evidence_records
        .iter()
        .zip(calibrated_weights)
        .map(|(ev, &cal_w)| {
            let strength = ev.score.abs();
            let decay_w = time_decay_weight(ev.occurred_at_or_created(), now, config);
            cal_w * strength * decay_w
        })
        .sum()
}

/// 计算新增证据对 E_total 的贡献。
///
/// 参数:
/// - `new_evidence`: 新增证据的 (event_confidence, event_created_at_ms) 列表。
/// - `now_ms`: 当前时间。
/// - `config`: 置信度配置。
///
/// 返回:
/// - 新增 E_total 贡献值。
pub fn compute_e_delta(new_evidence: &[(f64, i64)], now_ms: i64, config: &ConfidenceConfig) -> f64 {
    new_evidence
        .iter()
        .map(|&(conf, created_at)| {
            let decay_w = time_decay_weight(created_at, now_ms, config);
            conf * decay_w
        })
        .sum()
}

// =========================================================
// 一致度 C
// =========================================================

/// 从已有证据记录计算一致度 C。
///
/// 一致度 C 是所有证据 score（匹配度评分）的加权均值。
/// score > 0 = 支持该 trait，score < 0 = 矛盾该 trait。
///
/// 参数:
/// - `evidence_records`: 该 trait 的所有历史证据记录。
/// - `now_ms`: 当前时间。
/// - `config`: 置信度配置。
///
/// 返回:
/// - 一致度 C（0.0..1.0）。若无法计算返回 0.5（中性）。
pub fn compute_consistency(
    evidence_records: &[TraitEvidence],
    now: i64,
    config: &ConfidenceConfig,
) -> f64 {
    if evidence_records.is_empty() {
        return 0.5; // 无证据时中性
    }

    let mut weighted_sum = 0.0;
    let mut total_weight = 0.0;

    for ev in evidence_records {
        let decay_w = time_decay_weight(ev.occurred_at_or_created(), now, config);
        // score 本身已有方向，直接使用（不取绝对值）
        weighted_sum += ev.score * decay_w;
        total_weight += decay_w;
    }

    if total_weight < 1e-12 {
        return 0.5;
    }

    // 将 [-1, 1] 映射到 [0, 1]
    let raw_consistency = weighted_sum / total_weight;
    (raw_consistency + 1.0) / 2.0
}

/// 使用校准权重链计算一致度 C。
///
/// 与 `compute_consistency` 的区别：一致性加权计算中每条证据的权重 = `calibrated_weight × decay_w`，
/// 而非仅 `decay_w`。使得高重要性事件对一致性的影响与其证据量匹配。
///
/// 参数:
/// - `evidence_records`: 历史证据记录。
/// - `calibrated_weights`: 每个证据对应的校准权重（需与 evidence_records 一一对应）。
/// - `now`: 当前时间。
/// - `config`: 置信度配置。
///
/// 返回:
/// - 校准后的一致度 C（0.0..1.0）。
pub fn compute_consistency_calibrated(
    evidence_records: &[TraitEvidence],
    calibrated_weights: &[f64],
    now: i64,
    config: &ConfidenceConfig,
) -> f64 {
    if evidence_records.is_empty() {
        return 0.5;
    }

    let mut weighted_sum = 0.0;
    let mut total_weight = 0.0;

    for (ev, &cal_w) in evidence_records.iter().zip(calibrated_weights) {
        let decay_w = time_decay_weight(ev.occurred_at_or_created(), now, config);
        let combined_w = cal_w * decay_w;
        weighted_sum += ev.score * combined_w;
        total_weight += combined_w;
    }

    if total_weight < 1e-12 {
        return 0.5;
    }

    let raw_consistency = weighted_sum / total_weight;
    (raw_consistency + 1.0) / 2.0
}

/// 融合新旧一致度 C。
///
/// 使用有效样本量加权平滑:
/// C_new_combined = (C_old × E_old + C_new_batch × E_new) / (E_old + E_new)
///
/// 参数:
/// - `c_old`: 旧一致度。
/// - `e_old`: 旧 E_total。
/// - `c_new_batch`: 新批次的一致度。
/// - `e_new`: 新批次的 E_total 贡献。
///
/// 返回:
/// - 融合后的一致度。
pub fn merge_consistency(c_old: f64, e_old: f64, c_new_batch: f64, e_new: f64) -> f64 {
    let total = e_old + e_new;
    if total < 1e-12 {
        return 0.5;
    }
    (c_old * e_old + c_new_batch * e_new) / total
}

mod update;
pub use update::*;

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
