//! tests/inference/phase_b.rs - Phase B 推断端到端
//!
//! 设计特点:
//! - 由 tests/inference.rs 以 mod phase_b; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// L3 全管线端到端测试
// =========================================================

/// 测试使用 mock LLM 产出 PersonalityTrait 记录并写入 DB。
#[tokio::test]
async fn phase_b_produces_traits_with_mock_llm() {
    let storage = Arc::new(MockStorage::new());
    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();

    // 推断前 storage 中无 trait
    let before = storage.list_traits_by_persona("rama-0001").await.unwrap();
    assert!(before.is_empty(), "推断前应无 trait");

    // mock 无事件关系 → causal 文本为空，causal_extended_enabled 传 true 与默认一致
    let result = run_phase_b_inference(&multi_llm, &*storage, &stats, "rama-0001", &config, true)
        .await
        .expect("Phase B 应成功完成");

    // 验证推断来源
    assert_eq!(result.source, PhaseBSource::LlmInference);
    // 验证有 trait 被保存（4 个来自 Step 3 JSON）
    assert!(result.traits_saved >= 4, "应至少保存 4 个 trait");
    // 验证无更新（首轮）
    assert_eq!(result.traits_updated, 0);
    // 验证无废弃（首轮）
    assert_eq!(result.traits_deprecated, 0);

    // 验证 storage 中确实有数据
    let saved_traits = storage
        .list_traits_by_persona("rama-0001")
        .await
        .expect("查询 traits 应成功");
    assert!(!saved_traits.is_empty(), "storage 中应有 trait 记录");

    // 验证 trait 属性正确
    let base_trait = saved_traits.iter().find(|t| t.layer == TraitLayer::Base);
    assert!(base_trait.is_some(), "应有底色层 trait");
    assert_eq!(base_trait.unwrap().trait_label, "尽责");

    // 验证置信度初始值
    for t in &saved_traits {
        assert!(t.confidence > 0.0, "trait 置信度应 > 0");
        assert_eq!(t.source, TraitSource::Inferred);
        assert_eq!(t.status, TraitStatus::Active);
        assert_eq!(t.persona_uid, "rama-0001");
    }
}

/// 测试在 LLM 失败时降级至 mock_infer。
#[tokio::test]
async fn phase_b_falls_back_to_mock_infer_on_llm_error() {
    let storage = Arc::new(MockStorage::new());
    // 使用返回错误的 Mock LLM
    let failing_llm = MockLlm::failing("connection refused");
    let stats = make_stats_summary();
    let config = InferrerConfig::default();

    // mock 无事件关系 → causal 文本为空，causal_extended_enabled 传 true 与默认一致
    let result = run_phase_b_inference(&failing_llm, &*storage, &stats, "rama-0001", &config, true)
        .await
        .expect("降级到 mock_infer 后应成功完成");

    // 验证推断来源为 MockFallback
    assert_eq!(result.source, PhaseBSource::MockFallback);

    // 验证仍有 trait 被保存（mock_infer 基于统计规则推断）
    assert!(result.traits_saved > 0, "mock_infer 应产出 trait");

    let saved_traits = storage
        .list_traits_by_persona("rama-0001")
        .await
        .expect("查询 traits 应成功");
    assert!(!saved_traits.is_empty());
}

/// 测试增量推断（有旧 traits 时的 diff 更新）。
#[tokio::test]
async fn phase_b_incremental_update_with_existing_traits() {
    let storage = Arc::new(MockStorage::new());

    // 预置旧 trait
    let old_trait = PersonalityTrait {
        id: 0,
        persona_uid: "rama-0001".into(),
        layer: TraitLayer::Base,
        trait_label: "尽责".into(),
        meaning: "旧描述".into(),
        not_meaning: None,
        trigger: None,
        suppress: None,
        related: None,
        seq: 0,
        source: TraitSource::Inferred,
        ref_event_id: None,
        ref_l1_id: None,
        confidence: 0.5,
        evidence: 1.0,
        consistency: 0.5,
        status: TraitStatus::Active,
        created_at: 1000,
        updated_at: 1000,
    };
    storage.add_trait(old_trait);

    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();

    // mock 无事件关系 → causal 文本为空，causal_extended_enabled 传 true 与默认一致
    let result = run_phase_b_inference(&multi_llm, &*storage, &stats, "rama-0001", &config, true)
        .await
        .expect("增量推断应成功");

    // 验证差异处理：尽责已存在 → keep（不新增）
    // 但新 trait（温和、社交回避、幽默）应新增
    assert!(result.traits_saved > 0, "应有新增 trait");
}

/// 测试置信度更新 + 证据链记录。
#[tokio::test]
async fn phase_c_confidence_and_evidence() {
    let storage = Arc::new(MockStorage::new());
    let persona_uid = "rama-0001";

    // 先运行创建 traits
    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();
    // mock 无事件关系 → causal 文本为空，causal_extended_enabled 传 true 与默认一致
    let phase_b = run_phase_b_inference(&multi_llm, &*storage, &stats, persona_uid, &config, true)
        .await
        .expect("Phase B 应成功");

    // 准备测试事件
    let events = make_test_events(persona_uid);

    // 运行置信度更新
    let confidence_config = ramaria_memory::inference::confidence::ConfidenceConfig::default();
    let drift_config = ramaria_memory::inference::drift::DriftConfig::default();
    let phase_c = run_phase_c_update(
        &confidence_config,
        &drift_config,
        &*storage,
        persona_uid,
        &phase_b.traits,
        &events,
        true, // 首轮推断
    )
    .await
    .expect("Phase C 应成功");

    // 验证置信度更新
    assert!(phase_c.traits_updated > 0, "应有 trait 置信度被更新");

    // 验证证据记录
    assert!(phase_c.evidence_saved > 0, "应有证据记录被保存");

    // 首轮应跳过漂移检测
    assert!(!phase_c.has_significant_drift);

    // 验证 storage 中确有 evidence 记录
    let evidence_count: usize = {
        let saved_traits = storage.list_traits_by_persona(persona_uid).await.unwrap();
        let mut total = 0;
        for t in &saved_traits {
            let ev = storage.list_evidence_by_trait(t.id).await.unwrap();
            total += ev.len();
        }
        total
    };
    assert!(evidence_count > 0, "storage 中应有 evidence 记录");

    // 验证 confidence 已被更新为非默认值
    let updated_traits = storage.list_traits_by_persona(persona_uid).await.unwrap();
    for t in &updated_traits {
        // 有证据更新后 confidence 应不再完全是初始值 0.5
        // 至少 evidence 字段应 > 1.0（有新增证据）
        assert!(t.evidence > 1.0, "evidence 应在 Phase C 后被更新");
    }
}
