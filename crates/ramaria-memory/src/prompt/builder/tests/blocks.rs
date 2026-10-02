//! crates/ramaria-memory/src/prompt/builder/tests/blocks.rs - 记忆与情境区块渲染
//!
//! 设计特点:
//! - 由 父测试模块 以 mod blocks; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// ---- Insight 块测试 ----

#[test]
fn insight_with_traits() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        traits: vec![
            make_test_trait("乐观", TraitLayer::Base, "总是看到积极的一面"),
            make_test_trait("好奇心强", TraitLayer::Primary, "对新鲜事物充满兴趣"),
        ],
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("基础性格"));
    assert!(result.contains("乐观"));
    assert!(result.contains("好奇心强"));
}

#[test]
fn insight_traits_disabled() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        traits: vec![make_test_trait("乐观", TraitLayer::Base, "积极")],
        ..Default::default()
    };
    let config = PromptConfig {
        include_traits: false,
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &config);
    assert!(!result.contains("乐观"));
}

#[test]
fn insight_with_facts() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        facts: vec![PersonaFact {
            id: 1,
            persona_uid: "char-0001".into(),
            field: ProfileField::Interests,
            content: "喜欢编程、阅读科幻小说".into(),
            source: ramaria_core::types::FactSource::Manual,
            status: ramaria_core::types::FactStatus::Active,
            tier: ramaria_core::types::FactTier::Stable,
            version_of: None,
            confidence: 1.0,
            keyword_hint: None,
            ref_event_id: None,
            ref_l1_id: None,
            created_at: 1000,
            updated_at: 1000,
        }],
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("已知事实"));
    assert!(result.contains("科幻小说"));
}

// ---- Statement 块测试 ----

#[test]
fn statement_with_examples() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        examples: vec![
            make_test_example("你好呀", "嗨！今天想聊点什么呢？😊"),
            make_test_example("你会编程吗？", "当然啦！Python 和 Rust 我都会～"),
        ],
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("## 对话示例"));
    assert!(result.contains("你好呀"));
    assert!(result.contains("😊"));
    assert!(result.contains("Rust"));
}

#[test]
fn statement_disabled() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        examples: vec![make_test_example("你好", "嗨")],
        ..Default::default()
    };
    let config = PromptConfig {
        include_examples: false,
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &config);
    assert!(!result.contains("## 对话示例"));
}

#[test]
fn statement_max_examples_limit() {
    let examples: Vec<PersonaExample> = (0..10)
        .map(|i| make_test_example(&format!("test{i}"), &format!("reply{i}")))
        .collect();
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        examples,
        ..Default::default()
    };
    let config = PromptConfig {
        max_examples: 3,
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("示例 1"));
    assert!(result.contains("示例 2"));
    assert!(result.contains("示例 3"));
    assert!(!result.contains("示例 4"));
}

// ---- Memory 块测试 ----

#[test]
fn memory_with_rag_only() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        memory_context: Some("用户之前提到喜欢猫。".into()),
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    // 无近期摘要 → 无历史对话提示
    assert!(result.contains("无历史对话"));
    // RAG 结果
    assert!(result.contains("喜欢猫"));
    assert!(result.contains("记忆（脉络层）"));
}

#[test]
fn memory_without_rag_shows_placeholder() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        memory_context: None,
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("无历史对话"));
    assert!(result.contains("无相关记忆"));
}

#[test]
fn memory_with_recent_summaries_and_rag() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        recent_session_summaries: vec![
            "讨论了Python异步编程和FastAPI的使用".to_string(),
            "完成了Rust项目的第一个crate发布".to_string(),
            "探讨了AI助手的记忆系统设计".to_string(),
        ],
        memory_context: Some("用户：喜欢猫，养了一只橘猫".into()),
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("近期对话脉络"));
    assert!(result.contains("Python异步编程"));
    assert!(result.contains("Rust项目"));
    assert!(result.contains("相关历史记忆"));
    assert!(result.contains("橘猫"));
    // 跨 session 叙事引导句（多条摘要 + 延续用途）
    assert!(result.contains("你和对方聊过："));
    assert!(result.contains("可据此继续话题"));
}

#[test]
fn memory_single_recent_summary() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        recent_session_summaries: vec!["讨论了天气和出行计划，决定周末去爬山".to_string()],
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("近期对话脉络"));
    assert!(result.contains("你和对方聊过："));
    assert!(result.contains("爬山"));
}

// ---- 跨 session 叙事引导句测试 ----

#[test]
fn cross_session_narrative_single() {
    let summaries = vec!["用户今天学习了Rust编程语言".to_string()];
    let narrative = build_cross_session_narrative(&summaries);
    assert!(narrative.contains("你和对方聊过："));
    assert!(narrative.contains("Rust编程"));
    assert!(!narrative.contains("次对话"));
    assert!(
        !narrative.contains("可据此继续话题"),
        "单条摘要不追加延续提示"
    );
}

#[test]
fn cross_session_narrative_multiple() {
    let summaries = vec![
        "探讨了AI助手的记忆系统".to_string(),
        "完成了Rust项目发布".to_string(),
        "讨论了Python异步编程".to_string(),
    ];
    let narrative = build_cross_session_narrative(&summaries);
    assert!(narrative.contains("你和对方聊过："));
    assert!(narrative.contains("可据此继续话题"), "多条摘要给出延续用途");
}

#[test]
fn cross_session_narrative_empty() {
    let summaries: Vec<String> = vec![];
    let narrative = build_cross_session_narrative(&summaries);
    assert!(narrative.is_empty());
}

// ---- Capacity 块测试 ----

#[test]
fn capacity_custom_boundary() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        knowledge_boundary: Some("你是一个精通 Rust 的专家，但不了解 Python。".into()),
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("精通 Rust"));
}

#[test]
fn capacity_disabled() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        ..Default::default()
    };
    let config = PromptConfig {
        include_knowledge_boundary: false,
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &config);

    assert!(!result.contains("知识边界"));
}

// ---- 当前语境块测试 ----

#[test]
fn context_with_weather() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        current_time_str: Some("2026-06-10".into()),
        weather: Some("晴，25°C".into()),
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("2026-06-10"));
    assert!(result.contains("当前时间"));
    assert!(result.contains("晴"));
}

#[test]
fn context_defaults_to_readable_time() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("当前时间"));
    let after_time = result.split("当前时间：").nth(1).unwrap_or("");
    let time_part = after_time.lines().next().unwrap_or("");
    assert!(
        time_part.chars().filter(|c| c.is_ascii_digit()).count() <= 16,
        "时间部分不应是长整数时间戳: {time_part}"
    );
    assert!(time_part.contains('-'), "应包含日期连字符: {time_part}");
    assert!(time_part.contains(':'), "应包含时间冒号: {time_part}");
}

#[test]
fn context_with_last_active() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        current_time_str: Some("2026-06-16".into()),
        last_active_at: Some("2026-06-13 14:30".into()),
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("上次对话时间"));
    assert!(result.contains("2026-06-13"));
}

// ---- 完整装配 ----

#[test]
fn full_prompt_all_blocks() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        facts: vec![PersonaFact {
            id: 1,
            persona_uid: "char-0001".into(),
            field: ProfileField::Interests,
            content: "编程".into(),
            source: ramaria_core::types::FactSource::Manual,
            status: ramaria_core::types::FactStatus::Active,
            tier: ramaria_core::types::FactTier::Stable,
            version_of: None,
            confidence: 1.0,
            keyword_hint: None,
            ref_event_id: None,
            ref_l1_id: None,
            created_at: 1000,
            updated_at: 1000,
        }],
        traits: vec![make_test_trait("乐观", TraitLayer::Base, "积极")],
        examples: vec![make_test_example("你好", "嗨！")],
        recent_session_summaries: vec!["之前讨论了Rust编程".to_string()],
        memory_context: Some("用户：喜欢猫".into()),
        knowledge_boundary: Some("知识边界测试".into()),
        current_time_str: Some("2026-06-10".into()),
        last_active_at: Some("2026-06-08".into()),
        weather: Some("晴".into()),
        chat_style_rules: Some("测试回复规则".into()),
        utt_context: None,           // 默认无原文片段
        bridge_context: None,        // 默认无桥接内容
        behavior_decision: None,     // 默认无行为路由决策
        knowledge_facts: Vec::new(), // 默认无知识事实
        style_rule_text: None,       // 默认无自动风格规则（v1.6 语义等价）
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    // CRISPE 框架所有块
    assert!(result.contains("小明"), "Role 块缺失");
    assert!(result.contains("## 对话示例"), "对话示例子段缺失");
    assert!(result.contains("近期对话脉络"), "Memory 近期对话脉络缺失");
    assert!(result.contains("相关历史记忆"), "Memory 相关历史记忆缺失");
    assert!(result.contains("知识边界"), "Capacity 知识边界缺失");
    assert!(result.contains("当前时间"), "语境块缺失");
    assert!(result.contains("测试回复规则"), "Experiment 块缺失");
    // 跨 session 上下文
    assert!(result.contains("Rust编程"), "近期摘要未注入");
    assert!(result.contains("上次对话时间"), "最后活跃时间未注入");
}
