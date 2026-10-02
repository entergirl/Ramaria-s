//! crates/ramaria-memory/src/inference/stats/tests/batch.rs - 批量权重计算
//!
//! 设计特点:
//! - 由 父测试模块 以 mod batch; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 批量权重计算
// =========================================================

#[test]
fn test_compute_calibrated_weights_batch() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event(
            "E1",
            "s1",
            None,
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
        ),
        make_event(
            "E2",
            "s2",
            None,
            0.5,
            0.6,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let enrichments = vec![
        EventEnrichment::from_event(&events[0]),
        EventEnrichment::from_event(&events[1]),
    ];
    let weights = compute_calibrated_weights_batch(&events, &enrichments, &config);
    assert_eq!(weights.len(), 2);
    // E1 (confirmed) 应比 E2 (tentative) 权重更高
    assert!(
        weights[0] > weights[1],
        "confirmed 事件权重应高于 tentative"
    );
}

#[test]
#[should_panic(expected = "events 与 enrichments 长度必须一致")]
fn test_compute_calibrated_weights_batch_mismatch() {
    let config = CalibratedWeightConfig::default();
    let events = vec![make_event(
        "E1",
        "s",
        None,
        0.9,
        0.8,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    )];
    let enrichments = vec![EventEnrichment::default(), EventEnrichment::default()];
    compute_calibrated_weights_batch(&events, &enrichments, &config);
}
