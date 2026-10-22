//! crates/ramaria-core/src/behavior/tests.rs - 行为层类型单元测试
//!
//! 设计特点:
//! - 覆盖规则来源、目标类型与反馈信号的序列化形态
//! - 覆盖情境 / 参数 / 证据结构的 serde 往返与默认值
//! - 断言证据 JSON 不含任何原文内容

use super::*;

#[test]
fn rule_source_serde() {
    assert_eq!(
        serde_json::to_string(&RuleSource::Auto).unwrap(),
        r#""auto""#
    );
    assert_eq!(
        serde_json::to_string(&RuleSource::Manual).unwrap(),
        r#""manual""#
    );
    assert_eq!(RuleSource::Auto.as_str(), "auto");
    assert_eq!(RuleSource::Manual.as_str(), "manual");
}

#[test]
fn target_type_serde_snake_case() {
    assert_eq!(
        serde_json::to_string(&TargetType::BehaviorRule).unwrap(),
        r#""behavior_rule""#
    );
    assert_eq!(
        serde_json::to_string(&TargetType::PersonaFact).unwrap(),
        r#""persona_fact""#
    );
    assert_eq!(
        serde_json::to_string(&TargetType::PersonalityTrait).unwrap(),
        r#""personality_trait""#
    );
}

#[test]
fn signal_type_strong_detection() {
    assert!(SignalType::Edit.is_strong());
    assert!(SignalType::Disable.is_strong());
    assert!(!SignalType::Correction.is_strong());
    assert!(!SignalType::Continue.is_strong());
}

#[test]
fn feedback_log_s1_weight_is_1_0() {
    let log = FeedbackLog::new(
        "char-0001",
        TargetType::BehaviorRule,
        "3",
        SignalType::Disable,
        Some("sess-1".into()),
        None,
    );
    assert_eq!(log.weight, 1.0);
    assert!(log.id == 0);
    assert!(log.created_at > 0);
}

#[test]
fn feedback_weight_grading_matches_signal_levels() {
    // D-V17-007 weight 修正：S1=1.0 / S2(Correction)=0.6 / S3(Continue)=0.2
    assert_eq!(SignalType::Edit.weight(), 1.0);
    assert_eq!(SignalType::Disable.weight(), 1.0);
    assert_eq!(SignalType::Correction.weight(), 0.6);
    assert_eq!(SignalType::Continue.weight(), 0.2);
    // S2 复审权重 > S3 趋势权重（消费方语义）
    assert!(SignalType::Correction.weight() > SignalType::Continue.weight());
}

#[test]
fn feedback_log_s2_s3_weights_applied() {
    // S2 纠正（Correction）→ weight 0.6
    let s2 = FeedbackLog::new(
        "char-0001",
        TargetType::BehaviorRule,
        "3",
        SignalType::Correction,
        None,
        None,
    );
    assert!((s2.weight - 0.6).abs() < f64::EPSILON);
    // S3 继续（Continue）→ weight 0.2
    let s3 = FeedbackLog::new(
        "char-0001",
        TargetType::BehaviorRule,
        "3",
        SignalType::Continue,
        None,
        None,
    );
    assert!((s3.weight - 0.2).abs() < f64::EPSILON);
}

#[test]
fn behavior_params_default_clamped_range() {
    let p = BehaviorParams::default();
    assert_eq!(p.emotional_intensity, 0.0);
    assert!((0.0..=1.0).contains(&p.proactiveness));
    assert!((0.0..=1.0).contains(&p.detail_level));
    assert!((0.0..=1.0).contains(&p.formality));
}

#[test]
fn behavior_rule_roundtrip_json() {
    let rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation {
            keywords: vec!["加班".into(), "累".into()],
            centroid: Some(vec![0.1, 0.2, 0.3]),
            response_centroid: None,
            valence_mean: -0.4,
            valence_std: 0.2,
            sample_count: 6,
            presentation_dist: vec![PresentationFreq {
                presentation: Presentation::Subjective,
                freq: 0.8,
            }],
            situation_strength_mean: 3.5,
            time_span_days: 20.0,
            trait_refs: vec![],
        },
        Some("当聊到加班时，倾向表达疲惫并安慰对方。".into()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    let json = serde_json::to_string(&rule).unwrap();
    let back: BehaviorRule = serde_json::from_str(&json).unwrap();
    assert_eq!(rule, back);
    assert!(back.has_reaction());
    assert!(!back.is_candidate());
}

#[test]
fn candidate_rule_has_no_reaction() {
    let rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation::empty(),
        None,
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    assert!(rule.is_candidate());
    assert!(!rule.has_reaction());
}

#[test]
fn evidence_chain_is_id_and_weight_only() {
    let ev = BehaviorEvidence {
        event_id: 42,
        weight: 0.8,
    };
    let json = serde_json::to_string(&ev).unwrap();
    let back: BehaviorEvidence = serde_json::from_str(&json).unwrap();
    assert_eq!(ev, back);
    assert!(!json.contains("原文"), "证据 JSON 不应含任何原文内容");
}
