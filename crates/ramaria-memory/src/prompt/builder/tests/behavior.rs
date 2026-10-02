//! crates/ramaria-memory/src/prompt/builder/tests/behavior.rs - 行为层装配
//!
//! 设计特点:
//! - 由 父测试模块 以 mod behavior; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 行为层装配
// =========================================================

/// 构造行为路由合并决策（与 layers.rs 测试同构）。
fn make_behavior_decision() -> crate::behavior::MergedDecision {
    use ramaria_core::behavior::{BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource};
    let mut rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation {
            keywords: vec!["加班".to_string()],
            centroid: None,
            response_centroid: None,
            valence_mean: -0.5,
            valence_std: 0.2,
            sample_count: 6,
            presentation_dist: Vec::new(),
            situation_strength_mean: 3.0,
            time_span_days: 10.0,
            trait_refs: Vec::new(),
        },
        Some("先共情再给建议，语气疲惫但温和".to_string()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    rule.id = 1;
    crate::behavior::MergedDecision {
        primary_rule: rule,
        merged_avoid: vec!["深夜打扰".to_string()],
        merged_params: BehaviorParams {
            emotional_intensity: -0.4,
            proactiveness: 0.7,
            detail_level: 0.6,
            formality: 0.3,
        },
    }
}

#[test]
fn behavior_block_injected_between_role_and_style() {
    // 命中：行为块注入，位置在角色段与说话风格段之间（注入优先级 行为 > 表达）
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        behavior_decision: Some(make_behavior_decision()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(result.contains("## 行为规则"), "行为块缺失: {result}");
    assert!(result.contains("先共情再给建议"), "reaction 缺失");
    assert!(result.contains("深夜打扰"), "avoid 缺失");
    let role_pos = result.find("# 角色（行为层）").expect("角色段存在");
    let behavior_pos = result.find("## 行为规则").expect("行为块存在");
    let style_pos = result.find("# 说话风格（表达层）").expect("表达段存在");
    assert!(
        role_pos < behavior_pos && behavior_pos < style_pos,
        "行为块应位于角色段之后、表达段之前（行为 > 表达）"
    );
}

#[test]
fn behavior_block_absent_without_decision_equals_v1_4() {
    // 未命中/关闭（decision=None）→ 无行为块，输出不产生段落
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        behavior_decision: None,
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(!result.contains("## 行为规则"), "未命中不产生行为块");
    assert!(!result.contains("先共情再给建议"), "规则文本不泄漏");
}

#[test]
fn behavior_block_budget_applied_in_assemble() {
    // 极紧预算：行为块被截断到预算内（§8.3 固定小比例）
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        behavior_decision: Some(make_behavior_decision()),
        ..Default::default()
    };
    let config = PromptConfig {
        behavior_block_max_chars: Some(24),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &config);
    let behavior_pos = result.find("## 行为规则").expect("行为块存在");
    // 截取行为块文本（到下一个段落标题或结尾）
    let tail = &result[behavior_pos..];
    let block_len = tail.find("\n\n# ").unwrap_or(tail.len());
    let block = &tail[..block_len];
    assert!(block.chars().count() <= 24, "行为块 ≤ 预算: {block}");
    assert!(block.ends_with('…'), "截断提示: {block}");
}

#[test]
fn empty_context_produces_valid_prompt() {
    let ctx = PromptContext::default();
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(!result.is_empty());
    assert!(result.contains("Ramaria"));
    assert!(result.contains("无历史对话"));
}
