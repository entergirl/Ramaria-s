//! tests/inference/pipeline.rs - 全链路 Phase A→B→C 集成
//!
//! 设计特点:
//! - 由 tests/inference.rs 以 mod pipeline; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 全链路 Phase A→B→C 集成测试
// =========================================================

#[tokio::test]
async fn full_pipeline_with_m4_features() {
    // 1. 准备 StatsSummary（含动机统计）
    let stats = make_m4_stats_summary();

    // 验证动机统计已内嵌
    assert!(!stats.motive_stats.is_empty());
    assert_eq!(stats.motive_stats.len(), 3);

    // 2. 准备 MockStorage 和 MultiStepLlm
    let storage = MockStorage::new();

    // Step 1: 分类信号（JSON）
    let step1_json = r#"{
        "工作": {
            "signal_label": "尽责",
            "evidence_citation": "n_eff=4.2, valence_mean=-0.08, 地位维护驱动力强",
            "stability_judgment": "stable",
            "sufficient_evidence": true
        },
        "社交": {
            "signal_label": "亲和",
            "evidence_citation": "n_eff=1.8, valence_mean=0.35, 归属动机驱动",
            "stability_judgment": "contextual",
            "sufficient_evidence": false
        }
    }"#;

    // Step 2: 一致性分析（JSON）
    let step2_json = r#"{
        "base_candidates": ["尽责"],
        "primary_candidates": ["尽责"],
        "accent_candidates": ["动机-地位维护-驱动", "动机-归属-亲和"],
        "notes": "工作领域表现稳定，社交领域样本量不足；动机维度显著"
    }"#;

    // Step 3: 性格画像（JSON 数组）
    let step3_json = r#"[
        {"layer":"base","trait_label":"尽责","meaning":"对工作有强烈的完成驱动力","not_meaning":"并非完美主义","trigger":null,"suppress":null,"related":null,"seq":0},
        {"layer":"primary","trait_label":"尽责-工作","meaning":"工作中最突出尽责特质","not_meaning":null,"trigger":null,"suppress":null,"related":"base::尽责","seq":1},
        {"layer":"accent","trait_label":"地位维护-驱动","meaning":"在涉及权威和评价的情境下对地位感知强烈","not_meaning":"并非好斗","trigger":"方案评审、绩效考核等评价性场景","suppress":"一对一私下沟通时减弱","related":null,"seq":2},
        {"layer":"accent","trait_label":"归属-亲和","meaning":"在团建和亲密社交中展现亲和与投入","not_meaning":"并非社交焦虑","trigger":"非正式社交场合","suppress":null,"related":null,"seq":3}
    ]"#;

    let llm = MultiStepLlm::new(vec![
        step1_json.to_string(),
        step2_json.to_string(),
        step3_json.to_string(),
    ]);
    let config = InferrerConfig::default();

    // 3. 执行 Phase B（mock 无事件关系 → causal 文本为空，causal_extended_enabled 传 true 与默认一致）
    let phase_b_result =
        run_phase_b_inference(&llm, &storage, &stats, "persona-m4", &config, true).await;

    assert!(
        phase_b_result.is_ok(),
        "Phase B 应成功: {:?}",
        phase_b_result.err()
    );
    let pb = phase_b_result.unwrap();
    assert_eq!(pb.source, PhaseBSource::LlmInference);
    assert!(
        pb.traits_saved >= 2,
        "应至少保存 2 个 trait，实际: {}",
        pb.traits_saved
    );

    // 验证 traits 包含动机驱动的 accent trait
    let traits = storage.list_traits_by_persona("persona-m4").await.unwrap();
    let motive_traits: Vec<_> = traits
        .iter()
        .filter(|t| t.trait_label.contains("地位维护") || t.trait_label.contains("归属"))
        .collect();
    assert!(
        !motive_traits.is_empty(),
        "Phase B 输出应包含动机驱动的 trait"
    );

    // 4. 验证 Phase B 产出的 traits 置信度
    // （Phase C 需要真实事件数据来更新置信度；空事件会触发 compute_confidence→0.0）
    let final_traits = storage.list_traits_by_persona("persona-m4").await.unwrap();
    assert!(!final_traits.is_empty(), "Phase B 应已产出 traits");
    for t in &final_traits {
        assert!(t.confidence > 0.0, "trait '{}' 应有正置信度", t.trait_label);
        assert_eq!(t.status, TraitStatus::Active);
        // 验证层分配正确
        assert!(
            t.layer == TraitLayer::Base
                || t.layer == TraitLayer::Primary
                || t.layer == TraitLayer::Accent,
            "trait '{}' 应有有效的层分配: {:?}",
            t.trait_label,
            t.layer
        );
    }
}

#[tokio::test]
async fn full_pipeline_respects_calibrated_weights_in_output() {
    // 构造含 tentative 事件和多样化动机的 StatsSummary
    let stats = StatsSummary {
        total_events_in: 6,
        total_events_filtered: 5,
        confirmed_count: 3,
        tentative_count: 2,
        discarded_count: 1,
        category_count: 1,
        categories: vec![CategoryStats {
            category: "工作".into(),
            event_count: 5,
            n_eff: 3.0, // 校准权重显著低于原始事件数
            valence_mean: 0.1,
            valence_std: 0.4,
            valence_positive_ratio: 0.55,
            share_mean: 0.5,
            share_std: 0.2,
            presentation_objective_ratio: 0.3,
            presentation_subjective_ratio: 0.4,
            presentation_mixed_ratio: 0.3,
            group_weight: 1.0,
        }],
        cross_category: CrossCategoryMetrics {
            emotional_stability: 0.4,
            narrative_consistency: 0.8,
            attitude_contradiction_count: 0,
            share_skewness: 0.0,
            share_kurtosis: 0.0,
        },
        representative_events: vec![],
        motive_stats: vec![MotiveStats {
            motive: "地位维护".into(),
            event_count: 2,
            n_eff: 1.5,
            valence_mean: 0.2,
            valence_std: 0.3,
            valence_positive_ratio: 0.6,
            share_mean: 0.5,
            share_std: 0.15,
            presentation_objective_ratio: 0.2,
            presentation_subjective_ratio: 0.5,
            presentation_mixed_ratio: 0.3,
            avg_salience: 0.6,
        }],
    };

    let storage = MockStorage::new();
    let step1 = r#"{"工作":{"signal_label":"尽责","evidence_citation":"n_eff=3.0 calibrated","stability_judgment":"stable","sufficient_evidence":true}}"#;
    let step2 = r#"{"base_candidates":["尽责"],"primary_candidates":["尽责"],"accent_candidates":["动机-地位维护-驱动"],"notes":"tentative events half-weighted; motive-driven accent"}"#;
    let step3 = r#"[
        {"layer":"base","trait_label":"尽责","meaning":"工作中尽责","not_meaning":null,"trigger":null,"suppress":null,"related":null,"seq":0},
        {"layer":"accent","trait_label":"动机-地位维护-驱动","meaning":"地位维护驱动行为","not_meaning":null,"trigger":"评价性场景","suppress":null,"related":null,"seq":1}
    ]"#;

    let llm = MultiStepLlm::new(vec![step1.into(), step2.into(), step3.into()]);
    let config = InferrerConfig::default();

    // mock 无事件关系 → causal 文本为空，causal_extended_enabled 传 true 与默认一致
    let result =
        run_phase_b_inference(&llm, &storage, &stats, "persona-m4-cw", &config, true).await;
    assert!(result.is_ok());
    let pb = result.unwrap();

    // 验证 tentative 路径：确认动机维度 accent trait 被生成
    let traits = storage
        .list_traits_by_persona("persona-m4-cw")
        .await
        .unwrap();
    let accent_traits: Vec<_> = traits
        .iter()
        .filter(|t| t.layer == TraitLayer::Accent)
        .collect();
    assert!(
        !accent_traits.is_empty(),
        "校准权重路径下应有 accent trait，tentative events + motives 应产生点缀层"
    );

    // 执行 Phase C
    let conf_cfg = ramaria_memory::inference::confidence::ConfidenceConfig::default();
    let drift_cfg = ramaria_memory::inference::drift::DriftConfig::default();
    let _pc = run_phase_c_update(
        &conf_cfg,
        &drift_cfg,
        &storage,
        "persona-m4-cw",
        &pb.traits,
        &[],
        true,
    )
    .await;
    assert!(_pc.is_ok());
}

#[tokio::test]
async fn mock_infer_fallback_with_m4_stats() {
    // 测试 LLM 失败时降级到 mock_infer，验证动机统计仍被使用
    let stats = make_m4_stats_summary();
    let storage = MockStorage::new();

    // 第一步正常，第二步返回无效 JSON 触发降级
    let step1 = r#"{"工作":{"signal_label":"尽责","evidence_citation":"n_eff=4.2","stability_judgment":"stable","sufficient_evidence":true},"社交":{"signal_label":"亲和","evidence_citation":"n_eff=1.8","stability_judgment":"contextual","sufficient_evidence":false}}"#;
    let step2 = "not valid json"; // 将导致解析失败 → 降级
    let llm = MultiStepLlm::new(vec![step1.into(), step2.into()]);
    let config = InferrerConfig::default();

    // mock 无事件关系 → causal 文本为空，causal_extended_enabled 传 true 与默认一致
    let result =
        run_phase_b_inference(&llm, &storage, &stats, "persona-m4-fallback", &config, true).await;
    assert!(result.is_ok(), "降级路径不应 panic");
    let pb = result.unwrap();

    // 降级应使用 mock_infer
    assert_eq!(pb.source, PhaseBSource::MockFallback, "应降级到 mock_infer");
    assert!(pb.traits_saved > 0, "mock_infer 应生成 trait");

    // mock_infer 应利用动机统计生成动机驱动 trait
    let traits = storage
        .list_traits_by_persona("persona-m4-fallback")
        .await
        .unwrap();
    let motive_traits: Vec<_> = traits
        .iter()
        .filter(|t| t.trait_label.contains("动机-"))
        .collect();
    assert!(
        !motive_traits.is_empty(),
        "降级路径的 mock_infer 应生成动机驱动 trait"
    );
}

// LLM Step3 返回空数组 `[]`（无足够证据）是合法响应，
// 应走 LlmInference（saved=0），而不是误触发 MockFallback 用 mock 数据污染画像
#[tokio::test]
async fn llm_empty_traits_uses_llm_source_not_mock() {
    let stats = make_m4_stats_summary();
    let storage = MockStorage::new();

    let step1 = r#"{"工作":{"signal_label":"尽责","evidence_citation":"n_eff=4.2","stability_judgment":"stable","sufficient_evidence":true},"社交":{"signal_label":"亲和","evidence_citation":"n_eff=1.8","stability_judgment":"contextual","sufficient_evidence":false}}"#;
    let step2 = r#"{"base_candidates":[],"primary_candidates":[],"accent_candidates":[],"notes":"数据不足"}"#;
    let step3 = "[]"; // LLM 明确表示无可推断 traits（复现输入）
    let llm = MultiStepLlm::new(vec![step1.into(), step2.into(), step3.into()]);
    let config = InferrerConfig::default();

    // mock 无事件关系 → causal 文本为空，causal_extended_enabled 传 true 与默认一致
    let result =
        run_phase_b_inference(&llm, &storage, &stats, "persona-m4-empty", &config, true).await;
    assert!(result.is_ok(), "空数组响应不应触发降级 panic");
    let pb = result.unwrap();

    assert_eq!(
        pb.source,
        PhaseBSource::LlmInference,
        "空数组是 LLM 合法响应，不应降级 MockFallback"
    );
    assert_eq!(pb.traits_saved, 0, "LLM 确认无 traits，不伪造 mock 数据");
    assert_eq!(
        storage
            .list_traits_by_persona("persona-m4-empty")
            .await
            .unwrap()
            .len(),
        0,
        "库中不应出现 mock traits"
    );
}
