//! crates/ramaria-memory/src/prompt/builder/tests/coordinated.rs - 结构化装配与协调预算
//!
//! 设计特点:
//! - 由 父测试模块 以 mod coordinated; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 结构化装配（render_prompt_parts / 协调预算 assemble_prompt_coordinated）
// =========================================================

/// 行为命中上下文（供行为层注入块出现于结构化输出）。
fn behavior_hit_ctx() -> PromptContext {
    use ramaria_core::behavior::{BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource};
    let mut rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation {
            keywords: vec!["加班".to_string(), "累".to_string()],
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
        Some("先共情再给建议，语气温和".to_string()),
        BehaviorParams {
            emotional_intensity: -0.42,
            proactiveness: 0.82,
            detail_level: 0.65,
            formality: 0.58,
        },
        RuleSource::Auto,
    );
    rule.id = 1;
    let mut ctx = make_full_ctx();
    ctx.behavior_decision = Some(crate::behavior::MergedDecision {
        primary_rule: rule,
        merged_avoid: vec!["深夜打扰".to_string()],
        merged_params: BehaviorParams {
            emotional_intensity: -0.42,
            proactiveness: 0.82,
            detail_level: 0.65,
            formality: 0.58,
        },
    });
    ctx
}

/// 结构化输出与 assemble_prompt 逐字一致（行为等价重构的回归锁）。
#[test]
fn render_prompt_parts_joins_identically_to_assemble() {
    let ctx = behavior_hit_ctx();
    let config = PromptConfig::default();
    let parts = render_prompt_parts(&ctx, &config);
    // 7 个部件（含空块也保留，join 时跳过）
    assert_eq!(parts.len(), 7);
    // 固定骨架恰为 3 个
    let fixed = parts
        .iter()
        .filter(|p| p.kind == PromptPartKind::Fixed)
        .count();
    assert_eq!(fixed, 3);
    let injection_slots: Vec<InjectionSlot> = parts
        .iter()
        .filter_map(|p| match p.kind {
            PromptPartKind::Injection(slot) => Some(slot),
            PromptPartKind::Fixed => None,
        })
        .collect();
    assert_eq!(
        injection_slots,
        vec![
            InjectionSlot::Behavior,
            InjectionSlot::Style,
            InjectionSlot::Knowledge,
            InjectionSlot::Memory,
        ]
    );
    assert_eq!(join_prompt_parts(&parts), assemble_prompt(&ctx, &config));
}

/// 协调预算关闭时 assemble_prompt_coordinated 输出与 assemble_prompt 等价。
#[test]
fn coordinated_disabled_matches_plain_assemble() {
    let ctx = behavior_hit_ctx();
    let config = PromptConfig::default();
    let budget = ramaria_core::config::InjectionBudgetConfig::default(); // enabled=false
    let rag = "用户上次提到喜欢猫。";
    let out = assemble_prompt_coordinated(&ctx, &config, &budget, Some(rag));
    assert_eq!(out.system_prompt, assemble_prompt(&ctx, &config));
    assert_eq!(
        out.memory_context.as_deref(),
        Some(rag),
        "关闭时 RAG 原样保留"
    );
    assert!(out.dropped.is_empty());
}

/// 协调预算开启：总池超限时低优先整块丢弃，system_prompt 不再含被丢段落。
#[test]
fn coordinated_drops_low_priority_layer_blocks() {
    let ctx = behavior_hit_ctx();
    let config = PromptConfig::default();
    // 极小池只保留固定骨架能放下的最高优先注入块；memory(脉络) 先被整块丢弃
    let budget = ramaria_core::config::InjectionBudgetConfig {
        enabled: true,
        max_injection_tokens: 2,
        ..Default::default()
    };
    let out = assemble_prompt_coordinated(&ctx, &config, &budget, None);
    assert!(
        out.dropped.contains(&InjectionSlot::Memory),
        "脉络最低优先先丢"
    );
    // 固定骨架始终保留
    assert!(out.system_prompt.contains("# 能力边界"));
    assert!(out.system_prompt.contains("# 角色（行为层）"));
    assert!(out.system_prompt.contains("# 当前时间"));
    // 脉络块被整块丢弃（无段落标题与子段）
    assert!(
        !out.system_prompt.contains("# 记忆（脉络层）"),
        "脉络块被丢: {}",
        out.system_prompt
    );
    assert!(
        out.injected_tokens <= 2,
        "总注入 ≤ 预算: {}",
        out.injected_tokens
    );
}

/// 协调预算开启且 RAG 被保留：memory_context 返回协调后的 RAG，system_prompt 含注入。
#[test]
fn coordinated_keeps_rag_and_high_priority_layers() {
    let ctx = behavior_hit_ctx();
    let config = PromptConfig::default();
    let budget = ramaria_core::config::InjectionBudgetConfig {
        enabled: true,
        max_injection_tokens: 2000, // 充裕覆盖 RAG + 四层注入
        ..Default::default()
    };
    let rag = "用户上次提到喜欢猫，正在准备搬家。";
    let out = assemble_prompt_coordinated(&ctx, &config, &budget, Some(rag));
    // RAG（最高优先）保留
    assert!(out.memory_context.is_some(), "RAG 基座优先保留");
    // 未丢弃的层仍在 prompt 中；被丢层段落不出现
    assert_eq!(
        out.dropped,
        Vec::<InjectionSlot>::new(),
        "预算充足时无丢弃: {:?}",
        out.dropped
    );
    assert!(out.system_prompt.contains("## 行为规则"), "行为层保留");
    assert!(
        out.system_prompt.contains("# 说话风格（表达层）"),
        "表达层保留"
    );
    assert_eq!(
        out.system_prompt,
        assemble_prompt(&ctx, &config),
        "预算充足时与普通装配等价"
    );
}
