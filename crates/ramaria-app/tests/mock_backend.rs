//! rust/crates/ramaria-app/tests/mock_backend.rs - Mock StorageBackend + Mock LlmProvider
//!
//! 设计特点:
//! - `MockStorage`: 内存 HashMap 实现的 StorageBackend，用于 app 集成测试
//! - `MockLlm`: 返回预设回复的 LlmProvider，支持流式和非流式
//! - 所有 mock 都是 Send + Sync，可直接用于 Arc<dyn Trait>
//! - 支持测试场景：空存储、已有会话/消息、LLM 正常/错误回复
//!
//! 安全约束:
//! - 不使用真实 API key 或网络请求
//! - 不触碰文件系统

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::{Stream, stream};
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
// MockStorage
// =========================================================

/// 内存 Mock StorageBackend 实现。
///
/// 职责:
/// - 替代真实的 SQLite storage，支持 app 层集成测试
/// - 所有数据存于 HashMap，测试间完全隔离
pub struct MockStorage {
    sessions: Mutex<HashMap<Uuid, Session>>,
    messages: Mutex<HashMap<Uuid, Vec<Message>>>,
    #[allow(dead_code)]
    l1_list: Mutex<HashMap<Uuid, Vec<MemoryL1>>>,
    personas: Mutex<HashMap<String, Persona>>,
    persona_seq: Mutex<i64>,
    privacy_consents: Mutex<Vec<PrivacyConsent>>,
    backend_config: Mutex<Option<BackendConfig>>,
    index_version: Mutex<i32>,
    examples: Mutex<HashMap<String, Vec<PersonaExample>>>,
}

impl Default for MockStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl MockStorage {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            messages: Mutex::new(HashMap::new()),
            l1_list: Mutex::new(HashMap::new()),
            personas: Mutex::new(HashMap::new()),
            persona_seq: Mutex::new(0),
            privacy_consents: Mutex::new(Vec::new()),
            backend_config: Mutex::new(None),
            index_version: Mutex::new(0),
            examples: Mutex::new(HashMap::new()),
        }
    }

    /// 便捷方法：创建会话并预填充消息。
    #[allow(dead_code)]
    pub fn create_session_with_messages(&self, session_id: Uuid, messages: Vec<Message>) {
        self.sessions.lock().unwrap().insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1000,
                ended_at: None,
            },
        );
        self.messages.lock().unwrap().insert(session_id, messages);
    }

    /// 便捷方法：添加隐私确认。
    #[allow(dead_code)]
    pub fn add_privacy_consent(&self, consent: PrivacyConsent) {
        self.privacy_consents.lock().unwrap().push(consent);
    }

    /// 便捷方法：设置后端配置。
    #[allow(dead_code)]
    pub fn set_backend_config(&self, config: BackendConfig) {
        *self.backend_config.lock().unwrap() = Some(config);
    }
}

#[async_trait]
impl StorageBackend for MockStorage {
    async fn create_session(&self) -> RamariaResult<Session> {
        let session = Session {
            id: Uuid::new_v4(),
            started_at: 1000,
            ended_at: None,
        };
        self.sessions
            .lock()
            .unwrap()
            .insert(session.id, session.clone());
        Ok(session)
    }

    async fn close_session(&self, session_id: Uuid) -> RamariaResult<()> {
        if let Some(session) = self.sessions.lock().unwrap().get_mut(&session_id) {
            session.ended_at = Some(2000);
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
        // v1.1: 只读约束——已关闭 session 不可写入新消息
        let sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get(&message.session_id) {
            if session.ended_at.is_some() {
                return Err(RamariaError::validation(format!(
                    "session {} 已关闭，不可写入新消息",
                    message.session_id
                )));
            }
        }
        drop(sessions);

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

    async fn find_message_by_fingerprint(
        &self,
        _fingerprint: &str,
    ) -> RamariaResult<Option<Message>> {
        Ok(None)
    }

    async fn save_memory_l1(&self, _memory: &MemoryL1) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_memory_l1(&self, _session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        Ok(Vec::new())
    }

    async fn get_memory_l1(&self, _id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        Ok(None)
    }

    async fn mark_l1_absorbed(&self, _l1_ids: &[Uuid]) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_unabsorbed_l1(&self, _persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> {
        Ok(Vec::new())
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
        _uid: &str,
        _name: &str,
        _avatar: Option<&str>,
        _config: Option<&str>,
        _description: Option<&str>,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_event(&self, _event: &MemoryEvent) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_events_by_persona(
        &self,
        _persona_uid: &str,
        _offset: i64,
        _limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        Ok(Vec::new())
    }

    async fn list_unabsorbed_events(&self, _persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        Ok(Vec::new())
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
        _persona_uid: &str,
    ) -> RamariaResult<Vec<PersonalityTrait>> {
        Ok(Vec::new())
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

    async fn create_push(&self, _content: &str) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_pending_pushes(&self) -> RamariaResult<Vec<(i64, String)>> {
        Ok(Vec::new())
    }

    async fn mark_push_sent(&self, _id: i64) -> RamariaResult<()> {
        Ok(())
    }

    async fn get_setting(&self, _key: &str) -> RamariaResult<Option<String>> {
        Ok(None)
    }

    async fn set_setting(&self, _key: &str, _value: &str) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    async fn save_bm25(&self, _doc_id: i64, _layer: &str, _tokens_json: &str) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_bm25_by_doc(&self, _doc_id: i64) -> RamariaResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    async fn delete_bm25_by_doc(&self, _doc_id: i64) -> RamariaResult<()> {
        Ok(())
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
            config: BackendConfig::lm_studio_default(),
        }
    }

    /// 创建返回错误的 Mock LLM。
    #[allow(dead_code)]
    pub fn failing(error_msg: &str) -> MockFailingLlm {
        MockFailingLlm {
            error_msg: error_msg.to_string(),
            model_capability: ModelCapability {
                provider: LlmProviderKind::LmStudio,
                model_id: "mock-model".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
            config: BackendConfig::lm_studio_default(),
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
            Ok(StreamDelta {
                content: c.to_string(),
                done: i == reply.chars().count() - 1,
                metadata: if i == reply.chars().count() - 1 {
                    Some("stop".into())
                } else {
                    None
                },
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
// MockFailingLlm — 始终返回错误的 Mock
// =========================================================

/// Mock LLM Provider，始终返回错误（用于测试错误处理路径）。
#[allow(dead_code)]
pub struct MockFailingLlm {
    error_msg: String,
    model_capability: ModelCapability,
    config: BackendConfig,
}

#[async_trait]
impl LlmProvider for MockFailingLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        Err(RamariaError::llm(self.error_msg.clone()))
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        Err(RamariaError::llm(self.error_msg.clone()))
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
        "MockFailingLlm"
    }
}

// =========================================================
// MockEmbedding — 占位 Embedding Provider
// =========================================================

/// Mock Embedding Provider（不上真实模型）。
#[allow(dead_code)]
pub struct MockEmbedding {
    model_info: EmbeddingModelInfo,
}

impl Default for MockEmbedding {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
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

    fn model_info(&self) -> &EmbeddingModelInfo {
        &self.model_info
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
