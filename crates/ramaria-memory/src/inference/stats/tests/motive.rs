//! crates/ramaria-memory/src/inference/stats/tests/motive.rs - 动机维度统计
//!
//! 设计特点:
//! - 由 父测试模块 以 mod motive; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 动机维度统计（MotivesStats）
// =========================================================

/// 构造带 motives 字段的测试事件。
#[allow(clippy::too_many_arguments)]
fn make_event_with_motives(
    title: &str,
    summary: &str,
    keywords: Option<&str>,
    confidence: f64,
    salience: f64,
    valence: f64,
    share: f64,
    presentation: Presentation,
    attitude: Option<&str>,
    motives: Option<&str>,
) -> MemoryEvent {
    let now = now_ms();
    let mut ev = MemoryEvent::new(
        "test-persona".into(),
        title.into(),
        summary.into(),
        now - 1000,
        now,
    );
    ev.keywords = keywords.map(|k| k.into());
    ev.confidence = confidence;
    ev.salience = salience;
    ev.valence = valence;
    ev.share = share;
    ev.presentation = presentation;
    ev.attitude = attitude.map(|a| a.into());
    ev.motives = motives.map(|m| m.into());
    ev.situation_strength = Some(3);
    ev
}

/// extract_motive_tags 各分支参数化验证：多标签 / None / 纯空白 / 单标签 / 去除空白。
#[test]
fn extract_motive_tags_cases() {
    // 逗号分隔多标签
    let event = make_event_with_motives(
        "E1",
        "s",
        Some("工作"),
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
        Some("地位维护,自主性,归属"),
    );
    let tags = extract_motive_tags(&event);
    assert_eq!(tags.len(), 3);
    assert_eq!(tags[0], "地位维护");
    assert_eq!(tags[1], "自主性");
    assert_eq!(tags[2], "归属");
    // None → 空
    let event = make_event_with_motives(
        "E2",
        "s",
        Some("社交"),
        0.8,
        0.6,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
        None,
    );
    assert!(extract_motive_tags(&event).is_empty());
    // 纯空白 → 空
    let event = make_event_with_motives(
        "E3",
        "s",
        Some("社交"),
        0.8,
        0.6,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
        Some("  ,  ,  "),
    );
    assert!(extract_motive_tags(&event).is_empty());
    // 单标签
    let event = make_event_with_motives(
        "E4",
        "s",
        Some("工作"),
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
        Some("自主性"),
    );
    let tags = extract_motive_tags(&event);
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0], "自主性");
    // 去除首尾空白
    let event = make_event_with_motives(
        "E5",
        "s",
        Some("工作"),
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
        Some(" 地位维护 , 自主性 ,  归属 "),
    );
    let tags = extract_motive_tags(&event);
    assert_eq!(tags.len(), 3);
    assert_eq!(tags[0], "地位维护");
    assert_eq!(tags[1], "自主性");
    assert_eq!(tags[2], "归属");
}

#[test]
fn group_by_motive_handles_multi_tag_events() {
    let e1 = make_event_with_motives(
        "E1",
        "s",
        Some("工作"),
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
        Some("地位维护,自主性"),
    );
    let e2 = make_event_with_motives(
        "E2",
        "s",
        Some("社交"),
        0.8,
        0.6,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
        Some("归属"),
    );
    let e3 = make_event_with_motives(
        "E3",
        "s",
        Some("工作"),
        0.7,
        0.7,
        0.3,
        0.5,
        Presentation::Mixed,
        None,
        Some("地位维护"),
    );
    let events = vec![e1, e2, e3];
    let grouped = group_by_motive(&events);

    // 应该有 3 个动机标签: 地位维护, 自主性, 归属
    assert_eq!(grouped.len(), 3);

    // 地位维护应该有 2 个事件 (E1, E3)
    let status_group: Vec<_> = grouped.iter().filter(|(k, _)| k == "地位维护").collect();
    assert_eq!(status_group.len(), 1);
    assert_eq!(status_group[0].1.len(), 2);

    // 自主性应该有 1 个事件 (E1)
    let autonomy_group: Vec<_> = grouped.iter().filter(|(k, _)| k == "自主性").collect();
    assert_eq!(autonomy_group.len(), 1);
    assert_eq!(autonomy_group[0].1.len(), 1);

    // 归属应该有 1 个事件 (E2)
    let belonging_group: Vec<_> = grouped.iter().filter(|(k, _)| k == "归属").collect();
    assert_eq!(belonging_group.len(), 1);
    assert_eq!(belonging_group[0].1.len(), 1);
}

#[test]
fn group_by_motive_empty_when_no_motives() {
    let e1 = make_event_with_motives(
        "E1",
        "s",
        Some("工作"),
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
        None,
    );
    let e2 = make_event_with_motives(
        "E2",
        "s",
        Some("社交"),
        0.8,
        0.6,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
        None,
    );
    let grouped = group_by_motive(&[e1, e2]);
    assert!(grouped.is_empty());
}

#[test]
fn compute_motive_stats_basic() {
    let events = vec![
        make_event_with_motives(
            "E1",
            "s1",
            Some("工作"),
            0.9,
            0.8,
            0.6,
            0.5,
            Presentation::Mixed,
            None,
            Some("地位维护,自主性"),
        ),
        make_event_with_motives(
            "E2",
            "s2",
            Some("社交"),
            0.8,
            0.6,
            -0.3,
            0.7,
            Presentation::Subjective,
            None,
            Some("归属"),
        ),
        make_event_with_motives(
            "E3",
            "s3",
            Some("工作"),
            0.7,
            0.7,
            0.2,
            0.4,
            Presentation::Objective,
            None,
            Some("地位维护,公平"),
        ),
    ];

    let enrichments = EventEnrichment::derive_batch(&events);
    let config = CalibratedWeightConfig::default();
    let stats = compute_motive_stats(&events, &enrichments, &config);

    // 应该有 4 个动机标签: 地位维护, 自主性, 归属, 公平
    assert_eq!(stats.len(), 4);

    // 按 n_eff 降序排列，地位维护应该排第一（2个事件）
    assert_eq!(stats[0].motive, "地位维护");
    assert_eq!(stats[0].event_count, 2);
    assert!(stats[0].n_eff > 0.0);
    assert!(stats[0].valence_mean > 0.0); // 两个事件都正值

    // 归属只有1个事件，负效价
    let belonging = stats.iter().find(|s| s.motive == "归属").unwrap();
    assert_eq!(belonging.event_count, 1);
    assert!(belonging.valence_mean < 0.0);
    assert!(belonging.valence_positive_ratio < 0.5);
}

/// compute_motive_stats 空结果各分支参数化验证：事件无 motives / 空事件列表。
#[test]
fn compute_motive_stats_empty_cases() {
    // 事件存在但无 motives → 空
    let events = vec![make_event_with_motives(
        "E1",
        "s",
        Some("工作"),
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
        None,
    )];
    let enrichments = EventEnrichment::derive_batch(&events);
    let config = CalibratedWeightConfig::default();
    let stats = compute_motive_stats(&events, &enrichments, &config);
    assert!(stats.is_empty());
    // 空事件列表 → 空
    let enrichments: Vec<EventEnrichment> = Vec::new();
    let stats = compute_motive_stats(&[], &enrichments, &config);
    assert!(stats.is_empty());
}
