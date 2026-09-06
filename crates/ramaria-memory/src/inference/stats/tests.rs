//! crates/ramaria-memory/src/inference/stats/tests.rs - Ramaria 统计特征提取全模块单元测试
//!
//! 设计特点:
//! - 由 stats/mod.rs 以 #[cfg(test)] mod tests; 统一收纳，覆盖各子模块统计逻辑。
//! - 经 use super::* 取用 stats 根 re-export 的公共 API。
//! - are_different_batches / normalize_group_weights 为 pub(super)，测试套件经显式路径直接调用。
//! - 构造事件辅助统一复用 make_event / make_event_with_situation / make_event_with_time。
//!
//! 安全约束:
//! - 测试仅用合成 MemoryEvent，不依赖真实 LLM/embedding，可离线确定性运行。

use super::admission::are_different_batches;
use super::run::normalize_group_weights;
use super::*;
use ramaria_core::types::{MemoryEvent, Presentation, now_ms};

/// 构造测试用 MemoryEvent。
#[allow(clippy::too_many_arguments)]
fn make_event(
    title: &str,
    summary: &str,
    keywords: Option<&str>,
    confidence: f64,
    salience: f64,
    valence: f64,
    share: f64,
    presentation: Presentation,
    attitude: Option<&str>,
) -> MemoryEvent {
    let now = now_ms();
    let mut ev = MemoryEvent::new(
        "user-0001".into(),
        title.into(),
        summary.into(),
        now - 1000,
        now,
    );
    ev.keywords = keywords.map(|s| s.into());
    ev.confidence = confidence;
    ev.salience = salience;
    ev.valence = valence;
    ev.share = share;
    ev.presentation = presentation;
    ev.attitude = attitude.map(|s| s.into());
    ev
}

/// 构造测试用 MemoryEvent（含 situation_strength）。
fn make_event_with_situation(
    title: &str,
    summary: &str,
    keywords: Option<&str>,
    confidence: f64,
    salience: f64,
    valence: f64,
    share: f64,
    presentation: Presentation,
    attitude: Option<&str>,
    situation_strength: Option<i32>,
) -> MemoryEvent {
    let mut ev = make_event(
        title,
        summary,
        keywords,
        confidence,
        salience,
        valence,
        share,
        presentation,
        attitude,
    );
    ev.situation_strength = situation_strength;
    ev
}

// =========================================================
// 情境强度乘数
// =========================================================

/// situation_multiplier 全分支参数化验证：
/// - None / 3 / 非法值(0,6,100) → 中性 1.0
/// - 弱情境 (1,2) → 放大 1.5
/// - 强情境 (4,5) → 抑制 0.5
#[test]
fn situation_multiplier_cases() {
    let cases = [
        (None, 1.0),
        (Some(3), 1.0),
        (Some(1), 1.5),
        (Some(2), 1.5),
        (Some(4), 0.5),
        (Some(5), 0.5),
        (Some(0), 1.0),
        (Some(6), 1.0),
        (Some(100), 1.0),
    ];
    for (strength, expected) in cases {
        assert!(
            (situation_multiplier(strength) - expected).abs() < 1e-10,
            "strength={strength:?} 期望 {expected}",
        );
    }
}

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

// =========================================================
// 校准权重链核心
// =========================================================

/// calibrate_salience 各 (raw, rec, int, men) 组合参数化验证（含 floor/ceiling）。
#[test]
fn calibrate_salience_cases() {
    let config = CalibratedWeightConfig::default();
    let cases = [
        // (raw, recurrence, intensity, mention, expected)
        (0.8, 0.0, 0.0, 0.0, 0.8),    // 无加成 → 保持不变
        (0.8, 1.0, 0.0, 0.0, 1.0),    // rec=1.0 → boost 0.30 → clamp 1.0
        (0.5, 0.5, 0.5, 0.5, 0.6625), // rec=0.15 + int=0.10 + men=0.075
        (0.0, 0.0, 0.0, 0.0, 0.01),   // 极低 → 保底 0.01
        (1.0, 1.0, 1.0, 1.0, 1.0),    // 全加成 → clamp 1.0
    ];
    for (raw, rec, int, men, expected) in cases {
        let cal = calibrate_salience(raw, rec, int, men, &config);
        assert!((cal - expected).abs() < 1e-6, "raw={raw} 期望 {expected}");
    }
}

#[test]
fn compute_calibrated_weight_confirmed() {
    let config = CalibratedWeightConfig::default();
    let event = make_event(
        "E",
        "s",
        None,
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment {
        topic_recurrence_count: 0.5,
        emotional_intensity: 0.5,
        mention_frequency: 0.5,
        source_count: 3,
    };
    // salience_cal = 0.8 * (1 + 0.15 + 0.10 + 0.075) = 0.8 * 1.325 = 1.06 → clamp 1.0
    // confidence_factor = 1.0 (confirmed)
    // situation_multiplier = 1.0 (None → 中性)
    // source_support = min(1.0, 3/3) = 1.0
    // w = 1.0 * 1.0 * 1.0 * 1.0 = 1.0
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    assert!((w - 1.0).abs() < 1e-6);
}

#[test]
fn compute_calibrated_weight_tentative_half() {
    let config = CalibratedWeightConfig::default();
    let event = make_event(
        "E",
        "s",
        None,
        0.5,
        0.8,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment {
        source_count: 1,
        ..Default::default()
    };
    // salience_cal = 0.8 (no boosts)
    // confidence_factor = 0.5 (tentative)
    // situation_multiplier = 1.0
    // source_support = min(1.0, 1/3) = 0.333...
    // w = 0.8 * 0.5 * 1.0 * 0.333... = 0.1333...
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    assert!((w - 0.8 * 0.5 * (1.0 / 3.0)).abs() < 1e-6);
}

#[test]
fn compute_calibrated_weight_discarded_zero() {
    let config = CalibratedWeightConfig::default();
    let event = make_event(
        "E",
        "s",
        None,
        0.3,
        0.8,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment::default();
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    assert!((w - 0.0).abs() < 1e-10);
}

#[test]
fn compute_calibrated_weight_weak_situation_boost() {
    let config = CalibratedWeightConfig::default();
    let event = make_event_with_situation(
        "E",
        "s",
        None,
        0.9,
        0.8,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
        Some(2),
    );
    let enrichment = EventEnrichment {
        source_count: 1,
        ..Default::default()
    };
    // salience_cal = 0.8, conf_factor=1.0, sit_mult=1.5, source=1/3=0.333
    // w = 0.8 * 1.0 * 1.5 * 0.333 = 0.4
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    assert!((w - 0.8 * 1.5 / 3.0).abs() < 1e-6);
}

#[test]
fn compute_calibrated_weight_strong_situation_dampen() {
    let config = CalibratedWeightConfig::default();
    let event = make_event_with_situation(
        "E",
        "s",
        None,
        0.9,
        0.8,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
        Some(5),
    );
    let enrichment = EventEnrichment {
        source_count: 3,
        ..Default::default()
    };
    // salience_cal = 0.8, conf_factor=1.0, sit_mult=0.5, source=1.0
    // w = 0.8 * 0.5 = 0.4
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    assert!((w - 0.4).abs() < 1e-6);
}

#[test]
fn compute_calibrated_weight_full_source_support() {
    let config = CalibratedWeightConfig::default();
    let event = make_event(
        "E",
        "s",
        None,
        0.9,
        1.0,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment {
        source_count: 5, // > min_sources_for_full_support (3)
        ..Default::default()
    };
    let w = compute_calibrated_weight(&event, &enrichment, &config);
    // source_support = 1.0 (capped)
    assert!(w > 0.9);
}

#[test]
fn compute_simple_weight_vs_calibrated() {
    // 对比: 简单权重 vs 校准权重（无加成时）
    let config = CalibratedWeightConfig::default();
    let event = make_event(
        "E",
        "s",
        None,
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
    );
    let enrichment = EventEnrichment::default();

    let simple = compute_simple_weight(&event);
    let calibrated = compute_calibrated_weight(&event, &enrichment, &config);

    // simple = 0.8 * 1.0 = 0.8
    assert!((simple - 0.8).abs() < 1e-10);

    // calibrated 应该与简单权重有差异（因为 source_support < 1.0）
    assert!(
        calibrated < simple,
        "校准权重应因 source_support < 1.0 而降低"
    );
}

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

// =========================================================
// Tentative 跨批次复现自动提升
// =========================================================

/// 创建一个带有自定义 created_at 的事件（用于批次检测）。
fn make_event_with_time(
    title: &str,
    keywords: Option<&str>,
    confidence: f64,
    salience: f64,
    created_at: i64,
) -> MemoryEvent {
    let mut ev = make_event(
        title,
        "摘要",
        keywords,
        confidence,
        salience,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    );
    ev.created_at = created_at;
    ev
}

/// are_different_batches 各分支参数化验证：跨批次 / 间隔内 / created_at 为 0 保守同批次。
#[test]
fn are_different_batches_cases() {
    let config = TentativePromotionConfig::default(); // min_batch_interval_hours = 6.0
    let base_time = 1700000000000i64; // 某个 Unix 毫秒时间戳
    let a = make_event_with_time("E1", Some("工作"), 0.5, 0.6, base_time);
    // 8 小时后 → 不同批次
    let b = make_event_with_time("E2", Some("工作"), 0.5, 0.6, base_time + 8 * 3600 * 1000);
    assert!(are_different_batches(&a, &b, &config));
    // 3 小时后 → 同批次
    let b = make_event_with_time("E2", Some("工作"), 0.5, 0.6, base_time + 3 * 3600 * 1000);
    assert!(!are_different_batches(&a, &b, &config));
    // created_at == 0 → 保守视为同批次
    let c = make_event_with_time("E3", Some("工作"), 0.5, 0.6, 0);
    let d = make_event_with_time("E4", Some("工作"), 0.5, 0.6, base_time);
    assert!(!are_different_batches(&c, &d, &config));
}

#[test]
fn promote_tentative_cross_batch_promotes() {
    let config = TentativePromotionConfig::default();
    let base_time = 1700000000000i64;
    // 两条同簇（工作）tentative 事件，来自不同批次，关键词相似度高 → 应提升
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议, 压力"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("工作, 会议, 项目"),
            0.55,
            0.5,
            base_time + 8 * 3600 * 1000,
        ),
    ];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 2);
    assert_eq!(result.remaining_count, 0);
    // 提升后的置信度应为 0.6
    for event in &result.promoted {
        assert!(
            (event.confidence - 0.6).abs() < 1e-10,
            "提升后 confidence 应为 0.6，实际为 {}",
            event.confidence
        );
    }
}

#[test]
fn promote_tentative_single_event_not_promoted() {
    let config = TentativePromotionConfig::default();
    // 单条 tentative 事件 → 簇大小不足，不提升
    let tentative = vec![make_event_with_time(
        "E1",
        Some("工作, 会议"),
        0.5,
        0.6,
        1700000000000i64,
    )];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 0);
    assert_eq!(result.remaining_count, 1);
}

#[test]
fn promote_tentative_same_batch_not_promoted() {
    let config = TentativePromotionConfig::default();
    let base_time = 1700000000000i64;
    // 两条同簇事件，但来自同一批次（时间间隔不足）→ 不提升
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("工作, 项目"),
            0.55,
            0.5,
            base_time + 3600 * 1000, // 仅 1 小时后
        ),
    ];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 0);
    assert_eq!(result.remaining_count, 2);
}

#[test]
fn promote_tentative_low_keyword_similarity_not_promoted() {
    let config = TentativePromotionConfig::default();
    let base_time = 1700000000000i64;
    // 两条事件来自不同批次，但关键词无交集 → Jaccard=0，不提升
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("社交, 聚会"),
            0.55,
            0.5,
            base_time + 8 * 3600 * 1000,
        ),
    ];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 0);
    assert_eq!(result.remaining_count, 2);
}

#[test]
fn promote_tentative_mixed_clusters() {
    let config = TentativePromotionConfig::default();
    let base_time = 1700000000000i64;
    // 工作簇：2 条，跨批次，关键词相似 → 应提升
    // 社交簇：1 条 → 不提升
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议, 压力"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("工作, 会议, 项目"),
            0.55,
            0.5,
            base_time + 8 * 3600 * 1000,
        ),
        make_event_with_time("E3", Some("社交, 聚会"), 0.5, 0.4, base_time),
    ];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 2, "工作簇应提升");
    assert_eq!(result.remaining_count, 1, "社交簇不提升");
    // 验证提升的是工作簇
    let promoted_titles: Vec<&str> = result.promoted.iter().map(|e| e.title.as_str()).collect();
    assert!(promoted_titles.contains(&"E1"));
    assert!(promoted_titles.contains(&"E2"));
    assert_eq!(result.remaining_tentative[0].title, "E3");
}

#[test]
fn promote_tentative_empty_input() {
    let config = TentativePromotionConfig::default();
    let tentative: Vec<MemoryEvent> = vec![];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 0);
    assert_eq!(result.remaining_count, 0);
    assert!(result.promoted.is_empty());
    assert!(result.remaining_tentative.is_empty());
}

#[test]
fn promote_tentative_custom_min_cluster_size() {
    let config = TentativePromotionConfig {
        min_cluster_size: 3,
        ..Default::default()
    };
    let base_time = 1700000000000i64;
    // 3 条同簇事件，跨批次 → 满足 min_cluster_size=3
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议, 压力"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("工作, 会议, 项目"),
            0.55,
            0.5,
            base_time + 8 * 3600 * 1000,
        ),
        make_event_with_time(
            "E3",
            Some("工作, 会议, 汇报"),
            0.5,
            0.4,
            base_time + 16 * 3600 * 1000,
        ),
    ];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 3);
    assert_eq!(result.remaining_count, 0);
}

#[test]
fn promote_tentative_respects_confirmed_list() {
    // confirmed 列表存在但不应影响提升逻辑（当前为签名保留参数）
    let config = TentativePromotionConfig::default();
    let base_time = 1700000000000i64;
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("工作, 会议, 项目"),
            0.55,
            0.5,
            base_time + 8 * 3600 * 1000,
        ),
    ];
    let confirmed = vec![make_event(
        "E0_confirmed",
        "已有确认事件",
        Some("工作"),
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
    )];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    // confirmed 列表存在时提升逻辑不受影响
    assert_eq!(result.promoted_count, 2);
}

// =========================================================
// 主分类提取
// =========================================================

/// extract_primary_category 各分支参数化验证：多关键词取首个 / 单关键词 / None / 空串。
#[test]
fn extract_primary_category_cases() {
    let cases = [
        (Some("工作, 会议, 紧张"), "工作"),
        (Some("家庭"), "家庭"),
        (None, "未分类"),
        (Some(""), "未分类"),
    ];
    for (keywords, expected) in cases {
        let ev = make_event(
            "E1",
            "摘要",
            keywords,
            0.8,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        assert_eq!(extract_primary_category(&ev), expected);
    }
}

// =========================================================
// 加权统计
// =========================================================

/// weighted_mean 各分支参数化验证：等权重 / 非等权重 / 全零权重。
#[test]
fn weighted_mean_cases() {
    // 等权重 → 算术平均
    let values = vec![1.0, 2.0, 3.0];
    let weights = vec![1.0, 1.0, 1.0];
    let mean = weighted_mean(&values, &weights);
    assert!((mean - 2.0).abs() < 1e-10);
    // 非等权重 → 加权平均
    let values = vec![0.5, 0.5, 1.0];
    let weights = vec![0.2, 0.5, 0.8];
    let mean = weighted_mean(&values, &weights);
    assert!((mean - 0.7666).abs() < 0.001);
    // 全零权重 → 0.0
    let values = vec![1.0, 2.0];
    let weights = vec![0.0, 0.0];
    let mean = weighted_mean(&values, &weights);
    assert!((mean - 0.0).abs() < 1e-10);
}

#[test]
fn weighted_variance_basic() {
    let values = vec![1.0, 2.0, 3.0];
    let weights = vec![1.0, 1.0, 1.0];
    let var = weighted_variance(&values, &weights, 2.0);
    assert!((var - 2.0 / 3.0).abs() < 1e-10);
}

#[test]
fn weighted_ratio_basic() {
    let indicators = vec![1.0, 0.0, 1.0];
    let weights = vec![0.4, 0.6, 1.0];
    let ratio = weighted_ratio(&indicators, &weights);
    assert!((ratio - 0.7).abs() < 1e-10);
}

// =========================================================
// 单分类统计（校准权重 + 向后兼容）
// =========================================================

#[test]
fn category_stats_with_calibrated_weights() {
    let config = CalibratedWeightConfig::default();
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
            Some("满意"),
        ),
        make_event(
            "E2",
            "s2",
            Some("工作,项目"),
            0.8,
            0.6,
            -0.3,
            0.4,
            Presentation::Subjective,
            Some("焦虑"),
        ),
        make_event(
            "E3",
            "s3",
            Some("工作,汇报"),
            0.7,
            0.9,
            0.2,
            0.6,
            Presentation::Mixed,
            Some("一般"),
        ),
    ];
    let enrichments = EventEnrichment::derive_batch(&events);
    let stats = compute_category_stats("工作", &events, Some(&enrichments), &config);

    assert_eq!(stats.category, "工作");
    assert_eq!(stats.event_count, 3);
    // n_eff 应该由于校准权重而略小于简单加权（source_support < 1.0）
    let simple_stats = compute_category_stats("工作", &events, None, &config);
    assert!(
        stats.n_eff < simple_stats.n_eff,
        "校准 n_eff({}) 应小于简单加权 n_eff({})",
        stats.n_eff,
        simple_stats.n_eff
    );
    assert!(stats.n_eff > 0.0, "n_eff 应大于 0");
}

#[test]
fn category_stats_with_simple_weights_backward_compat() {
    let config = CalibratedWeightConfig::default();
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
            Some("满意"),
        ),
        make_event(
            "E2",
            "s2",
            Some("工作,项目"),
            0.8,
            0.6,
            -0.3,
            0.4,
            Presentation::Subjective,
            Some("焦虑"),
        ),
        make_event(
            "E3",
            "s3",
            Some("工作,汇报"),
            0.7,
            0.9,
            0.2,
            0.6,
            Presentation::Mixed,
            Some("一般"),
        ),
    ];
    // None enrichments → 简单权重
    let stats = compute_category_stats("工作", &events, None, &config);

    assert_eq!(stats.category, "工作");
    assert_eq!(stats.event_count, 3);
    // n_eff = 0.8 + 0.6 + 0.9 = 2.3
    assert!((stats.n_eff - 2.3).abs() < 1e-10);
}

#[test]
fn category_stats_single_event() {
    let config = CalibratedWeightConfig::default();
    let events = vec![make_event(
        "E1",
        "摘要",
        Some("家庭"),
        0.9,
        0.5,
        0.8,
        0.6,
        Presentation::Subjective,
        None,
    )];
    let stats = compute_category_stats("家庭", &events, None, &config);
    assert_eq!(stats.event_count, 1);
    assert!((stats.n_eff - 0.5).abs() < 1e-10);
    assert!((stats.valence_mean - 0.8).abs() < 1e-10);
    assert!((stats.valence_std - 0.0).abs() < 1e-10);
    assert!((stats.presentation_subjective_ratio - 1.0).abs() < 1e-10);
}

#[test]
fn category_stats_respects_situation_multiplier() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event_with_situation(
            "弱情境事件",
            "摘要",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
            Some(2),
        ),
        make_event_with_situation(
            "强情境事件",
            "摘要",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
            Some(5),
        ),
    ];
    let stats = compute_category_stats("工作", &events, None, &config);
    // 简单权重: n_eff = 0.8*1.5 + 0.8*0.5 = 1.2 + 0.4 = 1.6
    assert!((stats.n_eff - 1.6).abs() < 1e-10);
}

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

// =========================================================
// 跨分类指标
// =========================================================

#[test]
fn emotional_stability_with_calibrated_weights() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作"),
            0.9,
            0.5,
            0.5,
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
            -0.3,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let enrichments = EventEnrichment::derive_batch(&events);
    let stability = compute_emotional_stability(&events, Some(&enrichments), &config);
    assert!(stability > 0.0, "方差应大于零");
}

#[test]
fn narrative_consistency_perfect() {
    let cats = vec![
        CategoryStats {
            category: "工作".into(),
            event_count: 1,
            n_eff: 1.0,
            valence_mean: 0.0,
            valence_std: 0.0,
            valence_positive_ratio: 0.5,
            share_mean: 0.5,
            share_std: 0.0,
            presentation_objective_ratio: 0.6,
            presentation_subjective_ratio: 0.3,
            presentation_mixed_ratio: 0.1,
            group_weight: 0.5,
        },
        CategoryStats {
            category: "社交".into(),
            event_count: 1,
            n_eff: 1.0,
            valence_mean: 0.0,
            valence_std: 0.0,
            valence_positive_ratio: 0.5,
            share_mean: 0.5,
            share_std: 0.0,
            presentation_objective_ratio: 0.6,
            presentation_subjective_ratio: 0.3,
            presentation_mixed_ratio: 0.1,
            group_weight: 0.5,
        },
    ];
    let consistency = compute_narrative_consistency(&cats);
    assert!((consistency - 1.0).abs() < 1e-10);
}

#[test]
fn narrative_consistency_single_category() {
    let cats = vec![CategoryStats {
        category: "工作".into(),
        event_count: 1,
        n_eff: 1.0,
        valence_mean: 0.0,
        valence_std: 0.0,
        valence_positive_ratio: 0.5,
        share_mean: 0.5,
        share_std: 0.0,
        presentation_objective_ratio: 0.5,
        presentation_subjective_ratio: 0.3,
        presentation_mixed_ratio: 0.2,
        group_weight: 1.0,
    }];
    let consistency = compute_narrative_consistency(&cats);
    assert!((consistency - 1.0).abs() < 1e-10, "单个分类一致性为 1.0");
}

/// compute_share_skewness 各事件分布参数化验证。
#[test]
fn share_skewness_cases() {
    let config = CalibratedWeightConfig::default();
    // 对称分布（share 0.3/0.5/0.7）→ 偏度接近 0
    let events = vec![
        make_event(
            "E1",
            "s1",
            Some("工作"),
            0.9,
            0.5,
            0.0,
            0.3,
            Presentation::Mixed,
            None,
        ),
        make_event(
            "E2",
            "s2",
            Some("工作"),
            0.9,
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
            0.9,
            0.5,
            0.0,
            0.7,
            Presentation::Mixed,
            None,
        ),
    ];
    let skew = compute_share_skewness(&events, None, &config);
    assert!(skew.abs() < 0.1, "对称分布偏度应接近0，实际={skew}");
    // 单事件 → 偏度 0
    let events = vec![make_event(
        "E1",
        "s1",
        Some("工作"),
        0.9,
        0.5,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    )];
    let skew = compute_share_skewness(&events, None, &config);
    assert!((skew - 0.0).abs() < 1e-10);
}

#[test]
fn share_kurtosis_uniform() {
    let config = CalibratedWeightConfig::default();
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
            Some("工作"),
            0.9,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let kurt = compute_share_kurtosis(&events, None, &config);
    assert!((kurt - 0.0).abs() < 1e-10);
}

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

// =========================================================
// 跨分类指标（校准权重路径）
// =========================================================

#[test]
fn cross_category_metrics_with_calibrated_weights() {
    let config = CalibratedWeightConfig::default();
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
            "s2",
            Some("社交"),
            0.8,
            0.6,
            -0.3,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let enrichments = EventEnrichment::derive_batch(&events);
    let cat_cfg = CalibratedWeightConfig::default();
    let grouped = group_by_category(&events);
    let mut cats: Vec<CategoryStats> = grouped
        .iter()
        .map(|(cat, evts)| {
            let cat_enr: Vec<EventEnrichment> = evts
                .iter()
                .map(|e| {
                    let idx = events.iter().position(|ae| ae.id == e.id).unwrap_or(0);
                    enrichments.get(idx).cloned().unwrap_or_default()
                })
                .collect();
            compute_category_stats(cat, evts, Some(&cat_enr), &cat_cfg)
        })
        .collect();
    normalize_group_weights(&mut cats);

    let metrics = compute_cross_category_metrics(&events, &cats, Some(&enrichments), &config);
    assert!(metrics.emotional_stability >= 0.0);
    assert!(metrics.narrative_consistency >= 0.0);
}

// =========================================================
// 动机维度统计（MotivesStats）
// =========================================================

/// 构造带 motives 字段的测试事件。
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

// =========================================================
// CalibratedWeightConfig::default() 验证
// =========================================================

#[test]
fn calibrated_weight_config_defaults() {
    let config = CalibratedWeightConfig::default();
    assert!((config.salience_exponent - 1.0).abs() < 1e-10);
    assert!((config.recurrence_boost_max - 0.30).abs() < 1e-10);
    assert!((config.intensity_boost_max - 0.20).abs() < 1e-10);
    assert!((config.mention_boost_max - 0.15).abs() < 1e-10);
    assert_eq!(config.min_sources_for_full_support, 3);
    assert!((config.tentative_weight_factor - 0.5).abs() < 1e-10);
}

// =========================================================
// 批量权重计算
// =========================================================

#[test]
fn test_compute_calibrated_weights_batch() {
    let config = CalibratedWeightConfig::default();
    let events = vec![
        make_event(
            "E1",
            "s1",
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
            "s2",
            None,
            0.5,
            0.6,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        ),
    ];
    let enrichments = vec![
        EventEnrichment::from_event(&events[0]),
        EventEnrichment::from_event(&events[1]),
    ];
    let weights = compute_calibrated_weights_batch(&events, &enrichments, &config);
    assert_eq!(weights.len(), 2);
    // E1 (confirmed) 应比 E2 (tentative) 权重更高
    assert!(
        weights[0] > weights[1],
        "confirmed 事件权重应高于 tentative"
    );
}

#[test]
#[should_panic(expected = "events 与 enrichments 长度必须一致")]
fn test_compute_calibrated_weights_batch_mismatch() {
    let config = CalibratedWeightConfig::default();
    let events = vec![make_event(
        "E1",
        "s",
        None,
        0.9,
        0.8,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    )];
    let enrichments = vec![EventEnrichment::default(), EventEnrichment::default()];
    compute_calibrated_weights_batch(&events, &enrichments, &config);
}
