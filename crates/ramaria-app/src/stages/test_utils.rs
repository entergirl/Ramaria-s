//! rust/crates/ramaria-app/src/stages/test_utils.rs - Stage 单元测试共享 Mock 工具
//!
//! 设计特点:
//! - 提供功能完整的 MockStorage（HashMap 实现，支持 session/message/L1 增删查改）
//! - 提供可配置的 MockLlm（支持自定义回复、流式输出、隐私确认状态）
//! - 提供 test_context() 快速构建 PipelineContext
//! - 所有 Mock 均为 Send + Sync，可直接用于 Arc<dyn Trait>

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
    BackendConfig, ClusterSnapshot, EventRelation, MemoryEvent, MemoryL1, Message, ModelCapability,
    Persona, PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent, ProfileField, Session,
    TraitEvidence, TraitStatus,
};
use ramaria_memory::retriever::Retriever;
use uuid::Uuid;

use std::sync::{Arc, Mutex as StdMutex};

use crate::pipeline::PipelineContext;
use crate::session_lifecycle::SessionLifecycle;

// =========================================================
// MockStorage — 功能完整的内存 StorageBackend
// =========================================================

/// 内存 Mock StorageBackend，支持 session/message/L1/privacy 的增删查改。
///
/// 设计:
/// - 所有数据存于 HashMap，测试间完全隔离
/// - 支持配置预填充数据（session、message、L1、privacy_consent）
/// - 适配 Stage 3-4 测试需求
pub struct MockStorage {
    sessions: Mutex<HashMap<Uuid, Session>>,
    messages: Mutex<HashMap<Uuid, Vec<Message>>>,
    l1_by_persona: Mutex<HashMap<String, Vec<MemoryL1>>>,
    privacy_consents: Mutex<Vec<PrivacyConsent>>,
    backend_config: Mutex<Option<BackendConfig>>,
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
            l1_by_persona: Mutex::new(HashMap::new()),
            privacy_consents: Mutex::new(Vec::new()),
            backend_config: Mutex::new(None),
        }
    }

    /// 预填充一个活跃 session 并返回其 ID。
    pub fn add_active_session(&self, session_id: Uuid) {
        self.sessions.lock().unwrap().insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1000,
                ended_at: None,
                persona_uid: None,
            },
        );
    }

    /// 预填充一个已关闭 session 并返回其 ID。
    pub fn add_closed_session(&self, session_id: Uuid) {
        self.sessions.lock().unwrap().insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1000,
                ended_at: Some(2000),
                persona_uid: None,
            },
        );
    }

    /// 预填充消息到指定 session。
    pub fn add_messages(&self, session_id: Uuid, messages: Vec<Message>) {
        self.messages.lock().unwrap().insert(session_id, messages);
    }

    /// 预填充 L1 摘要到指定 persona。
    pub fn add_l1_summaries(&self, persona_uid: &str, summaries: Vec<MemoryL1>) {
        self.l1_by_persona
            .lock()
            .unwrap()
            .insert(persona_uid.to_string(), summaries);
    }

    /// 预填充隐私确认记录。
    pub fn add_privacy_consent(&self, consent: PrivacyConsent) {
        self.privacy_consents.lock().unwrap().push(consent);
    }
}

#[async_trait]
impl StorageBackend for MockStorage {
    async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
        let session = Session::with_persona(persona_uid.map(|s| s.to_string()));
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

    async fn list_messages_by_persona(&self, _uid: &str) -> RamariaResult<Vec<Message>> {
        Ok(Vec::new())
    }

    async fn find_message_by_fingerprint(&self, _fp: &str) -> RamariaResult<Option<Message>> {
        Ok(None)
    }

    async fn save_memory_l1(&self, _m: &MemoryL1) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_memory_l1(&self, _session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        Ok(Vec::new())
    }

    async fn get_memory_l1(&self, _id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        Ok(None)
    }

    async fn mark_l1_absorbed(&self, _ids: &[Uuid]) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_unabsorbed_l1(&self, _uid: &str) -> RamariaResult<Vec<MemoryL1>> {
        Ok(Vec::new())
    }

    async fn list_recent_l1_by_persona(
        &self,
        persona_uid: &str,
        limit: u32,
    ) -> RamariaResult<Vec<MemoryL1>> {
        Ok(self
            .l1_by_persona
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(limit as usize)
            .collect())
    }

    async fn create_persona(&self, _p: &Persona) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn get_persona_by_uid(&self, _uid: &str) -> RamariaResult<Option<Persona>> {
        Ok(None)
    }

    async fn list_personas(&self) -> RamariaResult<Vec<Persona>> {
        Ok(Vec::new())
    }

    async fn update_persona(
        &self,
        _uid: &str,
        _name: &str,
        _avatar: Option<&str>,
        _config: Option<&str>,
        _desc: Option<&str>,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_event(&self, _e: &MemoryEvent) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_events_by_persona(
        &self,
        _uid: &str,
        _offset: i64,
        _limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        Ok(Vec::new())
    }

    async fn list_unabsorbed_events(&self, _uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        Ok(Vec::new())
    }

    async fn mark_events_absorbed(&self, _event_ids: &[i64]) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_event_relation(&self, _r: &EventRelation) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn save_event_source(&self, _eid: i64, _l1: Uuid, _w: f64) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_fact(&self, _f: &PersonaFact) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_facts_by_persona(
        &self,
        _uid: &str,
        _field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        Ok(Vec::new())
    }

    async fn save_trait(&self, _t: &PersonalityTrait) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_traits_by_persona(&self, _uid: &str) -> RamariaResult<Vec<PersonalityTrait>> {
        Ok(Vec::new())
    }

    async fn update_trait_confidence(
        &self,
        _id: i64,
        _c: f64,
        _e: f64,
        _cons: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn update_trait_status(&self, _id: i64, _s: TraitStatus) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_evidence(&self, _e: &TraitEvidence) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_evidence_by_trait(&self, _id: i64) -> RamariaResult<Vec<TraitEvidence>> {
        Ok(Vec::new())
    }

    async fn save_example(&self, _e: &PersonaExample) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_selected_examples(&self, _uid: &str) -> RamariaResult<Vec<PersonaExample>> {
        Ok(Vec::new())
    }

    async fn save_cluster_snapshot(&self, _s: &ClusterSnapshot) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn get_current_snapshots(
        &self,
        _uid: &str,
        _cat: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        Ok(Vec::new())
    }

    async fn upsert_keyword(&self, _k: &str) -> RamariaResult<()> {
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
        Ok(1)
    }

    async fn set_index_version(&self, _v: i32) -> RamariaResult<()> {
        Ok(())
    }

    async fn create_background_job(&self, _t: &str, _p: Option<&str>) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn update_job_status(&self, _id: i64, _s: &str, _e: Option<&str>) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        Ok(Vec::new())
    }

    async fn create_conflict(
        &self,
        _f: &str,
        _t: &str,
        _o: Option<&str>,
        _n: Option<&str>,
        _d: Option<&str>,
    ) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_pending_conflicts(&self) -> RamariaResult<Vec<(i64, String, String, String)>> {
        Ok(Vec::new())
    }

    async fn resolve_conflict(&self, _id: i64) -> RamariaResult<()> {
        Ok(())
    }

    async fn create_push(&self, _c: &str) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_pending_pushes(&self) -> RamariaResult<Vec<(i64, String)>> {
        Ok(Vec::new())
    }

    async fn mark_push_sent(&self, _id: i64) -> RamariaResult<()> {
        Ok(())
    }

    async fn get_setting(&self, _k: &str) -> RamariaResult<Option<String>> {
        Ok(None)
    }

    async fn set_setting(&self, _k: &str, _v: &str) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    async fn save_bm25(&self, _d: i64, _l: &str, _t: &str) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_bm25_by_doc(&self, _d: i64) -> RamariaResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    async fn delete_bm25_by_doc(&self, _d: i64) -> RamariaResult<()> {
        Ok(())
    }

    async fn insert_graph_node(&self, _n: &str, _t: &str, _l: Option<Uuid>) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn get_graph_node(&self, _n: &str) -> RamariaResult<Option<(i64, String, String)>> {
        Ok(None)
    }

    async fn insert_graph_edge(
        &self,
        _s: i64,
        _t: i64,
        _r: &str,
        _d: Option<&str>,
        _l: Option<Uuid>,
    ) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_graph_edges(&self, _s: i64) -> RamariaResult<Vec<(i64, i64, i64, String)>> {
        Ok(Vec::new())
    }
}

// =========================================================
// MockLlm — 可配置的 LLM Provider
// =========================================================

/// 可配置 Mock LLM Provider。
///
/// 设计:
/// - `config` 字段决定 provider 类型（LM Studio / DeepSeek / OpenAI）
/// - 用于 Stage 2 隐私检查测试：线上 provider 触发隐私确认
pub struct MockLlm {
    config: BackendConfig,
}

impl MockLlm {
    /// 创建 LM Studio 本地 provider 的 Mock。
    pub fn local() -> Self {
        Self {
            config: BackendConfig::lm_studio_default(),
        }
    }

    /// 创建 DeepSeek 线上 provider 的 Mock。
    pub fn online_deepseek() -> Self {
        Self {
            config: BackendConfig::deepseek_default(),
        }
    }
}

#[async_trait]
impl LlmProvider for MockLlm {
    async fn chat(&self, _req: &ChatRequest) -> RamariaResult<String> {
        Ok("mock reply".into())
    }

    async fn chat_stream(
        &self,
        _req: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![Ok(StreamDelta {
            content: "mock".into(),
            done: true,
            metadata: Some("stop".into()),
        })])))
    }

    fn capability(&self) -> &ModelCapability {
        &self.config.capability
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
// MockEmbedding — 可用嵌入模型 Mock
// =========================================================

/// Mock Embedding Provider，返回固定维度零向量。
pub struct MockEmbedding {
    model_info: EmbeddingModelInfo,
}

impl Default for MockEmbedding {
    fn default() -> Self {
        Self::new()
    }
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

// =========================================================
// test_context — 构建 PipelineContext
// =========================================================

/// 构建测试用 PipelineContext。
///
/// 参数:
/// - `storage`: 自定义 MockStorage（可预填充数据）。
/// - `llm`: 自定义 MockLlm（决定 provider 类型）。
/// - `embedding`: 可选嵌入模型（None 表示未配置）。
///
/// 返回:
/// - 可用于 Stage 测试的 PipelineContext。
pub fn test_context(
    storage: Arc<MockStorage>,
    llm: Arc<MockLlm>,
    embedding: Option<Arc<MockEmbedding>>,
) -> PipelineContext {
    let storage_dyn: Arc<dyn StorageBackend> = storage;
    let llm_dyn: Arc<dyn LlmProvider> = llm;
    let embedding_dyn: Option<Arc<dyn EmbeddingProvider>> =
        embedding.map(|e| e as Arc<dyn EmbeddingProvider>);

    let config = ramaria_core::config::RamariaConfig::default();
    let retriever = Arc::new(StdMutex::new(Retriever::new()));
    let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());
    let lifecycle = Arc::new(SessionLifecycle::new(config.clone()));

    PipelineContext::new(
        storage_dyn,
        llm_dyn,
        embedding_dyn,
        config,
        retriever,
        keychain,
        lifecycle,
    )
}

/// 构建最简测试 PipelineContext（本地 LLM，无嵌入，空存储）。
pub fn simple_context() -> PipelineContext {
    test_context(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::local()),
        None,
    )
}
