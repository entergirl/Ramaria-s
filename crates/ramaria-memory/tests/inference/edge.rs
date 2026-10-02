//! tests/inference/edge.rs - 边界情况
//!
//! 设计特点:
//! - 由 tests/inference.rs 以 mod edge; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 边界情况测试
// =========================================================

#[test]
fn phase_a_empty_events_all_motive_stats_empty() {
    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&[], &config);
    assert!(summary.motive_stats.is_empty());
    assert_eq!(summary.total_events_in, 0);
    assert_eq!(summary.confirmed_count, 0);
    assert_eq!(summary.tentative_count, 0);
}

#[test]
fn phase_a_all_discarded_yields_empty_stats() {
    let events = vec![
        make_event(
            "D1",
            "s",
            Some("工作"),
            0.3,
            0.1,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            Some("地位维护"),
            Some(3),
        ),
        make_event(
            "D2",
            "s",
            Some("社交"),
            0.2,
            0.1,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            Some("归属"),
            Some(3),
        ),
    ];
    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&events, &config);

    assert_eq!(summary.total_events_in, 2);
    assert_eq!(summary.discarded_count, 2);
    assert_eq!(summary.total_events_filtered, 0);
    assert!(
        summary.motive_stats.is_empty(),
        "全 discarded 不应产生活跃动机统计"
    );
}
