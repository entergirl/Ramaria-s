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
mod tests {
    use super::*;
    use ramaria_core::types::now_ms;

    fn make_evidence(
        trait_id: i64,
        event_id: i64,
        score: f64,
        days_ago: f64,
        config: &ConfidenceConfig,
    ) -> TraitEvidence {
        let now = now_ms();
        let created_at = now - (days_ago * MS_PER_DAY) as i64;
        let decay = time_decay_weight(created_at, now, config);
        TraitEvidence {
            id: 0,
            trait_id,
            event_id,
            direction: if score >= 0.0 {
                ramaria_core::types::EvidenceDirection::Support
            } else {
                ramaria_core::types::EvidenceDirection::Contradict
            },
            score,
            decay,
            created_at,
        }
    }

    // ---- 时间衰减 ----

    /// time_decay_weight 各时间参数化验证（近期/长期/保底）。
    #[test]
    fn time_decay_cases() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        // 刚创建 → 权重接近 1.0
        let w = time_decay_weight(now, now, &config);
        assert!((w - 1.0).abs() < 0.01, "刚创建的事件权重应接近 1.0");
        // 180 天前 → w ≈ e^(-180/60) ≈ 0.05，不低于保底
        let created_at = now - (180.0 * MS_PER_DAY) as i64;
        let w = time_decay_weight(created_at, now, &config);
        assert!(w < 0.1, "180天前权重应 < 0.1，实际={}", w);
        assert!(w >= config.min_decay, "不应低于保底值");
        // 1000 天前 → 被保底值钳制
        let created_at = now - (1000.0 * MS_PER_DAY) as i64;
        let w = time_decay_weight(created_at, now, &config);
        assert!((w - config.min_decay).abs() < 1e-10, "应被保底值钳制");
    }

    // ---- E_total ----

    #[test]
    fn e_total_empty() {
        let config = ConfidenceConfig::default();
        let e = compute_e_total(&[], now_ms(), &config);
        assert!((e - 0.0).abs() < 1e-10);
    }

    #[test]
    fn e_total_computation() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        let ev1 = make_evidence(1, 1, 0.8, 0.0, &config); // 刚创建，score=0.8，decay≈1
        let ev2 = make_evidence(1, 2, 0.6, 0.0, &config); // 刚创建，score=0.6，decay≈1
        let evidence = vec![ev1, ev2];
        let e = compute_e_total(&evidence, now, &config);
        // E ≈ 0.8*1.0 + 0.6*1.0 = 1.4
        assert!((e - 1.4).abs() < 0.01);
    }

    #[test]
    fn e_total_with_contradiction() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        let ev1 = make_evidence(1, 1, 0.9, 0.0, &config);
        let ev2 = make_evidence(1, 2, -0.7, 0.0, &config); // 矛盾证据
        let evidence = vec![ev1, ev2];
        let e = compute_e_total(&evidence, now, &config);
        // E ≈ 0.9 + 0.7 = 1.6（取绝对值）
        assert!((e - 1.6).abs() < 0.01);
    }

    // ---- 一致度 C ----

    /// compute_consistency 各证据组合参数化验证。
    #[test]
    fn consistency_cases() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        // 全支持 → 一致度 > 0.9
        let evidence = vec![
            make_evidence(1, 1, 0.9, 0.0, &config),
            make_evidence(1, 2, 0.8, 0.0, &config),
        ];
        let c = compute_consistency(&evidence, now, &config);
        assert!(c > 0.9, "全支持证据一致度应 > 0.9，实际={}", c);
        // 正负混合 → 一致度偏低（C=(0.3+1)/2=0.65）
        let evidence = vec![
            make_evidence(1, 1, 0.9, 0.0, &config),
            make_evidence(1, 2, -0.3, 0.0, &config),
        ];
        let c = compute_consistency(&evidence, now, &config);
        assert!((c - 0.65).abs() < 0.01);
        assert!(c < 0.9, "混合证据一致度应偏低");
        // 无证据 → 0.5 中性
        let c = compute_consistency(&[], now_ms(), &config);
        assert!((c - 0.5).abs() < 1e-10, "无证据时一致度应为 0.5 中性");
    }

    // ---- 一致度融合 ----

    #[test]
    fn merge_consistency_basic() {
        // C_old=0.9, E_old=10, C_new=0.5, E_new=2
        // C_combined = (0.9*10 + 0.5*2) / 12 = (9+1)/12 = 0.833
        let c = merge_consistency(0.9, 10.0, 0.5, 2.0);
        assert!((c - 10.0 / 12.0).abs() < 0.01);
    }

    #[test]
    fn merge_consistency_dominated_by_old() {
        // 大量旧证据占主导
        let c = merge_consistency(0.8, 100.0, 0.2, 1.0);
        // (0.8*100 + 0.2*1) / 101 ≈ 0.794
        let expected = (80.0 + 0.2) / 101.0;
        assert!((c - expected).abs() < 0.01);
    }

    // ---- 置信度公式 ----

    #[test]
    fn confidence_no_evidence() {
        let conf = compute_confidence(0.5, 0.0);
        assert!((conf - 0.0).abs() < 1e-10);
    }

    #[test]
    fn confidence_high_both() {
        // C=0.9, E=300 → (1 - 1/301) ≈ 0.9967, conf ≈ 0.897
        let conf = compute_confidence(0.9, 300.0);
        assert!((conf - 0.9 * (1.0 - 1.0 / 301.0)).abs() < 0.001);
        assert!(conf > 0.85);
    }

    #[test]
    fn confidence_low_consistency() {
        // C=0.2（大量矛盾证据），E=300
        // conf = 0.2 × (1 - 1/301) ≈ 0.2 × 0.997 ≈ 0.199
        let conf = compute_confidence(0.2, 300.0);
        assert!(conf < 0.25, "低一致度应压低置信度");
    }

    #[test]
    fn confidence_limited_by_c() {
        // C=0.5, E 非常大 → conf ≈ 0.5
        let conf = compute_confidence(0.5, 1_000_000.0);
        assert!((conf - 0.5).abs() < 0.01, "E→∞ 时 conf 应收敛于 C");
    }

    #[test]
    fn confidence_documented_values() {
        // 算法文档 §5.3.2 中的示例值:
        // C≈0.9, E=300 → conf≈0.897
        let conf = compute_confidence(0.9, 300.0);
        assert!((conf - 0.897).abs() < 0.01);

        // 矛盾证据后: C降至0.79, E=350 → conf≈0.79 × 0.997 ≈ 0.787
        let conf2 = compute_confidence(0.79, 350.0);
        assert!((conf2 - 0.787).abs() < 0.01);
    }

    // ---- 完整更新 ----

    #[test]
    fn update_trait_confidence_new_evidence() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        let old_evidence = vec![
            make_evidence(1, 1, 0.9, 0.0, &config),
            make_evidence(1, 2, 0.8, 0.0, &config),
        ];
        let new_data = vec![(0.9, now), (0.8, now)];
        let new_scores = vec![0.85, 0.75];

        let update =
            update_trait_confidence(1, 0.89, &old_evidence, &new_data, &new_scores, now, &config);
        assert!(update.conf_after > 0.0);
        assert!(update.e_total_after > update.e_total_before);
    }

    #[test]
    fn update_trait_confidence_contradiction() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        let old_evidence = vec![
            make_evidence(1, 1, 0.9, 0.0, &config),
            make_evidence(1, 2, 0.8, 0.0, &config),
        ];
        // 新增矛盾证据
        let new_data = vec![(0.9, now), (0.8, now)];
        let new_scores = vec![-0.7, -0.6]; // LLM 判定为矛盾

        let update =
            update_trait_confidence(1, 0.89, &old_evidence, &new_data, &new_scores, now, &config);
        // 矛盾证据应降低置信度
        assert!(
            update.conf_after < update.conf_before,
            "矛盾证据应降低置信度。before={:.3}, after={:.3}",
            update.conf_before,
            update.conf_after
        );
    }

    #[test]
    fn run_confidence_update_batch() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        let evidence1 = vec![make_evidence(1, 1, 0.7, 0.0, &config)];
        let evidence2 = vec![make_evidence(2, 2, 0.6, 0.0, &config)];

        let trait_states = vec![(1i64, 0.6, evidence1), (2i64, 0.5, evidence2)];
        let new_data = vec![vec![(0.8, now)], vec![(0.7, now)]];
        let new_scores = vec![vec![0.7], vec![0.6]];

        let summary = run_confidence_update(&trait_states, &new_data, &new_scores, now, &config);
        assert_eq!(summary.updates.len(), 2);
        // 两条 trait 都应有提升
        for u in &summary.updates {
            assert!(u.conf_after > 0.0);
        }
    }

    // ---- 校准权重链版本 ----

    #[test]
    fn compute_e_total_calibrated_basic() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        let ev = make_evidence(1, 1, 0.8, 0.0, &config);
        // 校准权重 = 2.0（高重要性事件）
        let e = compute_e_total_calibrated(&[ev], &[2.0], now, &config);
        // E ≈ 2.0 × 0.8 × 1.0 = 1.6
        assert!((e - 1.6).abs() < 0.01, "E 应为 1.6，实际={}", e);
    }

    #[test]
    fn compute_e_total_calibrated_vs_original() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        let ev1 = make_evidence(1, 1, 0.8, 0.0, &config);
        let ev2 = make_evidence(1, 2, -0.5, 0.0, &config);
        let evidence = vec![ev1, ev2];

        // 原版：E = |0.8| + |0.5| = 1.3
        let e_orig = compute_e_total(&evidence, now, &config);
        assert!((e_orig - 1.3).abs() < 0.01);

        // 校准版：第一条权重 3.0，第二条权重 1.0
        // E = 3.0×0.8 + 1.0×0.5 = 2.4 + 0.5 = 2.9
        let e_cal = compute_e_total_calibrated(&evidence, &[3.0, 1.0], now, &config);
        assert!((e_cal - 2.9).abs() < 0.01, "E_cal 应为 2.9，实际={}", e_cal);

        // 高重要性事件对 E_total 的贡献显著增大
        assert!(e_cal > e_orig, "校准后 E_total 应更大");
    }

    #[test]
    fn compute_consistency_calibrated_high_weight_amplifies() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        // 证据1: 高支持(score=0.9) + 高权重(3.0)
        // 证据2: 中性(score=0.0) + 低权重(0.5)
        let ev1 = make_evidence(1, 1, 0.9, 0.0, &config);
        let ev2 = make_evidence(1, 2, 0.0, 0.0, &config);
        let evidence = vec![ev1, ev2];

        // 原版一致度：(0.9 + 0.0) / 2 → 原始均值 0.45，映射后 (0.45+1)/2 = 0.725
        let c_orig = compute_consistency(&evidence, now, &config);
        assert!((c_orig - 0.725).abs() < 0.01);

        // 校准版：高权重放大高支持证据的影响
        let c_cal = compute_consistency_calibrated(&evidence, &[3.0, 0.5], now, &config);
        // C 应高于原版（高权重支持证据占主导），但不超过 0.95
        assert!(
            c_cal > c_orig,
            "校准后一致度应更高，原={:.4}, 校准={:.4}",
            c_orig,
            c_cal
        );
        assert!(c_cal < 0.95, "不应过度放大");
    }

    #[test]
    fn update_trait_confidence_calibrated_basic() {
        let config = ConfidenceConfig::default();
        let now = now_ms();
        let old_evidence = vec![make_evidence(1, 1, 0.9, 0.0, &config)];
        let old_weights = vec![1.5]; // 校准权重
        let new_data = vec![(1.2, now)]; // (calibrated_weight=1.2, created_at)
        let new_scores = vec![0.7];

        let old_state = OldTraitState {
            trait_id: 1,
            conf_before: 0.6,
            old_evidence,
            old_calibrated_weights: old_weights,
        };
        let update =
            update_trait_confidence_calibrated(&old_state, &new_data, &new_scores, now, &config);
        assert!(update.conf_after > 0.0);
        assert!(update.e_total_after > update.e_total_before);
    }
}
