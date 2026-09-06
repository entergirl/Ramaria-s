//! crates/ramaria-memory/src/prompt/builder/tests.rs - 提示词组装器单元测试
//!
//! 设计特点:
//! - 覆盖 capacity/role/memory/四层注入/utt 上下文/跨会话脉络等 prompt 区块构建。
//! - 全部使用合成输入与 mock，不依赖真实 LLM/embedding。
use super::*;
use ramaria_core::types::{PersonaKind, TraitLayer, TraitSource, TraitStatus};

// ---- 辅助构造器 ----

fn make_test_persona() -> Persona {
    Persona {
        id: 1,
        uid: "char-0001".into(),
        name: "小明".into(),
        kind: PersonaKind::Char,
        seq: 1,
        source: "manual".into(),
        ref_id: None,
        avatar: None,
        active: true,
        config: Some(
            r#"{"description":"一个喜欢编程的大学生","speaking_style":"热情活泼，喜欢用emoji"}"#
                .into(),
        ),
        description: None,
        created_at: 1000,
        updated_at: 1000,
    }
}

fn make_test_trait(label: &str, layer: TraitLayer, meaning: &str) -> PersonalityTrait {
    PersonalityTrait {
        id: 1,
        persona_uid: "char-0001".into(),
        layer,
        trait_label: label.into(),
        meaning: meaning.into(),
        not_meaning: None,
        trigger: None,
        suppress: None,
        related: None,
        seq: 1,
        source: TraitSource::Manual,
        ref_event_id: None,
        ref_l1_id: None,
        confidence: 0.9,
        evidence: 0.8,
        consistency: 0.7,
        status: TraitStatus::Active,
        created_at: 1000,
        updated_at: 1000,
    }
}

fn make_test_example(partner: &str, reply: &str) -> PersonaExample {
    PersonaExample {
        id: 1,
        persona_uid: "char-0001".into(),
        partner: partner.into(),
        reply: reply.into(),
        session_id: None,
        context: None,
        valence: 0.5,
        tags: None,
        selected: true,
        length: reply.chars().count() as i32,
        created_at: 1000,
    }
}

// ---- Role 块测试 ----

#[test]
fn role_with_persona() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        ..Default::default()
    };
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);

    assert!(result.contains("小明"));
    assert!(result.contains("虚构角色"));
    assert!(result.contains("编程"));
    assert!(result.contains("emoji"));
    // 四层模板 markers
    assert!(result.contains("# 角色（行为层）"));
    assert!(result.contains("# 记忆（脉络层）"));
    assert!(result.contains("## 说话风格"));
    assert!(result.contains("# 当前时间"));
}

#[test]
fn role_without_persona_uses_default() {
    let ctx = PromptContext::default();
    let config = PromptConfig::default();
    let result = assemble_prompt(&ctx, &config);
    assert!(result.contains("Ramaria"));
    assert!(result.contains("AI 助手"));
}

// ---- 表达层：自动风格规则注入（A3） ----

/// 构造无手工 speaking_style 的 persona（config 不含该字段）。
fn make_persona_without_manual_style() -> Persona {
    Persona {
        config: Some(r#"{"description":"一个喜欢编程的大学生"}"#.into()),
        ..make_test_persona()
    }
}

#[test]
fn auto_style_rule_injected_when_no_manual_style() {
    // 自动规则存在 + 无手工 speaking_style → 注入 `## 自动风格规则`
    let ctx = PromptContext {
        persona: Some(make_persona_without_manual_style()),
        style_rule_text: Some("你习惯使用口癖词「哇塞」，常聊「电影」等话题。".into()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(
        result.contains("## 自动风格规则"),
        "自动风格规则应注入: {result}"
    );
    assert!(result.contains("口癖词「哇塞」"), "规则文本在 prompt 中");
    assert!(
        !result.contains("## 说话风格\n"),
        "无手工风格时不产生手工子段"
    );
}

#[test]
fn manual_style_overrides_auto_rule() {
    // 手工 speaking_style 存在 → 只注入手工，自动规则被覆盖（手工优先，D-V17-004）
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        style_rule_text: Some("自动规则文本不应出现".into()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(result.contains("## 说话风格"), "手工风格子段存在");
    assert!(result.contains("热情活泼"), "手工风格内容注入");
    assert!(
        !result.contains("## 自动风格规则"),
        "手工覆盖优先，自动规则不注入: {result}"
    );
    assert!(!result.contains("自动规则文本不应出现"));
}

#[test]
fn no_style_rule_keeps_v16_prompt_equivalent() {
    // style_rule_text=None（风格关闭/数据不足）→ prompt 与 v1.6 语义等价
    // （不产生 `## 自动风格规则` 段落，回归红线 1）
    let ctx = PromptContext {
        persona: Some(make_persona_without_manual_style()),
        style_rule_text: None,
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(
        !result.contains("## 自动风格规则"),
        "无自动规则时 prompt 与 v1.6 语义等价: {result}"
    );
}

#[test]
fn blank_style_rule_treated_as_missing() {
    // 空白规则文本（防御）→ 不注入
    let ctx = PromptContext {
        persona: Some(make_persona_without_manual_style()),
        style_rule_text: Some("   ".into()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(!result.contains("## 自动风格规则"));
}

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

    // 无近期摘要 → 首次对话提示
    assert!(result.contains("首次对话"));
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

    assert!(result.contains("首次对话"));
    assert!(result.contains("暂无与当前话题直接相关的历史记忆"));
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
    // 跨 session 叙事引导句
    assert!(result.contains("此前与用户进行了"));
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
    assert!(result.contains("你此前与用户讨论过"));
    assert!(result.contains("爬山"));
}

// ---- 跨 session 叙事引导句测试 ----

#[test]
fn cross_session_narrative_single() {
    let summaries = vec!["用户今天学习了Rust编程语言".to_string()];
    let narrative = build_cross_session_narrative(&summaries);
    assert!(narrative.contains("你此前与用户讨论过"));
    assert!(narrative.contains("Rust编程"));
    assert!(!narrative.contains("次对话"));
}

#[test]
fn cross_session_narrative_multiple() {
    let summaries = vec![
        "探讨了AI助手的记忆系统".to_string(),
        "完成了Rust项目发布".to_string(),
        "讨论了Python异步编程".to_string(),
    ];
    let narrative = build_cross_session_narrative(&summaries);
    assert!(narrative.contains("你此前与用户进行了 3 次对话"));
    assert!(narrative.contains("不久前"));
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
    let block_len = tail.find("\n\n# ").map(|p| p).unwrap_or(tail.len());
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
    assert!(result.contains("首次对话"));
}

// =========================================================
// utt 原文片段
// =========================================================

fn utt_hit(id: i64, persona: &str, text: &str, score: f64) -> crate::retriever::UttHit {
    crate::retriever::UttHit {
        doc: crate::retriever::UttDocView {
            id,
            persona_uid: persona.to_string(),
            session_id: uuid::Uuid::new_v4(),
            block_text: text.to_string(),
            msg_count: 2,
            created_at: 1000,
        },
        score,
        channel: "vector",
    }
}

#[test]
fn render_utt_context_keeps_all_within_budget() {
    let hits = vec![
        utt_hit(1, "char-0001", "今天天气真好", 0.9),
        utt_hit(2, "char-0001", "晚上吃火锅", 0.8),
    ];
    let out = render_utt_context(&hits, 500);
    assert!(out.contains("今天天气真好"));
    assert!(out.contains("晚上吃火锅"));
    assert!(
        out.contains(
            "

"
        ),
        "块间空行分隔"
    );
}

#[test]
fn render_utt_context_trims_by_budget_keeping_high_score() {
    // 预算只够一块：高分的保留，低分的整块丢弃
    // 块1 9 字符 ≤ 预算 10；块1+块2 = 9+5+2(空行) > 10 → 块2 被丢
    let hits = vec![
        utt_hit(1, "char-0001", "第一块内容很长很长", 0.9),
        utt_hit(2, "char-0001", "第二块内容", 0.8),
    ];
    let out = render_utt_context(&hits, 10);
    assert!(out.contains("第一块"), "高分块保留");
    assert!(!out.contains("第二块"), "超预算整块丢弃");
}

#[test]
fn render_utt_context_first_block_over_budget_yields_empty() {
    let hits = vec![utt_hit(1, "char-0001", "超长块内容", 0.9)];
    let out = render_utt_context(&hits, 2);
    assert!(out.is_empty(), "首块即超预算 → 不注入");
}

#[test]
fn render_utt_context_empty_hits_yields_empty() {
    assert!(render_utt_context(&[], 100).is_empty());
}

#[test]
fn render_utt_context_skips_blank_blocks() {
    let hits = vec![
        utt_hit(1, "char-0001", "   ", 0.9),
        utt_hit(2, "char-0001", "有效内容", 0.8),
    ];
    let out = render_utt_context(&hits, 100);
    assert!(out.contains("有效内容"));
    assert!(
        !out.contains(
            "

"
        ),
        "空白块被跳过不产生空段"
    );
}

#[test]
fn assemble_prompt_includes_utt_section_only_when_present() {
    // 无原文片段 → prompt 不含【原文片段】段落（白名单外/未命中）
    let ctx = PromptContext::default();
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(!result.contains("原文片段"), "无原文时不产生段落");

    // 有原文片段 → 段落出现
    let ctx2 = PromptContext {
        utt_context: Some("这是角色原话内容".to_string()),
        ..Default::default()
    };
    let result2 = assemble_prompt(&ctx2, &PromptConfig::default());
    assert!(result2.contains("## 原文片段"), "原文片段段落应出现");
    assert!(result2.contains("这是角色原话内容"));
}

/// 桥接内容存在时产生【桥接（上一会话尾部）】段落；
/// 缺失/空白时不产生段落（白名单外不注入）。
#[test]
fn assemble_prompt_includes_bridge_section_only_when_present() {
    // 无桥接内容 → 不产生段落
    let ctx = PromptContext::default();
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(
        !result.contains("桥接（上一会话尾部）"),
        "无桥接时不产生段落"
    );

    // 空白内容 → 不产生段落（防御）
    let ctx_blank = PromptContext {
        bridge_context: Some("   ".to_string()),
        ..Default::default()
    };
    let result_blank = assemble_prompt(&ctx_blank, &PromptConfig::default());
    assert!(
        !result_blank.contains("## 桥接"),
        "空白桥接内容不应产生段落"
    );

    // 有桥接内容 → 段落出现，含衔接说明与原文
    let ctx2 = PromptContext {
        bridge_context: Some("[2026-08-01 20:00] 角色: 上次聊到这里".to_string()),
        ..Default::default()
    };
    let result2 = assemble_prompt(&ctx2, &PromptConfig::default());
    assert!(
        result2.contains("## 桥接（上一会话尾部）"),
        "桥接段落应出现"
    );
    assert!(result2.contains("保持对话连贯性"), "应含衔接用途说明");
    assert!(result2.contains("上次聊到这里"), "应含桥接原文内容");
}

/// 桥接与原文片段并存时两个段落都渲染（互不覆盖）。
#[test]
fn assemble_prompt_bridge_and_utt_coexist() {
    let ctx = PromptContext {
        utt_context: Some("原文片段内容".to_string()),
        bridge_context: Some("桥接内容".to_string()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(result.contains("## 原文片段"), "原文片段段落应保留");
    assert!(result.contains("## 桥接（上一会话尾部）"), "桥接段落应出现");
    assert!(result.contains("原文片段内容") && result.contains("桥接内容"));
}

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
        "# 能力边界",           // Capacity 安全边界
        "记住与用户的对话历史", // 核心能力
        "# 角色（行为层）",     // 角色身份
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

// =========================================================
// 注入闸门渲染测试（探针消融：B0 无记忆 / F4 −脉络 / F3 −表达）
// =========================================================

/// 构造含全部记忆子段与表达子段的完整上下文（模拟 F0 全开输入）。
fn make_full_ctx() -> PromptContext {
    PromptContext {
        persona: Some(make_test_persona()),
        facts: vec![make_test_fact("喜欢周末爬山")],
        traits: vec![make_test_trait("开朗", TraitLayer::Base, "乐观外向")],
        examples: vec![make_test_example("今天天气不错", "是呀，适合出去走走")],
        style_rule_text: Some("你习惯使用口癖词「哇塞」。".into()),
        recent_session_summaries: vec!["上次聊了旅行计划".to_string()],
        utt_context: Some("上次的原话样例".to_string()),
        bridge_context: Some("上一段对话尾部内容".to_string()),
        memory_context: Some("相关历史记忆内容".to_string()),
        ..Default::default()
    }
}

/// 构造最小测试事实（走 PersonaFact::new，默认 active/stable/manual）。
fn make_test_fact(content: &str) -> PersonaFact {
    PersonaFact::new(
        "char-0001".into(),
        ProfileField::BasicInfo,
        content.into(),
        ramaria_core::types::FactSource::Manual,
    )
}

/// 全开（默认）时四层段落全部渲染——回归红线：默认行为不回归。
#[test]
fn ablation_all_on_renders_all_layers() {
    let result = assemble_prompt(&make_full_ctx(), &PromptConfig::default());
    assert!(result.contains("# 记忆（脉络层）"));
    assert!(result.contains("## 近期对话脉络"));
    assert!(result.contains("## 相关历史记忆"));
    assert!(result.contains("## 原文片段"));
    assert!(result.contains("## 桥接"));
    assert!(result.contains("# 说话风格（表达层）"));
    assert!(result.contains("## 对话示例"));
    assert!(result.contains("## 性格特征"));
    assert!(result.contains("## 已知事实"));
}

/// B0 无记忆注入：关闭全部记忆子段 + 表达子段后，
/// prompt 不含记忆块 / 行为块占位（行为/知识由数据层门控，渲染侧无段落）。
#[test]
fn ablation_b0_omits_memory_and_style_blocks() {
    let config = PromptConfig {
        include_speaking_style: false,
        include_examples: false,
        include_narrative: false,
        include_memory_rag: false,
        include_utt: false,
        include_bridge: false,
        ..Default::default()
    };
    let result = assemble_prompt(&make_full_ctx(), &config);
    // 记忆块（含各子段与占位）整体不产生
    assert!(
        !result.contains("# 记忆（脉络层）"),
        "B0 不应含记忆块: {result}"
    );
    assert!(!result.contains("## 近期对话脉络"));
    assert!(!result.contains("## 相关历史记忆"));
    assert!(!result.contains("## 原文片段"));
    assert!(!result.contains("## 桥接"));
    assert!(!result.contains("首次对话"), "B0 不应出现脉络占位");
    // 表达层（说话风格 + 自动风格规则 + 对话示例）不产生
    assert!(!result.contains("# 说话风格（表达层）"));
    assert!(!result.contains("## 自动风格规则"));
    assert!(!result.contains("## 说话风格"));
    assert!(!result.contains("## 对话示例"));
    // 纯角色保留（persona 身份是 B0 的"纯角色"组成部分）
    assert!(result.contains("# 角色（行为层）"));
    assert!(result.contains("小明"));
}

/// F4 −脉络层：近期对话脉络与桥接子段不渲染，原文片段仍在。
#[test]
fn ablation_f4_omits_narrative_and_bridge_keeps_utt() {
    let config = PromptConfig {
        include_narrative: false,
        include_bridge: false,
        ..Default::default()
    };
    let result = assemble_prompt(&make_full_ctx(), &config);
    assert!(!result.contains("## 近期对话脉络"), "F4 应无脉络: {result}");
    assert!(!result.contains("## 桥接"), "F4 应无桥接");
    assert!(result.contains("## 原文片段"), "F4 保留原文样例");
    assert!(!result.contains("首次对话"), "关闭脉络时不产生占位");
}

/// F3 −表达层：说话风格/自动风格规则/对话示例不渲染；记忆块仍保留。
#[test]
fn ablation_f3_omits_expression_keeps_memory() {
    let config = PromptConfig {
        include_speaking_style: false,
        include_examples: false,
        ..Default::default()
    };
    let result = assemble_prompt(&make_full_ctx(), &config);
    assert!(
        !result.contains("# 说话风格（表达层）"),
        "F3 应无表达层: {result}"
    );
    assert!(!result.contains("## 自动风格规则"));
    assert!(!result.contains("## 说话风格"));
    assert!(!result.contains("## 对话示例"));
    // 手工 speaking_style 随表达层一并关闭（build_personality 也受闸门控制）
    assert!(!result.contains("热情活泼"));
    assert!(result.contains("# 记忆（脉络层）"), "F3 保留记忆块");
    assert!(result.contains("## 近期对话脉络"));
}

/// 记忆子段全部关闭时整块不产生——行为/知识等由数据层负责，此处验证记忆块边界。
#[test]
fn ablation_memory_subsections_off_omits_whole_block() {
    let config = PromptConfig {
        include_narrative: false,
        include_memory_rag: false,
        include_utt: false,
        include_bridge: false,
        ..Default::default()
    };
    let result = assemble_prompt(&make_full_ctx(), &config);
    assert!(
        !result.contains("# 记忆（脉络层）"),
        "全部记忆子段关闭时整块省略: {result}"
    );
}
