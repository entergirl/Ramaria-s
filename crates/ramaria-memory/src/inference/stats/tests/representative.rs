//! crates/ramaria-memory/src/inference/stats/tests/representative.rs - 代表性事件选取
//!
//! 设计特点:
//! - 由 父测试模块 以 mod representative; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 代表性事件选取
// =========================================================

#[test]
fn representative_events_limit() {
    let config = StatsConfig {
        max_representative_events: 2,
        ..Default::default()
    };
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作"),
            0.9,
            0.9,
            0.5,
            0.5,
            Presentation::Mixed,
            Some("态度1"),
        ),
        make_event(
            "E2",
            "s2",
            Some("工作"),
            0.9,
            0.5,
            0.3,
            0.5,
            Presentation::Mixed,
            Some("态度2"),
        ),
        make_event(
            "E3",
            "s3",
            Some("工作"),
            0.9,
            0.8,
            0.6,
            0.5,
            Presentation::Mixed,
            Some("态度3"),
        ),
        make_event(
            "E4",
            "s4",
            Some("工作"),
            0.9,
            0.3,
            0.1,
            0.5,
            Presentation::Mixed,
            Some("态度4"),
        ),
    ];
    let cfg = CalibratedWeightConfig::default();
    let grouped = group_by_category(&events);
    let cats: Vec<CategoryStats> = grouped
        .iter()
        .map(|(cat, evts)| compute_category_stats(cat, evts, None, &cfg))
        .collect();
    let representatives = select_representative_events(&events, &cats, &config);
    assert_eq!(representatives.len(), 2, "最多 2 条");
    let saliences: Vec<f64> = representatives.iter().map(|r| r.salience).collect();
    assert!(saliences.contains(&0.9));
    assert!(saliences.contains(&0.8));
}

#[test]
fn representative_events_preserves_attitude_original() {
    let config = StatsConfig::default();
    let events = vec![make_event(
        "E1",
        "摘要",
        Some("工作"),
        0.9,
        0.9,
        0.5,
        0.5,
        Presentation::Mixed,
        Some("对项目进展感到满意"),
    )];
    let cfg = CalibratedWeightConfig::default();
    let grouped = group_by_category(&events);
    let cats: Vec<CategoryStats> = grouped
        .iter()
        .map(|(cat, evts)| compute_category_stats(cat, evts, None, &cfg))
        .collect();
    let reps = select_representative_events(&events, &cats, &config);
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0].attitude.as_deref(), Some("对项目进展感到满意"));
}
