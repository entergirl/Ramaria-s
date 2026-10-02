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
/// - `created_at_ms`: 事件创建时间（Unix 毫秒）。
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
            let decay_w = time_decay_weight(ev.created_at, now, config);
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
            let decay_w = time_decay_weight(ev.created_at, now, config);
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
        let decay_w = time_decay_weight(ev.created_at, now, config);
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
        let decay_w = time_decay_weight(ev.created_at, now, config);
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

// =========================================================
// 置信度公式
// =========================================================

/// 计算最终置信度。
///
/// 公式: conf = C × (1 - 1/(1 + E_total))
///
/// 行为:
/// - E_total = 0 → conf = 0（无证据）
/// - E_total → ∞ → conf → C（收敛于一致度）
/// - C 低（矛盾证据）→ conf 被压低
/// - C 高（一致性证据）→ conf 接近 1.0 - 1/(1+E_total)
///
/// 参数:
/// - `c`: 一致度 0.0..1.0。
/// - `e_total`: 有效证据量。
///
/// 返回:
/// - 置信度 0.0..1.0。
pub fn compute_confidence(c: f64, e_total: f64) -> f64 {
    let c_clamped = c.clamp(0.0, 1.0);
    if e_total <= 0.0 {
        return 0.0;
    }
    let evidence_factor = 1.0 - 1.0 / (1.0 + e_total);
    c_clamped * evidence_factor
}

// =========================================================
// 完整更新流程
// =========================================================

/// 对单条 trait 执行完整的置信度更新。
///
/// 流程:
/// 1. 从历史证据计算 E_total_old 和 C_old。
/// 2. 计算新证据的 E_delta 和 C_new。
/// 3. 融合得到 E_total_new 和 C_combined。
/// 4. 计算 conf_new。
///
/// 参数:
/// - `trait_id`: trait ID。
/// - `conf_before`: 当前数据库中记录的置信度。
/// - `old_evidence`: 该 trait 的旧证据记录。
/// - `new_event_data`: 新事件数据 (confidence, created_at_ms) 列表。
/// - `new_event_scores`: 新事件对该 trait 的匹配度评分（-1..1），由 LLM 给出。
/// - `now_ms`: 当前时间。
/// - `config`: 置信度配置。
///
/// 返回:
/// - TraitConfidenceUpdate。
pub fn update_trait_confidence(
    trait_id: i64,
    conf_before: f64,
    old_evidence: &[TraitEvidence],
    new_event_data: &[(f64, i64)],
    new_event_scores: &[f64],
    now_ms: i64,
    config: &ConfidenceConfig,
) -> TraitConfidenceUpdate {
    // 旧证据量
    let e_old = compute_e_total(old_evidence, now_ms, config);
    let c_old = compute_consistency(old_evidence, now_ms, config);

    // 新证据贡献
    let e_new = compute_e_delta(new_event_data, now_ms, config);

    // 新证据一致度
    let c_new_batch = if new_event_scores.is_empty() {
        0.5
    } else {
        let avg_score = new_event_scores.iter().sum::<f64>() / new_event_scores.len() as f64;
        (avg_score + 1.0) / 2.0 // [-1,1] → [0,1]
    };

    // 融合
    let e_total_new = e_old + e_new;
    let c_combined = merge_consistency(c_old, e_old, c_new_batch, e_new);
    let conf_after = compute_confidence(c_combined, e_total_new);

    TraitConfidenceUpdate {
        trait_id,
        conf_before,
        conf_after,
        e_total_before: e_old,
        e_total_after: e_total_new,
        consistency_before: c_old,
        consistency_after: c_combined,
        new_evidence_count: new_event_data.len(),
    }
}

/// 旧 trait 状态的输入包，用于校准权重链置信度更新。
///
/// 将旧证据和校准权重捆绑为单一参数，
/// 避免 `update_trait_confidence_calibrated` 参数过多。
#[derive(Debug, Clone)]
pub struct OldTraitState {
    /// trait ID
    pub trait_id: i64,
    /// 更新前的置信度
    pub conf_before: f64,
    /// 旧证据记录列表
    pub old_evidence: Vec<TraitEvidence>,
    /// 旧证据对应的校准权重（与 old_evidence 一一对应）
    pub old_calibrated_weights: Vec<f64>,
}

/// 使用校准权重链的单条 trait 置信度更新。
///
/// 与 `update_trait_confidence` 的区别：使用校准权重 `calibrated_weights`
/// 替代原始证据 score 强度，使 E_total 和一致度 C 的计算反映事件实际重要性。
///
/// 参数:
/// - `old_state`: 旧 trait 状态的输入包（含 trait_id、旧置信度、旧证据和校准权重）。
/// - `new_event_data`: 新事件数据 (calibrated_weight, created_at_ms) 列表。
/// - `new_event_scores`: 新事件对该 trait 的匹配度评分（-1..1）。
/// - `now_ms`: 当前时间。
/// - `config`: 置信度配置。
///
/// 返回:
/// - TraitConfidenceUpdate。
pub fn update_trait_confidence_calibrated(
    old_state: &OldTraitState,
    new_event_data: &[(f64, i64)],
    new_event_scores: &[f64],
    now_ms: i64,
    config: &ConfidenceConfig,
) -> TraitConfidenceUpdate {
    // 旧证据量（校准权重链）
    let e_old = compute_e_total_calibrated(
        &old_state.old_evidence,
        &old_state.old_calibrated_weights,
        now_ms,
        config,
    );
    let c_old = compute_consistency_calibrated(
        &old_state.old_evidence,
        &old_state.old_calibrated_weights,
        now_ms,
        config,
    );

    // 新证据贡献（使用 calibrated_weight 替代原始 event.confidence）
    let e_new = new_event_data
        .iter()
        .map(|&(cal_w, created_at)| {
            let decay_w = time_decay_weight(created_at, now_ms, config);
            cal_w * decay_w
        })
        .sum();

    // 新证据一致度
    let c_new_batch = if new_event_scores.is_empty() {
        0.5
    } else {
        let avg_score = new_event_scores.iter().sum::<f64>() / new_event_scores.len() as f64;
        (avg_score + 1.0) / 2.0
    };

    // 融合
    let e_total_new = e_old + e_new;
    let c_combined = merge_consistency(c_old, e_old, c_new_batch, e_new);
    let conf_after = compute_confidence(c_combined, e_total_new);

    TraitConfidenceUpdate {
        trait_id: old_state.trait_id,
        conf_before: old_state.conf_before,
        conf_after,
        e_total_before: e_old,
        e_total_after: e_total_new,
        consistency_before: c_old,
        consistency_after: c_combined,
        new_evidence_count: new_event_data.len(),
    }
}

/// 批量更新所有 trait 的置信度。
///
/// 参数:
/// - `trait_states`: 各 trait 的当前状态 (id, conf_before, old_evidence)。
/// - `new_event_data_by_trait`: 各 trait 的新事件数据。
/// - `new_event_scores_by_trait`: 各 trait 的新事件 LLM 匹配度评分。
/// - `now_ms`: 当前时间。
/// - `config`: 置信度配置。
///
/// 返回:
/// - ConfidenceSummary。
pub fn run_confidence_update(
    trait_states: &[(i64, f64, Vec<TraitEvidence>)],
    new_event_data_by_trait: &[Vec<(f64, i64)>],
    new_event_scores_by_trait: &[Vec<f64>],
    now_ms: i64,
    config: &ConfidenceConfig,
) -> ConfidenceSummary {
    let n = trait_states.len();
    let mut updates = Vec::with_capacity(n);

    for (i, state) in trait_states.iter().enumerate() {
        let (trait_id, conf_before, ref old_evidence) = *state;
        let new_data = new_event_data_by_trait
            .get(i)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let new_scores = new_event_scores_by_trait
            .get(i)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        let update = update_trait_confidence(
            trait_id,
            conf_before,
            old_evidence,
            new_data,
            new_scores,
            now_ms,
            config,
        );
        updates.push(update);
    }

    let has_significant_change = updates
        .iter()
        .any(|u| (u.conf_after - u.conf_before).abs() >= 0.05);

    ConfidenceSummary {
        updates,
        has_significant_change,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
