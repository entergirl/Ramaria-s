//! crates/ramaria-service/tests/suites/support/mock_backend/mod.rs - Mock StorageBackend + Mock LlmProvider
//!
//! 设计特点:
//! - `MockStorage`: 内存 HashMap 实现的 StorageBackend，用于服务层集成测试
//! - `MockLlm` / `MockFailingLlm`: 返回预设回复或固定错误的 LlmProvider，支持流式与非流式
//! - `MockEmbedding`: 确定性占位嵌入，不加载真实模型
//! - 所有 mock 都是 Send + Sync，可直接用于 Arc<dyn Trait>
//! - 支持测试场景：空存储、已有会话/消息、LLM 正常/错误回复
//!
//! 安全约束:
//! - 不使用真实 API key 或网络请求
//! - 不触碰文件系统

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use ramaria_core::behavior::{BehaviorRule, FeedbackLog};
use ramaria_core::types::{
    BackendConfig, ClusterSnapshot, EventSource, MemoryEvent, MemoryL1, Message, Persona,
    PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent, Session, TraitEvidence,
    UttBlock,
};
use uuid::Uuid;

// =========================================================
// MockStorage
// =========================================================

/// 后台任务表（background_jobs 表）内存存储：id → (job_type, payload, status)。
type JobStore = Mutex<HashMap<i64, (String, Option<String>, String)>>;

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
    l1_by_persona: Mutex<HashMap<String, Vec<MemoryL1>>>,
    personas: Mutex<HashMap<String, Persona>>,
    persona_seq: Mutex<i64>,
    privacy_consents: Mutex<Vec<PrivacyConsent>>,
    backend_config: Mutex<Option<BackendConfig>>,
    index_version: Mutex<i32>,
    examples: Mutex<HashMap<String, Vec<PersonaExample>>>,
    // 人格推断相关存储
    traits: Mutex<HashMap<i64, PersonalityTrait>>,
    traits_by_persona: Mutex<HashMap<String, Vec<i64>>>,
    trait_seq: AtomicI64,
    evidence: Mutex<HashMap<i64, Vec<TraitEvidence>>>,
    evidence_seq: AtomicI64,
    cluster_snapshots: Mutex<Vec<ClusterSnapshot>>,
    snapshot_seq: AtomicI64,
    /// utt 话语块（按 session_id 索引，供桥接与封存链路测试）
    utt_blocks: Mutex<HashMap<Uuid, Vec<UttBlock>>>,
    /// 事件
    events: Mutex<HashMap<i64, MemoryEvent>>,
    events_by_persona: Mutex<HashMap<String, Vec<i64>>>,
    event_seq: AtomicI64,
    /// 行为规则
    behavior_rules: Mutex<HashMap<i64, BehaviorRule>>,
    rules_by_persona: Mutex<HashMap<String, Vec<i64>>>,
    rule_seq: AtomicI64,
    /// 反馈日志
    feedback_logs: Mutex<Vec<FeedbackLog>>,
    feedback_seq: AtomicI64,
    /// 人物事实
    facts: Mutex<HashMap<i64, PersonaFact>>,
    facts_by_persona: Mutex<HashMap<String, Vec<i64>>>,
    fact_seq: AtomicI64,
    /// 事件 → 来源 L1 映射（event_sources）
    event_sources: Mutex<Vec<EventSource>>,
    event_source_seq: AtomicI64,
    /// 后台任务（background_jobs 表）。
    jobs: JobStore,
    job_seq: AtomicI64,
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
            l1_by_persona: Mutex::new(HashMap::new()),
            personas: Mutex::new(HashMap::new()),
            persona_seq: Mutex::new(0),
            privacy_consents: Mutex::new(Vec::new()),
            backend_config: Mutex::new(None),
            index_version: Mutex::new(0),
            examples: Mutex::new(HashMap::new()),
            traits: Mutex::new(HashMap::new()),
            traits_by_persona: Mutex::new(HashMap::new()),
            trait_seq: AtomicI64::new(1),
            evidence: Mutex::new(HashMap::new()),
            evidence_seq: AtomicI64::new(1),
            cluster_snapshots: Mutex::new(Vec::new()),
            snapshot_seq: AtomicI64::new(1),
            utt_blocks: Mutex::new(HashMap::new()),
            events: Mutex::new(HashMap::new()),
            events_by_persona: Mutex::new(HashMap::new()),
            event_seq: AtomicI64::new(1),
            behavior_rules: Mutex::new(HashMap::new()),
            rules_by_persona: Mutex::new(HashMap::new()),
            rule_seq: AtomicI64::new(1),
            feedback_logs: Mutex::new(Vec::new()),
            feedback_seq: AtomicI64::new(1),
            facts: Mutex::new(HashMap::new()),
            facts_by_persona: Mutex::new(HashMap::new()),
            fact_seq: AtomicI64::new(1),
            event_sources: Mutex::new(Vec::new()),
            event_source_seq: AtomicI64::new(1),
            jobs: Mutex::new(HashMap::new()),
            job_seq: AtomicI64::new(1),
        }
    }

    /// 便捷方法：登记一个 pending 后台任务（模拟"封存时 L1 失败"留下的重试登记）。
    #[allow(dead_code)]
    pub fn add_pending_job(&self, job_type: &str, payload: Option<&str>) -> i64 {
        let id = self.job_seq.fetch_add(1, Ordering::Relaxed);
        self.jobs.lock().unwrap().insert(
            id,
            (
                job_type.to_string(),
                payload.map(|p| p.to_string()),
                "pending".to_string(),
            ),
        );
        id
    }

    /// 便捷方法：读取某任务状态（None = 任务不存在）。
    #[allow(dead_code)]
    pub fn job_status(&self, job_id: i64) -> Option<String> {
        self.jobs
            .lock()
            .unwrap()
            .get(&job_id)
            .map(|(_, _, status)| status.clone())
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
                persona_uid: None,
                ..Session::default()
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

    /// 便捷方法：添加活跃 session。
    #[allow(dead_code)]
    pub fn add_active_session(&self, session_id: Uuid) {
        self.sessions.lock().unwrap().insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1000,
                ended_at: None,
                persona_uid: None,
                ..Session::default()
            },
        );
    }

    /// 便捷方法：添加已关闭会话（ended_at 固定为 2000，供桥接等场景）。
    #[allow(dead_code)]
    pub fn add_closed_session(&self, session_id: Uuid) {
        self.sessions.lock().unwrap().insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1000,
                ended_at: Some(2000),
                persona_uid: None,
                ..Session::default()
            },
        );
    }

    /// 便捷方法：添加 persona（供白名单/桥接/推断测试）。
    #[allow(dead_code)]
    pub fn add_persona(&self, persona: Persona) {
        self.personas
            .lock()
            .unwrap()
            .insert(persona.uid.clone(), persona);
    }

    /// 便捷方法：注入一条 Few-shot 示例（selected 可控）。
    ///
    /// 配置传播测试使用——`examples.enabled=false`
    /// 回退静态 selected 查询，`enabled=true` 时从候选池评分轮换。
    #[allow(dead_code)]
    pub fn add_example(&self, persona_uid: &str, example: PersonaExample) {
        self.examples
            .lock()
            .unwrap()
            .entry(persona_uid.to_string())
            .or_default()
            .push(example);
    }

    /// 便捷方法：为指定会话添加一条 utt 话语块（追加到该会话块列表尾部）。
    ///
    /// 说明:
    /// - id 自动分配（当前块数 + 1），模拟存储层自增。
    /// - 覆盖 `get_latest_utt_block_by_session`（取列表尾部）。
    #[allow(dead_code)]
    pub fn add_utt_block(&self, mut block: UttBlock) {
        let mut map = self.utt_blocks.lock().unwrap();
        let list = map.entry(block.session_id).or_default();
        block.id = list.len() as i64 + 1;
        list.push(block);
    }

    /// 便捷方法：添加消息。
    #[allow(dead_code)]
    pub fn add_messages(&self, session_id: Uuid, msgs: Vec<Message>) {
        self.messages.lock().unwrap().insert(session_id, msgs);
    }

    /// 便捷方法：按 persona 添加 L1 摘要。
    #[allow(dead_code)]
    pub fn add_l1_summaries(&self, persona_uid: &str, summaries: Vec<MemoryL1>) {
        self.l1_by_persona
            .lock()
            .unwrap()
            .insert(persona_uid.to_string(), summaries);
    }

    /// 便捷方法：注入一条 PersonaFact（自动分配 ID 并建立 persona 索引）。
    ///
    /// 用于知识层降级/隔离测试。状态默认为调用方给定，供 active/candidate/superseded 场景。
    #[allow(dead_code)]
    pub fn add_fact(&self, mut f: PersonaFact) -> i64 {
        let id = self.fact_seq.fetch_add(1, Ordering::SeqCst);
        f.id = id;
        let persona = f.persona_uid.clone();
        self.facts.lock().unwrap().insert(id, f);
        self.facts_by_persona
            .lock()
            .unwrap()
            .entry(persona)
            .or_default()
            .push(id);
        id
    }

    /// 便捷方法：添加 PersonalityTrait 记录（自动分配 ID 并建立 persona 索引）。
    #[allow(dead_code)]
    pub fn add_trait(&self, mut t: PersonalityTrait) -> i64 {
        let id = self.trait_seq.fetch_add(1, Ordering::SeqCst);
        t.id = id;
        let persona = t.persona_uid.clone();
        self.traits.lock().unwrap().insert(id, t);
        self.traits_by_persona
            .lock()
            .unwrap()
            .entry(persona)
            .or_default()
            .push(id);
        id
    }

    /// 便捷方法：添加 TraitEvidence 记录（自动分配 ID）。
    #[allow(dead_code)]
    pub fn add_evidence(&self, mut e: TraitEvidence) -> i64 {
        let id = self.evidence_seq.fetch_add(1, Ordering::SeqCst);
        e.id = id;
        self.evidence
            .lock()
            .unwrap()
            .entry(e.trait_id)
            .or_default()
            .push(e);
        id
    }

    /// 便捷方法：添加 ClusterSnapshot 记录（自动分配 ID）。
    #[allow(dead_code)]
    pub fn add_cluster_snapshot(&self, mut s: ClusterSnapshot) -> i64 {
        let id = self.snapshot_seq.fetch_add(1, Ordering::SeqCst);
        s.id = id;
        self.cluster_snapshots.lock().unwrap().push(s);
        id
    }

    /// 便捷方法：获取已存储的 trait 数量（用于测试断言）。
    #[allow(dead_code)]
    pub fn trait_count(&self) -> usize {
        self.traits.lock().unwrap().len()
    }

    /// 便捷方法：获取已存储的 evidence 数量（用于测试断言）。
    #[allow(dead_code)]
    pub fn evidence_count(&self) -> usize {
        self.evidence
            .lock()
            .unwrap()
            .values()
            .map(|v| v.len())
            .sum()
    }
}

mod embedding;
mod llm;
mod storage_crud;
mod storage_infra;

pub use embedding::MockEmbedding;
pub use llm::{MockFailingLlm, MockLlm};
