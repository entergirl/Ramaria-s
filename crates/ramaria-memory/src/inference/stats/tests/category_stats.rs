//! crates/ramaria-memory/src/inference/stats/tests/category_stats.rs - 单分类统计
//!
//! 设计特点:
//! - 由 父测试模块 以 mod category_stats; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 单分类统计（校准权重 + 向后兼容）
// =========================================================

#[test]
fn category_stats_with_calibrated_weights() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作,会议"),
            0.9,
            0.8,
            0.5,
            0.7,
            Presentation::Objective,
            Some("满意"),
        ),
        make_event(
            "E2",
            "s2",
            Some("工作,项目"),
            0.8,
            0.6,
            -0.3,
            0.4,
            Presentation::Subjective,
            Some("焦虑"),
        ),
        make_event(
            "E3",
            "s3",
            Some("工作,汇报"),
            0.7,
            0.9,
            0.2,
            0.6,
            Presentation::Mixed,
            Some("一般"),
        ),
    ];
    let enrichments = EventEnrichment::derive_batch(&events);
    let stats = compute_category_stats("工作", &events, Some(&enrichments), &config);

    assert_eq!(stats.category, "工作");
    assert_eq!(stats.event_count, 3);
    // n_eff 应该由于校准权重而略小于简单加权（source_support < 1.0）
    let simple_stats = compute_category_stats("工作", &events, None, &config);
    assert!(
        stats.n_eff < simple_stats.n_eff,
        "校准 n_eff({}) 应小于简单加权 n_eff({})",
        stats.n_eff,
        simple_stats.n_eff
    );
    assert!(stats.n_eff > 0.0, "n_eff 应大于 0");
}

#[test]
fn category_stats_with_simple_weights_backward_compat() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作,会议"),
            0.9,
            0.8,
            0.5,
            0.7,
            Presentation::Objective,
            Some("满意"),
        ),
        make_event(
            "E2",
            "s2",
            Some("工作,项目"),
            0.8,
            0.6,
            -0.3,
            0.4,
            Presentation::Subjective,
            Some("焦虑"),
        ),
        make_event(
            "E3",
            "s3",
            Some("工作,汇报"),
            0.7,
            0.9,
            0.2,
            0.6,
            Presentation::Mixed,
            Some("一般"),
        ),
    ];
    // None enrichments → 简单权重
    let stats = compute_category_stats("工作", &events, None, &config);

    assert_eq!(stats.category, "工作");
    assert_eq!(stats.event_count, 3);
    // n_eff = 0.8 + 0.6 + 0.9 = 2.3
    assert!((stats.n_eff - 2.3).abs() < 1e-10);
}

#[test]
fn category_stats_single_event() {
    let config = CalibratedWeightConfig::default();
    let events = vec![make_event(
        "E1",
        "摘要",
        Some("家庭"),
        0.9,
        0.5,
        0.8,
        0.6,
        Presentation::Subjective,
        None,
    )];
    let stats = compute_category_stats("家庭", &events, None, &config);
    assert_eq!(stats.event_count, 1);
    assert!((stats.n_eff - 0.5).abs() < 1e-10);
    assert!((stats.valence_mean - 0.8).abs() < 1e-10);
    assert!((stats.valence_std - 0.0).abs() < 1e-10);
    assert!((stats.presentation_subjective_ratio - 1.0).abs() < 1e-10);
}

#[test]
fn category_stats_respects_situation_multiplier() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event_with_situation(
            "弱情境事件",
            "摘要",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
            Some(2),
        ),
        make_event_with_situation(
            "强情境事件",
            "摘要",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
            Some(5),
        ),
    ];
    let stats = compute_category_stats("工作", &events, None, &config);
    // 简单权重: n_eff = 0.8*1.5 + 0.8*0.5 = 1.2 + 0.4 = 1.6
    assert!((stats.n_eff - 1.6).abs() < 1e-10);
}
