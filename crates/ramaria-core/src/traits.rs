//! rust/crates/ramaria-core/src/traits.rs - Ramaria 核心能力抽象模块
//!
//! 设计特点:
//! - 定义三类核心边界: LLM Provider、Embedding Provider、Storage Backend
//! - 上层 crate 依赖 trait，不依赖具体数据库、模型服务或向量实现
//! - 支持流式 LLM 响应，统一 request_id、delta、done 和 metadata 语义
//! - 支持 embedding 模型下载、进度查询、可用性校验和批量向量化
//! - Storage trait 暴露业务级 CRUD + 基础设施 CRUD，不泄露 sqlx 连接池或具体表结构

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use uuid::Uuid;

use crate::error::RamariaResult;
use crate::types::{
    BackendConfig, ClusterSnapshot, EventRelation, EventSource, MemoryEvent, MemoryL1, Message,
    MessageRole, ModelCapability, Persona, PersonaExample, PersonaFact, PersonalityTrait,
    PrivacyConsent, ProfileField, Session, TraitEvidence, TraitStatus,
};

// =========================================================
// LLM Provider 抽象层
// =========================================================

/// 流式响应的单个增量片段。
///
/// 格式:
/// - `content`: 本次增量文本。
/// - `done`: 是否为当前 assistant 消息的最后一个片段。
/// - `metadata`: provider 返回的附加信息，例如 finish_reason。
///
/// 用途:
/// - Tauri Event 和 CLI 流式输出共用此结构。
/// - app 层通过 request_id 将多个 `StreamDelta` 串联为一次请求。
#[derive(Debug, Clone)]
pub struct StreamDelta {
    /// 增量文本内容
    pub content: String,
    /// 是否为此条消息的最后一个片段
    pub done: bool,
    /// 附加元数据（如 finish_reason）
    pub metadata: Option<String>,
}

/// LLM 请求参数。
///
/// 职责:
/// - 汇总一次聊天请求所需的 system prompt、记忆上下文、历史消息和当前输入。
/// - 将生成参数和 request_id 一并传入 provider，便于日志追踪和流式事件关联。
///
/// 字段约定:
/// - `system_prompt`: 人格、时间、系统规则等稳定提示。
/// - `memory_context`: L1/L2/L3 检索结果格式化文本，可为空。
/// - `history`: 当前会话历史消息，不包含本次用户输入。
/// - `user_message`: 本次用户输入。
/// - `request_id`: 当前请求唯一标识。
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// 系统提示（角色 identity、时间上下文等）
    pub system_prompt: String,
    /// 注入的记忆上下文（L1/L2/L3 格式化文本）
    pub memory_context: Option<String>,
    /// 对话历史消息
    pub history: Vec<ChatMessage>,
    /// 用户当前输入
    pub user_message: String,
    /// 生成温度 0.0..2.0
    pub temperature: f64,
    /// 最大输出 tokens
    pub max_tokens: u32,
    /// 请求标识，用于流式事件串联
    pub request_id: Uuid,
}

/// 对话消息（简化为 trait 所需格式）。
///
/// 用途:
/// - 表示发送给 provider 的历史消息。
/// - 与 OpenAI-compatible role 语义保持一致。
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
}

/// LLM Provider 抽象 trait。
///
/// 职责:
/// - 抽象 LM Studio、DeepSeek、OpenAI 等 provider 的聊天能力。
/// - 为 app 层提供统一的非流式和流式调用入口。
/// - 为 memory 层提供摘要、合并、画像提炼所需的 LLM 能力。
///
/// 实现要求:
/// - 不记录 API key、完整 prompt 或完整用户消息。
/// - `validate` 应检查连接、模型和流式能力是否满足当前配置。
/// - provider 内部错误应转换为 `RamariaError::Llm` 或更精确分类。
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 执行非流式聊天完成请求。
    ///
    /// 参数:
    /// - `request`: 完整聊天请求。
    ///
    /// 返回:
    /// - 成功时返回完整 assistant 文本。
    /// - 失败时返回统一错误类型。
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String>;

    /// 执行流式聊天完成请求。
    ///
    /// 参数:
    /// - `request`: 完整聊天请求。
    ///
    /// 返回:
    /// - 成功时返回异步流，每个元素是一段增量文本。
    /// - 流中每个错误都应保留 provider 上下文。
    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>>;

    /// 获取此 provider 的模型能力描述。
    ///
    /// 返回:
    /// - 当前 provider/model 的流式、JSON、上下文长度等能力。
    fn capability(&self) -> &ModelCapability;

    /// 获取此 provider 的后端配置。
    ///
    /// 返回:
    /// - 非敏感后端配置，不包含 API key。
    fn config(&self) -> &BackendConfig;

    /// 验证 provider 可用性。
    ///
    /// 检查内容:
    /// - base_url 是否可连接。
    /// - 模型是否可用。
    /// - 必需的 streaming 能力是否可用。
    async fn validate(&self) -> RamariaResult<()>;

    /// 快速健康检查（轻量级探测，用于启动时判断后端是否可达）。
    ///
    /// 与 `validate` 的区别:
    /// - `health_check`: 仅检查 base_url 可达，不检查模型能力或 API key 有效性。
    /// - `validate`: 完整检查（模型、流式能力、关键配置）。
    ///
    /// 默认实现: 直接返回 Ok(()), 适用于无需网络探测的场景。
    /// 线上 provider 应覆写为真正的 HTTP 探测。
    ///
    /// 说明:
    /// - 用于 `run_setup` 末尾的启动探测，不可用时置为 Degraded 状态。
    /// - 超时 5 秒，避免启动阻塞过久。
    async fn health_check(&self) -> RamariaResult<()> {
        // 默认实现：不阻塞，适用于本地 provider 或无需网络探测的场景
        tracing::debug!("health_check: 默认实现（无网络探测）");
        Ok(())
    }

    /// 返回 provider 名称。
    ///
    /// 返回:
    /// - 静态名称，用于日志、诊断和 UI 展示。
    fn name(&self) -> &'static str;
}

// =========================================================
// Embedding Provider 抽象层
// =========================================================

/// Embedding 模型信息。
///
/// 职责:
/// - 描述当前 embedding 模型的稳定标识和向量维度。
/// - 供配置向导、索引初始化和一致性检查使用。
#[derive(Debug, Clone)]
pub struct EmbeddingModelInfo {
    /// 模型标识
    pub model_id: String,
    /// 向量维度
    pub dimension: usize,
}

/// 单条嵌入结果。
///
/// 职责:
/// - 将业务对象 ID 与向量数据绑定。
/// - 供向量索引写入时使用。
#[derive(Debug, Clone)]
pub struct Embedding {
    /// 向量 ID
    pub id: Uuid,
    /// 向量数据
    pub vector: Vec<f32>,
}

/// Embedding Provider 抽象 trait。
///
/// 职责:
/// - 下载和校验 embedding 模型。
/// - 将文本转换为向量，供混合 RAG 的向量通道使用。
/// - 暴露下载进度和可用性，供首次配置向导展示。
///
/// 实现要求:
/// - 未完成下载或校验失败时，`is_available` 必须返回 false。
/// - `validate` 至少应执行一次测试向量生成。
/// - 不应在核心层直接依赖具体模型库。
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// 为单条文本生成嵌入向量。
    ///
    /// 参数:
    /// - `text`: 待向量化文本。
    ///
    /// 返回:
    /// - 成功时返回向量。
    /// - 模型不可用或生成失败时返回错误。
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>>;

    /// 为多条文本批量生成嵌入向量。
    ///
    /// 参数:
    /// - `texts`: 待向量化文本列表。
    ///
    /// 返回:
    /// - 与输入顺序一致的向量列表。
    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>>;

    /// 获取模型信息。
    fn model_info(&self) -> &EmbeddingModelInfo;

    /// 验证模型可用。
    ///
    /// 检查内容:
    /// - 模型文件是否存在。
    /// - 单条测试文本是否能成功生成向量。
    async fn validate(&self) -> RamariaResult<()>;

    /// 下载模型。
    ///
    /// 说明:
    /// - 若模型已存在，实现可以直接返回成功。
    /// - 下载进度通过 `download_progress` 暴露。
    async fn download_model(&self) -> RamariaResult<()>;

    /// 返回下载进度 0.0..1.0。
    fn download_progress(&self) -> f64;

    /// 模型是否已下载且可用。
    fn is_available(&self) -> bool;
}

// =========================================================
// 存储后端抽象层
// =========================================================

/// 存储后端抽象 trait。
///
/// 职责:
/// - 定义 app 和 memory 层需要的业务级 CRUD + 基础设施 CRUD，覆盖全部 23 张表。
/// - 隔离 SQLite/sqlx 细节，避免上层持有连接池或拼接 SQL。
///
/// 实现要求:
/// - 具体实现位于 `ramaria-storage`。
/// - 所有可恢复错误应转换为 `RamariaError::Storage` 或更精确分类。
///
/// ID 类型约定:
/// - TEXT 主键表（sessions/messages/memory_l1）使用 Uuid
/// - INTEGER AUTOINCREMENT 表使用 i64
/// - FK 列类型与目标表 PK 类型一致
///
/// 破坏性变更（vs 旧 StorageBackend）:
/// - 删除: save_memory_l2, save_l2_sources, get_l2_sources, save_user_profile,
/// get_current_profile, mark_profile_historical
/// - 新增: personas, memory_events, event_relations, event_sources, persona_facts,
/// personality_traits, trait_evidence, persona_examples, persona_cluster_snapshots,
/// keyword_pool 十组方法
#[async_trait]
pub trait StorageBackend: Send + Sync {
    // -- Session --
    /// 创建新 session，可选的 persona_uid 用于 Session-Persona 绑定（v1.2）。
    ///
    /// 参数:
    /// - `persona_uid`: 对话人格标识（None 兼容存量调用）。
    async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session>;
    async fn close_session(&self, session_id: Uuid) -> RamariaResult<()>;
    async fn get_session(&self, session_id: Uuid) -> RamariaResult<Option<Session>>;
    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>>;
    async fn list_sessions(&self) -> RamariaResult<Vec<Session>>;
    async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()>;

    // -- Message (L0) --
    async fn save_message(&self, message: &Message) -> RamariaResult<()>;
    async fn list_messages(&self, session_id: Uuid) -> RamariaResult<Vec<Message>>;
    /// 按发言人查询消息（Persona-Aware RAG 的原话过滤）。
    async fn list_messages_by_persona(&self, persona_uid: &str) -> RamariaResult<Vec<Message>>;
    async fn find_message_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> RamariaResult<Option<Message>>;

    /// v1.3 (P-6): 按创建时间降序分页加载最近消息。
    ///
    /// 职责:
    /// - 替代 `list_messages` 全量加载，支持按 limit/offset 分页
    /// - 返回的消息按 `created_at DESC` 排序（最新在前），调用方按需反转
    ///
    /// 参数:
    /// - `session_id`: 会话 ID。
    /// - `limit`: 每页最大条数。
    /// - `offset`: 分页偏移量（第一页为 0）。
    ///
    /// 返回:
    /// - 按 `created_at DESC` 排序的消息列表。
    ///
    /// 默认实现:
    /// - 委托 `list_messages` 全量加载后手动排序截断（兼容存量实现）。
    /// - 子 crate（ramaria-storage）应覆写为高效 SQL（`ORDER BY created_at DESC LIMIT ? OFFSET ?`）。
    async fn list_messages_paginated(
        &self,
        session_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> RamariaResult<Vec<Message>> {
        let mut all = self.list_messages(session_id).await?;
        all.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        let start = offset as usize;
        let end = (offset + limit).min(all.len() as i64) as usize;
        Ok(all
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect())
    }
    /// 获取指定 session 最后一条消息的时间（Unix 毫秒）。
    ///
    /// 职责:
    /// - 供空闲检测线程判断 session 是否超过空闲阈值。
    /// - 默认实现返回 `Ok(None)`，子 crate 应覆写为高效 SQL（`SELECT MAX(created_at)`）。
    ///
    /// 返回:
    /// - `Ok(Some(ms))`: 最后消息时间戳。
    /// - `Ok(None)`: session 无消息或未实现。
    async fn get_last_message_time(&self, _session_id: Uuid) -> RamariaResult<Option<i64>> {
        Ok(None)
    }

    /// 统计指定 session 的消息数量。
    ///
    /// 职责:
    /// - 供前端 session 列表展示每条 session 的真实消息数。
    /// - 默认实现通过 `list_messages` 的 len 计算，子 crate 应覆写为 `SELECT COUNT(*)`。
    ///
    /// 返回:
    /// - 消息数量（无消息时为 0）。
    async fn count_messages(&self, session_id: Uuid) -> RamariaResult<u32> {
        Ok(self.list_messages(session_id).await?.len() as u32)
    }

    // -- Memory L1 --
    async fn save_memory_l1(&self, memory: &MemoryL1) -> RamariaResult<()>;
    async fn list_memory_l1(&self, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>>;
    async fn get_memory_l1(&self, id: Uuid) -> RamariaResult<Option<MemoryL1>>;
    async fn mark_l1_absorbed(&self, l1_ids: &[Uuid]) -> RamariaResult<()>;
    /// v1.2: 删除指定 session 中 persona_uid 为 NULL 的 L1 摘要（仅清理导入残留）
    async fn delete_memory_l1_by_session(&self, session_id: Uuid) -> RamariaResult<usize> {
        let _ = session_id;
        Ok(0) // 默认空实现：存量 mock 无需修改即可编译
    }
    async fn list_unabsorbed_l1(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryL1>>;

    /// 按创建时间降序获取指定 persona 的最近 N 条 L1 摘要。
    ///
    /// 职责:
    /// - 供跨 session 上下文注入：新 session 创建时自动加载最近对话摘要。
    /// - 不区分 absorbed 状态——即使已被 L2 吸收，近期摘要仍有叙事价值。
    ///
    /// 参数:
    /// - `persona_uid`: 人格标识。
    /// - `limit`: 最多返回条数（建议 3-5）。
    ///
    /// 返回:
    /// - 按 `created_at DESC` 排序的 MemoryL1 列表。
    ///
    /// 默认实现:
    /// - 返回空 Vec，子 crate 应覆写为高效 SQL（`ORDER BY created_at DESC LIMIT ?`）。
    async fn list_recent_l1_by_persona(
        &self,
        _persona_uid: &str,
        _limit: u32,
    ) -> RamariaResult<Vec<MemoryL1>> {
        Ok(Vec::new())
    }

    // -- Personas (id: i64) --
    async fn create_persona(&self, persona: &Persona) -> RamariaResult<i64>;
    async fn get_persona_by_uid(&self, uid: &str) -> RamariaResult<Option<Persona>>;
    async fn list_personas(&self) -> RamariaResult<Vec<Persona>>;
    /// 更新 persona 的可变字段（name/avatar/config/description）。
    /// uid 为业务标识，不可变更。
    /// 所有可选字段：`None` 表示保持旧值不变，不设为 NULL。
    async fn update_persona(
        &self,
        uid: &str,
        name: &str,
        avatar: Option<&str>,
        config: Option<&str>,
        description: Option<&str>,
    ) -> RamariaResult<()>;

    // -- Memory Events (L2 事件层, id: i64) --
    async fn save_event(&self, event: &MemoryEvent) -> RamariaResult<i64>;
    async fn list_events_by_persona(
        &self,
        persona_uid: &str,
        offset: i64,
        limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>>;
    async fn list_unabsorbed_events(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>>;

    /// 标记事件已被 L3 推断吸收。
    ///
    /// 参数:
    /// - `event_ids`: 要标记的事件 ID 列表。
    ///
    /// 说明:
    /// - 将 `memory_events.absorbed` 设为 1，使这些事件不再出现在 `list_unabsorbed_events` 中。
    /// - 幂等操作：已标记的事件重复调用无副作用。
    async fn mark_events_absorbed(&self, event_ids: &[i64]) -> RamariaResult<()>;

    // -- Event Relations (from_id/to_id: i64) --
    async fn save_event_relation(&self, rel: &EventRelation) -> RamariaResult<i64>;

    /// 按 persona_uid 查询该角色相关的所有事件关系。
    ///
    /// 通过 JOIN memory_events 过滤，返回 from 事件属于该 persona 的关系。
    /// 默认返回空列表——不会破坏已有 mock 实现。
    async fn list_event_relations_by_persona(
        &self,
        _persona_uid: &str,
    ) -> RamariaResult<Vec<EventRelation>> {
        Ok(Vec::new())
    }

    // -- Event Sources (event_id: i64, l1_id: Uuid) --
    async fn save_event_source(&self, event_id: i64, l1_id: Uuid, weight: f64)
    -> RamariaResult<()>;

    /// 查询指定事件的所有溯源 L1 记录。
    ///
    /// 职责:
    /// - 用于前端性格画像证据链展开：事件 → L1 摘要 → evidence_notes。
    /// - 返回该事件关联的全部 L1 source 记录（含 weight）。
    ///
    /// 默认实现返回空列表，子 crate 应覆写为 SQL 查询。
    async fn list_event_sources_by_event(&self, _event_id: i64) -> RamariaResult<Vec<EventSource>> {
        Ok(Vec::new())
    }

    // -- Persona Facts (id: i64) --
    async fn save_fact(&self, fact: &PersonaFact) -> RamariaResult<i64>;
    /// 按 persona_uid 和字段分类查询事实。
    /// `field` 使用 `ProfileField` 枚举以确保类型安全，避免传入非法字段名。
    async fn list_facts_by_persona(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>>;
    /// 按 persona_uid 一次性统计所有字段的 fact 数量（GROUP BY）。
    ///
    /// 返回:
    /// - `Vec<(ProfileField, usize)>`：每个字段及其对应的 fact 数量。
    /// - 某字段无记录时结果为 0。
    ///
    /// 性能:
    /// - 单次 SQL GROUP BY 查询，替代 v1.2 的 N+1 循环查询。
    /// - 用于冷启动已有画像的 fact 计数。
    async fn count_all_facts_for_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<(ProfileField, usize)>> {
        // 默认实现：委托 list_facts_by_persona 逐字段查询（兼容非 SQL 后端）
        let fields = [
            ProfileField::BasicInfo,
            ProfileField::PersonalStatus,
            ProfileField::Interests,
            ProfileField::Social,
            ProfileField::History,
            ProfileField::RecentContext,
            ProfileField::SpeakingStyle,
        ];
        let mut result = Vec::with_capacity(fields.len());
        for &field in &fields {
            let count = self.list_facts_by_persona(persona_uid, field).await?.len();
            result.push((field, count));
        }
        Ok(result)
    }

    // -- Personality Traits (L3 性格层, id: i64) --
    async fn save_trait(&self, t: &PersonalityTrait) -> RamariaResult<i64>;
    async fn list_traits_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonalityTrait>>;
    /// `id` 为 personality_traits 表的 INTEGER 主键。
    async fn update_trait_confidence(
        &self,
        id: i64,
        confidence: f64,
        evidence: f64,
        consistency: f64,
    ) -> RamariaResult<()>;
    /// `id` 为 personality_traits 表的 INTEGER 主键。
    async fn update_trait_status(&self, id: i64, status: TraitStatus) -> RamariaResult<()>;

    // -- Trait Evidence (trait_id/event_id: i64) --
    async fn save_evidence(&self, e: &TraitEvidence) -> RamariaResult<i64>;
    /// `trait_id` 为 personality_traits 表的 INTEGER 主键。
    async fn list_evidence_by_trait(&self, trait_id: i64) -> RamariaResult<Vec<TraitEvidence>>;

    // -- Persona Examples (id: i64) --
    async fn save_example(&self, e: &PersonaExample) -> RamariaResult<i64>;
    async fn list_selected_examples(&self, persona_uid: &str)
    -> RamariaResult<Vec<PersonaExample>>;

    // -- Persona Cluster Snapshots (id: i64) --
    async fn save_cluster_snapshot(&self, s: &ClusterSnapshot) -> RamariaResult<i64>;
    async fn get_current_snapshots(
        &self,
        persona_uid: &str,
        category: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>>;
    /// v1.3: 查询该 persona 的所有历史快照（含非 current），仅返回有 semantic_label_embedding 的条目。
    /// 用于跨版本簇匹配。
    async fn get_all_snapshots_with_embeddings(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        let _ = persona_uid;
        Ok(Vec::new()) // 默认空实现，保持向后兼容
    }

    // -- Keyword Pool --
    async fn upsert_keyword(&self, keyword: &str) -> RamariaResult<()>;
    async fn list_keywords(&self) -> RamariaResult<Vec<String>>;
    /// 按 use_count DESC 返回所有关键词及其使用量。（v1.3 新增）
    async fn list_keyword_counts(&self) -> RamariaResult<Vec<(String, u32)>> {
        let _ = self;
        Ok(Vec::new()) // 默认空实现，保持向后兼容
    }

    // -- Keyword Refs (v1.3 新增) --
    /// 插入一条关键词引用记录。
    async fn insert_keyword_ref(
        &self,
        keyword_id: &str,
        doc_type: &str,
        doc_id: &str,
        persona_uid: &str,
        weight: f64,
    ) -> RamariaResult<()>;
    /// 根据关键词文本查询所有引用（倒排查）。
    async fn find_refs_by_keyword(
        &self,
        keyword_id: &str,
    ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>>;
    /// 根据文档查询所有引用（正排查）。
    async fn find_refs_by_doc(
        &self,
        doc_type: &str,
        doc_id: &str,
    ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>>;
    /// 删除指定文档的所有关键词引用。
    async fn delete_refs_by_doc(&self, _doc_type: &str, _doc_id: &str) -> RamariaResult<u64> {
        let _ = self;
        Ok(0)
    }

    // -- Privacy Consent --
    async fn save_privacy_consent(&self, consent: &PrivacyConsent) -> RamariaResult<()>;
    async fn get_privacy_consent(
        &self,
        provider: &str,
        base_url: &str,
    ) -> RamariaResult<Option<PrivacyConsent>>;

    // -- Backend Config --
    async fn save_backend_config(&self, config: &BackendConfig) -> RamariaResult<()>;
    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>>;

    // -- 索引一致性 --
    async fn get_schema_version(&self) -> RamariaResult<i32>;
    async fn get_index_version(&self) -> RamariaResult<i32>;
    async fn set_index_version(&self, version: i32) -> RamariaResult<()>;

    // =========================================================
    // 基础设施方法 — 后台任务 / 冲突队列 / 推送 / 设置 / BM25 / 图谱
    // =========================================================

    // -- Background Jobs --
    async fn create_background_job(
        &self,
        job_type: &str,
        payload: Option<&str>,
    ) -> RamariaResult<i64>;
    async fn update_job_status(
        &self,
        id: i64,
        status: &str,
        error: Option<&str>,
    ) -> RamariaResult<()>;
    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>>;

    // -- Conflict Queue --
    async fn create_conflict(
        &self,
        field: &str,
        conflict_type: &str,
        old_content: Option<&str>,
        new_content: Option<&str>,
        desc: Option<&str>,
    ) -> RamariaResult<i64>;
    async fn list_pending_conflicts(&self) -> RamariaResult<Vec<(i64, String, String, String)>>;
    async fn resolve_conflict(&self, id: i64) -> RamariaResult<()>;

    // -- Pending Push --
    async fn create_push(&self, content: &str) -> RamariaResult<i64>;
    async fn list_pending_pushes(&self) -> RamariaResult<Vec<(i64, String)>>;
    async fn mark_push_sent(&self, id: i64) -> RamariaResult<()>;

    // -- Settings --
    async fn get_setting(&self, key: &str) -> RamariaResult<Option<String>>;
    async fn set_setting(&self, key: &str, value: &str) -> RamariaResult<()>;
    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>>;

    // -- BM25 Index --
    async fn save_bm25(&self, doc_id: i64, layer: &str, tokens_json: &str) -> RamariaResult<()>;
    async fn list_bm25_by_doc(&self, doc_id: i64) -> RamariaResult<Vec<(String, String)>>;
    async fn delete_bm25_by_doc(&self, doc_id: i64) -> RamariaResult<()>;

    // -- Graph --
    async fn insert_graph_node(
        &self,
        entity_name: &str,
        entity_type: &str,
        source_l1_id: Option<Uuid>,
    ) -> RamariaResult<i64>;
    async fn get_graph_node(
        &self,
        entity_name: &str,
    ) -> RamariaResult<Option<(i64, String, String)>>;
    async fn insert_graph_edge(
        &self,
        source_id: i64,
        target_id: i64,
        relation_type: &str,
        detail: Option<&str>,
        source_l1_id: Option<Uuid>,
    ) -> RamariaResult<i64>;
    async fn list_graph_edges(&self, source_id: i64)
    -> RamariaResult<Vec<(i64, i64, i64, String)>>;
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 trait 可通过 trait object 引用，便于后续 mock 或依赖注入。
    #[test]
    fn trait_definitions_exist() {
        // 编译期验证：trait 已定义且可被引用
        fn _check_llm(_p: &dyn LlmProvider) {}
        fn _check_embedding(_p: &dyn EmbeddingProvider) {}
        fn _check_storage(_p: &dyn StorageBackend) {}
    }

    #[test]
    fn chat_request_construction() {
        let req = ChatRequest {
            system_prompt: "你是一个助手".into(),
            memory_context: Some("用户偏好：喜欢猫".into()),
            history: vec![ChatMessage {
                role: MessageRole::User,
                content: "你好".into(),
            }],
            user_message: "今天天气如何？".into(),
            temperature: 0.3,
            max_tokens: 1024,
            request_id: Uuid::new_v4(),
        };
        assert_eq!(req.temperature, 0.3);
        assert!(req.memory_context.is_some());
        assert!(!req.history.is_empty());
    }

    #[test]
    fn stream_delta_construction() {
        let delta = StreamDelta {
            content: "今天".into(),
            done: false,
            metadata: None,
        };
        assert!(!delta.done);
        assert_eq!(delta.content, "今天");
    }

    #[test]
    fn embedding_model_info() {
        let info = EmbeddingModelInfo {
            model_id: "bge-small-zh".into(),
            dimension: 512,
        };
        assert_eq!(info.dimension, 512);
        assert_eq!(info.model_id, "bge-small-zh");
    }
}
