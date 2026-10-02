//! crates/ramaria-memory/src/inference/stats/tests/metrics.rs - 跨分类指标
//!
//! 设计特点:
//! - 由 父测试模块 以 mod metrics; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 跨分类指标
// =========================================================

#[test]
fn emotional_stability_with_calibrated_weights() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作"),
            0.9,
            0.5,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
        ),
        make_event(
            "E2",
            "s2",
            Some("社交"),
            0.8,
            0.5,
            -0.3,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let enrichments = EventEnrichment::derive_batch(&events);
    let stability = compute_emotional_stability(&events, Some(&enrichments), &config);
    assert!(stability > 0.0, "方差应大于零");
}

#[test]
fn narrative_consistency_perfect() {
    let cats = vec![
        CategoryStats {
            category: "工作".into(),
            event_count: 1,
            n_eff: 1.0,
            valence_mean: 0.0,
            valence_std: 0.0,
            valence_positive_ratio: 0.5,
            share_mean: 0.5,
            share_std: 0.0,
            presentation_objective_ratio: 0.6,
            presentation_subjective_ratio: 0.3,
            presentation_mixed_ratio: 0.1,
            group_weight: 0.5,
        },
        CategoryStats {
            category: "社交".into(),
            event_count: 1,
            n_eff: 1.0,
            valence_mean: 0.0,
            valence_std: 0.0,
            valence_positive_ratio: 0.5,
            share_mean: 0.5,
            share_std: 0.0,
            presentation_objective_ratio: 0.6,
            presentation_subjective_ratio: 0.3,
            presentation_mixed_ratio: 0.1,
            group_weight: 0.5,
        },
    ];
    let consistency = compute_narrative_consistency(&cats);
    assert!((consistency - 1.0).abs() < 1e-10);
}

#[test]
fn narrative_consistency_single_category() {
    let cats = vec![CategoryStats {
        category: "工作".into(),
        event_count: 1,
        n_eff: 1.0,
        valence_mean: 0.0,
        valence_std: 0.0,
        valence_positive_ratio: 0.5,
        share_mean: 0.5,
        share_std: 0.0,
        presentation_objective_ratio: 0.5,
        presentation_subjective_ratio: 0.3,
        presentation_mixed_ratio: 0.2,
        group_weight: 1.0,
    }];
    let consistency = compute_narrative_consistency(&cats);
    assert!((consistency - 1.0).abs() < 1e-10, "单个分类一致性为 1.0");
}

/// compute_share_skewness 各事件分布参数化验证。
#[test]
fn share_skewness_cases() {
    let config = CalibratedWeightConfig::default();
    // 对称分布（share 0.3/0.5/0.7）→ 偏度接近 0
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作"),
            0.9,
            0.5,
            0.0,
            0.3,
            Presentation::Mixed,
            None,
        ),
        make_event(
            "E2",
            "s2",
            Some("工作"),
            0.9,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
        make_event(
            "E3",
            "s3",
            Some("工作"),
            0.9,
            0.5,
            0.0,
            0.7,
            Presentation::Mixed,
            None,
        ),
    ];
    let skew = compute_share_skewness(&events, None, &config);
    assert!(skew.abs() < 0.1, "对称分布偏度应接近0，实际={skew}");
    // 单事件 → 偏度 0
    let events = vec![make_event(
        "E1",
        "s1",
        Some("工作"),
        0.9,
        0.5,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    )];
    let skew = compute_share_skewness(&events, None, &config);
    assert!((skew - 0.0).abs() < 1e-10);
}

#[test]
fn share_kurtosis_uniform() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作"),
            0.9,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
        make_event(
            "E2",
            "s2",
            Some("工作"),
            0.9,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let kurt = compute_share_kurtosis(&events, None, &config);
    assert!((kurt - 0.0).abs() < 1e-10);
}
