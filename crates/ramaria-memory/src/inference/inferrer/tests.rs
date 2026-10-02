//! crates/ramaria-memory/src/inference/inferrer/tests.rs - 三步推断单元测试
//!
//! 设计特点:
//! - 覆盖 Prompt 构建、Mock 推断与后处理差异计算
//! - 使用最小字段集构造 StatsSummary，结果不依赖真实 LLM 与时钟
//! - 断言锁定各层输出与差异动作，行为等价可回归

use super::*;
use crate::inference::stats::{
    CategoryStats, CrossCategoryMetrics, RepresentativeEvent, StatsSummary,
};
use ramaria_core::{PersonalityTrait, TraitLayer, TraitSource, TraitStatus};

fn make_test_stats() -> StatsSummary {
    StatsSummary {
        total_events_in: 10,
        total_events_filtered: 8,
        confirmed_count: 8,
        tentative_count: 0,
        discarded_count: 2,
        category_count: 2,
        categories: vec![
            CategoryStats {
                category: "工作".into(),
                event_count: 5,
                n_eff: 3.5,
                valence_mean: 0.6,
                valence_std: 0.3,
                valence_positive_ratio: 0.8,
                share_mean: 0.7,
                share_std: 0.2,
                presentation_objective_ratio: 0.5,
                presentation_subjective_ratio: 0.3,
                presentation_mixed_ratio: 0.2,
                group_weight: 0.6,
            },
            CategoryStats {
                category: "社交".into(),
                event_count: 3,
                n_eff: 1.8,
                valence_mean: -0.2,
                valence_std: 0.5,
                valence_positive_ratio: 0.4,
                share_mean: 0.8,
                share_std: 0.1,
                presentation_objective_ratio: 0.2,
                presentation_subjective_ratio: 0.6,
                presentation_mixed_ratio: 0.2,
                group_weight: 0.4,
            },
        ],
        cross_category: CrossCategoryMetrics {
            emotional_stability: 0.45,
            narrative_consistency: 0.7,
            attitude_contradiction_count: 0,
            share_skewness: 0.1,
            share_kurtosis: -0.5,
        },
        representative_events: vec![RepresentativeEvent {
            title: "项目验收".into(),
            summary: "顺利完成项目验收".into(),
            attitude: Some("对成果感到满意".into()),
            valence: 0.8,
            salience: 0.9,
            category: "工作".into(),
        }],
        motive_stats: Vec::new(),
    }
}

// ---- Prompt 构建 ----

#[test]
fn build_step1_prompt_is_valid() {
    let stats = make_test_stats();
    let config = InferrerConfig::default();
    let prompt = build_step1_prompt(&stats, &config, None, None);
    assert!(prompt.contains("工作"));
    assert!(prompt.contains("社交"));
    assert!(prompt.contains("n_eff"));
    assert!(prompt.contains("分类统计"));
}

#[test]
fn build_step2_prompt_is_valid() {
    let stats = make_test_stats();
    let _config = InferrerConfig::default();
    let result = mock_infer(&stats, "user-0001");
    let prompt = build_step2_prompt(
        &result.category_signals,
        &stats.cross_category,
        &stats.categories,
    );
    assert!(prompt.contains("base_candidates"));
    assert!(prompt.contains("excluded_categories"));
    assert!(prompt.contains("special处理规则") || prompt.contains("特殊处理"));
}

#[test]
fn build_step3_prompt_is_valid() {
    let stats = make_test_stats();
    let result = mock_infer(&stats, "user-0001");
    let prompt = build_step3_prompt(&result.consistency, &result.category_signals, &stats);
    assert!(prompt.contains("layer"));
    assert!(prompt.contains("trait_label"));
}

// ---- Mock 推断 ----

#[test]
fn mock_infer_generates_signals() {
    let stats = make_test_stats();
    let result = mock_infer(&stats, "user-0001");
    assert_eq!(result.category_signals.len(), 2);
    // 工作分类 n_eff=3.5 < 5，应标记 insufficient_evidence
    let work_signal = result
        .category_signals
        .iter()
        .find(|s| s.category == "工作")
        .unwrap();
    assert!(!work_signal.sufficient_evidence);
    assert!(!work_signal.signal_label.is_empty());
}

#[test]
fn mock_infer_generates_traits() {
    let stats = make_test_stats();
    let result = mock_infer(&stats, "user-0001");
    assert!(!result.traits.is_empty(), "应至少生成 traits");
    // 所有 trait 应有 persona_uid
    for t in &result.traits {
        assert_eq!(t.persona_uid, "user-0001");
    }
}

#[test]
fn mock_infer_empty_stats() {
    let stats = StatsSummary {
        total_events_in: 0,
        total_events_filtered: 0,
        confirmed_count: 0,
        tentative_count: 0,
        discarded_count: 0,
        category_count: 0,
        categories: vec![],
        cross_category: CrossCategoryMetrics {
            emotional_stability: 0.0,
            narrative_consistency: 1.0,
            attitude_contradiction_count: 0,
            share_skewness: 0.0,
            share_kurtosis: 0.0,
        },
        representative_events: vec![],
        motive_stats: Vec::new(),
    };
    let result = mock_infer(&stats, "user-0001");
    assert!(result.category_signals.is_empty());
    assert!(result.traits.is_empty());
}

// ---- 动机维度 mock 推断 ----

#[test]
fn mock_infer_with_motive_stats_generates_motive_traits() {
    use crate::inference::stats::MotiveStats;

    let mut stats = make_test_stats();
    // 添加两个动机统计条目
    stats.motive_stats = vec![
        MotiveStats {
            motive: "地位维护".into(),
            event_count: 3,
            n_eff: 2.5,
            valence_mean: 0.5,
            valence_std: 0.2,
            valence_positive_ratio: 0.8,
            share_mean: 0.6,
            share_std: 0.2,
            presentation_objective_ratio: 0.3,
            presentation_subjective_ratio: 0.5,
            presentation_mixed_ratio: 0.2,
            avg_salience: 0.7,
        },
        MotiveStats {
            motive: "自主性".into(),
            event_count: 2,
            n_eff: 2.2,
            valence_mean: -0.4,
            valence_std: 0.3,
            valence_positive_ratio: 0.2,
            share_mean: 0.8,
            share_std: 0.1,
            presentation_objective_ratio: 0.1,
            presentation_subjective_ratio: 0.7,
            presentation_mixed_ratio: 0.2,
            avg_salience: 0.6,
        },
    ];

    let result = mock_infer(&stats, "user-0001");
    // 应该包含动机驱动的 accent trait
    let motive_traits: Vec<_> = result
        .traits
        .iter()
        .filter(|t| t.trait_label.contains("动机-"))
        .collect();
    assert!(
        !motive_traits.is_empty(),
        "mock_infer 应在有动机数据时生成动机相关 trait，实际 traits: {:?}",
        result
            .traits
            .iter()
            .map(|t| &t.trait_label)
            .collect::<Vec<_>>()
    );
}

#[test]
fn build_step1_prompt_includes_calibration_preamble() {
    let stats = make_test_stats();
    let config = InferrerConfig::default();
    let prompt = build_step1_prompt(&stats, &config, None, None);
    assert!(prompt.contains("校准权重链"));
    assert!(prompt.contains("confidence_factor"));
    assert!(prompt.contains("tentative 事件"));
    assert!(prompt.contains("分层经验贝叶斯收缩"));
}

#[test]
fn build_step1_prompt_with_motive_text() {
    use crate::inference::stats::MotiveStats;

    let mut stats = make_test_stats();
    stats.motive_stats = vec![MotiveStats {
        motive: "归属".into(),
        event_count: 2,
        n_eff: 1.8,
        valence_mean: 0.3,
        valence_std: 0.2,
        valence_positive_ratio: 0.7,
        share_mean: 0.5,
        share_std: 0.15,
        presentation_objective_ratio: 0.4,
        presentation_subjective_ratio: 0.3,
        presentation_mixed_ratio: 0.3,
        avg_salience: 0.55,
    }];

    let motive_text = format_motive_stats(&stats.motive_stats, 5);
    let prompt = build_step1_prompt(&stats, &InferrerConfig::default(), None, Some(&motive_text));
    assert!(prompt.contains("动机维度统计"));
    assert!(prompt.contains("归属"));
}

// ---- 后处理（差异计算） ----

#[test]
fn compute_diff_new_traits() {
    let stats = make_test_stats();
    let result = mock_infer(&stats, "user-0001");
    let post = post_process_inference(&result, &[], "user-0001");
    assert!(!post.to_add.is_empty(), "旧画像为空时所有 trait 应新增");
    assert!(post.to_update.is_empty());
    assert!(post.to_deprecate.is_empty());
}

#[test]
fn compute_diff_matching_traits() {
    let stats = make_test_stats();
    let result = mock_infer(&stats, "user-0001");
    let old = result.traits.clone(); // 模拟已有相同 traits
    let post = post_process_inference(&result, &old, "user-0001");
    // 所有 trait 应被匹配（无 Add），且 Keep ≥ 旧 trait 数（因可能有多层同名 label）
    let add_count = post
        .diffs
        .iter()
        .filter(|d| d.action == DiffAction::Add)
        .count();
    assert_eq!(add_count, 0, "相同 traits 对比不应产生 Add");
    let keep_count = post
        .diffs
        .iter()
        .filter(|d| d.action == DiffAction::Keep)
        .count();
    assert!(keep_count > 0, "应有至少一个 Keep");
}

#[test]
fn compute_diff_accent_deprecation() {
    // 创建一个旧 accent trait，新推断中不包含
    let now = ramaria_core::types::now_ms();
    let old_accent = PersonalityTrait {
        id: 99,
        persona_uid: "user-0001".into(),
        layer: TraitLayer::Accent,
        trait_label: "过时标签".into(),
        meaning: "旧意义".into(),
        not_meaning: None,
        trigger: Some("旧条件".into()),
        suppress: None,
        related: None,
        seq: 0,
        source: TraitSource::Inferred,
        ref_event_id: None,
        ref_l1_id: None,
        confidence: 0.3,
        evidence: 0.5,
        consistency: 0.3,
        status: TraitStatus::Active,
        created_at: now,
        updated_at: now,
    };

    let stats = make_test_stats();
    let result = mock_infer(&stats, "user-0001");
    let post = post_process_inference(&result, &[old_accent], "user-0001");
    // 旧 accent 应被标记废弃
    assert!(post.to_deprecate.contains(&99));
}

// ---- InferrerConfig ----

#[test]
fn inferrer_config_defaults() {
    let config = InferrerConfig::default();
    assert_eq!(config.low_evidence_threshold, 5.0);
    assert_eq!(config.temperature, 0.3);
}
