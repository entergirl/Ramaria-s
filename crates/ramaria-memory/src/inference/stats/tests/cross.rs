//! crates/ramaria-memory/src/inference/stats/tests/cross.rs - 跨分类指标（校准权重）
//!
//! 设计特点:
//! - 由 父测试模块 以 mod cross; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 跨分类指标（校准权重路径）
// =========================================================

#[test]
fn cross_category_metrics_with_calibrated_weights() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作"),
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
            Some("社交"),
            0.8,
            0.6,
            -0.3,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let enrichments = EventEnrichment::derive_batch(&events);
    let cat_cfg = CalibratedWeightConfig::default();
    let grouped = group_by_category(&events);
    let mut cats: Vec<CategoryStats> = grouped
        .iter()
        .map(|(cat, evts)| {
            let cat_enr: Vec<EventEnrichment> = evts
                .iter()
                .map(|e| {
                    let idx = events.iter().position(|ae| ae.id == e.id).unwrap_or(0);
                    enrichments.get(idx).cloned().unwrap_or_default()
                })
                .collect();
            compute_category_stats(cat, evts, Some(&cat_enr), &cat_cfg)
        })
        .collect();
    normalize_group_weights(&mut cats);

    let metrics = compute_cross_category_metrics(&events, &cats, Some(&enrichments), &config);
    assert!(metrics.emotional_stability >= 0.0);
    assert!(metrics.narrative_consistency >= 0.0);
}
