//! crates/ramaria-memory/src/inference/stats/tests/calibrate.rs - 校准权重链核心
//!
//! 设计特点:
//! - 由 父测试模块 以 mod calibrate; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 校准权重链核心
// =========================================================

/// calibrate_salience 各 (raw, rec, int, men) 组合参数化验证（含 floor/ceiling）。
#[test]
fn calibrate_salience_cases() {
    let config = CalibratedWeightConfig::default();
    let cases = [
        // (raw, recurrence, intensity, mention, expected)
        (0.8, 0.0, 0.0, 0.0, 0.8),    // 无加成 → 保持不变
        (0.8, 1.0, 0.0, 0.0, 1.0),    // rec=1.0 → boost 0.30 → clamp 1.0
        (0.5, 0.5, 0.5, 0.5, 0.6625), // rec=0.15 + int=0.10 + men=0.075
        (0.0, 0.0, 0.0, 0.0, 0.01),   // 极低 → 保底 0.01
        (1.0, 1.0, 1.0, 1.0, 1.0),    // 全加成 → clamp 1.0
    ];
    for (raw, rec, int, men, expected) in cases {
        let cal = calibrate_salience(raw, rec, int, men, &config);
        assert!((cal - expected).abs() < 1e-6, "raw={raw} 期望 {expected}");
    }
}

#[test]
fn compute_calibrated_weight_confirmed() {
    let config = CalibratedWeightConfig::default();
    let event = make_event(
        "E",
        "s",
        None,
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment {
        topic_recurrence_count: 0.5,
        emotional_intensity: 0.5,
        mention_frequency: 0.5,
        source_count: 3,
    };
    // salience_cal = 0.8 * (1 + 0.15 + 0.10 + 0.075) = 0.8 * 1.325 = 1.06 → clamp 1.0
    // confidence_factor = 1.0 (confirmed)
    // situation_multiplier = 1.0 (None → 中性)
    // source_support = min(1.0, 3/3) = 1.0
    // w = 1.0 * 1.0 * 1.0 * 1.0 = 1.0
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    assert!((w - 1.0).abs() < 1e-6);
}

#[test]
fn compute_calibrated_weight_tentative_half() {
    let config = CalibratedWeightConfig::default();
    let event = make_event(
        "E",
        "s",
        None,
        0.5,
        0.8,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment {
        source_count: 1,
        ..Default::default()
    };
    // salience_cal = 0.8 (no boosts)
    // confidence_factor = 0.5 (tentative)
    // situation_multiplier = 1.0
    // source_support = min(1.0, 1/3) = 0.333...
    // w = 0.8 * 0.5 * 1.0 * 0.333... = 0.1333...
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    assert!((w - 0.8 * 0.5 * (1.0 / 3.0)).abs() < 1e-6);
}

#[test]
fn compute_calibrated_weight_discarded_zero() {
    let config = CalibratedWeightConfig::default();
    let event = make_event(
        "E",
        "s",
        None,
        0.3,
        0.8,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment::default();
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    assert!((w - 0.0).abs() < 1e-10);
}

#[test]
fn compute_calibrated_weight_weak_situation_boost() {
    let config = CalibratedWeightConfig::default();
    let event = make_event_with_situation(
        "E",
        "s",
        None,
        0.9,
        0.8,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
        Some(2),
    );
    let enrichment = EventEnrichment {
        source_count: 1,
        ..Default::default()
    };
    // salience_cal = 0.8, conf_factor=1.0, sit_mult=1.5, source=1/3=0.333
    // w = 0.8 * 1.0 * 1.5 * 0.333 = 0.4
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    assert!((w - 0.8 * 1.5 / 3.0).abs() < 1e-6);
}

#[test]
fn compute_calibrated_weight_strong_situation_dampen() {
    let config = CalibratedWeightConfig::default();
    let event = make_event_with_situation(
        "E",
        "s",
        None,
        0.9,
        0.8,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
        Some(5),
    );
    let enrichment = EventEnrichment {
        source_count: 3,
        ..Default::default()
    };
    // salience_cal = 0.8, conf_factor=1.0, sit_mult=0.5, source=1.0
    // w = 0.8 * 0.5 = 0.4
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    assert!((w - 0.4).abs() < 1e-6);
}

#[test]
fn compute_calibrated_weight_full_source_support() {
    let config = CalibratedWeightConfig::default();
    let event = make_event(
        "E",
        "s",
        None,
        0.9,
        1.0,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment {
        source_count: 5, // > min_sources_for_full_support (3)
        ..Default::default()
    };
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    // source_support = 1.0 (capped)
    assert!(w > 0.9);
}

#[test]
fn compute_simple_weight_vs_calibrated() {
    // 对比: 简单权重 vs 校准权重（无加成时）
    let config = CalibratedWeightConfig::default();
    let event = make_event(
        "E",
        "s",
        None,
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment::default();

    let simple = compute_simple_weight(&event);
    let calibrated = compute_calibrated_weight(&event, &enrichment, &config);

    // simple = 0.8 * 1.0 = 0.8
    assert!((simple - 0.8).abs() < 1e-10);

    // calibrated 应该与简单权重有差异（因为 source_support < 1.0）
    assert!(
        calibrated < simple,
        "校准权重应因 source_support < 1.0 而降低"
    );
}
