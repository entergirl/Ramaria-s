//! tests/common/mod.rs - CLI 集成测试共享 Mock 基础设施
//!
//! 设计特点:
//! - MockStorage: 内存 HashMap 实现的 StorageBackend，支持预填充测试数据
//! - MockLlm: 返回预设回复的 LlmProvider
//! - build_test_app: 一键构造 ready 状态的 App 实例供 CLI 命令测试
//! - 不调用真实 LLM、不触碰文件系统、不访问 OS keychain
//!
//! 安全约束:
//! - 不使用真实 API key 或网络请求
//! - 所有 mock 都是 Send + Sync
//! - 测试间通过独立 App 实例完全隔离
//!
//! 注意: 本文件的所有 pub 项均由其他测试文件（command_tests.rs / ui_tests.rs）
//! 通过 `mod common;` 引用使用。Rust 编译器在单独分析本文件时会误报 dead_code，
//! 此处显式 allow。

#![allow(dead_code)]

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{Stream, stream};
use ramaria_core::behavior::{BehaviorRule, FeedbackLog};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{
    ChatRequest, EmbeddingModelInfo, EmbeddingProvider, LlmProvider, StorageBackend, StreamDelta,
};
use ramaria_core::types::{
    BackendConfig, ClusterSnapshot, EventRelation, LlmProvider as LlmProviderKind, MemoryEvent,
    MemoryL1, Message, ModelCapability, Persona, PersonaExample, PersonaFact, PersonalityTrait,
    PrivacyConsent, ProfileField, Session, TraitEvidence, TraitStatus,
};
use uuid::Uuid;

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
}

#[async_trait]
impl StorageBackend for MockStorage {
    async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
        let session = Session {
            id: Uuid::new_v4(),
            started_at: 1_717_977_600_000,
            ended_at: None,
            persona_uid: persona_uid.map(|s| s.to_string()),
        };
        self.sessions
            .lock()
            .unwrap()
            .insert(session.id, session.clone());
        Ok(session)
    }

    async fn close_session(&self, session_id: Uuid) -> RamariaResult<()> {
        if let Some(s) = self.sessions.lock().unwrap().get_mut(&session_id) {
            s.ended_at = Some(1_717_986_240_000);
        }
        Ok(())
    }

    async fn get_session(&self, session_id: Uuid) -> RamariaResult<Option<Session>> {
        Ok(self.sessions.lock().unwrap().get(&session_id).cloned())
    }

    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
        Ok(self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.ended_at.is_none())
            .cloned()
            .collect())
    }

    async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
        Ok(self.sessions.lock().unwrap().values().cloned().collect())
    }

    async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()> {
        self.sessions.lock().unwrap().remove(&session_id);
        self.messages.lock().unwrap().remove(&session_id);
        Ok(())
    }

    async fn save_message(&self, message: &Message) -> RamariaResult<()> {
        self.messages
            .lock()
            .unwrap()
            .entry(message.session_id)
            .or_default()
            .push(message.clone());
        Ok(())
    }

    async fn list_messages(&self, session_id: Uuid) -> RamariaResult<Vec<Message>> {
        Ok(self
            .messages
            .lock()
            .unwrap()
            .get(&session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn list_messages_by_persona(&self, _persona_uid: &str) -> RamariaResult<Vec<Message>> {
        Ok(Vec::new())
    }

    async fn save_memory_l1(&self, _memory: &MemoryL1) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_memory_l1(&self, _session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        Ok(self
            .l1_list
            .lock()
            .unwrap()
            .get(&_session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn get_memory_l1(&self, _id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        Ok(None)
    }

    async fn mark_l1_absorbed(&self, _l1_ids: &[Uuid]) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_unabsorbed_l1(&self, _persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> {
        // 返回所有 L1（MockStorage 不做 absorb 标记区分）
        let all: Vec<MemoryL1> = self
            .l1_list
            .lock()
            .unwrap()
            .values()
            .flatten()
            .cloned()
            .collect();
        Ok(all)
    }

    async fn list_unabsorbed_l1_unbound(&self) -> RamariaResult<Vec<MemoryL1>> {
        // MockStorage 不做 absorb/persona 区分：返回全部 L1（与 list_unabsorbed_l1 一致）
        self.list_unabsorbed_l1("").await
    }

    async fn create_persona(&self, persona: &Persona) -> RamariaResult<i64> {
        let mut seq = self.persona_seq.lock().unwrap();
        *seq += 1;
        let id = *seq;
        let mut p = persona.clone();
        p.id = id;
        self.personas.lock().unwrap().insert(persona.uid.clone(), p);
        Ok(id)
    }

    async fn get_persona_by_uid(&self, uid: &str) -> RamariaResult<Option<Persona>> {
        Ok(self.personas.lock().unwrap().get(uid).cloned())
    }

    async fn list_personas(&self) -> RamariaResult<Vec<Persona>> {
        Ok(self.personas.lock().unwrap().values().cloned().collect())
    }

    async fn update_persona(
        &self,
        uid: &str,
        name: &str,
        avatar: Option<&str>,
        config: Option<&str>,
        _description: Option<&str>,
    ) -> RamariaResult<()> {
        let mut personas = self.personas.lock().unwrap();
        if let Some(p) = personas.get_mut(uid) {
            p.name = name.to_string();
            if let Some(av) = avatar {
                p.avatar = Some(av.to_string());
            }
            if let Some(cfg) = config {
                p.config = Some(cfg.to_string());
            }
            tracing::debug!(%uid, "MockStorage: persona 已更新");
            Ok(())
        } else {
            Err(RamariaError::storage(format!("persona 不存在: uid={uid}")))
        }
    }

    async fn save_event(&self, event: &MemoryEvent) -> RamariaResult<i64> {
        let id = self
            .event_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut ev = event.clone();
        ev.id = id;
        self.events
            .lock()
            .unwrap()
            .entry(ev.persona_uid.clone())
            .or_default()
            .push(ev);
        Ok(id)
    }

    async fn get_event(&self, id: i64) -> RamariaResult<Option<MemoryEvent>> {
        let events = self.events.lock().unwrap();
        Ok(events.values().flatten().find(|e| e.id == id).cloned())
    }

    async fn list_events_by_persona(
        &self,
        persona_uid: &str,
        _offset: i64,
        _limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        Ok(self
            .events
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default())
    }

    async fn list_unabsorbed_events(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        self.list_events_by_persona(persona_uid, 0, i64::MAX).await
    }

    async fn mark_events_absorbed(&self, _event_ids: &[i64]) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_event_relation(&self, _rel: &EventRelation) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn save_event_source(
        &self,
        _event_id: i64,
        _l1_id: Uuid,
        _weight: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_fact(&self, _fact: &PersonaFact) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_facts_by_persona(
        &self,
        _persona_uid: &str,
        _field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        Ok(Vec::new())
    }

    async fn save_trait(&self, _t: &PersonalityTrait) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_traits_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonalityTrait>> {
        Ok(self
            .traits
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default())
    }

    async fn update_trait_confidence(
        &self,
        _id: i64,
        _confidence: f64,
        _evidence: f64,
        _consistency: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn update_trait_status(&self, _id: i64, _status: TraitStatus) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_evidence(&self, _e: &TraitEvidence) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_evidence_by_trait(&self, _trait_id: i64) -> RamariaResult<Vec<TraitEvidence>> {
        Ok(Vec::new())
    }

    async fn save_example(&self, _e: &PersonaExample) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_selected_examples(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaExample>> {
        Ok(self
            .examples
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default())
    }

    async fn save_cluster_snapshot(&self, _s: &ClusterSnapshot) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn get_current_snapshots(
        &self,
        _persona_uid: &str,
        _category: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        Ok(Vec::new())
    }

    async fn upsert_keyword(&self, _keyword: &str) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn save_privacy_consent(&self, consent: &PrivacyConsent) -> RamariaResult<()> {
        self.privacy_consents.lock().unwrap().push(consent.clone());
        Ok(())
    }

    async fn get_privacy_consent(
        &self,
        provider: &str,
        base_url: &str,
    ) -> RamariaResult<Option<PrivacyConsent>> {
        Ok(self
            .privacy_consents
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|c| c.provider.as_str() == provider && c.base_url == base_url)
            .cloned())
    }

    async fn save_backend_config(&self, config: &BackendConfig) -> RamariaResult<()> {
        *self.backend_config.lock().unwrap() = Some(config.clone());
        Ok(())
    }

    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        Ok(self.backend_config.lock().unwrap().clone())
    }

    async fn get_schema_version(&self) -> RamariaResult<i32> {
        Ok(1)
    }

    async fn get_index_version(&self) -> RamariaResult<i32> {
        Ok(*self.index_version.lock().unwrap())
    }

    async fn set_index_version(&self, version: i32) -> RamariaResult<()> {
        *self.index_version.lock().unwrap() = version;
        Ok(())
    }

    // ---- 基础设施方法 ----

    async fn create_background_job(
        &self,
        _job_type: &str,
        _payload: Option<&str>,
    ) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn update_job_status(
        &self,
        _id: i64,
        _status: &str,
        _error: Option<&str>,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        Ok(Vec::new())
    }

    async fn create_conflict(
        &self,
        _field: &str,
        _conflict_type: &str,
        _old_content: Option<&str>,
        _new_content: Option<&str>,
        _desc: Option<&str>,
    ) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_pending_conflicts(&self) -> RamariaResult<Vec<(i64, String, String, String)>> {
        Ok(Vec::new())
    }

    async fn resolve_conflict(&self, _id: i64) -> RamariaResult<()> {
        Ok(())
    }

    async fn get_setting(&self, key: &str) -> RamariaResult<Option<String>> {
        Ok(self.settings.lock().unwrap().get(key).cloned())
    }

    async fn set_setting(&self, key: &str, value: &str) -> RamariaResult<()> {
        self.settings
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        Ok(self
            .settings
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    async fn insert_graph_node(
        &self,
        _entity_name: &str,
        _entity_type: &str,
        _source_l1_id: Option<Uuid>,
    ) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn get_graph_node(
        &self,
        _entity_name: &str,
    ) -> RamariaResult<Option<(i64, String, String)>> {
        Ok(None)
    }

    async fn insert_graph_edge(
        &self,
        _source_id: i64,
        _target_id: i64,
        _relation_type: &str,
        _detail: Option<&str>,
        _source_l1_id: Option<Uuid>,
    ) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_graph_edges(
        &self,
        _source_id: i64,
    ) -> RamariaResult<Vec<(i64, i64, i64, String)>> {
        Ok(Vec::new())
    }

    // =========================================================
    // Keyword Refs (Mock 空实现)
    // =========================================================

    async fn insert_keyword_ref(
        &self,
        _keyword_id: &str,
        _doc_type: &str,
        _doc_id: &str,
        _persona_uid: &str,
        _weight: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn find_refs_by_keyword(
        &self,
        _keyword_id: &str,
    ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>> {
        Ok(Vec::new())
    }

    async fn find_refs_by_doc(
        &self,
        _doc_type: &str,
        _doc_id: &str,
    ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>> {
        Ok(Vec::new())
    }

    async fn delete_refs_by_doc(&self, _doc_type: &str, _doc_id: &str) -> RamariaResult<u64> {
        Ok(0)
    }

    // -- 行为规则（CLI 测试） --

    async fn save_behavior_rule(&self, rule: &BehaviorRule) -> RamariaResult<i64> {
        let id = self
            .rule_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut r = rule.clone();
        r.id = id;
        self.behavior_rules.lock().unwrap().insert(id, r.clone());
        self.rules_by_persona
            .lock()
            .unwrap()
            .entry(r.persona_uid.clone())
            .or_default()
            .push(id);
        Ok(id)
    }

    async fn get_behavior_rule(&self, id: i64) -> RamariaResult<Option<BehaviorRule>> {
        Ok(self.behavior_rules.lock().unwrap().get(&id).cloned())
    }

    async fn list_behavior_rules_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<BehaviorRule>> {
        let ids = self
            .rules_by_persona
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let rules = self.behavior_rules.lock().unwrap();
        Ok(ids.iter().filter_map(|id| rules.get(id).cloned()).collect())
    }

    async fn update_behavior_rule(&self, rule: &BehaviorRule) -> RamariaResult<()> {
        self.behavior_rules
            .lock()
            .unwrap()
            .insert(rule.id, rule.clone());
        Ok(())
    }

    async fn delete_behavior_rule(&self, id: i64) -> RamariaResult<()> {
        let removed = self.behavior_rules.lock().unwrap().remove(&id);
        if let Some(rule) = removed {
            let mut by_persona = self.rules_by_persona.lock().unwrap();
            if let Some(ids) = by_persona.get_mut(&rule.persona_uid) {
                ids.retain(|&x| x != id);
            }
        }
        Ok(())
    }

    async fn set_rule_enabled(&self, id: i64, enabled: bool) -> RamariaResult<()> {
        let mut rules = self.behavior_rules.lock().unwrap();
        if let Some(rule) = rules.get_mut(&id) {
            rule.enabled = enabled;
        }
        Ok(())
    }

    async fn save_feedback_log(&self, log: &FeedbackLog) -> RamariaResult<i64> {
        let id = self
            .feedback_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut l = log.clone();
        l.id = id;
        self.feedback_logs.lock().unwrap().push(l);
        Ok(id)
    }

    async fn list_feedback_logs_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<FeedbackLog>> {
        Ok(self
            .feedback_logs
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.persona_uid == persona_uid)
            .cloned()
            .collect())
    }
}

// =========================================================
// MockLlm
// =========================================================

/// Mock LLM Provider，返回预设回复。
pub struct MockLlm {
    reply: String,
    model_capability: ModelCapability,
    config: BackendConfig,
}

impl MockLlm {
    /// 创建返回固定回复的 Mock LLM。
    pub fn new(reply: &str) -> Self {
        let config = BackendConfig::lm_studio_default();
        Self {
            reply: reply.to_string(),
            model_capability: ModelCapability {
                provider: LlmProviderKind::LmStudio,
                model_id: "mock-model".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
            config,
        }
    }

    /// 创建使用指定 BackendConfig 的 Mock LLM（用于 config 命令测试）。
    pub fn with_config(config: BackendConfig) -> Self {
        Self {
            reply: "mock reply".to_string(),
            model_capability: ModelCapability {
                provider: config.provider,
                model_id: config.capability.model_id.clone(),
                base_url: config.base_url.clone(),
                supports_streaming: config.capability.supports_streaming,
                supports_json_mode: config.capability.supports_json_mode,
                context_window: config.capability.context_window,
                max_output_tokens: config.capability.max_output_tokens,
            },
            config,
        }
    }
}

#[async_trait]
impl LlmProvider for MockLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        Ok(self.reply.clone())
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        let reply = self.reply.clone();
        let chars: Vec<char> = reply.chars().collect();
        let stream = stream::iter(chars.into_iter().enumerate().map(move |(i, c)| {
            let is_last = i == reply.chars().count() - 1;
            Ok(StreamDelta {
                content: c.to_string(),
                done: is_last,
                metadata: if is_last { Some("stop".into()) } else { None },
            })
        }));
        Ok(Box::pin(stream))
    }

    fn capability(&self) -> &ModelCapability {
        &self.model_capability
    }

    fn config(&self) -> &BackendConfig {
        &self.config
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "MockLlm"
    }
}

// =========================================================
// MockEmbedding — 占位
// =========================================================

pub struct MockEmbedding {
    model_info: EmbeddingModelInfo,
}

impl MockEmbedding {
    pub fn new() -> Self {
        Self {
            model_info: EmbeddingModelInfo {
                model_id: "mock-embedding".into(),
                dimension: 128,
            },
        }
    }
}

#[async_trait]
impl EmbeddingProvider for MockEmbedding {
    async fn embed(&self, _text: &str) -> RamariaResult<Vec<f32>> {
        Ok(vec![0.0; 128])
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0; 128]).collect())
    }

    fn model_info(&self) -> EmbeddingModelInfo {
        self.model_info.clone()
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    async fn download_model(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn download_progress(&self) -> f64 {
        1.0
    }

    fn is_available(&self) -> bool {
        true
    }
}

// =========================================================
// App 构造器
// =========================================================

/// 构造一个 Ready 状态的测试 App 实例。
///
/// 使用 LM Studio provider（本地，无需隐私确认），MockStorage 和 MockLlm。
/// 返回 (Arc<App>, Arc<MockStorage>) 以便测试代码可直接操作 mock 数据。
pub fn build_test_app() -> (Arc<ramaria_app::App>, Arc<MockStorage>) {
    use ramaria_app::App;
    use ramaria_core::config::RamariaConfig;

    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("Hello, World!"));
    let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());
    let config = RamariaConfig::default();

    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        config,
        keychain,
    );

    // 设置为 Ready 状态以跳过 setup 检查
    app.set_state(ramaria_core::types::AppState::Ready);

    (Arc::new(app), storage)
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
