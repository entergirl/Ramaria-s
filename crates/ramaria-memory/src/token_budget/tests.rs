//! crates/ramaria-memory/src/token_budget/tests.rs - //! crates/ramaria-memory/src/token_budget.rs - Token 预算管理模块单元测试
//!
//! 设计特点:
//! - 位于 token_budget 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 token_budget.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

// ---- estimate_tokens ----

/// estimate_tokens 各输入参数化验证。
#[test]
fn estimate_tokens_cases() {
    // (input, expected)：CJK 按 2 字符/token，拉丁按 4 字符/token，向上取整
    let cases = [
        ("", 0),
        ("你好世界", 2),    // 4 CJK → 2
        ("Hello World", 3), // 11 latin → 3
        ("你好 World", 3),  // 2/2 + 6/4 = 1 + 2
        ("a", 1),
        ("中", 1),
    ];
    for (input, expected) in cases {
        assert_eq!(estimate_tokens(input), expected, "input={input:?}");
    }
    // 长文本应有合理的 token 数
    let text = "这是一段较长的中文文本用于测试token估算的准确性。".repeat(10);
    assert!(estimate_tokens(&text) > 50, "长文本应有合理的 token 数");
}

// ---- truncate_at_boundary ----

#[test]
fn truncate_within_limit_unchanged() {
    let text = "短文本。";
    let result = truncate_at_boundary(text, 100);
    assert_eq!(result, text);
}

#[test]
fn truncate_at_period() {
    let text = "第一句话。第二句话。第三句话。";
    let result = truncate_at_boundary(text, 10);
    // "第一句话。" = 5 chars → fits within 10
    assert!(result.ends_with('…'));
    assert!(result.starts_with("第一句话。"));
}

#[test]
fn truncate_at_newline() {
    let text = "第一行\n第二行\n第三行";
    let result = truncate_at_boundary(text, 8);
    assert!(result.ends_with('…'));
    assert!(result.contains("第一行\n"));
}

#[test]
fn truncate_no_boundary_falls_back_to_whitespace() {
    let text = "Hello World from Rust";
    // 24 chars, max 10 → "Hello Worl…" (last space after "Hello")
    let result = truncate_at_boundary(text, 10);
    assert!(result.ends_with('…'));
    assert!(result.starts_with("Hello"));
}

#[test]
fn truncate_no_boundary_no_whitespace() {
    let text = "abcdefghijklmnopqrstuvwxyz"; // no boundaries
    let result = truncate_at_boundary(text, 5);
    assert!(result.ends_with('…'));
    assert_eq!(result.chars().count(), 6); // 5 chars + '…'
}

// ---- 类型计数 ----

#[test]
fn char_type_counting() {
    let (cjk, latin, other) = count_char_types("你好World！");
    assert_eq!(cjk, 2); // "你好"
    assert_eq!(latin, 5); // "World"
    assert_eq!(other, 1); // "！" (fullwidth)
}

// ---- apply_token_budget ----

#[test]
fn budget_small_window_preserves_user_message() {
    let config = TokenBudgetConfig::new(500, 256);
    let system_prompt = "你是一个测试助手。";
    let memory = Some("相关记忆：用户之前提到过喜欢编程。");
    let history = vec![
        ChatMessage {
            role: ramaria_core::types::MessageRole::User,
            content: "你好".to_string(),
        },
        ChatMessage {
            role: ramaria_core::types::MessageRole::Assistant,
            content: "你好！有什么可以帮你的？".to_string(),
        },
    ];
    let user_message = "今天天气怎么样？";

    let result = apply_token_budget(system_prompt, memory, &history, user_message, &config);

    // 用户消息始终保留 → system_prompt 应存在
    assert!(!result.system_prompt.is_empty());
    // estimated_tokens 应在预算内
    assert!(
        result.estimated_tokens <= config.context_window,
        "estimated {} > {}",
        result.estimated_tokens,
        config.context_window
    );
}

#[test]
fn budget_large_window_preserves_all() {
    let config = TokenBudgetConfig::new(10000, 512);
    let system_prompt = "你是一个测试助手。";
    let memory = Some("相关记忆。");
    let history = vec![ChatMessage {
        role: ramaria_core::types::MessageRole::User,
        content: "你好".to_string(),
    }];
    let user_message = "测试消息";

    let result = apply_token_budget(system_prompt, memory, &history, user_message, &config);

    // 大窗口应保留所有内容
    assert_eq!(result.system_prompt, system_prompt);
    assert_eq!(result.memory_context.as_deref(), memory);
    assert_eq!(result.history.len(), 1);
}

#[test]
fn budget_trims_history_when_tight() {
    // 120 token 窗口，256 输出 → budget 不够
    let config = TokenBudgetConfig::new(120, 100);
    let system_prompt = "助手";
    let memory = None;
    let history = vec![
        ChatMessage {
            role: ramaria_core::types::MessageRole::User,
            content: "非常长的消息".repeat(20),
        },
        ChatMessage {
            role: ramaria_core::types::MessageRole::Assistant,
            content: "回复".to_string(),
        },
    ];
    let user_message = "hi";

    let result = apply_token_budget(system_prompt, memory, &history, user_message, &config);

    // output reserve + user + system consumes most of 120 → history should be empty
    assert!(
        result.history.is_empty() || result.history.len() < 2,
        "tight budget should trim history"
    );
    assert!(
        result.estimated_tokens <= config.context_window,
        "estimated {} > {}",
        result.estimated_tokens,
        config.context_window
    );
}

#[test]
fn budget_zero_history_budget() {
    // context_window(50) < output_reserve(256): unrealistic edge case
    // → flexible_budget = 0 → memory and history should be empty
    let config = TokenBudgetConfig::new(50, 256);
    let result = apply_token_budget(
        "助手",
        Some("记忆"),
        &[ChatMessage {
            role: ramaria_core::types::MessageRole::User,
            content: "旧消息".to_string(),
        }],
        "新消息",
        &config,
    );
    // flexible_budget = 0 → history and memory should be empty
    assert!(result.history.is_empty());
    assert!(result.memory_context.is_none());
    // Note: estimated_tokens may exceed context_window when output_reserve
    // alone is larger than the window — this is expected for the edge case.
}

#[test]
fn estimate_tokens_english_text() {
    let text =
        "The quick brown fox jumps over the lazy dog. This is a longer sentence for testing.";
    let tokens = estimate_tokens(text);
    // 87 chars mostly latin → ~87/4 ≈ 22 tokens
    assert!((15..=30).contains(&tokens), "got {tokens}");
}

// ---- 注入协调预算（allocate_injection_budget） ----

/// 构造启用/停用 + 指定总池上限的协调配置。
fn cfg(enabled: bool, max_tokens: usize) -> InjectionBudgetConfig {
    InjectionBudgetConfig {
        enabled,
        max_injection_tokens: max_tokens,
        ..Default::default()
    }
}

/// 固定 3 token 的中文测试段（6 汉字 ≈ 3 token）。
fn zh3(text: &str, slot: InjectionSlot) -> (InjectionSlot, String) {
    assert_eq!(text.chars().count(), 6, "helper 需要 6 汉字段: {text}");
    (slot, text.to_string())
}

#[test]
fn coordinated_disabled_keeps_everything_unchanged() {
    // enabled=false（v1.7 等价路径的防御）：不做任何裁剪，原样保留
    let layers = vec![
        zh3("行为规则文本", InjectionSlot::Behavior),
        zh3("知识卡片内容", InjectionSlot::Knowledge),
        zh3("表达风格示例", InjectionSlot::Style),
        zh3("脉络最近摘要", InjectionSlot::Memory),
    ];
    let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &cfg(false, 4));
    assert_eq!(out.kept, layers, "关闭时各层原样保留");
    assert_eq!(out.memory_context.as_deref(), Some("记忆摘要内容"));
    assert!(out.dropped.is_empty());
    assert!(!out.fallback_truncated);
}

#[test]
fn coordinated_within_budget_keeps_all() {
    let layers = vec![
        zh3("行为规则文本", InjectionSlot::Behavior),
        zh3("知识卡片内容", InjectionSlot::Knowledge),
        zh3("表达风格示例", InjectionSlot::Style),
        zh3("脉络最近摘要", InjectionSlot::Memory),
    ];
    // 5 项各 3 token：总池 15 → 全部保留
    let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &cfg(true, 15));
    assert_eq!(out.kept.len(), 4);
    assert!(out.memory_context.is_some());
    assert!(out.dropped.is_empty());
    assert_eq!(out.injected_tokens, 15);
}

#[test]
fn coordinated_over_budget_drops_lowest_priority_first() {
    // 默认 order：rag > behavior > knowledge > style > memory
    // 总池 9 → rag + behavior + knowledge 恰好装满；style/memory 先被丢
    let layers = vec![
        zh3("行为规则文本", InjectionSlot::Behavior),
        zh3("知识卡片内容", InjectionSlot::Knowledge),
        zh3("表达风格示例", InjectionSlot::Style),
        zh3("脉络最近摘要", InjectionSlot::Memory),
    ];
    let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &cfg(true, 9));
    let slots: Vec<InjectionSlot> = out.kept.iter().map(|(s, _)| *s).collect();
    assert_eq!(
        slots,
        vec![InjectionSlot::Behavior, InjectionSlot::Knowledge],
        "高优先注入块保留（RAG 走 memory_context 独立通道）"
    );
    assert_eq!(
        out.dropped,
        vec![InjectionSlot::Style, InjectionSlot::Memory],
        "低优先通道先被整块丢弃"
    );
    assert_eq!(out.memory_context.as_deref(), Some("记忆摘要内容"));
    assert_eq!(out.injected_tokens, 9, "总注入 ≤ 预算");
}

#[test]
fn coordinated_custom_order_and_unlisted_drop_first() {
    // order = [style, memory, rag]；behavior/knowledge 未列出 → 最先被丢
    let mut c = cfg(true, 9);
    c.order = vec![
        InjectionSlot::Style,
        InjectionSlot::Memory,
        InjectionSlot::Rag,
    ];
    let layers = vec![
        zh3("行为规则文本", InjectionSlot::Behavior),
        zh3("知识卡片内容", InjectionSlot::Knowledge),
        zh3("表达风格示例", InjectionSlot::Style),
        zh3("脉络最近摘要", InjectionSlot::Memory),
    ];
    let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &c);
    let slots: Vec<InjectionSlot> = out.kept.iter().map(|(s, _)| *s).collect();
    assert_eq!(
        slots,
        vec![InjectionSlot::Style, InjectionSlot::Memory],
        "列出的高优先注入块保留（RAG 走 memory_context 独立通道）"
    );
    assert_eq!(
        out.dropped,
        vec![InjectionSlot::Behavior, InjectionSlot::Knowledge],
        "未列出通道比所有列出通道更低优先"
    );
    assert_eq!(out.memory_context.as_deref(), Some("记忆摘要内容"));
    assert_eq!(out.injected_tokens, 9);
}

#[test]
fn coordinated_total_never_exceeds_budget_across_budget_levels() {
    // 各层/rag 长度不等（中英混合），遍历预算档位断言总注入 ≤ 预算
    let layers = vec![
        (
            InjectionSlot::Behavior,
            "行为规则：共情优先、避免说教、简短回应。".to_string(),
        ),
        (
            InjectionSlot::Knowledge,
            "对方喜欢科幻电影与编程，最近在学习 Rust。".to_string(),
        ),
        (
            InjectionSlot::Style,
            "Hello World! 语气轻松活泼一些。".to_string(),
        ),
        (
            InjectionSlot::Memory,
            "上次聊了出行计划，气氛轻松愉快。".to_string(),
        ),
    ];
    let rag = "用户提到喜欢猫，养了一只橘猫，最近想换工作。";
    let total = layers
        .iter()
        .map(|(_, c)| estimate_tokens(c))
        .sum::<usize>()
        + estimate_tokens(rag);

    for budget in [0usize, 1, 3, 5, 10, 20, 50, 1000] {
        let out = allocate_injection_budget(&layers, Some(rag), &cfg(true, budget));
        assert!(
            out.injected_tokens <= budget,
            "总注入 {} > 预算 {budget}",
            out.injected_tokens
        );
        // budget=0 时全丢；budget 极大时全保留
        if budget == 0 {
            assert!(out.kept.is_empty());
            assert!(out.memory_context.is_none());
        }
        if budget >= total {
            assert!(out.dropped.is_empty(), "大预算不应丢弃");
        }
    }
}

#[test]
fn coordinated_zero_budget_drops_all() {
    let layers = vec![
        zh3("行为规则文本", InjectionSlot::Behavior),
        zh3("脉络最近摘要", InjectionSlot::Memory),
    ];
    let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &cfg(true, 0));
    assert!(out.kept.is_empty());
    assert!(out.memory_context.is_none());
    assert_eq!(out.injected_tokens, 0);
    assert_eq!(
        out.dropped,
        vec![InjectionSlot::Behavior, InjectionSlot::Memory],
        "预算 0 → 全部通道丢弃"
    );
}

#[test]
fn coordinated_empty_contents_skipped_without_dropping() {
    let layers = vec![
        (InjectionSlot::Behavior, "   ".to_string()),
        (InjectionSlot::Knowledge, String::new()),
    ];
    let out = allocate_injection_budget(&layers, Some("   "), &cfg(true, 100));
    assert!(out.kept.is_empty(), "空白注入块不占预算");
    assert!(out.memory_context.is_none(), "空白 RAG 视为无注入");
    assert!(out.dropped.is_empty());
    assert_eq!(out.injected_tokens, 0);
}

#[test]
fn coordinated_rag_dropped_when_low_priority_and_over_budget() {
    // rag 未列入 order → 低优先；预算只够 behavior → rag 被整体丢弃
    let mut c = cfg(true, 3);
    c.order = vec![InjectionSlot::Behavior];
    let layers = vec![zh3("行为规则文本", InjectionSlot::Behavior)];
    let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &c);
    assert_eq!(out.kept.len(), 1, "behavior 保留");
    assert!(out.memory_context.is_none(), "RAG 被整块丢弃");
    assert_eq!(out.dropped, vec![InjectionSlot::Rag]);
}

#[test]
fn coordinated_single_huge_layer_fallback_truncation() {
    // 唯一高优先通道单块超总池 → 兜底句子边界截断，估算 ≤ 预算
    let mut c = cfg(true, 10);
    c.order = vec![InjectionSlot::Behavior];
    let huge = "行为规则".to_string() + &"共情回应注意语气自然避免机械说教。".repeat(10);
    assert!(estimate_tokens(&huge) > 10, "构造应超预算");
    let layers = vec![(InjectionSlot::Behavior, huge)];
    let out = allocate_injection_budget(&layers, None, &c);
    assert!(out.fallback_truncated, "单块超池触发兜底截断");
    assert_eq!(out.kept.len(), 1);
    let kept_tokens = estimate_tokens(&out.kept[0].1);
    assert!(kept_tokens <= 10, "兜底截断后 {kept_tokens} ≤ 预算");
    assert!(out.dropped.is_empty(), "最高优先兜底不算丢弃");
}

#[test]
fn coordinated_rag_independent_cap_trims_first() {
    // max_rag_tokens > 0：先做 RAG 独立上限截断，再入总池
    let mut c = cfg(true, 100);
    c.max_rag_tokens = 6;
    let long_rag = "一二三四五六七八九十甲乙丙丁戊己庚辛壬癸子丑寅卯".to_string();
    assert!(estimate_tokens(&long_rag) > 6);
    let out = allocate_injection_budget(&[], Some(&long_rag), &c);
    let rag = out.memory_context.expect("RAG 截断保留");
    assert!(!rag.is_empty());
    assert!(
        estimate_tokens(&rag) <= 6,
        "RAG 独立上限截断: {}",
        estimate_tokens(&rag)
    );
    assert!(out.fallback_truncated);
}

#[test]
fn coordinated_utf8_multibyte_safe_on_truncation() {
    // UTF-8 多字节（emoji）裁剪不 panic、结果合法且不超预算换算字符
    let mut c = cfg(true, 4);
    c.order = vec![InjectionSlot::Memory];
    let emoji = "😀😀😀😀😀😀😀😀😀😀".to_string();
    assert!(estimate_tokens(&emoji) > 4);
    let layers = vec![(InjectionSlot::Memory, emoji.clone())];
    let out = allocate_injection_budget(&layers, None, &c);
    assert!(out.fallback_truncated);
    let kept = out.kept.first().map(|(_, t)| t.as_str()).unwrap_or("");
    assert!(!kept.is_empty());
    assert!(
        kept.chars().count() <= 8,
        "字符安全截断（≤ 预算×2 chars）: {}",
        kept
    );
    assert!(estimate_tokens(kept) <= 4, "估算 ≤ 预算");
    // 输入保持合法（无中间 panic、无替换符）
    assert!(!kept.contains('\u{FFFD}'), "不应产生替换符");
}
