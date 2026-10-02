//! crates/ramaria-memory/src/inference/stats/tests/prefilter.rs - prefilter_events 兼容
//!
//! 设计特点:
//! - 由 父测试模块 以 mod prefilter; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 向后兼容: prefilter_events
// =========================================================

#[test]
fn prefilter_excludes_low_confidence() {
    let config = StatsConfig::default();
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
            None,
        ),
        make_event(
            "E2",
            "s2",
            Some("社交,聚会"),
            0.3,
            0.6,
            -0.2,
            0.3,
            Presentation::Subjective,
            None,
        ),
        make_event(
            "E3",
            "s3",
            Some("工作,项目"),
            0.8,
            0.5,
            0.6,
            0.9,
            Presentation::Mixed,
            None,
        ),
    ];
    let (filtered, excluded) = prefilter_events(&events, &config);
    assert_eq!(excluded, 1, "应排除 1 条低置信度事件");
    assert_eq!(filtered.len(), 2);
    let titles: Vec<&str> = filtered.iter().map(|e| e.title.as_str()).collect();
    assert!(titles.contains(&"E1"));
    assert!(titles.contains(&"E3"));
    assert!(!titles.contains(&"E2"));
}

#[test]
fn prefilter_all_pass_when_high_confidence() {
    let config = StatsConfig::default();
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
            Some("社交"),
            0.8,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let (filtered, excluded) = prefilter_events(&events, &config);
    assert_eq!(excluded, 0);
    assert_eq!(filtered.len(), 2);
}

#[test]
fn prefilter_empty_input() {
    let config = StatsConfig::default();
    let (filtered, excluded) = prefilter_events(&[], &config);
    assert_eq!(excluded, 0);
    assert!(filtered.is_empty());
}
