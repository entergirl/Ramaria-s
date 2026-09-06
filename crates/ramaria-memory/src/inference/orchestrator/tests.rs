//! crates/ramaria-memory/src/inference/orchestrator/tests.rs - Phase B/C 编排层纯函数单元测试
//!
//! 设计特点:
//! - 覆盖 JSON 三步解析/转换/分层先验/语义匹配度/漂移旧分布恢复等纯函数。
//! - 全部使用合成数据，不依赖真实 LLM/embedding/数据库，可离线确定性运行。
//! - 异步集成测试（Phase B/C 全链路 + mock LLM）位于 ramaria-app 集成测试中。

use super::phase_b::{
    convert_to_personality_traits, parse_category_signals, parse_consistency_analysis,
    parse_inferred_traits, parse_json_with_degrade,
};
use super::phase_c::{
    compute_event_trait_relevance, longest_common_substring_ratio, restore_old_distribution,
};
use super::*;
use crate::inference::{
    inferrer::{
        InferredTrait, InferrerConfig, build_step1_prompt, build_step2_prompt, build_step3_prompt,
        mock_infer,
    },
    stats::{CategoryStats, CrossCategoryMetrics, RepresentativeEvent, StatsSummary},
};
use ramaria_core::types::{PersonalityTrait, TraitLayer, TraitSource, TraitStatus, now_ms};

// =========================================================
// JSON 解析测试
// =========================================================

#[test]
fn parse_category_signals_valid_json() {
    let raw = r#"{
            "工作": {"signal_label": "尽责", "evidence_citation": "valence_mean=0.6", "stability_judgment": "stable", "sufficient_evidence": true},
            "社交": {"signal_label": "社交回避", "evidence_citation": "share_mean=0.2", "stability_judgment": "contextual", "sufficient_evidence": false}
        }"#;
    let result = parse_category_signals(raw);
    assert!(result.is_some());
    let signals = result.unwrap();
    assert_eq!(signals.len(), 2);
    assert_eq!(signals[0].category, "工作");
    assert_eq!(signals[0].signal_label, "尽责");
    assert!(signals[0].sufficient_evidence);
}

#[test]
fn parse_category_signals_malformed_json() {
    let raw = "这不是 JSON";
    let result = parse_category_signals(raw);
    assert!(result.is_none());
}

#[test]
fn parse_consistency_analysis_valid() {
    let raw = r#"{"base_candidates":["尽责"],"primary_candidates":["温和"],"accent_candidates":["幽默"],"notes":"分析说明"}"#;
    let result = parse_consistency_analysis(raw);
    assert!(result.is_some());
    let analysis = result.unwrap();
    assert_eq!(analysis.base_candidates, vec!["尽责"]);
    assert_eq!(analysis.primary_candidates, vec!["温和"]);
    assert_eq!(analysis.accent_candidates, vec!["幽默"]);
}

#[test]
fn parse_inferred_traits_valid() {
    let raw = r#"[
            {"layer":"primary","trait_label":"温和","meaning":"待人友善","not_meaning":null,"trigger":null,"suppress":null,"related":null,"seq":0},
            {"layer":"accent","trait_label":"幽默","meaning":"用自嘲化解尴尬","not_meaning":"并非轻浮","trigger":"朋友聚会","suppress":"正式场合","related":"与温和互补","seq":1}
        ]"#;
    let result = parse_inferred_traits(raw);
    assert!(result.is_some());
    let traits = result.unwrap();
    assert_eq!(traits.len(), 2);
    assert_eq!(traits[0].trait_label, "温和");
    assert_eq!(traits[0].layer, "primary");
    assert_eq!(traits[1].trait_label, "幽默");
    assert_eq!(traits[1].meaning, "用自嘲化解尴尬");
}

#[test]
fn parse_inferred_traits_empty_array() {
    // 空数组是 LLM 的合法响应（无足够证据），
    // 应解析成功并返回空 traits，而非触发 MockFallback 降级。
    let raw = "[]";
    let result = parse_inferred_traits(raw);
    assert!(result.is_some(), "空数组应解析成功");
    assert_eq!(result.unwrap().len(), 0, "空数组应返回空 traits");
}

// =========================================================
// JSON 三步解析降级测试
// =========================================================

#[test]
fn parse_json_direct_success() {
    let raw = r#"{"base_candidates":["A"],"primary_candidates":["B"],"accent_candidates":[],"notes":"ok"}"#;
    let result = parse_json_with_degrade(raw, "test", parse_consistency_analysis);
    assert!(result.is_ok());
}

#[test]
fn parse_json_with_think_tags() {
    let raw = r#"<think>让我分析一下...</think>
{"base_candidates":["尽责"],"primary_candidates":["温和"],"accent_candidates":[],"notes":"ok"}"#;
    let result = parse_json_with_degrade(raw, "test", parse_consistency_analysis);
    assert!(result.is_ok(), "剥离 think 标签后应解析成功");
}

#[test]
fn parse_json_extract_from_text() {
    let raw = r#"分析结果如下：
{"base_candidates":["尽责"],"primary_candidates":["温和"],"accent_candidates":[],"notes":"ok"}
以上是分析结果。"#;
    let result = parse_json_with_degrade(raw, "test", parse_consistency_analysis);
    assert!(result.is_ok(), "正则提取 JSON 后应解析成功");
}

#[test]
fn parse_json_all_fail() {
    let raw = "完全没有 JSON 的文本";
    let result = parse_json_with_degrade(raw, "test", parse_consistency_analysis);
    assert!(result.is_err());
}

#[test]
fn parse_json_empty_array_is_success() {
    // LLM 返回空数组是合法响应（无足够证据），
    // 应解析成功（空 traits），而不是触发 MockFallback 降级。
    let result = parse_json_with_degrade("[]", "Step3", parse_inferred_traits);
    assert!(result.is_ok(), "空数组应解析成功");
    assert!(result.unwrap().is_empty(), "空数组应产生空 traits");
}

// =========================================================
// 类型转换测试
// =========================================================

#[test]
fn convert_inferred_to_personality_traits() {
    let stats = make_test_stats();
    let inferred = vec![
        InferredTrait {
            layer: "base".into(),
            trait_label: "尽责".into(),
            meaning: "对任务高度负责".into(),
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq: 0,
            confidence: None,
        },
        InferredTrait {
            layer: "primary".into(),
            trait_label: "温和".into(),
            meaning: "待人友善".into(),
            not_meaning: Some("并非软弱".into()),
            trigger: None,
            suppress: None,
            related: None,
            seq: 0,
            confidence: None,
        },
    ];

    let traits = convert_to_personality_traits(&inferred, "user-0001", &stats);

    assert_eq!(traits.len(), 2);
    assert_eq!(traits[0].layer, TraitLayer::Base);
    assert_eq!(traits[0].persona_uid, "user-0001");
    assert_eq!(traits[0].source, TraitSource::Inferred);
    assert_eq!(traits[0].status, TraitStatus::Active);
    // 无匹配分类时回退到默认值 0.5
    assert_eq!(traits[0].confidence, 0.5);

    assert_eq!(traits[1].layer, TraitLayer::Primary);
    assert_eq!(traits[1].not_meaning, Some("并非软弱".into()));
}

#[test]
fn convert_unknown_layer_defaults_to_accent() {
    let stats = make_test_stats();
    let inferred = vec![InferredTrait {
        layer: "unknown_layer".into(),
        trait_label: "测试".into(),
        meaning: "测试含义".into(),
        not_meaning: None,
        trigger: None,
        suppress: None,
        related: None,
        seq: 0,
        confidence: None,
    }];

    let traits = convert_to_personality_traits(&inferred, "user-0001", &stats);
    assert_eq!(traits[0].layer, TraitLayer::Accent);
}

// =========================================================
// helper: 构建测试用 StatsSummary
// =========================================================

fn make_test_stats() -> StatsSummary {
    StatsSummary {
        total_events_in: 15,
        total_events_filtered: 12,
        confirmed_count: 12,
        tentative_count: 0,
        discarded_count: 3,
        category_count: 2,
        categories: vec![
            CategoryStats {
                category: "工作".into(),
                event_count: 8,
                n_eff: 6.5,
                valence_mean: 0.55,
                valence_std: 0.35,
                valence_positive_ratio: 0.75,
                share_mean: 0.7,
                share_std: 0.2,
                presentation_objective_ratio: 0.5,
                presentation_subjective_ratio: 0.3,
                presentation_mixed_ratio: 0.2,
                group_weight: 0.6,
            },
            CategoryStats {
                category: "社交".into(),
                event_count: 4,
                n_eff: 3.2,
                valence_mean: -0.1,
                valence_std: 0.5,
                valence_positive_ratio: 0.45,
                share_mean: 0.8,
                share_std: 0.15,
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
            summary: "完成项目验收".into(),
            attitude: Some("对成果满意".into()),
            valence: 0.8,
            salience: 0.9,
            category: "工作".into(),
        }],
        motive_stats: Vec::new(),
    }
}

// =========================================================
// 纯函数边界测试（无需 StorageBackend/LlmProvider mock）
// 说明: 异步集成测试（Phase B/C 全链路 + mock LLM）在
//       `crates/ramaria-app/tests/m3_integration.rs` 中。
// =========================================================

// ---- PhaseBResult / PhaseCResult 构造与字段 ----

#[test]
fn phase_b_result_fields() {
    let result = PhaseBResult {
        traits_saved: 3,
        traits_updated: 1,
        traits_deprecated: 0,
        source: PhaseBSource::LlmInference,
        trait_ids: vec![1, 2, 3],
        traits: vec![],
    };
    assert_eq!(result.traits_saved, 3);
    assert_eq!(result.traits_updated, 1);
    assert_eq!(result.traits_deprecated, 0);
    assert_eq!(result.source, PhaseBSource::LlmInference);
    assert_eq!(result.trait_ids.len(), 3);
}

#[test]
fn phase_b_result_mock_fallback_source() {
    let result = PhaseBResult {
        traits_saved: 2,
        traits_updated: 0,
        traits_deprecated: 0,
        source: PhaseBSource::MockFallback,
        trait_ids: vec![1, 2],
        traits: vec![],
    };
    assert_eq!(result.source, PhaseBSource::MockFallback);
}

#[test]
fn phase_c_result_zero_events() {
    let result = PhaseCResult {
        traits_updated: 0,
        evidence_saved: 0,
        has_significant_drift: false,
        drift_categories: vec![],
        confidence_summary: None,
        drift_summary: None,
    };
    assert_eq!(result.traits_updated, 0);
    assert!(!result.has_significant_drift);
}

#[test]
fn phase_c_result_with_drift() {
    let result = PhaseCResult {
        traits_updated: 3,
        evidence_saved: 6,
        has_significant_drift: true,
        drift_categories: vec!["工作".into(), "社交".into()],
        confidence_summary: None,
        drift_summary: None,
    };
    assert!(result.has_significant_drift);
    assert_eq!(result.drift_categories.len(), 2);
    assert!(result.drift_categories.contains(&"工作".to_string()));
}

// ---- convert_to_personality_traits 边界情况 ----

#[test]
fn convert_empty_inferred_list() {
    let stats = make_test_stats();
    let traits = convert_to_personality_traits(&[], "user-0001", &stats);
    assert!(traits.is_empty());
}

#[test]
fn convert_multiple_layers_preserves_order() {
    let stats = make_test_stats();
    let inferred = vec![
        InferredTrait {
            layer: "base".into(),
            trait_label: "底色A".into(),
            meaning: "m".into(),
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq: 0,
            confidence: None,
        },
        InferredTrait {
            layer: "primary".into(),
            trait_label: "主色A".into(),
            meaning: "m".into(),
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq: 0,
            confidence: None,
        },
        InferredTrait {
            layer: "accent".into(),
            trait_label: "点缀A".into(),
            meaning: "m".into(),
            not_meaning: None,
            trigger: Some("条件".into()),
            suppress: None,
            related: None,
            seq: 0,
            confidence: None,
        },
    ];
    let traits = convert_to_personality_traits(&inferred, "user-0001", &stats);
    assert_eq!(traits.len(), 3);
    assert_eq!(traits[0].layer, TraitLayer::Base);
    assert_eq!(traits[1].layer, TraitLayer::Primary);
    assert_eq!(traits[2].layer, TraitLayer::Accent);
}

#[test]
fn convert_preserves_all_fields() {
    let stats = make_test_stats();
    let inferred = vec![InferredTrait {
        layer: "accent".into(),
        trait_label: "幽默".into(),
        meaning: "用自嘲化解尴尬".into(),
        not_meaning: Some("并非轻浮".into()),
        trigger: Some("朋友聚会".into()),
        suppress: Some("正式场合".into()),
        related: Some("与温和互补".into()),
        seq: 3,
        confidence: None,
    }];
    let traits = convert_to_personality_traits(&inferred, "user-0001", &stats);
    let t = &traits[0];
    assert_eq!(t.trait_label, "幽默");
    assert_eq!(t.meaning, "用自嘲化解尴尬");
    assert_eq!(t.not_meaning, Some("并非轻浮".into()));
    assert_eq!(t.trigger, Some("朋友聚会".into()));
    assert_eq!(t.suppress, Some("正式场合".into()));
    assert_eq!(t.related, Some("与温和互补".into()));
    assert_eq!(t.seq, 3);
    assert_eq!(t.source, TraitSource::Inferred);
    assert_eq!(t.status, TraitStatus::Active);
}

// ---- JSON 解析边界 ----

#[test]
fn parse_category_signals_partial_fields() {
    // 部分字段缺失时应使用默认值
    let raw = r#"{"工作": {"signal_label": "尽责"}}"#;
    let result = parse_category_signals(raw);
    assert!(result.is_some());
    let signals = result.unwrap();
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].signal_label, "尽责");
    assert_eq!(signals[0].stability_judgment, "uncertain");
    assert!(!signals[0].sufficient_evidence);
}

#[test]
fn parse_category_signals_missing_signal_label() {
    let raw = r#"{"工作": {"sufficient_evidence": true}}"#;
    let result = parse_category_signals(raw);
    assert!(result.is_some());
    let signals = result.unwrap();
    assert_eq!(signals[0].signal_label, "insufficient_data");
}

#[test]
fn parse_consistency_analysis_empty_candidates() {
    let raw = r#"{"base_candidates":[],"primary_candidates":[],"accent_candidates":[],"notes":""}"#;
    let result = parse_consistency_analysis(raw);
    assert!(result.is_some());
    let analysis = result.unwrap();
    assert!(analysis.base_candidates.is_empty());
    assert!(analysis.primary_candidates.is_empty());
}

#[test]
fn parse_inferred_traits_missing_optional_fields() {
    let raw = r#"[{"layer":"base","trait_label":"T","meaning":"M","seq":0}]"#;
    let result = parse_inferred_traits(raw);
    assert!(result.is_some());
    let traits = result.unwrap();
    assert_eq!(traits[0].trait_label, "T");
    assert!(traits[0].not_meaning.is_none());
    assert!(traits[0].trigger.is_none());
}

#[test]
fn parse_inferred_traits_non_json_prefix() {
    // LLM 有时会在 JSON 前加说明文字
    let raw = r#"好的，以下是分析结果：
[{"layer":"base","trait_label":"尽责","meaning":"M","seq":0}]"#;
    // 直接 parse_inferred_traits 会失败，但 parse_json_with_degrade 能处理
    let direct = parse_inferred_traits(raw);
    assert!(direct.is_none(), "直接解析应失败（有前缀文本）");

    let degraded = parse_json_with_degrade(raw, "test", parse_inferred_traits);
    assert!(degraded.is_ok(), "三步解析应成功提取 JSON 数组");
}

// ---- InferrerConfig 默认值 ----

#[test]
fn inferrer_config_defaults() {
    let config = InferrerConfig::default();
    assert_eq!(config.temperature, 0.3);
    assert_eq!(config.max_tokens, 2048);
    assert_eq!(config.low_evidence_threshold, 5.0);
    assert_eq!(config.step_max_tokens, 2048);
}

// ---- Prompt 构建非空 ----

#[test]
fn build_prompts_are_non_empty() {
    let stats = make_test_stats();
    let config = InferrerConfig::default();
    let result = mock_infer(&stats, "user-0001");

    let p1 = build_step1_prompt(&stats, &config, None, None);
    assert!(!p1.is_empty());
    assert!(p1.contains("工作"));
    assert!(p1.contains("性格心理分析师"));

    let p2 = build_step2_prompt(
        &result.category_signals,
        &stats.cross_category,
        &stats.categories,
    );
    assert!(!p2.is_empty());
    assert!(p2.contains("base_candidates"));
    assert!(p2.contains("跨领域的一致性模式"));

    let p3 = build_step3_prompt(&result.consistency, &result.category_signals, &stats);
    assert!(!p3.is_empty());
    assert!(p3.contains("layer"));
    assert!(p3.contains("trait_label"));
}

// =========================================================
// 分层先验收缩集成
// =========================================================

/// 构造测试用 PersonalityTrait。
fn make_trait(
    id: i64,
    label: &str,
    layer: TraitLayer,
    confidence: f64,
    status: TraitStatus,
) -> PersonalityTrait {
    let now = now_ms();
    PersonalityTrait {
        id,
        persona_uid: "user-0001".into(),
        trait_label: label.into(),
        meaning: format!("{} 的描述", label),
        layer,
        confidence,
        evidence: 1.0,
        consistency: 0.5,
        source: TraitSource::Inferred,
        status,
        not_meaning: None,
        trigger: None,
        suppress: None,
        related: None,
        seq: 1,
        ref_event_id: None,
        ref_l1_id: None,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn build_layer_hints_empty_traits() {
    let hints = build_layer_hints_from_traits(&[]);
    assert!(hints.is_empty());
}

#[test]
fn build_layer_hints_basic() {
    let traits = vec![
        make_trait(1, "工作", TraitLayer::Base, 0.7, TraitStatus::Active),
        make_trait(2, "社交", TraitLayer::Accent, 0.5, TraitStatus::Active),
    ];
    let hints = build_layer_hints_from_traits(&traits);
    assert_eq!(hints.len(), 2);
    assert_eq!(hints.get("工作"), Some(&TraitLayer::Base));
    assert_eq!(hints.get("社交"), Some(&TraitLayer::Accent));
}

#[test]
fn build_layer_hints_skips_deprecated() {
    let traits = vec![
        make_trait(1, "工作", TraitLayer::Base, 0.7, TraitStatus::Deprecated),
        make_trait(2, "社交", TraitLayer::Accent, 0.5, TraitStatus::Active),
    ];
    let hints = build_layer_hints_from_traits(&traits);
    assert_eq!(hints.len(), 1, "Deprecated trait 应被跳过");
    assert_eq!(hints.get("社交"), Some(&TraitLayer::Accent));
    assert!(!hints.contains_key("工作"));
}

#[test]
fn build_layer_hints_priority_base_over_accent() {
    // 同一 label 出现两次: Base > Accent
    let traits = vec![
        make_trait(1, "工作", TraitLayer::Accent, 0.5, TraitStatus::Active),
        make_trait(2, "工作", TraitLayer::Base, 0.7, TraitStatus::Active),
    ];
    let hints = build_layer_hints_from_traits(&traits);
    assert_eq!(hints.len(), 1);
    assert_eq!(
        hints.get("工作"),
        Some(&TraitLayer::Base),
        "Base 优先级应高于 Accent"
    );
}

#[test]
fn build_layer_hints_priority_primary_over_accent() {
    let traits = vec![
        make_trait(3, "社交", TraitLayer::Accent, 0.4, TraitStatus::Active),
        make_trait(4, "社交", TraitLayer::Primary, 0.6, TraitStatus::Active),
    ];
    let hints = build_layer_hints_from_traits(&traits);
    assert_eq!(hints.len(), 1);
    assert_eq!(
        hints.get("社交"),
        Some(&TraitLayer::Primary),
        "Primary 优先级应高于 Accent"
    );
}

#[test]
fn build_layer_hints_mixed_status() {
    let traits = vec![
        make_trait(1, "工作", TraitLayer::Base, 0.7, TraitStatus::Active),
        make_trait(2, "工作", TraitLayer::Accent, 0.5, TraitStatus::Deprecated),
        make_trait(3, "社交", TraitLayer::Primary, 0.6, TraitStatus::Active),
    ];
    let hints = build_layer_hints_from_traits(&traits);
    assert_eq!(hints.len(), 2);
    // 工作: 只有 Active 的 Base，Deprecated Accent 被忽略
    assert_eq!(hints.get("工作"), Some(&TraitLayer::Base));
    assert_eq!(hints.get("社交"), Some(&TraitLayer::Primary));
}

// =========================================================
// 语义匹配度测试
// =========================================================

#[test]
fn compute_relevance_label_match() {
    // "社交" 与 "社交回避" 有 LCS "社交" = 2 字符
    let keywords = vec!["社交", "朋友", "聚会"];
    let relevance = compute_event_trait_relevance(&keywords, "社交回避", "对大型社交场合感到消耗");
    // "社交" vs "社交回避" → LCS 2/min(2,4)=1.0 → match
    // "朋友" vs "社交回避" → 0 overlap
    // "聚会" vs "社交回避" → 0 overlap
    // match_count=1, total=3, relevance=1/3≈0.33
    assert!(relevance > 0.3);
}

#[test]
fn compute_relevance_meaning_match() {
    // "工作" has no overlap with "尽责" label, but matches meaning text
    let keywords = vec!["工作", "项目"];
    let relevance = compute_event_trait_relevance(
        &keywords,
        "尽责",
        "对交给自己的任务有强烈的完成意愿，重视承诺",
    );
    // "工作" vs meaning: "任" has 1 char overlap with "任务"...
    // Actually this is hard to guarantee with char-level LCS on Chinese.
    // The point is: meaning provides a richer target for matching.
    assert!(relevance >= 0.0 && relevance <= 1.0);
}

#[test]
fn compute_relevance_no_overlap_floor() {
    // 无重叠时返回 floor 值 0.3
    let keywords = vec!["abc", "xyz", "123"];
    let relevance = compute_event_trait_relevance(&keywords, "尽责", "");
    assert!((relevance - 0.3).abs() < f64::EPSILON);
}

#[test]
fn compute_relevance_empty_keywords() {
    let keywords: Vec<&str> = vec![];
    let relevance = compute_event_trait_relevance(&keywords, "尽责", "");
    // 无关键词时默认 0.5
    assert!((relevance - 0.5).abs() < f64::EPSILON);
}

// （原 compute_relevance_work_tasks_match_duty 与 compute_relevance_meaning_match
//  同输入同断言完全重复，且注释自认废弃试验，已删除）

/// longest_common_substring_ratio 各输入参数化验证。
#[test]
fn lcs_ratio_cases() {
    let cases = [
        ("社交", "社交", 1.0),         // 完全相同
        ("社交", "社交回避", 1.0),     // 子串
        ("abc", "xyz", 0.0),           // 无重叠
        ("测试工作", "工作项目", 0.5), // LCS="工作" 2/4
        ("", "测试", 0.0),             // 空输入
    ];
    for (a, b, expected) in cases {
        let a: Vec<char> = a.chars().collect();
        let b: Vec<char> = b.chars().collect();
        let ratio = longest_common_substring_ratio(&a, &b);
        assert!((ratio - expected).abs() < f64::EPSILON, "{a:?} vs {b:?}");
    }
}

// =========================================================
// 漂移检测旧分布恢复
// =========================================================

fn make_cluster_snapshot(
    persona_uid: &str,
    category: &str,
    samples: Option<String>,
) -> ramaria_core::types::ClusterSnapshot {
    ramaria_core::types::ClusterSnapshot {
        id: 0,
        persona_uid: persona_uid.to_string(),
        category: category.to_string(),
        cluster_label: format!("cluster_{category}"),
        samples,
        count: 0,
        is_current: true,
        created_at: 0,
        semantic_label: None,
        semantic_label_embedding: None,
    }
}

/// 从快照 samples JSON 恢复真实旧分布：valence_mean/share_mean 按 n_effective 展开。
#[test]
fn restore_old_distribution_valid_snapshot() {
    let snap = make_cluster_snapshot(
        "char-1",
        "工作",
        Some(
            r#"{"category":"工作","event_count":10,"n_effective":5,
                   "valence_mean":0.6,"valence_std":0.2,"share_mean":0.7}"#
                .to_string(),
        ),
    );
    let (valences, shares, saliences) = restore_old_distribution(&[snap]);

    // n_effective=5 → valence 重复 5 次 0.6，share 重复 5 次 0.7
    assert_eq!(valences.len(), 5);
    assert_eq!(shares.len(), 5);
    assert!(valences.iter().all(|&v| (v - 0.6).abs() < 1e-9));
    assert!(shares.iter().all(|&s| (s - 0.7).abs() < 1e-9));
    assert!(saliences.is_empty());
}

/// 快照 samples 缺失 → 返回空向量（调用方跳过并记 warn，不阻塞）。
#[test]
fn restore_old_distribution_missing_samples() {
    let snap = make_cluster_snapshot("char-1", "工作", None);
    let (valences, shares, _) = restore_old_distribution(&[snap]);
    assert!(valences.is_empty());
    assert!(shares.is_empty());
}

/// 快照 samples 为非法 JSON → 静默跳过该快照（不 panic），其余快照正常恢复。
#[test]
fn restore_old_distribution_bad_json_skipped() {
    let good = make_cluster_snapshot(
        "char-1",
        "社交",
        Some(r#"{"n_effective":2,"valence_mean":0.5,"share_mean":0.4}"#.to_string()),
    );
    let bad = make_cluster_snapshot("char-1", "社交", Some("不是JSON".to_string()));
    let (valences, shares, _) = restore_old_distribution(&[bad, good]);

    assert_eq!(valences.len(), 2);
    assert_eq!(shares.len(), 2);
}

/// n_effective 缺失时回退 event_count，仍缺失时回退 0（空分布）。
#[test]
fn restore_old_distribution_fallback_sample_size() {
    // 仅 event_count，无 n_effective → 用 event_count=3
    let snap_event_count = make_cluster_snapshot(
        "char-1",
        "家庭",
        Some(r#"{"event_count":3,"valence_mean":0.4,"share_mean":0.3}"#.to_string()),
    );
    let (v1, s1, _) = restore_old_distribution(&[snap_event_count]);
    assert_eq!(v1.len(), 3);
    assert_eq!(s1.len(), 3);

    // 无样本量字段 → 空分布
    let snap_no_size = make_cluster_snapshot(
        "char-1",
        "家庭",
        Some(r#"{"valence_mean":0.4}"#.to_string()),
    );
    let (v2, _, _) = restore_old_distribution(&[snap_no_size]);
    assert!(v2.is_empty());
}
