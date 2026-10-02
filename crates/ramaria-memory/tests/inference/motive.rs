//! tests/inference/motive.rs - 动机维度统计
//!
//! 设计特点:
//! - 由 tests/inference.rs 以 mod motive; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 动机维度统计测试
// =========================================================

#[test]
fn motive_stats_computed_from_events() {
    let events = make_diverse_events();
    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&events, &config);

    // 动机统计应在 Phase A 输出中
    assert!(
        !summary.motive_stats.is_empty(),
        "fixture 包含多种动机，应有动机统计输出"
    );

    // 地位维护动机出现次数最多
    let status_motive = summary.motive_stats.iter().find(|m| m.motive == "地位维护");
    assert!(status_motive.is_some(), "应有'地位维护'动机统计");
    let status = status_motive.unwrap();
    assert!(status.event_count >= 3, "地位维护至少出现 3 次");
    assert!(status.n_eff > 0.0);
    // 地位维护主要是负效价
    assert!(status.valence_mean < 0.0, "地位维护应主要为负效价");

    // 归属动机应正向
    let belonging = summary.motive_stats.iter().find(|m| m.motive == "归属");
    assert!(belonging.is_some(), "应有'归属'动机统计");
    assert!(belonging.unwrap().valence_mean > 0.0, "归属应为正效价");
}

#[test]
fn motive_stats_empty_when_no_motives() {
    // 全无 motives 事件
    let events: Vec<MemoryEvent> = (0..3)
        .map(|i| {
            make_event(
                &format!("E{}", i),
                "s",
                Some("工作"),
                0.8,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
                None,
                Some(3),
            )
        })
        .collect();

    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&events, &config);
    assert!(
        summary.motive_stats.is_empty(),
        "无 motives 时动机统计应为空"
    );
}

#[test]
fn motive_stats_only_from_active_events() {
    // 确保 discarded 事件不参与动机统计
    let events = vec![
        make_event(
            "Discarded",
            "被丢弃",
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
            "Confirmed",
            "确认的",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            Some("好"),
            Some("自主性"),
            Some(3),
        ),
    ];

    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&events, &config);

    // discarded 事件 (conf=0.3) 不应参与动机统计
    let status_motive = summary.motive_stats.iter().find(|m| m.motive == "地位维护");
    assert!(status_motive.is_none(), "discarded 事件不应参与动机统计");
}
