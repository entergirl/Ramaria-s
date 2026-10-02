//! crates/ramaria-memory/src/prompt/builder/tests/template.rs - 模板精简与语义等价回归
//!
//! 设计特点:
//! - 由 父测试模块 以 mod template; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 模板精简与语义等价回归
// =========================================================

/// 模板结构映射表（`TEMPLATE_LAYER_MAP`）与四层模板常量一致（文档化核对）。
#[test]
fn template_layer_map_matches_rendered_paragraphs() {
    // 映射表覆盖四层 + 能力边界 + 当前时间
    let titles: Vec<&str> = TEMPLATE_LAYER_MAP.iter().map(|(t, _, _)| *t).collect();
    assert!(titles.contains(&"# 能力边界"));
    assert!(titles.contains(&"# 角色（行为层）"));
    assert!(titles.contains(&"# 说话风格（表达层）"));
    assert!(titles.contains(&"# 知识（知识层，按需）"));
    assert!(titles.contains(&"# 记忆（脉络层）"));
    assert!(titles.contains(&"# 当前时间"));

    // 模板常量包含全部占位符（结构可机械核对）
    for (i, placeholder) in [
        "capacity",
        "role_layer",
        "behavior",
        "style_layer",
        "knowledge",
        "memory",
        "context_block",
    ]
    .iter()
    .enumerate()
    {
        let _ = i;
        assert!(
            LAYER_TEMPLATE.contains(&format!("{{{placeholder}}}")),
            "模板缺少占位符 {{{placeholder}}}"
        );
    }
}

/// 助手类 persona（原文白名单外）的不注入原文内容——
/// 全部关键语义元素（能力边界/角色身份/性格/事实/示例/回复规则/记忆引用规则/
/// 近期脉络/相关记忆/当前时间）保留，且不产生原文/桥接段落。
///
/// 说明: 白名单闸门在检索/桥接加载层（`retrieve_memory.rs` / `bridge.rs`，
/// 已分别有断言测试），本测试验证装配层对 `None` 注入源不产生段落。
#[test]
fn rama_persona_prompt_semantically_equivalent_to_v13() {
    let ctx = PromptContext {
        persona: Some(Persona {
            id: 1,
            uid: "rama-0001".into(),
            name: "Ramaria".into(),
            kind: PersonaKind::Rama, // 助手类：原文白名单外
            seq: 1,
            source: "system".into(),
            ref_id: None,
            avatar: None,
            active: true,
            config: Some(r#"{"description":"系统助手"}"#.into()),
            description: None,
            created_at: 1000,
            updated_at: 1000,
        }),
        facts: vec![PersonaFact {
            id: 1,
            persona_uid: "rama-0001".into(),
            field: ProfileField::Interests,
            content: "用户喜欢编程".into(),
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
        traits: vec![make_test_trait("严谨", TraitLayer::Base, "做事认真")],
        examples: vec![make_test_example("你好", "你好呀")],
        recent_session_summaries: vec!["昨天讨论了项目排期".to_string()],
        memory_context: Some("用户：最近在学 Rust".into()),
        chat_style_rules: Some("回复简洁，用口语化表达".into()),
        current_time_str: Some("2026-08-08 10:00".into()),
        // 白名单外：注入源为 None（检索/桥接层闸门保证），不产生段落
        utt_context: None,
        bridge_context: None,
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());

    // ---- 语义元素齐全 ----
    let semantic_elements = [
        "# 能力边界",             // Capacity 安全边界
        "你记得和对方过往的对话", // 核心能力
        "# 角色（行为层）",       // 角色身份
        "Ramaria",
        "## 性格特征", // Insight traits
        "严谨",
        "## 已知事实", // Insight facts
        "用户喜欢编程",
        "## 回复规范", // Experiment 回复规则
        "回复简洁，用口语化表达",
        "记忆引用规则", // 记忆引用规则保留
        "## 对话示例",  // Statement Few-shot
        "你好呀",
        "## 近期对话脉络", // Memory 脉络
        "项目排期",
        "## 相关历史记忆", // RAG
        "Rust",
        "# 当前时间", // 语境
        "2026-08-08",
    ];
    for elem in semantic_elements {
        assert!(result.contains(elem), "助手类 prompt 缺少语义元素: {elem}");
    }

    // ---- 原文/桥接不注入（隐私红线，装配层对 None 源不产生段落） ----
    assert!(
        !result.contains("## 原文片段"),
        "白名单外不得产生原文片段段落"
    );
    assert!(
        !result.contains("## 桥接（上一会话尾部）"),
        "白名单外不得产生桥接段落"
    );
}

/// 行为/知识槽位为空时不产生空段落。
#[test]
fn empty_slots_do_not_produce_blank_paragraphs() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());

    // 槽位标题不出现（空实现）
    assert!(
        !result.contains("# 知识（知识层，按需）"),
        "知识槽位为空不产生段落"
    );
    // 无三连空行（join 拼接，空块已跳过）
    assert!(!result.contains("\n\n\n\n"), "不应出现多空行");
    // 段落顺序：能力边界在最前，当前时间在最后
    let capacity_pos = result.find("# 能力边界").expect("能力边界应存在");
    let time_pos = result.rfind("# 当前时间").expect("当前时间应存在");
    assert!(capacity_pos < time_pos, "能力边界应前置，当前时间应置尾");
}

/// 脉络层预算——超限时原文/桥接被裁剪，
/// 脉络摘要保最近（装配层集成验证，分配器单测在 layers.rs）。
#[test]
fn memory_layer_budget_applied_in_assemble() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        recent_session_summaries: vec!["最近的摘要内容".to_string(), "较旧的摘要内容".to_string()],
        utt_context: Some("第一块原文内容\n\n第二块原文内容".to_string()),
        bridge_context: Some("第一行桥接内容\n第二行桥接内容".to_string()),
        ..Default::default()
    };
    // 极紧预算：只够脉络摘要
    let config = PromptConfig {
        memory_layer_budget_chars: Some(8),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &config);
    assert!(result.contains("最近的摘要内容"), "脉络摘要保最近");
    assert!(!result.contains("较旧的摘要内容"), "最旧摘要被裁");
    assert!(!result.contains("原文内容"), "原文块被裁");
    assert!(!result.contains("桥接内容"), "桥接被裁");
}
