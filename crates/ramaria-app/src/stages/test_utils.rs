//! crates/ramaria-app/src/stages/test_utils.rs - Stage 单元测试共享 Mock 工具
//!
//! 设计特点:
//! - 提供功能完整的 MockStorage（HashMap 实现，支持 session/message/L1 增删查改）
//! - 提供可配置的 MockLlm（支持自定义回复、流式输出、隐私确认状态）
//! - 提供 test_context() 快速构建 PipelineContext
//! - 所有 Mock 均为 Send + Sync，可直接用于 Arc<dyn Trait>

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use futures::{Stream, stream};
use ramaria_core::behavior::FeedbackLog;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{
    ChatRequest, EmbeddingModelInfo, EmbeddingProvider, LlmProvider, StorageBackend, StreamDelta,
};
use ramaria_core::types::{
    BackendConfig, ClusterSnapshot, EventRelation, MemoryEvent, MemoryL1, Message, ModelCapability,
    Persona, PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent, ProfileField, Session,
    TraitEvidence, TraitStatus, UttBlock,
};
use ramaria_memory::retriever::Retriever;
use uuid::Uuid;

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
    examples: Mutex<Vec<PersonaExample>>,
    next_example_id: Mutex<i64>,
    personas: Mutex<HashMap<String, Persona>>,
    /// utt 话语块（按 session 索引，v1.4 M5：桥接测试支持）
    utt_blocks: Mutex<HashMap<Uuid, Vec<UttBlock>>>,
    /// 测试注入：bind_session_persona_uid 是否强制失败（降级路径测试）
    fail_bind: AtomicBool,
    /// touch_l1 调用记录（v1.7 touch 接线测试）：最近一次 touch 的 L1 id 列表
    touch_l1_ids: Mutex<Vec<Uuid>>,
    /// feedback_log 记录（v1.7 H2 弱反馈测试）：按 persona 索引
    feedback_logs: Mutex<Vec<ramaria_core::behavior::FeedbackLog>>,
    /// settings 键值（v1.7 H2 复审队列 / S3 历史测试）
    settings: Mutex<std::collections::HashMap<String, String>>,
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
            examples: Mutex::new(Vec::new()),
            next_example_id: Mutex::new(1),
            personas: Mutex::new(HashMap::new()),
            utt_blocks: Mutex::new(HashMap::new()),
            fail_bind: AtomicBool::new(false),
            touch_l1_ids: Mutex::new(Vec::new()),
            feedback_logs: Mutex::new(Vec::new()),
            settings: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// 返回最近一次 touch_l1 的 L1 id 列表（空表示从未调用）。
    pub fn last_touched_l1_ids(&self) -> Vec<Uuid> {
        self.touch_l1_ids.lock().unwrap().clone()
    }

    /// 测试注入：让 bind_session_persona_uid 返回错误（验证降级不阻塞发送）。
    pub fn set_bind_fails(&self, fail: bool) {
        self.fail_bind.store(fail, Ordering::Relaxed);
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
        self.add_closed_session_at(session_id, 2000);
    }

    /// 预填充一个已关闭 session（ended_at 可控，供"取最近已关闭会话"类测试）。
    pub fn add_closed_session_at(&self, session_id: Uuid, ended_at: i64) {
        self.sessions.lock().unwrap().insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1000,
                ended_at: Some(ended_at),
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

    /// 预填充 persona（v1.4 M5：桥接白名单/名称解析测试）。
    pub fn add_persona(&self, persona: Persona) {
        self.personas
            .lock()
            .unwrap()
            .insert(persona.uid.clone(), persona);
    }

    /// 为指定会话添加一条 utt 话语块（追加到该会话列表尾部，id 自动分配）。
    pub fn add_utt_block(&self, mut block: UttBlock) {
        let mut map = self.utt_blocks.lock().unwrap();
        let list = map.entry(block.session_id).or_default();
        block.id = list.len() as i64 + 1;
        list.push(block);
    }

    /// 返回该 persona 的全部反馈日志（供弱反馈测试断言）。
    pub fn feedback_logs_for(&self, persona_uid: &str) -> Vec<ramaria_core::behavior::FeedbackLog> {
        self.feedback_logs
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.persona_uid == persona_uid)
            .cloned()
            .collect()
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
        self.utt_blocks.lock().unwrap().remove(&session_id);
        Ok(())
    }

    async fn bind_session_persona_uid(
        &self,
        session_id: Uuid,
        persona_uid: &str,
    ) -> RamariaResult<()> {
        if self.fail_bind.load(Ordering::Relaxed) {
            return Err(RamariaError::unsupported(
                "测试注入：bind_session_persona_uid 强制失败",
            ));
        }
        if let Some(session) = self.sessions.lock().unwrap().get_mut(&session_id) {
            session.persona_uid = Some(persona_uid.to_string());
        }
        Ok(())
    }

    // -- Utt Blocks（v1.4 M5：桥接测试支持） --

    async fn insert_utt_block(&self, block: &UttBlock) -> RamariaResult<i64> {
        let mut map = self.utt_blocks.lock().unwrap();
        let list = map.entry(block.session_id).or_default();
        let id = list.len() as i64 + 1;
        let mut b = block.clone();
        b.id = id;
        list.push(b);
        Ok(id)
    }

    async fn list_utt_blocks_by_persona(&self, persona_uid: &str) -> RamariaResult<Vec<UttBlock>> {
        Ok(self
            .utt_blocks
            .lock()
            .unwrap()
            .values()
            .flatten()
            .filter(|b| b.persona_uid == persona_uid)
            .cloned()
            .collect())
    }

    async fn get_latest_utt_block_by_session(
        &self,
        session_id: Uuid,
    ) -> RamariaResult<Option<UttBlock>> {
        Ok(self
            .utt_blocks
            .lock()
            .unwrap()
            .get(&session_id)
            .and_then(|list| list.last())
            .cloned())
    }

    async fn delete_utt_blocks_by_session(&self, session_id: Uuid) -> RamariaResult<usize> {
        Ok(self
            .utt_blocks
            .lock()
            .unwrap()
            .remove(&session_id)
            .map(|l| l.len())
            .unwrap_or(0))
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

    async fn list_unabsorbed_l1_unbound(&self) -> RamariaResult<Vec<MemoryL1>> {
        // 无主 L1（persona_uid IS NULL）：测试中用空字符串键（""）预填充
        Ok(self
            .l1_by_persona
            .lock()
            .unwrap()
            .get("")
            .cloned()
            .unwrap_or_default())
    }

    async fn assign_l1_persona_uid(
        &self,
        l1_ids: &[Uuid],
        persona_uid: &str,
    ) -> RamariaResult<usize> {
        // 模拟真实归属语义（与 SQL 实现一致）：
        // 把无主（"" 键）L1 按 id 移动到目标 persona 键，并回填 persona_uid；
        // 仅移动仍为 NULL 且未吸收的记录——已归属/已吸收的不动（幂等）。
        let mut map = self.l1_by_persona.lock().unwrap();
        let unbound = map.get("").cloned().unwrap_or_default();
        let (kept, to_move): (Vec<MemoryL1>, Vec<MemoryL1>) = unbound
            .into_iter()
            .partition(|l| !l1_ids.contains(&l.id) || l.persona_uid.is_some() || l.absorbed);
        map.insert("".to_string(), kept);

        let target = map.entry(persona_uid.to_string()).or_default();
        let mut assigned = 0usize;
        for mut l in to_move {
            l.persona_uid = Some(persona_uid.to_string());
            target.push(l);
            assigned += 1;
        }
        Ok(assigned)
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

    async fn touch_l1(&self, l1_ids: &[Uuid], _now_ms: i64) -> RamariaResult<()> {
        // 记录调用供 touch 接线测试断言（不真正修改内存数据）
        *self.touch_l1_ids.lock().unwrap() = l1_ids.to_vec();
        Ok(())
    }

    async fn create_persona(&self, p: &Persona) -> RamariaResult<i64> {
        self.personas
            .lock()
            .unwrap()
            .insert(p.uid.clone(), p.clone());
        Ok(1)
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

    async fn save_example(&self, e: &PersonaExample) -> RamariaResult<i64> {
        let mut id_guard = self
            .next_example_id
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let id = *id_guard;
        *id_guard += 1;
        let mut ex = e.clone();
        ex.id = id;
        self.examples
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ex);
        Ok(id)
    }

    async fn list_selected_examples(&self, uid: &str) -> RamariaResult<Vec<PersonaExample>> {
        Ok(self
            .examples
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|e| e.persona_uid == uid && e.selected)
            .cloned()
            .collect())
    }

    async fn list_all_examples(&self, uid: &str) -> RamariaResult<Vec<PersonaExample>> {
        Ok(self
            .examples
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|e| e.persona_uid == uid)
            .cloned()
            .collect())
    }

    async fn find_example_by_pair(
        &self,
        uid: &str,
        partner: &str,
        reply: &str,
    ) -> RamariaResult<Option<PersonaExample>> {
        Ok(self
            .examples
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|e| e.persona_uid == uid && e.partner == partner && e.reply == reply)
            .cloned())
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
        Ok(vec![])
    }

    async fn find_refs_by_doc(
        &self,
        _doc_type: &str,
        _doc_id: &str,
    ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>> {
        Ok(vec![])
    }

    // -- Feedback Log（v1.7 H2：S2/S3 弱反馈落库） --
    async fn save_feedback_log(&self, log: &FeedbackLog) -> RamariaResult<i64> {
        let mut logs = self.feedback_logs.lock().unwrap();
        let id = logs.len() as i64 + 1;
        let mut l = log.clone();
        l.id = id;
        logs.push(l);
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
// MockLlm — 可配置的 LLM Provider
// =========================================================

/// 可配置 Mock LLM Provider。
///
/// 设计:
/// - `config` 字段决定 provider 类型（LM Studio / DeepSeek / OpenAI）
/// - 用于 Stage 2 隐私检查测试：线上 provider 触发隐私确认
/// - `chat_reply` 字段控制非流式 `chat()` 的返回文本：默认 "mock reply"
///   （既有测试依赖该值），结构化输出测试可用 [`Self::with_reply`] 注入
///   L1 JSON 等可解析文本
pub struct MockLlm {
    config: BackendConfig,
    chat_reply: String,
}

impl MockLlm {
    /// 创建 LM Studio 本地 provider 的 Mock。
    pub fn local() -> Self {
        Self::with_reply("mock reply")
    }

    /// 创建 DeepSeek 线上 provider 的 Mock。
    pub fn online_deepseek() -> Self {
        Self {
            config: BackendConfig::deepseek_default(),
            chat_reply: "mock reply".to_string(),
        }
    }

    /// 创建指定非流式回复的 Mock（用于注入结构化输出，如 L1 JSON）。
    ///
    /// 说明:
    /// - 仅影响 `chat()`（summarizer/事件提取等使用非流式调用）；
    /// - `chat_stream()` 保持固定 "mock" 文本（既有流式断言依赖）。
    pub fn with_reply(reply: &str) -> Self {
        Self {
            config: BackendConfig::lm_studio_default(),
            chat_reply: reply.to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for MockLlm {
    async fn chat(&self, _req: &ChatRequest) -> RamariaResult<String> {
        Ok(self.chat_reply.clone())
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
    let retriever = Arc::new(RwLock::new(Retriever::new()));
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
