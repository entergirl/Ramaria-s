//! crates/ramaria-memory/src/inference/confidence/update.rs - 置信度更新编排
//!
//! 设计特点:
//! - 单 trait 更新：证据量 / 一致度 → conf，写入证据行并更新 trait
//! - calibrated 族保留备用（当前生产路径为未校准版）
//! - 批量更新按活跃 trait 逐个处理，无证据时跳过

use super::*;

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
