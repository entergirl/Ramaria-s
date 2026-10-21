//! tests/common/mod.rs - CLI 集成测试共享基础设施
//!
//! 设计特点:
//! - MockStorage / MockLlm 单份实现下沉到 ramaria-testkit，本模块只做路径转出
//! - build_test_engine: 一键构造 ready 状态的服务层引擎实例供 CLI 命令测试
//! - make_* 自由函数提供确定性消息 / L1 / 事件 / 性格 / 人格夹具
//! - 不调用真实 LLM、不触碰文件系统、不访问 OS keychain

#![allow(dead_code, unused_imports)]

use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{MemoryEvent, MemoryL1, Message, Persona, PersonalityTrait};
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use uuid::Uuid;

pub use ramaria_testkit::mock::{MockEmbedding, MockLlm, MockStorage};

// =========================================================
// 引擎装配
// =========================================================

/// 构造 ready 状态的测试引擎（空存储 + 固定回复 LLM）。
pub fn build_test_engine() -> (Arc<ramaria_service::Engine>, Arc<MockStorage>) {
    use ramaria_core::config::RamariaConfig;
    use ramaria_service::Engine;

    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("Hello, World!"));
    let config = RamariaConfig::default();

    let engine = Engine::from_parts(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        None,
        config,
    );

    // 设置为 Ready 状态以跳过 setup 检查
    engine.set_state(ramaria_core::types::AppState::Ready);

    (Arc::new(engine), storage)
}

// =========================================================
// 夹具
// =========================================================

/// 构造测试用的用户 Message。
pub fn make_user_message(session_id: Uuid, content: &str) -> Message {
    Message {
        id: Uuid::new_v4(),
        session_id,
        role: ramaria_core::types::MessageRole::User,
        source: ramaria_core::types::MessageSource::Local,
        content: content.to_string(),
        created_at: 1_717_977_600_000,
        fingerprint: None,
        persona_uid: None,
        is_proactive: false,
        sender_ref: None,
        sender_name: None,
        platform_message_id: None,
        recalled_at: None,
    }
}

/// 构造测试用的 AI Message。
pub fn make_assistant_message(session_id: Uuid, content: &str) -> Message {
    Message {
        id: Uuid::new_v4(),
        session_id,
        role: ramaria_core::types::MessageRole::Assistant,
        source: ramaria_core::types::MessageSource::Online,
        content: content.to_string(),
        created_at: 1_717_977_601_000,
        fingerprint: None,
        persona_uid: None,
        is_proactive: false,
        sender_ref: None,
        sender_name: None,
        platform_message_id: None,
        recalled_at: None,
    }
}

/// 构造测试用的 L1 记忆。
pub fn make_test_l1(session_id: Uuid, summary: &str) -> MemoryL1 {
    MemoryL1 {
        id: Uuid::new_v4(),
        session_id,
        persona_uid: Some("user-0001".to_string()),
        summary: summary.to_string(),
        keywords: None,
        time_period: None,
        atmosphere: Some("neutral".to_string()),
        valence: 0.5,
        salience: 0.7,
        context_json: None,
        absorbed: false,
        created_at: 1_717_977_600_000,
        occurred_at: None,
        last_accessed_at: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    }
}

/// 构造测试用的 L2 事件。
pub fn make_test_event(id: i64, title: &str) -> MemoryEvent {
    MemoryEvent {
        id,
        persona_uid: "user-0001".to_string(),
        title: title.to_string(),
        summary: format!("{title} 的详细摘要"),
        keywords: None,
        participants: Some("[\"user-0001\"]".to_string()),
        start: 1_717_977_600_000,
        end: 1_717_986_240_000,
        valence: 0.3,
        share: 0.8,
        presentation: ramaria_core::types::Presentation::Subjective,
        confidence: 0.85,
        salience: 0.6,
        attitude: None,
        paraphrase: None,
        absorbed: 0,
        situation_strength: None,
        motives: None,
        created_at: 1_717_977_600_000,
        last_accessed_at: None,
    }
}

/// 构造测试用的 L3 性格标签。
pub fn make_test_trait(label: &str, layer: ramaria_core::types::TraitLayer) -> PersonalityTrait {
    PersonalityTrait {
        id: rand_id(),
        persona_uid: "user-0001".to_string(),
        trait_label: label.to_string(),
        meaning: format!("{label} 的含义说明"),
        not_meaning: None,
        trigger: None,
        suppress: None,
        related: None,
        seq: 1,
        source: ramaria_core::types::TraitSource::Inferred,
        ref_event_id: None,
        ref_l1_id: None,
        layer,
        confidence: 0.8,
        evidence: 5.0,
        consistency: 0.7,
        status: ramaria_core::types::TraitStatus::Active,
        created_at: 1_717_977_600_000,
        updated_at: 1_717_977_600_000,
    }
}

/// 构造测试用的 Persona。
pub fn make_test_persona(
    uid: &str,
    name: &str,
    kind: ramaria_core::types::PersonaKind,
    config: Option<&str>,
) -> Persona {
    let mut p = Persona::new(
        uid.to_string(),
        name.to_string(),
        kind,
        1,
        "system".to_string(),
    );
    p.config = config.map(|s| s.to_string());
    p
}

fn rand_id() -> i64 {
    use std::sync::atomic::Ordering;
    static COUNTER: AtomicI64 = AtomicI64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
