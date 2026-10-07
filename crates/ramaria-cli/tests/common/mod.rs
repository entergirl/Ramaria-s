//! tests/common/mod.rs - CLI 集成测试共享 Mock 基础设施
//!
//! 设计特点:
//! - MockStorage: 内存 HashMap 实现的 StorageBackend，支持预填充测试数据
//! - MockLlm: 返回预设回复的 LlmProvider
//! - build_test_engine: 一键构造 ready 状态的服务层引擎实例供 CLI 命令测试
//! - 不调用真实 LLM、不触碰文件系统、不访问 OS keychain
//!
//! 安全约束:
//! - 不使用真实 API key 或网络请求
//! - 所有 mock 都是 Send + Sync
//! - 测试间通过独立引擎实例完全隔离
//!
//! 注意: 本文件的所有 pub 项均由其他测试文件（command_tests.rs / ui_tests.rs）
//! 通过 `mod common;` 引用使用。Rust 编译器在单独分析本文件时会误报 dead_code，
//! 此处显式 allow。

#![allow(dead_code, unused_imports)]

use ramaria_core::behavior::{BehaviorRule, FeedbackLog};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    BackendConfig, MemoryEvent, MemoryL1, Message, Persona, PersonaExample, PersonaFact,
    PersonaStyleStats, PersonalityTrait, PrivacyConsent, Session,
};
use std::collections::HashMap;
use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

mod embedding;
mod llm;
mod storage_crud;
mod storage_infra;

pub use embedding::MockEmbedding;
pub use llm::MockLlm;

// =========================================================
// MockStorage — 可预填充的测试用 StorageBackend
// =========================================================

/// 内存 Mock StorageBackend。
///
/// 职责:
/// - 替代真实 SQLite，支持 CLI 命令集成测试
/// - 数据存于 HashMap，测试间完全隔离
/// - 便捷方法支持预填充各类测试场景数据
pub struct MockStorage {
    sessions: Mutex<HashMap<Uuid, Session>>,
    messages: Mutex<HashMap<Uuid, Vec<Message>>>,
    l1_list: Mutex<HashMap<Uuid, Vec<MemoryL1>>>,
    personas: Mutex<HashMap<String, Persona>>,
    events: Mutex<HashMap<String, Vec<MemoryEvent>>>,
    traits: Mutex<HashMap<String, Vec<PersonalityTrait>>>,
    persona_seq: Mutex<i64>,
    privacy_consents: Mutex<Vec<PrivacyConsent>>,
    backend_config: Mutex<Option<BackendConfig>>,
    settings: Mutex<HashMap<String, String>>,
    index_version: Mutex<i32>,
    examples: Mutex<HashMap<String, Vec<PersonaExample>>>,
    event_seq: AtomicI64,
    /// 行为规则（CLI 测试）
    behavior_rules: Mutex<HashMap<i64, BehaviorRule>>,
    rules_by_persona: Mutex<HashMap<String, Vec<i64>>>,
    rule_seq: AtomicI64,
    /// 反馈日志（CLI 测试）
    feedback_logs: Mutex<Vec<FeedbackLog>>,
    feedback_seq: AtomicI64,
    /// 知识事实（内存版版本链，CLI fact 契约测试）
    facts: Mutex<Vec<PersonaFact>>,
    fact_seq: AtomicI64,
    /// 表达层风格统计（persona_style_stats，CLI style 命令测试）
    style_stats: Mutex<HashMap<String, PersonaStyleStats>>,
}

impl MockStorage {
    /// 创建空的 MockStorage。
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            messages: Mutex::new(HashMap::new()),
            l1_list: Mutex::new(HashMap::new()),
            personas: Mutex::new(HashMap::new()),
            events: Mutex::new(HashMap::new()),
            traits: Mutex::new(HashMap::new()),
            persona_seq: Mutex::new(0),
            privacy_consents: Mutex::new(Vec::new()),
            backend_config: Mutex::new(None),
            settings: Mutex::new(HashMap::new()),
            index_version: Mutex::new(0),
            examples: Mutex::new(HashMap::new()),
            event_seq: AtomicI64::new(1),
            behavior_rules: Mutex::new(HashMap::new()),
            rules_by_persona: Mutex::new(HashMap::new()),
            rule_seq: AtomicI64::new(1),
            feedback_logs: Mutex::new(Vec::new()),
            feedback_seq: AtomicI64::new(1),
            facts: Mutex::new(Vec::new()),
            fact_seq: AtomicI64::new(1),
            style_stats: Mutex::new(HashMap::new()),
        }
    }

    /// 创建会话并填充消息（用于 session 查看/导出测试）。
    pub fn create_session_with_messages(&self, session_id: Uuid, messages: Vec<Message>) {
        self.sessions.lock().unwrap().insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1_717_977_600_000, // 2024-06-10T08:00:00 UTC
                ended_at: None,
                persona_uid: None,
                ..Session::default()
            },
        );
        self.messages.lock().unwrap().insert(session_id, messages);
    }

    /// 创建已结束会话。
    pub fn create_ended_session(&self, session_id: Uuid) {
        self.sessions.lock().unwrap().insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1_717_977_600_000,
                ended_at: Some(1_717_986_240_000), // 24h later
                persona_uid: None,
                ..Session::default()
            },
        );
    }

    /// 添加 L1 记忆。
    pub fn add_l1(&self, session_id: Uuid, memory: MemoryL1) {
        self.l1_list
            .lock()
            .unwrap()
            .entry(session_id)
            .or_default()
            .push(memory);
    }

    /// 添加 L2 事件。
    pub fn add_event(&self, persona_uid: &str, event: MemoryEvent) {
        self.events
            .lock()
            .unwrap()
            .entry(persona_uid.to_string())
            .or_default()
            .push(event);
    }

    /// 添加 L3 性格标签。
    pub fn add_personality_trait(&self, persona_uid: &str, t: PersonalityTrait) {
        self.traits
            .lock()
            .unwrap()
            .entry(persona_uid.to_string())
            .or_default()
            .push(t);
    }

    /// 添加设置项。
    pub fn add_setting(&self, key: &str, value: &str) {
        self.settings
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
    }

    /// 设置后端配置。
    pub fn set_backend_config(&self, config: BackendConfig) {
        *self.backend_config.lock().unwrap() = Some(config);
    }

    /// 添加隐私确认。
    pub fn add_privacy_consent(&self, consent: PrivacyConsent) {
        self.privacy_consents.lock().unwrap().push(consent);
    }

    /// 添加一个 persona 到 mock 存储（用于 persona 命令测试）。
    pub fn add_persona(&self, persona: Persona) {
        self.personas
            .lock()
            .unwrap()
            .insert(persona.uid.clone(), persona);
    }

    /// 添加一条知识事实（CLI fact 契约测试用）。
    ///
    /// 说明:
    /// - 自动分配 id（内存自增），与真实库一致。
    /// - 默认 status=active；调用方可通过 `fact.status` 覆写以构造 superseded/candidate。
    pub fn add_fact(&self, mut fact: PersonaFact) -> i64 {
        let id = self
            .fact_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        fact.id = id;
        self.facts.lock().unwrap().push(fact);
        id
    }

    /// 构造一条版本链覆盖：旧事实置 superseded + 新事实写入（version_of 指向旧 id）。
    ///
    /// 说明:
    /// - 模拟真实 `save_fact_with_version` 的原子语义（旧置 superseded + 新 insert + 链指针）。
    /// - 返回新事实 id。
    pub fn add_fact_with_version(&self, old: &PersonaFact, mut fresh: PersonaFact) -> i64 {
        let mut facts = self.facts.lock().unwrap();
        if let Some(o) = facts.iter_mut().find(|f| f.id == old.id) {
            o.status = ramaria_core::types::FactStatus::Superseded;
        }
        let new_id = self
            .fact_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        fresh.id = new_id;
        fresh.status = ramaria_core::types::FactStatus::Active;
        fresh.version_of = Some(old.id);
        facts.push(fresh);
        new_id
    }
}

// =========================================================
// MockLlm
// =========================================================

/// Mock LLM Provider，返回预设回复。
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

/// 构造测试用的 Message。
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
        indexed_at: None,
        index_version: None,
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
    use std::sync::atomic::{AtomicI64, Ordering};
    static COUNTER: AtomicI64 = AtomicI64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
