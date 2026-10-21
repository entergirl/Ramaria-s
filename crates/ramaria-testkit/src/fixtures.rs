//! crates/ramaria-testkit/src/fixtures.rs - Ramaria 测试领域夹具模块
//!
//! 设计特点:
//! - 固定时间基准（2023-11-14T22:13:20Z）：同一参数两次构造关键字段完全一致
//! - 覆盖会话 / 消息 / L1 / L2 事件 / 人格 / utt 块 / 事实 / 性格标签
//! - 仅填充领域字段，不写存储；标识符使用随机 UUID，正文不含敏感内容
//! - 供各 crate 测试直接复用，避免各自手写结构体导致口径漂移

use ramaria_core::types::{
    FactSource, MemoryEvent, MemoryL1, Message, MessageRole, MessageSource, Persona, PersonaFact,
    PersonaKind, PersonalityTrait, Session, TraitLayer, TraitSource, UttBlock,
};
use uuid::Uuid;

/// 固定时间基准（Unix 毫秒）。
pub const BASE_MS: i64 = 1_700_000_000_000;

/// 构造会话（活跃，本地通道）。
///
/// 参数:
/// - `persona_uid`: 会话绑定人格；None 表示未绑定。
pub fn session(persona_uid: Option<&str>) -> Session {
    Session {
        id: Uuid::new_v4(),
        started_at: BASE_MS,
        ended_at: None,
        persona_uid: persona_uid.map(str::to_string),
        channel: "local".to_string(),
        external_ref: None,
    }
}

/// 构造已关闭会话（结束时间为基准 + 1 小时）。
pub fn closed_session(persona_uid: Option<&str>) -> Session {
    Session {
        ended_at: Some(BASE_MS + 3_600_000),
        ..session(persona_uid)
    }
}

/// 构造指定角色的消息（创建时间为基准时间）。
pub fn message(session_id: Uuid, role: MessageRole, content: &str) -> Message {
    Message {
        id: Uuid::new_v4(),
        session_id,
        role,
        content: content.to_string(),
        created_at: BASE_MS,
        source: MessageSource::Local,
        fingerprint: None,
        persona_uid: None,
        is_proactive: false,
        sender_ref: None,
        sender_name: None,
        platform_message_id: None,
        recalled_at: None,
    }
}

/// 构造用户消息。
pub fn user_message(session_id: Uuid, content: &str) -> Message {
    message(session_id, MessageRole::User, content)
}

/// 构造助手消息（来源为线上）。
pub fn assistant_message(session_id: Uuid, content: &str) -> Message {
    let mut msg = message(session_id, MessageRole::Assistant, content);
    msg.source = MessageSource::Online;
    msg
}

/// 构造 L1 摘要（字段全部合法）。
///
/// 参数:
/// - `session_id`: 摘要所属会话；
/// - `persona_uid`: 摘要描述的人格；
/// - `summary`: 摘要正文。
pub fn l1(session_id: Uuid, persona_uid: Option<&str>, summary: &str) -> MemoryL1 {
    MemoryL1 {
        id: Uuid::new_v4(),
        session_id,
        summary: summary.to_string(),
        keywords: Some("测试,夹具".to_string()),
        time_period: Some("上午".to_string()),
        atmosphere: Some("平静".to_string()),
        valence: 0.5,
        salience: 0.5,
        absorbed: false,
        created_at: BASE_MS,
        occurred_at: Some(BASE_MS - 60_000),
        last_accessed_at: None,
        persona_uid: persona_uid.map(str::to_string),
        context_json: None,
        situation_strength: Some(3),
        evidence_notes: None,
        continuation: None,
    }
}

/// 构造 L2 事件（id 由存储层分配）。
pub fn event(persona_uid: &str, title: &str) -> MemoryEvent {
    let mut event = MemoryEvent::new(
        persona_uid.to_string(),
        title.to_string(),
        format!("{title} 的详细摘要"),
        BASE_MS - 60_000,
        BASE_MS,
    );
    event.created_at = BASE_MS;
    event
}

/// 构造人格注册条目（历史人物以外的最常见类型）。
pub fn persona(uid: &str) -> Persona {
    let kind = if uid.starts_with("user") {
        PersonaKind::User
    } else if uid.starts_with("rama") {
        PersonaKind::Rama
    } else {
        PersonaKind::Char
    };
    let mut persona = Persona::new(
        uid.to_string(),
        format!("测试人格 {uid}"),
        kind,
        1,
        "local".to_string(),
    );
    persona.created_at = BASE_MS;
    persona.updated_at = BASE_MS;
    persona
}

/// 构造 utt 原文话语块。
pub fn utt_block(session_id: Uuid, persona_uid: &str, content: &str) -> UttBlock {
    let start_id = Uuid::new_v4();
    let mut block = UttBlock::new(
        persona_uid.to_string(),
        session_id,
        start_id,
        start_id,
        content.to_string(),
        1,
        0,
    );
    block.created_at = BASE_MS;
    block
}

/// 构造人物事实（基础信息字段，active）。
pub fn fact(persona_uid: &str, content: &str) -> PersonaFact {
    let mut fact = PersonaFact::new(
        persona_uid.to_string(),
        ramaria_core::types::ProfileField::BasicInfo,
        content.to_string(),
        FactSource::Manual,
    );
    fact.created_at = BASE_MS;
    fact.updated_at = BASE_MS;
    fact
}

/// 构造性格标签（base 层，手工来源）。
pub fn trait_record(persona_uid: &str, label: &str) -> PersonalityTrait {
    let mut record = PersonalityTrait::new(
        persona_uid.to_string(),
        TraitLayer::Base,
        label.to_string(),
        format!("{label} 的具体含义"),
        TraitSource::Manual,
        1,
    );
    record.created_at = BASE_MS;
    record.updated_at = BASE_MS;
    record
}
