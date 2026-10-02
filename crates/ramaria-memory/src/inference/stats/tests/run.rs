//! crates/ramaria-memory/src/inference/stats/tests/run.rs - 完整管线三轨与校准
//!
//! 设计特点:
//! - 由 父测试模块 以 mod run; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 完整管线: 三轨 + 校准权重
// =========================================================

#[test]
fn run_phase_a_stats_v13_calibrated() {
    let config = StatsConfig::default(); // use_calibrated_weights = true
    let events = vec![
        make_event(
            "E1",
            "工作会议摘要",
            Some("工作,会议"),
            0.9,
            0.8,
            0.7,
            0.8,
            Presentation::Objective,
            Some("对成果满意"),
        ),
        make_event(
            "E2",
            "低置信事件",
            Some("工作,闲聊"),
            0.3,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
        make_event(
            "E3",
            "社交聚会摘要",
            Some("社交,聚会"),
            0.8,
            0.7,
            0.5,
            0.9,
            Presentation::Subjective,
            Some("聚会很愉快"),
        ),
        make_event(
            "E4",
            "家庭事件摘要",
            Some("家庭,晚餐"),
            0.7,
            0.6,
            -0.2,
            0.3,
            Presentation::Mixed,
            Some("家庭小摩擦"),
        ),
    ];
    let summary = run_phase_a_stats(&events, &config);

    assert_eq!(summary.total_events_in, 4);
    // E2 被 discarded (conf=0.3)，其余 3 条为 active
    assert_eq!(summary.total_events_filtered, 3);
    assert_eq!(summary.discarded_count, 1);
    assert!(summary.confirmed_count > 0);
    assert_eq!(summary.category_count, 3); // 工作、社交、家庭
    assert!(!summary.categories.is_empty());
    // 分类按 group_weight 降序
    assert!(summary.categories[0].group_weight >= summary.categories.last().unwrap().group_weight);
    assert!(summary.cross_category.emotional_stability >= 0.0);
    assert!(!summary.representative_events.is_empty());
}

#[test]
fn run_phase_a_stats_v13_includes_tentative() {
    let config = StatsConfig::default();
    // 一个 tentative 事件（conf=0.5）+ 一个 confirmed 事件
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
            "待定事件",
            Some("工作"),
            0.5,
            0.6,
            -0.3,
            0.3,
            Presentation::Subjective,
            None,
        ),
    ];
    let summary = run_phase_a_stats(&events, &config);

    assert_eq!(summary.total_events_in, 2);
    // tentative 事件应在 active 中（以半权重参与统计）
    assert_eq!(summary.total_events_filtered, 2);
    assert_eq!(summary.confirmed_count, 1);
    assert_eq!(summary.tentative_count, 1);
    assert_eq!(summary.discarded_count, 0);
    assert_eq!(summary.category_count, 1);
}

#[test]
fn run_phase_a_stats_v13_all_discarded() {
    let config = StatsConfig::default();
    let events = vec![
        make_event(
            "E1",
            "低置信",
            Some("工作"),
            0.3,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
        make_event(
            "E2",
            "也低置信",
            Some("社交"),
            0.2,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let summary = run_phase_a_stats(&events, &config);
    assert_eq!(summary.total_events_in, 2);
    assert_eq!(summary.total_events_filtered, 0);
    assert_eq!(summary.discarded_count, 2);
    assert_eq!(summary.category_count, 0);
    assert!(summary.categories.is_empty());
}

#[test]
fn run_phase_a_stats_v13_empty_input() {
    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&[], &config);
    assert_eq!(summary.total_events_in, 0);
    assert_eq!(summary.total_events_filtered, 0);
}

#[test]
fn run_phase_a_stats_v12_compat_path() {
    // 使用 use_calibrated_weights=false 回退到旧行为
    let config = StatsConfig {
        use_calibrated_weights: false,
        ..Default::default()
    };
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作,会议"),
            0.9,
            0.8,
            0.7,
            0.8,
            Presentation::Objective,
            Some("满意"),
        ),
        make_event(
            "E2",
            "低置信事件",
            Some("工作,闲聊"),
            0.3,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
        make_event(
            "E3",
            "s3",
            Some("社交,聚会"),
            0.8,
            0.7,
            0.5,
            0.9,
            Presentation::Subjective,
            Some("愉快"),
        ),
        make_event(
            "E4",
            "s4",
            Some("家庭,晚餐"),
            0.7,
            0.6,
            -0.2,
            0.3,
            Presentation::Mixed,
            Some("小摩擦"),
        ),
    ];
    let summary = run_phase_a_stats(&events, &config);

    assert_eq!(summary.total_events_in, 4);
    // 旧路径: E2 被硬截断排除
    assert_eq!(summary.total_events_filtered, 3);
    assert_eq!(summary.category_count, 3);
    // 旧路径将所有通过的事件视为 confirmed
    assert!(summary.confirmed_count > 0);
    assert_eq!(summary.tentative_count, 0);
}
