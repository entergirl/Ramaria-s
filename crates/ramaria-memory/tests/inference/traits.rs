//! tests/inference/traits.rs - System Prompt Block A
//!
//! 设计特点:
//! - 由 tests/inference.rs 以 mod traits; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// System Prompt Block A 验证
// =========================================================

/// 验证 L3 推断后的 trait 包含结构化性格标签，可用于 System Prompt Block A。
#[tokio::test]
async fn traits_have_structured_labels_for_system_prompt() {
    let storage = Arc::new(MockStorage::new());
    let persona_uid = "rama-0001";

    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();
    // mock 无事件关系 → causal 文本为空，causal_extended_enabled 传 true 与默认一致
    run_phase_b_inference(&multi_llm, &*storage, &stats, persona_uid, &config, true)
        .await
        .expect("Phase B 应成功");

    let traits = storage.list_traits_by_persona(persona_uid).await.unwrap();

    // 验证三层模型齐全
    let has_base = traits.iter().any(|t| t.layer == TraitLayer::Base);
    let has_primary = traits.iter().any(|t| t.layer == TraitLayer::Primary);
    let has_accent = traits.iter().any(|t| t.layer == TraitLayer::Accent);

    assert!(has_base, "应包含底色层 trait");
    assert!(has_primary, "应包含主色调层 trait");
    assert!(has_accent, "应包含点缀层 trait");

    // 验证每个 trait 都有用于 System Prompt 的必要字段
    for t in &traits {
        assert!(!t.trait_label.is_empty(), "trait_label 不应为空");
        assert!(!t.meaning.is_empty(), "meaning 不应为空");
        // accent trait 应有 trigger
        if t.layer == TraitLayer::Accent {
            assert!(t.trigger.is_some(), "accent trait 应有 trigger 字段");
        }
    }
}

/// 验证 L3 推断后产物可组装为 Block A 格式文本。
#[tokio::test]
async fn traits_can_format_as_block_a_text() {
    let storage = Arc::new(MockStorage::new());
    let persona_uid = "rama-0001";

    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();
    // mock 无事件关系 → causal 文本为空，causal_extended_enabled 传 true 与默认一致
    run_phase_b_inference(&multi_llm, &*storage, &stats, persona_uid, &config, true)
        .await
        .expect("Phase B 应成功");

    let traits = storage.list_traits_by_persona(persona_uid).await.unwrap();

    // 模拟 build_system_prompt_with_context 中 Block A 的格式化逻辑
    let block_a = format_traits_for_prompt(&traits);

    // 验证 Block A 包含关键性格标签
    assert!(block_a.contains("尽责"), "Block A 应包含底色 trait");
    assert!(block_a.contains("温和"), "Block A 应包含主色调 trait");
    assert!(
        block_a.contains("社交回避") || block_a.contains("幽默"),
        "Block A 应包含点缀 trait"
    );

    // 验证 Block A 包含结构化信息
    assert!(block_a.contains("底色"), "Block A 应有层级标签");
    assert!(
        block_a.contains("重视承诺"),
        "Block A 应包含 trait 的具体含义说明"
    );
}
