//! crates/ramaria-memory/src/inference/stats/tests/enrichment.rs - EventEnrichment 派生
//!
//! 设计特点:
//! - 由 父测试模块 以 mod enrichment; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// EventEnrichment 派生
// =========================================================

#[test]
fn enrichment_from_event() {
    let event = make_event(
        "E",
        "s",
        None,
        0.9,
        0.8,
        -0.5,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment::from_event(&event);
    assert!((enrichment.emotional_intensity - 0.5).abs() < 1e-10);
    assert!((enrichment.mention_frequency - 0.8).abs() < 1e-10);
    assert_eq!(enrichment.source_count, 1);
}

#[test]
fn enrichment_derive_batch_recurrence() {
    let events = vec![
        make_event(
            "E1",
            "s",
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
            "s",
            Some("工作"),
            0.9,
            0.6,
            -0.2,
            0.3,
            Presentation::Subjective,
            None,
        ),
        make_event(
            "E3",
            "s",
            Some("社交"),
            0.8,
            0.5,
            0.6,
            0.9,
            Presentation::Mixed,
            None,
        ),
    ];
    let enrichments = EventEnrichment::derive_batch(&events);
    assert_eq!(enrichments.len(), 3);

    // 工作 ×2, 社交 ×1 → max=2
    // 工作 recurrence = 2/2 = 1.0
    assert!((enrichments[0].topic_recurrence_count - 1.0).abs() < 1e-10);
    assert!((enrichments[1].topic_recurrence_count - 1.0).abs() < 1e-10);
    // 社交 recurrence = 1/2 = 0.5
    assert!((enrichments[2].topic_recurrence_count - 0.5).abs() < 1e-10);
}

#[test]
fn enrichment_derive_batch_empty() {
    let enrichments = EventEnrichment::derive_batch(&[]);
    assert!(enrichments.is_empty());
}
