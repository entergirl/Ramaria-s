//! crates/ramaria-memory/src/inference/stats/tests/group.rs - 分组与权重归一化
//!
//! 设计特点:
//! - 由 父测试模块 以 mod group; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 分组 + 权重归一化
// =========================================================

#[test]
fn group_by_category_works() {
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
        make_event(
            "E3",
            "s3",
            Some("工作"),
            0.7,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let grouped = group_by_category(&events);
    assert_eq!(grouped.len(), 2);
    let social_group = grouped.iter().find(|(k, _)| k == "社交").unwrap();
    assert_eq!(social_group.1.len(), 1);
    let work_group = grouped.iter().find(|(k, _)| k == "工作").unwrap();
    assert_eq!(work_group.1.len(), 2);
}

#[test]
fn group_weights_normalize() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作"),
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
            Some("社交"),
            0.8,
            0.6,
            0.3,
            0.5,
            Presentation::Subjective,
            None,
        ),
    ];
    let grouped = group_by_category(&events);
    let mut cats: Vec<CategoryStats> = grouped
        .iter()
        .map(|(cat, evts)| compute_category_stats(cat, evts, None, &config))
        .collect();
    normalize_group_weights(&mut cats);
    let total: f64 = cats.iter().map(|c| c.group_weight).sum();
    assert!(
        (total - 1.0).abs() < 1e-10,
        "权重应归一化为和为1，实际为{}",
        total
    );
}
