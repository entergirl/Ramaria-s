//! crates/ramaria-memory/src/prompt/builder/tests/mod.rs - 提示词组装器单元测试
//!
//! 设计特点:
//! - 覆盖 capacity/role/memory/四层注入/utt 上下文/跨会话脉络等 prompt 区块构建。
//! - 全部使用合成输入与 mock，不依赖真实 LLM/embedding。
use super::*;
use ramaria_core::types::{PersonaKind, ProfileField, TraitLayer, TraitSource, TraitStatus};

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

/// 构造最小测试事实（走 PersonaFact::new，默认 active/stable/manual）。
fn make_test_fact(content: &str) -> PersonaFact {
    PersonaFact::new(
        "char-0001".into(),
        ProfileField::BasicInfo,
        content.into(),
        ramaria_core::types::FactSource::Manual,
    )
}

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

mod ablation;
mod behavior;
mod blocks;
mod boilerplate;
mod coordinated;
mod persona;
mod social;
mod template;
mod utt_bridge;
