//! crates/ramaria-memory/src/inference/confidence/tests.rs - //! crates/ramaria-memory/src/inference/confidence.rs - 证据累积式置信度更新单元测试
//!
//! 设计特点:
//! - 位于 inference::confidence 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 confidence.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

/// 固定测试基准时间（Unix 毫秒），保证用例不依赖真实时钟、连续运行结果一致。
const TEST_NOW_MS: i64 = 1_760_000_000_000;

fn make_evidence(
    trait_id: i64,
    event_id: i64,
    score: f64,
    days_ago: f64,
    config: &ConfidenceConfig,
) -> TraitEvidence {
    let now = TEST_NOW_MS;
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
    let now = TEST_NOW_MS;
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
    let e = compute_e_total(&[], TEST_NOW_MS, &config);
    assert!((e - 0.0).abs() < 1e-10);
}

#[test]
fn e_total_computation() {
    let config = ConfidenceConfig::default();
    let now = TEST_NOW_MS;
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
    let now = TEST_NOW_MS;
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
    let now = TEST_NOW_MS;
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
    let c = compute_consistency(&[], TEST_NOW_MS, &config);
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
    let now = TEST_NOW_MS;
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
    let now = TEST_NOW_MS;
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
    let now = TEST_NOW_MS;
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
    let now = TEST_NOW_MS;
    let ev = make_evidence(1, 1, 0.8, 0.0, &config);
    // 校准权重 = 2.0（高重要性事件）
    let e = compute_e_total_calibrated(&[ev], &[2.0], now, &config);
    // E ≈ 2.0 × 0.8 × 1.0 = 1.6
    assert!((e - 1.6).abs() < 0.01, "E 应为 1.6，实际={}", e);
}

#[test]
fn compute_e_total_calibrated_vs_original() {
    let config = ConfidenceConfig::default();
    let now = TEST_NOW_MS;
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
    let now = TEST_NOW_MS;
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
    let now = TEST_NOW_MS;
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
