//! crates/ramaria-testkit/src/mock/storage/mod.rs - Ramaria 内存存储 mock 模块
//!
//! 设计特点:
//! - 单份实现：StoreCrud 与 StoreInfrastructure 的全部方法集中于此，按领域拆分 impl 块
//! - 失败注入：每个方法先过 guard，可指定方法持续失败或仅失败一次
//! - 未建模能力（表情资产）显式 panic 并带方法名，不静默返回默认值
//! - 语义对齐真实存储：会话关闭抢占、已关闭会话拒写、事实版本链、事件吸收、后台任务
//! - 全程内存 HashMap，无文件、无网络、无全局状态

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicUsize};

use ramaria_core::behavior::{BehaviorRule, FeedbackLog};
use ramaria_core::keyword::KeywordPoolRow;
use ramaria_core::types::{
    BackendConfig, ChannelBinding, ClusterSnapshot, EventRelation, EventSource, MemoryEvent,
    MemoryL1, MemoryVector, Message, MessageAttachment, Persona, PersonaEventAggregate,
    PersonaExample, PersonaFact, PersonaStyleStats, PersonalityTrait, PrivacyConsent, Session,
    SessionMember, StickerAsset, TraitEvidence, UttBlock,
};
use uuid::Uuid;

mod behavior;
mod event;
mod fact_trait;
mod helpers;
mod infra;
mod memory;
mod message;
mod persona;
mod session;
mod trait_impl;
mod utt;

/// 后台任务表内存存储：id 到 (job_type, payload, status)。
type JobStore = Mutex<HashMap<i64, (String, Option<String>, String)>>;

/// 内存存储 mock。
///
/// 职责:
/// - 替代真实 SQLite，承载服务层 / CLI / 记忆管线的全部存储交互；
/// - 数据存于 HashMap，测试间完全隔离；
/// - 提供便捷方法预填各类测试数据。
pub struct MockStorage {
    sessions: Mutex<HashMap<Uuid, Session>>,
    messages: Mutex<HashMap<Uuid, Vec<Message>>>,
    l1_list: Mutex<HashMap<Uuid, Vec<MemoryL1>>>,
    l1_by_persona: Mutex<HashMap<String, Vec<MemoryL1>>>,
    personas: Mutex<HashMap<String, Persona>>,
    persona_seq: Mutex<i64>,
    privacy_consents: Mutex<Vec<PrivacyConsent>>,
    backend_config: Mutex<Option<BackendConfig>>,
    settings: Mutex<HashMap<String, String>>,
    examples: Mutex<HashMap<String, Vec<PersonaExample>>>,
    traits: Mutex<HashMap<i64, PersonalityTrait>>,
    traits_by_persona: Mutex<HashMap<String, Vec<i64>>>,
    trait_seq: AtomicI64,
    evidence: Mutex<HashMap<i64, Vec<TraitEvidence>>>,
    evidence_seq: AtomicI64,
    cluster_snapshots: Mutex<Vec<ClusterSnapshot>>,
    snapshot_seq: AtomicI64,
    utt_blocks: Mutex<HashMap<Uuid, Vec<UttBlock>>>,
    events: Mutex<HashMap<i64, MemoryEvent>>,
    events_by_persona: Mutex<HashMap<String, Vec<i64>>>,
    event_seq: AtomicI64,
    behavior_rules: Mutex<HashMap<i64, BehaviorRule>>,
    rules_by_persona: Mutex<HashMap<String, Vec<i64>>>,
    rule_seq: AtomicI64,
    feedback_logs: Mutex<Vec<FeedbackLog>>,
    feedback_seq: AtomicI64,
    facts: Mutex<HashMap<i64, PersonaFact>>,
    facts_by_persona: Mutex<HashMap<String, Vec<i64>>>,
    fact_seq: AtomicI64,
    event_sources: Mutex<Vec<EventSource>>,
    event_source_seq: AtomicI64,
    jobs: JobStore,
    job_seq: AtomicI64,
    attachments: Mutex<HashMap<i64, MessageAttachment>>,
    attachment_seq: AtomicI64,
    keywords: Mutex<Vec<String>>,
    keyword_pool_rows: Mutex<Vec<KeywordPoolRow>>,
    pending_alias_writes: Mutex<Vec<(String, i64)>>,
    pool_reads: AtomicUsize,
    persona_event_aggregates: Mutex<Option<Vec<PersonaEventAggregate>>>,
    persona_event_aggregate_error: Mutex<Option<String>>,
    style_stats: Mutex<HashMap<String, PersonaStyleStats>>,
    session_members: Mutex<HashMap<(Uuid, String), SessionMember>>,
    channel_bindings: Mutex<HashMap<(String, String), ChannelBinding>>,
    binding_seq: AtomicI64,
    last_read_at: Mutex<HashMap<Uuid, i64>>,
    l2_fingerprints: Mutex<HashSet<(String, String)>>,
    bm25_version: AtomicI32,
    memory_vectors: Mutex<HashMap<(String, String), MemoryVector>>,
    stickers: Mutex<HashMap<i64, StickerAsset>>,
    sticker_seq: AtomicI64,
    event_relations: Mutex<Vec<EventRelation>>,
    relation_seq: AtomicI64,
    failures: Mutex<HashSet<String>>,
    fail_once: Mutex<HashSet<String>>,
    fail_all: AtomicBool,
    injection_count: AtomicUsize,
}

impl Default for MockStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl MockStorage {
    /// 创建空的内存存储。
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
            settings: Mutex::new(HashMap::new()),
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
            attachments: Mutex::new(HashMap::new()),
            attachment_seq: AtomicI64::new(1),
            keywords: Mutex::new(Vec::new()),
            keyword_pool_rows: Mutex::new(Vec::new()),
            pending_alias_writes: Mutex::new(Vec::new()),
            pool_reads: AtomicUsize::new(0),
            persona_event_aggregates: Mutex::new(None),
            persona_event_aggregate_error: Mutex::new(None),
            style_stats: Mutex::new(HashMap::new()),
            session_members: Mutex::new(HashMap::new()),
            channel_bindings: Mutex::new(HashMap::new()),
            binding_seq: AtomicI64::new(1),
            last_read_at: Mutex::new(HashMap::new()),
            l2_fingerprints: Mutex::new(HashSet::new()),
            bm25_version: AtomicI32::new(1),
            memory_vectors: Mutex::new(HashMap::new()),
            stickers: Mutex::new(HashMap::new()),
            sticker_seq: AtomicI64::new(1),
            event_relations: Mutex::new(Vec::new()),
            relation_seq: AtomicI64::new(1),
            failures: Mutex::new(HashSet::new()),
            fail_once: Mutex::new(HashSet::new()),
            fail_all: AtomicBool::new(false),
            injection_count: AtomicUsize::new(0),
        }
    }
}
