//! crates/ramaria-memory/src/inference/stats/tests/classify.rs - 准入轨道分类
//!
//! 设计特点:
//! - 由 父测试模块 以 mod classify; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 准入轨道分类
// =========================================================

/// classify_event 各置信度分支参数化验证（含边界值与 NaN/负值防御）。
#[test]
fn classify_event_cases() {
    let cases = [
        (0.9, AdmissionTrack::Confirmed),
        (0.6, AdmissionTrack::Confirmed), // 边界值
        (0.5, AdmissionTrack::Tentative),
        (0.45, AdmissionTrack::Tentative),   // 边界值
        (0.5999, AdmissionTrack::Tentative), // 刚好低于 confirmed
        (0.3, AdmissionTrack::Discarded),
        (0.4499, AdmissionTrack::Discarded), // 刚好低于 tentative
        (f64::NAN, AdmissionTrack::Discarded), // NaN 防御
        (-0.1, AdmissionTrack::Discarded),   // 负值防御
    ];
    for (confidence, expected) in cases {
        let ev = make_event(
            "E",
            "s",
            None,
            confidence,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        assert_eq!(classify_event(&ev), expected, "confidence={confidence}");
    }
}

#[test]
fn classify_events_mixed() {
    let events = vec![
        make_event(
            "E1",
            "s",
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
            "s",
            None,
            0.5,
            0.6,
            -0.2,
            0.3,
            Presentation::Subjective,
            None,
        ),
        make_event(
            "E3",
            "s",
            None,
            0.3,
            0.5,
            0.6,
            0.9,
            Presentation::Mixed,
            None,
        ),
    ];
    let classified = classify_events(&events);
    assert_eq!(classified.confirmed.len(), 1);
    assert_eq!(classified.tentative.len(), 1);
    assert_eq!(classified.discarded_count, 1);
    assert_eq!(classified.active_count(), 2);
}

#[test]
fn classify_events_empty() {
    let classified = classify_events(&[]);
    assert_eq!(classified.confirmed.len(), 0);
    assert_eq!(classified.tentative.len(), 0);
    assert_eq!(classified.discarded_count, 0);
}

#[test]
fn admission_track_confidence_factor() {
    assert!((AdmissionTrack::Confirmed.confidence_factor(0.5) - 1.0).abs() < 1e-10);
    assert!((AdmissionTrack::Tentative.confidence_factor(0.5) - 0.5).abs() < 1e-10);
    assert!((AdmissionTrack::Discarded.confidence_factor(0.5) - 0.0).abs() < 1e-10);

    // 自定义 tentative_factor
    assert!((AdmissionTrack::Tentative.confidence_factor(0.3) - 0.3).abs() < 1e-10);
}

#[test]
fn admission_track_as_str() {
    assert_eq!(AdmissionTrack::Confirmed.as_str(), "confirmed");
    assert_eq!(AdmissionTrack::Tentative.as_str(), "tentative");
    assert_eq!(AdmissionTrack::Discarded.as_str(), "discarded");
}
