//! crates/ramaria-memory/src/prompt/layers/tests.rs - //! crates/ramaria-memory/src/prompt/layers.rs - 四层注入结构与预算分配器单元测试
//!
//! 设计特点:
//! - 位于 prompt::layers 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 layers.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

// ---- LayerKind / InjectionBlock ----

#[test]
fn layer_priority_follows_order() {
    // 行为 > 知识 > 表达 > 脉络
    assert!(LayerKind::Behavior.priority() < LayerKind::Knowledge.priority());
    assert!(LayerKind::Knowledge.priority() < LayerKind::Style.priority());
    assert!(LayerKind::Style.priority() < LayerKind::Memory.priority());
}

#[test]
fn injection_block_empty_when_content_blank() {
    let block = InjectionBlock::new(LayerKind::Memory, "# 记忆", "   ".to_string());
    assert!(block.is_empty(), "全空白内容视为空块");
    let block2 = InjectionBlock::new(LayerKind::Memory, "# 记忆", String::new());
    assert!(block2.is_empty());
    let block3 = InjectionBlock::new(LayerKind::Memory, "# 记忆", "内容".to_string());
    assert!(!block3.is_empty());
}

// ---- LayerBudgetConfig ----

#[test]
fn budget_chars_default_is_600() {
    // 1000 tokens × 30% × 2 chars/token = 600 字符
    let cfg = LayerBudgetConfig::default();
    assert_eq!(cfg.budget_chars(), 600);
}

#[test]
fn budget_chars_scales_with_config() {
    let cfg = LayerBudgetConfig {
        system_prompt_reserve_tokens: 2000,
        memory_layer_ratio: 0.25,
    };
    assert_eq!(cfg.budget_chars(), 1000);
}

#[test]
fn budget_chars_at_least_one() {
    let cfg = LayerBudgetConfig {
        system_prompt_reserve_tokens: 0,
        memory_layer_ratio: 0.0,
    };
    assert_eq!(cfg.budget_chars(), 1, "预算至少 1 字符");
}

// ---- allocate_memory_layer_budget ----

#[test]
fn all_within_budget_kept_unchanged() {
    let out = allocate_memory_layer_budget(
        Some("块一内容\n\n块二内容"),
        Some("桥接内容"),
        &["摘要一".to_string(), "摘要二".to_string()],
        Some("相关记忆"),
        1000,
    );
    assert_eq!(out.utt.as_deref(), Some("块一内容\n\n块二内容"));
    assert_eq!(out.bridge.as_deref(), Some("桥接内容"));
    assert_eq!(out.summaries, vec!["摘要一", "摘要二"]);
    assert_eq!(out.rag.as_deref(), Some("相关记忆"));
}

#[test]
fn empty_inputs_yield_empty_budget() {
    let out = allocate_memory_layer_budget(None, None, &[], None, 600);
    assert_eq!(out, MemoryLayerBudget::default());
}

#[test]
fn trim_utt_low_score_blocks_first() {
    // 预算只够一块：高分块（头部）保留，低分块（尾部）整块丢弃
    // 预算 12：摘要(3) + 桥接(4) = 7，剩 5 只够原文第一块（5 字符）
    let out = allocate_memory_layer_budget(
        Some("高分块内容\n\n低分块内容"),
        Some("桥接内容"),
        &["摘要一".to_string()],
        None,
        12,
    );
    assert_eq!(out.utt.as_deref(), Some("高分块内容"), "高分块保留");
    assert_eq!(out.bridge.as_deref(), Some("桥接内容"), "桥接保留");
    assert_eq!(out.summaries, vec!["摘要一"]);
}

#[test]
fn trim_bridge_keeps_recent_tail() {
    // 预算紧张：原文被完全丢弃，桥接截头部保尾部
    // 预算 11：摘要(3) + 桥接保留 8（…+第三行 7）→ 原文(4) 放不下整体丢弃
    let out = allocate_memory_layer_budget(
        Some("原文块"),
        Some("第一行桥接内容\n第二行桥接内容\n第三行桥接内容"),
        &["摘要一".to_string()],
        None,
        11,
    );
    assert_eq!(out.utt, None, "预算不足原文整体丢弃");
    let bridge = out.bridge.expect("桥接应保留尾部");
    assert!(bridge.contains("第三行桥接内容"), "桥接保最近: {bridge}");
    assert!(!bridge.contains("第一行桥接内容"), "桥接截头部: {bridge}");
    assert!(bridge.starts_with('…'), "截断带省略号前缀");
}

#[test]
fn trim_summaries_keeps_recent_drops_oldest() {
    // 预算只够一条摘要：最近的（头部）保留，最旧的（尾部）丢弃
    let out = allocate_memory_layer_budget(
        None,
        None,
        &[
            "最近的摘要内容".to_string(),
            "较旧的摘要内容".to_string(),
            "最旧的摘要内容".to_string(),
        ],
        None,
        8,
    );
    assert_eq!(out.summaries, vec!["最近的摘要内容"], "保最近丢最旧");
}

#[test]
fn trim_rag_at_sentence_boundary() {
    // 预算不足以容纳完整 RAG → 句子边界截断（保前部最相关）
    let out = allocate_memory_layer_budget(
        None,
        None,
        &[],
        Some("第一句相关记忆。第二句相关记忆。第三句相关记忆。"),
        10,
    );
    let rag = out.rag.expect("RAG 应截断保留");
    assert!(rag.starts_with("第一句相关记忆。"), "保前部最相关: {rag}");
    assert!(rag.ends_with('…'), "截断带省略号: {rag}");
}

#[test]
fn zero_budget_yields_nothing() {
    let out = allocate_memory_layer_budget(
        Some("原文"),
        Some("桥接"),
        &["摘要".to_string()],
        Some("记忆"),
        0,
    );
    assert_eq!(out, MemoryLayerBudget::default(), "预算 0 全部不注入");
}

#[test]
fn budget_exhausted_by_priority_order() {
    // 极小预算：只保脉络摘要，其余全部不注入（优先级验证）
    let out = allocate_memory_layer_budget(
        Some("原文内容"),
        Some("桥接内容"),
        &["摘要内容".to_string()],
        Some("记忆内容"),
        4,
    );
    assert_eq!(out.summaries, vec!["摘要内容"], "脉络优先级最高");
    assert_eq!(out.rag, None);
    assert_eq!(out.bridge, None);
    assert_eq!(out.utt, None);
}

#[test]
fn blank_inputs_skipped_not_counted() {
    let out = allocate_memory_layer_budget(
        Some("   "),
        None,
        &["  ".to_string(), "有效摘要".to_string()],
        None,
        600,
    );
    assert_eq!(out.utt, None, "空白原文不注入");
    assert_eq!(out.summaries, vec!["有效摘要"], "空白摘要跳过");
}

// ---- keep_high_score_blocks ----

#[test]
fn keep_blocks_stops_at_first_over_budget() {
    let text = "第一块内容\n\n第二块内容\n\n第三块内容";
    let kept = keep_high_score_blocks(text, 6);
    assert_eq!(kept, "第一块内容", "首块超预算即停");
}

#[test]
fn keep_blocks_charges_separator_to_budget() {
    // 分隔符计费：块各 3 字符，max=7 时块1(3)+分隔(2)+块2(3)=8 > 7 → 只保留块1
    let kept = keep_high_score_blocks("AAA\n\nBBB", 7);
    assert_eq!(kept, "AAA", "分隔符计入预算");
    assert!(kept.chars().count() <= 7, "输出总长 ≤ 预算");
    // max=8 时可容纳两块
    let kept2 = keep_high_score_blocks("AAA\n\nBBB", 8);
    assert_eq!(kept2, "AAA\n\nBBB");
}

#[test]
fn keep_blocks_skips_blank_segments() {
    let text = "块一\n\n\n\n块二";
    let kept = keep_high_score_blocks(text, 100);
    assert_eq!(kept, "块一\n\n块二", "空段折叠");
}

// ---- take_tail ----

#[test]
fn take_tail_within_budget_unchanged() {
    assert_eq!(take_tail("短内容", 100), "短内容");
}

#[test]
fn take_tail_trims_head_keeps_recent_lines() {
    let text = "第一行\n第二行\n第三行";
    let tail = take_tail(text, 6);
    assert!(tail.contains("第三行"), "保最近行: {tail}");
    assert!(!tail.contains("第一行"), "截头部: {tail}");
    assert!(tail.starts_with('…'), "截断前缀提示");
}

#[test]
fn take_tail_single_line_truncated() {
    let tail = take_tail("没有换行的长文本内容", 5);
    assert!(tail.starts_with('…'));
    assert_eq!(tail.chars().count(), 5, "结果总长 ≤ 预算（含省略号）");
}

#[test]
fn take_tail_zero_budget_returns_empty() {
    assert_eq!(take_tail("任何内容", 0), "", "预算 0 不产生省略号占位");
}

#[test]
fn rag_truncation_never_exceeds_budget() {
    // 句子边界恰在窗口末尾：truncate_at_boundary 可能返回 max+1，clamp 后不超支
    let out = allocate_memory_layer_budget(
        Some("原文内容"),
        Some("桥接内容"),
        &["摘要".to_string()],
        Some("第一句。第二句。第三句。"),
        8,
    );
    let rag = out.rag.expect("RAG 应保留");
    assert!(rag.chars().count() <= 5, "RAG 截断不超预算: {rag}");
    // 摘要(2) + RAG(≤5) ≤ 8；桥接/原文因预算耗尽不注入
    assert_eq!(out.summaries, vec!["摘要"]);
}

// ---- 行为层渲染 ----

#[test]
fn behavior_and_knowledge_slots_are_empty_for_now() {
    // 行为槽位：无路由决策（未命中/关闭）时返回 None
    let ctx = PromptContext::default();
    let config = PromptConfig::default();
    assert!(
        render_behavior_block(&ctx, &config).is_none(),
        "行为层未命中/关闭 → 不产生段落"
    );
    assert!(
        render_knowledge_block(&ctx, &PromptConfig::default()).is_none(),
        "无知识事实 → 不产生知识段落"
    );
}

// ---- 行为层渲染辅助 ----

/// 构造带指定 params 的合并决策（与 routing.rs 测试同构）。
fn make_decision_with_params(
    params: BehaviorParams,
    reaction: Option<&str>,
    avoid: &[&str],
) -> MergedDecision {
    use ramaria_core::behavior::{BehaviorRule, BehaviorSituation, RuleSource};
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
        reaction.map(|s| s.to_string()),
        params,
        RuleSource::Auto,
    );
    rule.id = 1;
    MergedDecision {
        primary_rule: rule,
        merged_avoid: avoid.iter().map(|s| s.to_string()).collect(),
        merged_params: params,
    }
}

/// 构造默认偏离参数（-0.42 / 0.82 / 0.65 / 0.58）的合并决策。
fn make_decision(reaction: Option<&str>, avoid: &[&str]) -> MergedDecision {
    make_decision_with_params(
        BehaviorParams {
            emotional_intensity: -0.42,
            proactiveness: 0.82,
            detail_level: 0.65,
            formality: 0.58,
        },
        reaction,
        avoid,
    )
}

#[test]
fn render_behavior_block_hit_renders_rule_section() {
    let decision = make_decision(
        Some("用疲惫但温和的语气回应，先共情再给建议"),
        &["深夜打扰"],
    );
    let ctx = PromptContext {
        behavior_decision: Some(decision),
        ..Default::default()
    };
    let block = render_behavior_block(&ctx, &PromptConfig::default()).expect("命中时应渲染行为块");
    assert_eq!(block.layer, LayerKind::Behavior);
    let content = &block.content;
    assert!(content.contains("## 行为规则"), "小节标题: {content}");
    assert!(content.contains("「加班」、「累」"), "关键词: {content}");
    assert!(
        content.contains("用疲惫但温和的语气回应"),
        "reaction 注入: {content}"
    );
    assert!(
        content.contains("- 表达倾向：语气偏冷 · 更主动一点 · 说细一点"),
        "params 程度词注入: {content}"
    );
    assert!(content.contains("深夜打扰"), "avoid 注入: {content}");
}

#[test]
fn render_behavior_block_candidate_rule_without_reaction() {
    // 候选规则（reaction=None）命中 → 仅表达倾向程度词注入，不产生空规则行
    let decision = make_decision(None, &[]);
    let ctx = PromptContext {
        behavior_decision: Some(decision),
        ..Default::default()
    };
    let block = render_behavior_block(&ctx, &PromptConfig::default())
        .expect("候选规则命中时仍渲染（仅程度词）");
    let content = &block.content;
    assert!(
        content.contains("按表达倾向调整回应"),
        "候选规则占位: {content}"
    );
    assert!(
        content.contains("- 表达倾向：语气偏冷 · 更主动一点 · 说细一点"),
        "params 程度词注入: {content}"
    );
    assert!(
        !content.contains("- 避免"),
        "空 avoid 不输出避免行: {content}"
    );
}

#[test]
fn render_behavior_block_budget_truncates_head_first() {
    // 极紧预算：输出总长 ≤ 预算，截断保前部（规则文本优先）并带 `…`
    let decision = make_decision(Some("这是一段很长的规则文本内容，用于验证预算裁剪"), &["a"]);
    let ctx = PromptContext {
        behavior_decision: Some(decision),
        ..Default::default()
    };
    let config = PromptConfig {
        behavior_block_max_chars: Some(24),
        ..Default::default()
    };
    let block = render_behavior_block(&ctx, &config).expect("命中时渲染");
    assert!(block.content.chars().count() <= 24, "总长 ≤ 预算");
    assert!(block.content.ends_with('…'), "截断提示: {}", block.content);
    assert!(
        block.content.starts_with("## 行为规则"),
        "保前部: {}",
        block.content
    );
}

#[test]
fn render_behavior_block_zero_budget_returns_none() {
    let decision = make_decision(Some("规则文本"), &[]);
    let ctx = PromptContext {
        behavior_decision: Some(decision),
        ..Default::default()
    };
    let config = PromptConfig {
        behavior_block_max_chars: Some(0),
        ..Default::default()
    };
    assert!(
        render_behavior_block(&ctx, &config).is_none(),
        "预算 0 → 不产生空段落"
    );
}

#[test]
fn render_behavior_block_no_decision_is_none() {
    let ctx = PromptContext {
        behavior_decision: None,
        ..Default::default()
    };
    assert!(
        render_behavior_block(&ctx, &PromptConfig::default()).is_none(),
        "未命中/关闭 → None（v1.4 语义等价）"
    );
}

#[test]
fn render_behavior_decision_no_keywords_fallback() {
    use ramaria_core::behavior::{BehaviorRule, BehaviorSituation, RuleSource};
    let rule = BehaviorRule::new(
        "char-0001",
        BehaviorSituation {
            keywords: vec![],
            centroid: None,
            response_centroid: None,
            valence_mean: 0.0,
            valence_std: 0.1,
            sample_count: 5,
            presentation_dist: Vec::new(),
            situation_strength_mean: 2.0,
            time_span_days: 5.0,
            trait_refs: Vec::new(),
        },
        Some("规则文本".to_string()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    let decision = MergedDecision {
        primary_rule: rule,
        merged_avoid: Vec::new(),
        merged_params: BehaviorParams::default(),
    };
    let text = render_behavior_decision(&decision, 400).expect("渲染成功");
    assert!(text.contains("相关话题"), "无关键词回退: {text}");
}

// ---- 表达倾向程度词映射 ----

#[test]
fn behavior_tendency_all_high_terms_in_order() {
    let line = format_behavior_tendency(&BehaviorParams {
        emotional_intensity: 0.9,
        proactiveness: 0.9,
        detail_level: 0.9,
        formality: 0.9,
    })
    .expect("全高时应输出表达倾向行");
    assert_eq!(
        line,
        "- 表达倾向：语气偏热 · 更主动一点 · 说细一点 · 偏正式"
    );
}

#[test]
fn behavior_tendency_all_low_terms_in_order() {
    let line = format_behavior_tendency(&BehaviorParams {
        emotional_intensity: -0.9,
        proactiveness: 0.1,
        detail_level: 0.1,
        formality: 0.1,
    })
    .expect("全低时应输出表达倾向行");
    assert_eq!(line, "- 表达倾向：语气偏冷 · 安静一点 · 说简一点 · 更随意");
}

#[test]
fn behavior_tendency_all_neutral_yields_none() {
    assert_eq!(
        format_behavior_tendency(&BehaviorParams {
            emotional_intensity: 0.5,
            proactiveness: 0.5,
            detail_level: 0.5,
            formality: 0.5,
        }),
        None,
        "全中性不输出表达倾向行"
    );
    // 边界 0.4 / 0.6 视为中性
    assert_eq!(
        format_behavior_tendency(&BehaviorParams {
            emotional_intensity: 0.4,
            proactiveness: 0.6,
            detail_level: 0.4,
            formality: 0.6,
        }),
        None,
        "边界值视为中性"
    );
}

#[test]
fn behavior_tendency_mixed_outputs_hits_only() {
    // 情感 0.42 / 正式 0.58 中性；主动 0.82、详细 0.65 命中高档
    let line = format_behavior_tendency(&BehaviorParams {
        emotional_intensity: 0.42,
        proactiveness: 0.82,
        detail_level: 0.65,
        formality: 0.58,
    })
    .expect("部分命中时应输出表达倾向行");
    assert_eq!(line, "- 表达倾向：更主动一点 · 说细一点");
}

#[test]
fn render_behavior_decision_neutral_params_omit_tendency_line() {
    let decision = make_decision_with_params(
        BehaviorParams {
            emotional_intensity: 0.5,
            proactiveness: 0.5,
            detail_level: 0.5,
            formality: 0.5,
        },
        Some("规则文本"),
        &["深夜打扰"],
    );
    let text = render_behavior_decision(&decision, 400).expect("渲染成功");
    assert!(!text.contains("- 表达倾向"), "全中性不输出倾向行: {text}");
    assert!(text.contains("规则文本"), "规则文本保留: {text}");
    assert!(text.contains("- 避免：深夜打扰"), "avoid 行保留: {text}");
}

#[test]
fn render_behavior_decision_truncation_keeps_head_including_tendency() {
    let decision = make_decision(Some("这是一段很长的规则文本内容，用于验证预算裁剪"), &[]);
    // 预算 55：保前 54 字符（含 `- 表达倾向：` 前缀）+ `…`
    let text = render_behavior_decision(&decision, 55).expect("渲染成功");
    assert!(text.chars().count() <= 55, "总长 ≤ 预算: {text}");
    assert!(text.ends_with('…'), "截断提示: {text}");
    assert!(text.starts_with("## 行为规则"), "保前部: {text}");
    assert!(text.contains("- 表达倾向："), "前部保留程度词行: {text}");
}

// ---- 知识层渲染与预算接线 ----

/// 构造一条 Interests active 事实。
fn knowledge_fact(content: &str) -> ramaria_core::types::PersonaFact {
    use ramaria_core::types::FactSource;
    let mut f = ramaria_core::types::PersonaFact::new(
        "char-0001".into(),
        ramaria_core::types::ProfileField::Interests,
        content.into(),
        FactSource::Event,
    );
    f.keyword_hint = Some("电影,科幻".to_string());
    f
}

/// 默认预算（`knowledge_block_max_chars=None`）回退 800，与既有行为等价：
/// 短事实卡片完整渲染（不因预算接线改变默认输出）。
#[test]
fn render_knowledge_block_default_budget_matches_legacy() {
    let ctx = PromptContext {
        knowledge_facts: vec![knowledge_fact("喜欢科幻电影")],
        ..Default::default()
    };
    let block =
        render_knowledge_block(&ctx, &PromptConfig::default()).expect("知识事实存在时应渲染知识块");
    assert_eq!(block.layer, LayerKind::Knowledge);
    assert_eq!(block.title, "# 知识（知识层，按需）");
    assert!(
        block.content.contains("喜欢科幻电影"),
        "内容: {}",
        block.content
    );
}

/// 显式预算（`knowledge_block_max_chars`）真实生效：长卡片被裁剪到预算内。
#[test]
fn render_knowledge_block_explicit_budget_truncates() {
    let ctx = PromptContext {
        knowledge_facts: vec![knowledge_fact("很喜欢阅读长篇科幻小说")],
        ..Default::default()
    };
    let config = PromptConfig {
        knowledge_block_max_chars: Some(6),
        ..Default::default()
    };
    let block = render_knowledge_block(&ctx, &config).expect("知识事实存在时应渲染知识块");
    assert!(
        block.content.chars().count() <= 7,
        "显式预算裁剪生效: {}",
        block.content
    );
}
