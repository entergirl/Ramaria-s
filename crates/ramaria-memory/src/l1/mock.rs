//! rust/crates/ramaria-memory/src/l1/mock.rs - 测试用 mock LlmProvider + StorageBackend
//!
//! 设计特点:
//! - 仅 #[cfg(test)] 编译，零运行时开销
//! - MockLlmProvider: 可预设 chat 返回值，用于模拟 LLM 响应
//! - MockStorage: 内存 HashMap 存储 messages / l1 / keywords
//! - 未实现的方法返回 `unimplemented!()`，确保测试仅覆盖声明路径
//! - 所有存储操作为同步（直接插入 HashMap），不需要真实数据库

#![allow(dead_code)]

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Mutex;

use async_trait::async_trait;
use ramaria_core::traits::{ChatRequest, StreamDelta};
use ramaria_core::types::{
    BackendConfig, ClusterSnapshot, EventRelation, MemoryEvent, MemoryL1, Message, ModelCapability,
    Persona, PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent, ProfileField, Session,
    TraitEvidence, TraitStatus,
};
use ramaria_core::{LlmProviderTrait, RamariaError, RamariaResult, StorageBackend};
use uuid::Uuid;

// =========================================================
// Mock LLM Provider
// =========================================================

/// 测试用 mock LLM Provider。
///
/// 用法:
/// - 通过 `set_response()` 预设下次 `chat()` 返回的文本。
/// - `chat_stream()` 返回 unimplemented（摘要管线不使用流式）。
pub struct MockLlmProvider {
    name: &'static str,
    response: Mutex<Option<String>>,
}

impl MockLlmProvider {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            response: Mutex::new(None),
        }
    }

    /// 预设下次 chat() 的返回值。
    pub fn set_response(&self, text: impl Into<String>) {
        *self.response.lock().unwrap() = Some(text.into());
    }
}

#[async_trait]
impl LlmProviderTrait for MockLlmProvider {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        self.response
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| RamariaError::llm("mock: 未预设 chat 返回值"))
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn futures::Stream<Item = RamariaResult<StreamDelta>> + Send>>>
    {
        unimplemented!("mock chat_stream 未实现")
    }

    fn capability(&self) -> &ModelCapability {
        unimplemented!("mock capability 未实现")
    }

    fn config(&self) -> &BackendConfig {
        unimplemented!("mock config 未实现")
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        self.name
    }
}

// =========================================================
// Mock Storage Backend
// =========================================================

/// 测试用内存存储后端。
///
/// 仅实现 L1 摘要管线需要的方法:
/// - `list_messages`, `list_keywords`, `save_memory_l1`, `upsert_keyword`
///
/// 其余方法返回 `unimplemented!()`，确保测试边界清晰。
pub struct MockStorage {
    messages: Mutex<HashMap<Uuid, Vec<Message>>>,
    l1_entries: Mutex<Vec<MemoryL1>>,
    keywords: Mutex<Vec<String>>,
}

impl MockStorage {
    pub fn new() -> Self {
        Self {
            messages: Mutex::new(HashMap::new()),
            l1_entries: Mutex::new(Vec::new()),
            keywords: Mutex::new(Vec::new()),
        }
    }

    /// 为指定 session 添加消息。
    pub fn add_messages(&self, session_id: Uuid, msgs: Vec<Message>) {
        self.messages.lock().unwrap().insert(session_id, msgs);
    }

    /// 设置关键词词典。
    pub fn set_keywords(&self, kws: Vec<String>) {
        *self.keywords.lock().unwrap() = kws;
    }

    /// 获取已保存的 L1 条目（用于断言）。
    pub fn saved_l1_entries(&self) -> Vec<MemoryL1> {
        self.l1_entries.lock().unwrap().clone()
    }

    /// 获取关键词 upsert 调用次数（用于断言）。
    pub fn keyword_count(&self) -> usize {
        self.keywords.lock().unwrap().len()
    }
}

#[async_trait]
impl StorageBackend for MockStorage {
    // -- Session (unused by summarizer) --
    async fn create_session(&self) -> RamariaResult<Session> {
        unimplemented!()
    }
    async fn close_session(&self, _: Uuid) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn get_session(&self, _: Uuid) -> RamariaResult<Option<Session>> {
        unimplemented!()
    }
    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
        unimplemented!()
    }
    async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
        unimplemented!()
    }
    async fn delete_session(&self, _: Uuid) -> RamariaResult<()> {
        unimplemented!()
    }

    // -- Message (used by summarizer) --
    async fn save_message(&self, _: &Message) -> RamariaResult<()> {
        unimplemented!()
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

    async fn list_messages_by_persona(&self, _: &str) -> RamariaResult<Vec<Message>> {
        unimplemented!()
    }
    async fn find_message_by_fingerprint(&self, _: &str) -> RamariaResult<Option<Message>> {
        unimplemented!()
    }

    // -- Memory L1 (used by summarizer) --
    async fn save_memory_l1(&self, memory: &MemoryL1) -> RamariaResult<()> {
        self.l1_entries.lock().unwrap().push(memory.clone());
        Ok(())
    }

    async fn list_memory_l1(&self, _: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        unimplemented!()
    }
    async fn get_memory_l1(&self, _: Uuid) -> RamariaResult<Option<MemoryL1>> {
        unimplemented!()
    }
    async fn mark_l1_absorbed(&self, _: &[Uuid]) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn list_unabsorbed_l1(&self, _: &str) -> RamariaResult<Vec<MemoryL1>> {
        unimplemented!()
    }

    // -- Personas --
    async fn create_persona(&self, _: &Persona) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn get_persona_by_uid(&self, _: &str) -> RamariaResult<Option<Persona>> {
        unimplemented!()
    }
    async fn list_personas(&self) -> RamariaResult<Vec<Persona>> {
        unimplemented!()
    }
    async fn update_persona(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> RamariaResult<()> {
        unimplemented!()
    }

    // -- Memory Events --
    async fn save_event(&self, _: &MemoryEvent) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_events_by_persona(
        &self,
        _: &str,
        _: i64,
        _: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        unimplemented!()
    }
    async fn list_unabsorbed_events(&self, _: &str) -> RamariaResult<Vec<MemoryEvent>> {
        unimplemented!()
    }

    // -- Event Relations --
    async fn save_event_relation(&self, _: &EventRelation) -> RamariaResult<i64> {
        unimplemented!()
    }

    // -- Event Sources --
    async fn save_event_source(&self, _: i64, _: Uuid, _: f64) -> RamariaResult<()> {
        unimplemented!()
    }

    // -- Persona Facts --
    async fn save_fact(&self, _: &PersonaFact) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_facts_by_persona(
        &self,
        _: &str,
        _: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        unimplemented!()
    }

    // -- Personality Traits --
    async fn save_trait(&self, _: &PersonalityTrait) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_traits_by_persona(&self, _: &str) -> RamariaResult<Vec<PersonalityTrait>> {
        unimplemented!()
    }
    async fn update_trait_confidence(&self, _: i64, _: f64, _: f64, _: f64) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn update_trait_status(&self, _: i64, _: TraitStatus) -> RamariaResult<()> {
        unimplemented!()
    }

    // -- Trait Evidence --
    async fn save_evidence(&self, _: &TraitEvidence) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_evidence_by_trait(&self, _: i64) -> RamariaResult<Vec<TraitEvidence>> {
        unimplemented!()
    }

    // -- Persona Examples --
    async fn save_example(&self, _: &PersonaExample) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_selected_examples(&self, _: &str) -> RamariaResult<Vec<PersonaExample>> {
        unimplemented!()
    }

    // -- Cluster Snapshots --
    async fn save_cluster_snapshot(&self, _: &ClusterSnapshot) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn get_current_snapshots(&self, _: &str, _: &str) -> RamariaResult<Vec<ClusterSnapshot>> {
        unimplemented!()
    }

    // -- Keyword Pool (used by summarizer) --
    async fn upsert_keyword(&self, keyword: &str) -> RamariaResult<()> {
        let mut kws = self.keywords.lock().unwrap();
        if !kws.contains(&keyword.to_string()) {
            kws.push(keyword.to_string());
        }
        Ok(())
    }

    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        Ok(self.keywords.lock().unwrap().clone())
    }

    // -- Privacy Consent --
    async fn save_privacy_consent(&self, _: &PrivacyConsent) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn get_privacy_consent(&self, _: &str, _: &str) -> RamariaResult<Option<PrivacyConsent>> {
        unimplemented!()
    }

    // -- Backend Config --
    async fn save_backend_config(&self, _: &BackendConfig) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        unimplemented!()
    }

    // -- 索引一致性 --
    async fn get_schema_version(&self) -> RamariaResult<i32> {
        unimplemented!()
    }
    async fn get_index_version(&self) -> RamariaResult<i32> {
        unimplemented!()
    }
    async fn set_index_version(&self, _: i32) -> RamariaResult<()> {
        unimplemented!()
    }

    // -- 基础设施 --
    async fn create_background_job(&self, _: &str, _: Option<&str>) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn update_job_status(&self, _: i64, _: &str, _: Option<&str>) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        unimplemented!()
    }
    async fn create_conflict(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
    ) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_pending_conflicts(&self) -> RamariaResult<Vec<(i64, String, String, String)>> {
        unimplemented!()
    }
    async fn resolve_conflict(&self, _: i64) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn create_push(&self, _: &str) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_pending_pushes(&self) -> RamariaResult<Vec<(i64, String)>> {
        unimplemented!()
    }
    async fn mark_push_sent(&self, _: i64) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn get_setting(&self, _: &str) -> RamariaResult<Option<String>> {
        unimplemented!()
    }
    async fn set_setting(&self, _: &str, _: &str) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        unimplemented!()
    }
    async fn save_bm25(&self, _: i64, _: &str, _: &str) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn list_bm25_by_doc(&self, _: i64) -> RamariaResult<Vec<(String, String)>> {
        unimplemented!()
    }
    async fn delete_bm25_by_doc(&self, _: i64) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn insert_graph_node(&self, _: &str, _: &str, _: Option<Uuid>) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn get_graph_node(&self, _: &str) -> RamariaResult<Option<(i64, String, String)>> {
        unimplemented!()
    }
    async fn insert_graph_edge(
        &self,
        _: i64,
        _: i64,
        _: &str,
        _: Option<&str>,
        _: Option<Uuid>,
    ) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_graph_edges(&self, _: i64) -> RamariaResult<Vec<(i64, i64, i64, String)>> {
        unimplemented!()
    }
}

// =========================================================
// 辅助函数：构造测试用消息
// =========================================================

/// 创建测试用消息。
pub fn make_msg(
    session_id: Uuid,
    role: ramaria_core::types::MessageRole,
    content: &str,
) -> Message {
    Message::new(
        session_id,
        role,
        content.to_string(),
        ramaria_core::types::MessageSource::Local,
    )
}
