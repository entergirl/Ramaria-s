//! rust/crates/ramaria-app/src/pipeline.rs - Pipeline + Stage 核心基础设施
//!
//! 设计特点:
//! - PipelineStage trait 定义统一 Stage 接口，每个 Stage 独立可测试
//! - PipelineContext 全 Arc 引用，Stage 间零拷贝传递共享依赖
//! - PipelineData 贯穿整个管线，承载各阶段中间结果
//! - PipelineError 区分 Retryable（可重试）和 Fatal（不可恢复），编排器统一处理
//! - SendMessagePipeline 编排器按顺序执行 Stage 序列，任一失败即中止
//! - 向后兼容：App::send_message 对外接口不变，内部委托 SendMessagePipeline::execute

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{
    ChatMessage, ChatRequest, EmbeddingProvider, LlmProvider, StorageBackend, StreamDelta,
};
use ramaria_core::types::{AppState, BackendConfig, Session};
use ramaria_llm::keychain::Keychain;
use ramaria_memory::retriever::Retriever;
use uuid::Uuid;

use crate::app::SendMessageStream;
use crate::session_lifecycle::SessionLifecycle;

// =========================================================
// 类型别名
// =========================================================

/// LLM provider 返回的原始流类型。
///
/// 用途:
/// - Stage 9 (CallLlm) 产出此类型。
/// - Stage 10 (PersistMessage) 消费此类型，转换为 `SendMessageStream`。
pub type LlmRawStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = RamariaResult<StreamDelta>> + Send>>;

// =========================================================
// PipelineError
// =========================================================

/// 管线错误类型。
///
/// 职责:
/// - 区分可恢复（Retryable）和不可恢复（Fatal）错误
/// - 携带失败 Stage 名称，便于日志定位和 UI 诊断
/// - 包装底层 RamariaError，保留完整错误链
///
/// 分类语义:
/// - `Retryable`: 暂时性故障（LLM 超时、存储暂时不可用），上层可重试管线
/// - `Fatal`: 不可恢复错误（校验失败、FatalError 状态），重试无意义
#[derive(Debug)]
pub enum PipelineError {
    /// 可重试错误——暂时性故障，上层可选择重新执行管线。
    Retryable {
        /// 失败的 Stage 名称
        stage: &'static str,
        /// 底层错误
        source: RamariaError,
    },

    /// 不可恢复错误——管线中止，不可重试。
    Fatal {
        /// 失败的 Stage 名称
        stage: &'static str,
        /// 底层错误
        source: RamariaError,
    },
}

impl PipelineError {
    /// 创建可重试错误。
    ///
    /// 参数:
    /// - `stage`: 失败的 Stage 名称。
    /// - `source`: 底层错误。
    pub fn retryable(stage: &'static str, source: RamariaError) -> Self {
        Self::Retryable { stage, source }
    }

    /// 创建不可恢复错误。
    ///
    /// 参数:
    /// - `stage`: 失败的 Stage 名称。
    /// - `source`: 底层错误。
    pub fn fatal(stage: &'static str, source: RamariaError) -> Self {
        Self::Fatal { stage, source }
    }

    /// 判断错误是否可重试。
    ///
    /// 返回:
    /// - `true`: Retryable 变体，上层可重新执行管线。
    /// - `false`: Fatal 变体，不可重试。
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable { .. })
    }

    /// 返回失败的 Stage 名称。
    pub fn stage(&self) -> &'static str {
        match self {
            Self::Retryable { stage, .. } | Self::Fatal { stage, .. } => stage,
        }
    }

    /// 返回底层错误引用。
    pub fn source_error(&self) -> &RamariaError {
        match self {
            Self::Retryable { source, .. } | Self::Fatal { source, .. } => source,
        }
    }
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable { stage, source } => {
                write!(f, "pipeline stage '{stage}' failed (retryable): {source}")
            }
            Self::Fatal { stage, source } => {
                write!(f, "pipeline stage '{stage}' failed (fatal): {source}")
            }
        }
    }
}

impl std::error::Error for PipelineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Retryable { source, .. } | Self::Fatal { source, .. } => Some(source),
        }
    }
}

impl From<PipelineError> for RamariaError {
    /// 将 PipelineError 转换为 RamariaError。
    ///
    /// 说明:
    /// - 保留底层 RamariaError，丢弃 Stage 名称（Stage 名称已由编排器记录到日志）。
    /// - 用于 `send_message` 等上层方法将管线错误统一为 `RamariaResult`。
    fn from(e: PipelineError) -> Self {
        match e {
            PipelineError::Retryable { source, .. } | PipelineError::Fatal { source, .. } => source,
        }
    }
}

// =========================================================
// PipelineStage trait
// =========================================================

/// 管线阶段统一接口。
///
/// 职责:
/// - 每个 Stage 接收共享上下文和上阶段输出，产生本阶段输出
/// - Stage 间通过 Input/Output 关联类型定义数据流
/// - 独立可测试：可注入 mock 依赖编写确定性单元测试
///
/// 实现要求:
/// - `execute` 为异步方法，内部不应持有 `std::sync::MutexGuard` 跨 `.await`
/// - 失败时返回 `PipelineError`，根据错误性质选择 Retryable 或 Fatal
/// - `name` 返回静态字符串，用于日志和错误诊断
///
/// 用法:
/// ```ignore
/// #[async_trait]
/// impl PipelineStage for MyStage {
///     type Input = PipelineData;
///     type Output = PipelineData;
///     fn name(&self) -> &'static str { "MyStage" }
///     async fn execute(&self, ctx: &PipelineContext, input: Self::Input)
///         -> Result<Self::Output, PipelineError> { /* ... */ }
/// }
/// ```
#[async_trait]
pub trait PipelineStage: Send + Sync {
    /// 输入类型
    type Input: Send;
    /// 输出类型
    type Output: Send;

    /// 返回 Stage 名称（用于日志和错误诊断）。
    fn name(&self) -> &'static str;

    /// 执行当前 Stage。
    ///
    /// 参数:
    /// - `ctx`: 共享管线上下文（只读资源引用）。
    /// - `input`: 上一阶段的输出数据。
    ///
    /// 返回:
    /// - `Ok(output)`: 本阶段执行成功，产出数据传递给下一阶段。
    /// - `Err(PipelineError)`: 本阶段失败，管线中止。
    async fn execute(
        &self,
        ctx: &PipelineContext,
        input: Self::Input,
    ) -> Result<Self::Output, PipelineError>;
}

// =========================================================
// PipelineContext
// =========================================================

/// 管线执行上下文——所有 Stage 共享的只读资源。
///
/// 设计原则:
/// - 全 Arc 引用，零拷贝传递
/// - 不可变——Stage 间不通过 Context 传递可变状态
/// - 可测试——可注入 mock 实现的 StorageBackend / LlmProvider
///
/// 字段约定:
/// - `storage`: 存储后端（23 张表 CRUD）
/// - `llm`: 当前 LLM provider（配置热更新时由 App 替换）
/// - `embedding`: 可选嵌入模型（None 表示未配置，进入 Degraded 状态）
/// - `config`: 应用配置（只读快照）
/// - `retriever`: 内存检索器（BM25 + 向量 + 图谱），需 Mutex 保护读写并发
/// - `keychain`: OS keychain（API key 存取）
/// - `lifecycle`: Session 生命周期编排器
pub struct PipelineContext {
    /// 存储后端
    pub storage: Arc<dyn StorageBackend>,
    /// LLM provider
    pub llm: Arc<dyn LlmProvider>,
    /// 嵌入模型 provider（None 表示未配置）
    pub embedding: Option<Arc<dyn EmbeddingProvider>>,
    /// 应用配置
    pub config: RamariaConfig,
    /// 内存检索器（Mutex 保护内部三通道索引的读写并发）
    pub retriever: Arc<Mutex<Retriever>>,
    /// OS keychain
    pub keychain: Arc<Keychain>,
    /// Session 生命周期编排器
    pub lifecycle: Arc<SessionLifecycle>,
}

impl PipelineContext {
    /// 创建管线上下文。
    ///
    /// 参数:
    /// - `storage`: 存储后端。
    /// - `llm`: LLM provider。
    /// - `embedding`: 可选嵌入模型（None 表示未配置）。
    /// - `config`: 应用配置。
    /// - `retriever`: 检索器（Arc<Mutex> 包裹，支持并发读写）。
    /// - `keychain`: OS keychain。
    /// - `lifecycle`: Session 生命周期编排器。
    ///
    /// 返回:
    /// - 可用于 `SendMessagePipeline::execute` 的共享上下文。
    pub fn new(
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
        config: RamariaConfig,
        retriever: Arc<Mutex<Retriever>>,
        keychain: Arc<Keychain>,
        lifecycle: Arc<SessionLifecycle>,
    ) -> Self {
        Self {
            storage,
            llm,
            embedding,
            config,
            retriever,
            keychain,
            lifecycle,
        }
    }
}

// =========================================================
// PipelineData
// =========================================================

/// 管线数据载体——贯穿整个 send_message 管线。
///
/// 职责:
/// - 承载用户输入参数（构造时设置）
/// - 承载各 Stage 产出的中间结果（Stage 执行时填充）
/// - 最终包含 LLM 流和输出流，供 send_message 返回
///
/// 字段分组:
/// - 输入参数: `user_input`, `persona_uid`, `session_id`, `request_id`
/// - Stage 1 (CheckState): `app_state`
/// - Stage 2 (CheckPrivacy): `backend_config`
/// - Stage 3 (ResolveSession): `session`
/// - Stage 4 (LoadHistory): `history_messages`, `recent_summaries`, `last_active_at`
/// - Stage 5 (RetrieveMemory): `memory_context`
/// - Stage 6 (BuildPrompt): `system_prompt`
/// - Stage 7 (TokenBudget): `budgeted_system_prompt`, `budgeted_memory_context`, `budgeted_history`, `estimated_tokens`
/// - Stage 8 (BuildRequest): `chat_request`
/// - Stage 9 (CallLlm): `llm_stream`
/// - Stage 10 (PersistMessage): `output_stream`
///
/// 使用约定:
/// - 每个 Stage 读取所需字段，写入本阶段产出字段
/// - 未执行的 Stage 对应字段保持初始值（None 或空集合）
/// - 编排器按顺序执行，前序 Stage 的产出可供后续 Stage 读取
pub struct PipelineData {
    // === 输入参数（构造时设置） ===
    /// 用户输入文本
    pub user_input: String,
    /// 当前对话人格 UID（None 表示 rama 自身）
    pub persona_uid: Option<String>,
    /// 前端传入的 session ID（None 表示创建新会话）
    pub session_id: Option<Uuid>,
    /// 本次请求唯一标识
    pub request_id: Uuid,

    // === Stage 1: CheckState ===
    /// 应用当前状态（Ready / Degraded / FatalError 等）
    pub app_state: Option<AppState>,

    // === Stage 2: CheckPrivacy ===
    /// LLM 后端配置（含 provider、base_url、temperature 等）
    pub backend_config: Option<BackendConfig>,

    // === Stage 3: ResolveSession ===
    /// 当前活跃会话
    pub session: Option<Session>,

    // === Stage 4: LoadHistory ===
    /// 当前 session 的历史消息（ChatMessage 格式）
    pub history_messages: Vec<ChatMessage>,
    /// 近期 L1 摘要列表（跨 session 上下文，预格式化文本）
    pub recent_summaries: Vec<String>,
    /// 最后活跃时间字符串（YYYY-MM-DD HH:MM 格式）
    pub last_active_at: Option<String>,

    // === Stage 5: RetrieveMemory ===
    /// RAG 检索结果格式化文本（None 表示无相关记忆）
    pub memory_context: Option<String>,

    // === Stage 6: BuildPrompt ===
    /// 5-Block System Prompt
    pub system_prompt: Option<String>,

    // === Stage 7: TokenBudget ===
    /// Token 预算管理后的 System Prompt
    pub budgeted_system_prompt: Option<String>,
    /// Token 预算管理后的记忆上下文
    pub budgeted_memory_context: Option<String>,
    /// Token 预算管理后的历史消息
    pub budgeted_history: Vec<ChatMessage>,
    /// 估算的 Token 总数
    pub estimated_tokens: usize,

    // === Stage 8: BuildRequest ===
    /// 构造完成的 ChatRequest
    pub chat_request: Option<ChatRequest>,

    // === Stage 9: CallLlm ===
    /// LLM provider 返回的原始流
    pub llm_stream: Option<LlmRawStream>,

    // === Stage 10: PersistMessage ===
    /// 最终输出流（含 StreamEvent，供 CLI/Desktop 消费）
    pub output_stream: Option<SendMessageStream>,
}

impl PipelineData {
    /// 创建管线数据载体。
    ///
    /// 参数:
    /// - `user_input`: 用户输入文本。
    /// - `persona_uid`: 人格 UID（None 表示 rama 自身）。
    /// - `session_id`: 前端传入的 session ID（None 表示创建新会话）。
    /// - `request_id`: 请求唯一标识。
    ///
    /// 返回:
    /// - 除输入参数外，所有 Stage 产出字段为初始值（None 或空集合）。
    pub fn new(
        user_input: String,
        persona_uid: Option<String>,
        session_id: Option<Uuid>,
        request_id: Uuid,
    ) -> Self {
        Self {
            user_input,
            persona_uid,
            session_id,
            request_id,
            app_state: None,
            backend_config: None,
            session: None,
            history_messages: Vec::new(),
            recent_summaries: Vec::new(),
            last_active_at: None,
            memory_context: None,
            system_prompt: None,
            budgeted_system_prompt: None,
            budgeted_memory_context: None,
            budgeted_history: Vec::new(),
            estimated_tokens: 0,
            chat_request: None,
            llm_stream: None,
            output_stream: None,
        }
    }

    /// 链式设置初始应用状态（供 Stage 1 CheckState 读取）。
    ///
    /// 用法:
    /// ```ignore
    /// let data = PipelineData::new(input, uid, sid, rid)
    ///     .with_app_state(app.current_state());
    /// ```
    ///
    /// 参数:
    /// - `state`: 当前应用状态。
    ///
    /// 返回:
    /// - 设置了 `app_state` 字段的 `PipelineData`（链式调用）。
    pub fn with_app_state(mut self, state: AppState) -> Self {
        self.app_state = Some(state);
        self
    }
}

// =========================================================
// SendMessagePipeline 编排器
// =========================================================

/// 管线编排器。
///
/// 职责:
/// - 按顺序执行 Stage 序列
/// - 任意 Stage 失败时中止并返回错误
/// - 传递 PipelineData 贯穿所有 Stage
/// - 记录每个 Stage 的执行日志
///
/// 用法:
/// ```ignore
/// let pipeline = SendMessagePipeline::new(vec![
///     Box::new(StageCheckState::new()),
///     Box::new(StageCheckPrivacy::new()),
///     // ...
/// ]);
/// let result = pipeline.execute(&ctx, data).await?;
/// ```
pub struct SendMessagePipeline {
    /// Stage 序列（按执行顺序排列）
    stages: Vec<Box<dyn PipelineStage<Input = PipelineData, Output = PipelineData>>>,
}

impl SendMessagePipeline {
    /// 创建管线编排器。
    ///
    /// 参数:
    /// - `stages`: Stage 序列（按执行顺序排列）。
    pub fn new(
        stages: Vec<Box<dyn PipelineStage<Input = PipelineData, Output = PipelineData>>>,
    ) -> Self {
        Self { stages }
    }

    /// 执行管线。
    ///
    /// 参数:
    /// - `ctx`: 共享管线上下文。
    /// - `data`: 初始管线数据（含用户输入参数）。
    ///
    /// 返回:
    /// - `Ok(data)`: 全部 Stage 执行成功，`data` 包含最终产出。
    /// - `Err(PipelineError)`: 某个 Stage 失败，管线中止。
    ///
    /// 说明:
    /// - Stage 按注册顺序依次执行。
    /// - 每个 Stage 的输出作为下一个 Stage 的输入。
    /// - 任意 Stage 返回 Err 时，立即中止并返回错误。
    /// - 每个 Stage 执行前后记录 trace 级别日志，失败时记录 error 日志。
    pub async fn execute(
        &self,
        ctx: &PipelineContext,
        mut data: PipelineData,
    ) -> Result<PipelineData, PipelineError> {
        for stage in &self.stages {
            let stage_name = stage.name();
            tracing::trace!(stage = stage_name, "Pipeline stage 开始");

            data = stage.execute(ctx, data).await.map_err(|e| {
                tracing::error!(
                    stage = stage_name,
                    error = %e,
                    retryable = e.is_retryable(),
                    "Pipeline stage 失败，管线中止"
                );
                e
            })?;

            tracing::trace!(stage = stage_name, "Pipeline stage 完成");
        }

        tracing::debug!(
            stages_executed = self.stages.len(),
            "Pipeline 全部 Stage 执行完成"
        );
        Ok(data)
    }

    /// 返回管线中 Stage 的数量。
    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use ramaria_core::types::MessageRole;
    use ramaria_core::types::{
        ClusterSnapshot, EventRelation, LlmProvider as LlmProviderKind, MemoryEvent, MemoryL1,
        ModelCapability, Persona, PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent,
        ProfileField, TraitEvidence, TraitStatus,
    };
    use std::pin::Pin;

    // =========================================================
    // 测试 Mock: 最小化 StorageBackend 实现
    // =========================================================

    /// 测试用 Mock StorageBackend——所有方法返回 Ok(default)。
    ///
    /// 设计:
    /// - 不维护任何状态，仅满足 trait 编译要求
    /// - 编排器测试中的 Mock Stage 不会调用任何 Storage 方法
    struct TestStorage;

    #[async_trait::async_trait]
    impl StorageBackend for TestStorage {
        async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
            Ok(Session::with_persona(persona_uid.map(|s| s.to_string())))
        }
        async fn close_session(&self, _id: Uuid) -> RamariaResult<()> {
            Ok(())
        }
        async fn get_session(&self, _id: Uuid) -> RamariaResult<Option<Session>> {
            Ok(None)
        }
        async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
            Ok(Vec::new())
        }
        async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
            Ok(Vec::new())
        }
        async fn delete_session(&self, _id: Uuid) -> RamariaResult<()> {
            Ok(())
        }
        async fn save_message(&self, _msg: &ramaria_core::types::Message) -> RamariaResult<()> {
            Ok(())
        }
        async fn list_messages(
            &self,
            _id: Uuid,
        ) -> RamariaResult<Vec<ramaria_core::types::Message>> {
            Ok(Vec::new())
        }
        async fn list_messages_by_persona(
            &self,
            _uid: &str,
        ) -> RamariaResult<Vec<ramaria_core::types::Message>> {
            Ok(Vec::new())
        }
        async fn find_message_by_fingerprint(
            &self,
            _fp: &str,
        ) -> RamariaResult<Option<ramaria_core::types::Message>> {
            Ok(None)
        }
        async fn save_memory_l1(&self, _m: &MemoryL1) -> RamariaResult<()> {
            Ok(())
        }
        async fn list_memory_l1(&self, _id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
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
        async fn save_privacy_consent(&self, _c: &PrivacyConsent) -> RamariaResult<()> {
            Ok(())
        }
        async fn get_privacy_consent(
            &self,
            _p: &str,
            _b: &str,
        ) -> RamariaResult<Option<PrivacyConsent>> {
            Ok(None)
        }
        async fn save_backend_config(&self, _c: &BackendConfig) -> RamariaResult<()> {
            Ok(())
        }
        async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
            Ok(None)
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
        async fn update_job_status(
            &self,
            _id: i64,
            _s: &str,
            _e: Option<&str>,
        ) -> RamariaResult<()> {
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
        async fn list_pending_conflicts(
            &self,
        ) -> RamariaResult<Vec<(i64, String, String, String)>> {
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
        async fn insert_graph_node(
            &self,
            _n: &str,
            _t: &str,
            _l: Option<Uuid>,
        ) -> RamariaResult<i64> {
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
    }

    // =========================================================
    // 测试 Mock: 最小化 LlmProvider 实现
    // =========================================================

    /// 测试用 Mock LlmProvider——返回空回复。
    struct TestLlm {
        config: BackendConfig,
        capability: ModelCapability,
    }

    impl TestLlm {
        fn new() -> Self {
            Self {
                config: BackendConfig::lm_studio_default(),
                capability: ModelCapability {
                    provider: LlmProviderKind::LmStudio,
                    model_id: "test-model".into(),
                    base_url: "http://localhost:1234/v1".into(),
                    supports_streaming: true,
                    supports_json_mode: false,
                    context_window: 4096,
                    max_output_tokens: 4096,
                },
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for TestLlm {
        async fn chat(&self, _req: &ChatRequest) -> RamariaResult<String> {
            Ok(String::new())
        }
        async fn chat_stream(
            &self,
            _req: &ChatRequest,
        ) -> RamariaResult<Pin<Box<dyn futures::Stream<Item = RamariaResult<StreamDelta>> + Send>>>
        {
            Ok(Box::pin(stream::iter(vec![Ok(StreamDelta {
                content: String::new(),
                done: true,
                metadata: Some("stop".into()),
            })])))
        }
        fn capability(&self) -> &ModelCapability {
            &self.capability
        }
        fn config(&self) -> &BackendConfig {
            &self.config
        }
        async fn validate(&self) -> RamariaResult<()> {
            Ok(())
        }
        fn name(&self) -> &'static str {
            "TestLlm"
        }
    }

    // =========================================================
    // 测试辅助: 构建 PipelineContext
    // =========================================================

    /// 构建测试用 PipelineContext。
    ///
    /// 使用 TestStorage + TestLlm，检索器为空，配置为默认值。
    fn test_context() -> PipelineContext {
        let storage: Arc<dyn StorageBackend> = Arc::new(TestStorage);
        let llm: Arc<dyn LlmProvider> = Arc::new(TestLlm::new());
        let config = RamariaConfig::default();
        let retriever = Arc::new(Mutex::new(Retriever::new()));
        let keychain = Arc::new(Keychain::new());
        let lifecycle = Arc::new(SessionLifecycle::new(config.clone()));

        PipelineContext::new(storage, llm, None, config, retriever, keychain, lifecycle)
    }

    // =========================================================
    // 测试辅助: Mock Stage 实现
    // =========================================================

    /// 透传 Stage——不做任何修改，直接返回输入数据。
    struct PassThroughStage {
        stage_name: &'static str,
    }

    #[async_trait]
    impl PipelineStage for PassThroughStage {
        type Input = PipelineData;
        type Output = PipelineData;

        fn name(&self) -> &'static str {
            self.stage_name
        }

        async fn execute(
            &self,
            _ctx: &PipelineContext,
            input: Self::Input,
        ) -> Result<Self::Output, PipelineError> {
            Ok(input)
        }
    }

    /// 失败 Stage——始终返回 Fatal 错误。
    struct FailStage {
        stage_name: &'static str,
    }

    #[async_trait]
    impl PipelineStage for FailStage {
        type Input = PipelineData;
        type Output = PipelineData;

        fn name(&self) -> &'static str {
            self.stage_name
        }

        async fn execute(
            &self,
            _ctx: &PipelineContext,
            _input: Self::Input,
        ) -> Result<Self::Output, PipelineError> {
            Err(PipelineError::fatal(
                self.stage_name,
                RamariaError::validation("stage deliberately failed for testing"),
            ))
        }
    }

    /// 标记 Stage——在 PipelineData 中写入标记值，验证 Stage 确实被执行。
    struct MarkStage {
        stage_name: &'static str,
    }

    #[async_trait]
    impl PipelineStage for MarkStage {
        type Input = PipelineData;
        type Output = PipelineData;

        fn name(&self) -> &'static str {
            self.stage_name
        }

        async fn execute(
            &self,
            _ctx: &PipelineContext,
            mut input: Self::Input,
        ) -> Result<Self::Output, PipelineError> {
            // 在 system_prompt 字段追加标记，证明此 Stage 被执行
            let mark = input.system_prompt.unwrap_or_default();
            input.system_prompt = Some(format!("{mark}+{stage}", stage = self.stage_name));
            Ok(input)
        }
    }

    // =========================================================
    // T-V12-1-004: PipelineError 测试
    // =========================================================

    #[test]
    fn pipeline_error_retryable_construction() {
        let err = PipelineError::retryable("CallLlm", RamariaError::llm("connection timeout"));
        assert!(err.is_retryable());
        assert_eq!(err.stage(), "CallLlm");
        assert_eq!(err.source_error().category(), "llm");
    }

    #[test]
    fn pipeline_error_fatal_construction() {
        let err = PipelineError::fatal(
            "CheckState",
            RamariaError::validation("app in fatal error state"),
        );
        assert!(!err.is_retryable());
        assert_eq!(err.stage(), "CheckState");
        assert_eq!(err.source_error().category(), "validation");
    }

    #[test]
    fn pipeline_error_display_retryable() {
        let err = PipelineError::retryable("RetrieveMemory", RamariaError::storage("index locked"));
        let msg = err.to_string();
        assert!(msg.contains("RetrieveMemory"));
        assert!(msg.contains("retryable"));
        assert!(msg.contains("index locked"));
    }

    #[test]
    fn pipeline_error_display_fatal() {
        let err =
            PipelineError::fatal("ResolveSession", RamariaError::validation("session closed"));
        let msg = err.to_string();
        assert!(msg.contains("ResolveSession"));
        assert!(msg.contains("fatal"));
        assert!(msg.contains("session closed"));
    }

    #[test]
    fn pipeline_error_stage_name_both_variants() {
        let retryable = PipelineError::retryable("A", RamariaError::llm("x"));
        let fatal = PipelineError::fatal("B", RamariaError::storage("y"));
        assert_eq!(retryable.stage(), "A");
        assert_eq!(fatal.stage(), "B");
    }

    #[test]
    fn pipeline_error_source_error_preserves_category() {
        let err = PipelineError::retryable("CallLlm", RamariaError::privacy("not confirmed"));
        assert_eq!(err.source_error().category(), "privacy");
        assert_eq!(err.source_error().context(), "not confirmed");
    }

    #[test]
    fn pipeline_error_to_ramaria_error() {
        let original = RamariaError::llm("timeout");
        let pipeline_err = PipelineError::retryable("CallLlm", original);
        let ramaria_err: RamariaError = pipeline_err.into();
        assert_eq!(ramaria_err.category(), "llm");
        assert!(ramaria_err.context().contains("timeout"));
    }

    #[test]
    fn pipeline_error_fatal_to_ramaria_error() {
        let original = RamariaError::validation("bad state");
        let pipeline_err = PipelineError::fatal("CheckState", original);
        let ramaria_err: RamariaError = pipeline_err.into();
        assert_eq!(ramaria_err.category(), "validation");
    }

    // =========================================================
    // T-V12-1-004: PipelineData 测试
    // =========================================================

    #[test]
    fn pipeline_data_new_sets_input_fields() {
        let request_id = Uuid::new_v4();
        let data = PipelineData::new(
            "你好".to_string(),
            Some("rama-0001".to_string()),
            Some(Uuid::new_v4()),
            request_id,
        );
        assert_eq!(data.user_input, "你好");
        assert_eq!(data.persona_uid.as_deref(), Some("rama-0001"));
        assert!(data.session_id.is_some());
        assert_eq!(data.request_id, request_id);
    }

    #[test]
    fn pipeline_data_new_defaults_stage_outputs() {
        let data = PipelineData::new("test".to_string(), None, None, Uuid::new_v4());
        // Stage 1-3 产出应为 None
        assert!(data.app_state.is_none());
        assert!(data.backend_config.is_none());
        assert!(data.session.is_none());

        // Stage 4 集合字段应为空
        assert!(data.history_messages.is_empty());
        assert!(data.recent_summaries.is_empty());
        assert!(data.last_active_at.is_none());

        // Stage 5-8 可选字段应为 None
        assert!(data.memory_context.is_none());
        assert!(data.system_prompt.is_none());
        assert!(data.budgeted_system_prompt.is_none());
        assert!(data.budgeted_memory_context.is_none());
        assert!(data.chat_request.is_none());

        // Stage 7 数值字段应为 0
        assert_eq!(data.estimated_tokens, 0);
        assert!(data.budgeted_history.is_empty());

        // Stage 9-10 流字段应为 None
        assert!(data.llm_stream.is_none());
        assert!(data.output_stream.is_none());
    }

    #[test]
    fn pipeline_data_fields_are_writable() {
        let mut data = PipelineData::new("hello".to_string(), None, None, Uuid::new_v4());

        // 模拟 Stage 1 写入
        data.app_state = Some(AppState::Ready);
        assert_eq!(data.app_state, Some(AppState::Ready));

        // 模拟 Stage 3 写入
        let session = Session::new();
        data.session = Some(session.clone());
        assert_eq!(data.session.as_ref().unwrap().id, session.id);

        // 模拟 Stage 4 写入
        data.history_messages.push(ChatMessage {
            role: MessageRole::User,
            content: "历史消息".into(),
        });
        assert_eq!(data.history_messages.len(), 1);

        // 模拟 Stage 6 写入
        data.system_prompt = Some("System prompt".into());
        assert_eq!(data.system_prompt.as_deref(), Some("System prompt"));
    }

    // =========================================================
    // T-V12-1-004: SendMessagePipeline 编排器测试
    // =========================================================

    #[tokio::test]
    async fn pipeline_empty_returns_data_unchanged() {
        let ctx = test_context();
        let pipeline = SendMessagePipeline::new(vec![]);
        let request_id = Uuid::new_v4();
        let data = PipelineData::new("test".into(), None, None, request_id);

        let result = pipeline.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("empty pipeline should succeed");
        assert_eq!(output.user_input, "test");
        assert_eq!(output.request_id, request_id);
    }

    #[tokio::test]
    async fn pipeline_empty_stage_count_zero() {
        let pipeline = SendMessagePipeline::new(vec![]);
        assert_eq!(pipeline.stage_count(), 0);
    }

    #[tokio::test]
    async fn pipeline_single_pass_through() {
        let ctx = test_context();
        let pipeline = SendMessagePipeline::new(vec![Box::new(PassThroughStage {
            stage_name: "OnlyStage",
        })]);

        let data = PipelineData::new("hello".into(), None, None, Uuid::new_v4());
        let result = pipeline.execute(&ctx, data).await;

        assert!(result.is_ok());
        assert_eq!(pipeline.stage_count(), 1);
    }

    #[tokio::test]
    async fn pipeline_multiple_pass_through_preserves_data() {
        let ctx = test_context();
        let pipeline = SendMessagePipeline::new(vec![
            Box::new(PassThroughStage {
                stage_name: "Stage1",
            }),
            Box::new(PassThroughStage {
                stage_name: "Stage2",
            }),
            Box::new(PassThroughStage {
                stage_name: "Stage3",
            }),
        ]);

        let data = PipelineData::new(
            "pipeline test".into(),
            Some("rama-0001".into()),
            None,
            Uuid::new_v4(),
        );
        let result = pipeline.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("all pass-through should succeed");
        assert_eq!(output.user_input, "pipeline test");
        assert_eq!(output.persona_uid.as_deref(), Some("rama-0001"));
        assert_eq!(pipeline.stage_count(), 3);
    }

    #[tokio::test]
    async fn pipeline_stops_on_fatal_error() {
        let ctx = test_context();
        let pipeline = SendMessagePipeline::new(vec![
            Box::new(PassThroughStage {
                stage_name: "BeforeFail",
            }),
            Box::new(FailStage {
                stage_name: "FailingStage",
            }),
            Box::new(PassThroughStage {
                stage_name: "AfterFail",
            }),
        ]);

        let data = PipelineData::new("error path".into(), None, None, Uuid::new_v4());
        let result = pipeline.execute(&ctx, data).await;

        let err = match result {
            Ok(_) => panic!("should fail at FailingStage"),
            Err(e) => e,
        };
        assert!(!err.is_retryable());
        assert_eq!(err.stage(), "FailingStage");
    }

    #[tokio::test]
    async fn pipeline_stops_on_retryable_error() {
        let ctx = test_context();

        // 自定义 Retryable 失败 Stage
        struct RetryableFailStage;
        #[async_trait]
        impl PipelineStage for RetryableFailStage {
            type Input = PipelineData;
            type Output = PipelineData;
            fn name(&self) -> &'static str {
                "RetryableFail"
            }
            async fn execute(
                &self,
                _ctx: &PipelineContext,
                _input: Self::Input,
            ) -> Result<Self::Output, PipelineError> {
                Err(PipelineError::retryable(
                    "RetryableFail",
                    RamariaError::llm("temporary timeout"),
                ))
            }
        }

        let pipeline = SendMessagePipeline::new(vec![
            Box::new(PassThroughStage { stage_name: "Pass" }),
            Box::new(RetryableFailStage),
            Box::new(PassThroughStage {
                stage_name: "NeverReached",
            }),
        ]);

        let data = PipelineData::new("retry test".into(), None, None, Uuid::new_v4());
        let result = pipeline.execute(&ctx, data).await;

        let err = match result {
            Ok(_) => panic!("should fail at RetryableFail"),
            Err(e) => e,
        };
        assert!(err.is_retryable());
        assert_eq!(err.stage(), "RetryableFail");
    }

    #[tokio::test]
    async fn pipeline_stages_executed_in_order() {
        let ctx = test_context();
        let pipeline = SendMessagePipeline::new(vec![
            Box::new(MarkStage {
                stage_name: "Alpha",
            }),
            Box::new(MarkStage { stage_name: "Beta" }),
            Box::new(MarkStage {
                stage_name: "Gamma",
            }),
        ]);

        let data = PipelineData::new("order test".into(), None, None, Uuid::new_v4());
        let result = pipeline.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("mark stages should succeed");
        // MarkStage 在 system_prompt 中追加 "+StageName"
        // 执行顺序应为 Alpha → Beta → Gamma
        assert_eq!(output.system_prompt.as_deref(), Some("+Alpha+Beta+Gamma"));
    }

    #[tokio::test]
    async fn pipeline_error_at_first_stage() {
        let ctx = test_context();
        let pipeline = SendMessagePipeline::new(vec![Box::new(FailStage {
            stage_name: "FirstAndOnly",
        })]);

        let data = PipelineData::new("immediate fail".into(), None, None, Uuid::new_v4());
        let result = pipeline.execute(&ctx, data).await;

        let err = match result {
            Ok(_) => panic!("should fail immediately"),
            Err(e) => e,
        };
        assert_eq!(err.stage(), "FirstAndOnly");
    }

    #[tokio::test]
    async fn pipeline_error_propagates_source_error() {
        let ctx = test_context();
        let pipeline = SendMessagePipeline::new(vec![Box::new(FailStage {
            stage_name: "ValidationError",
        })]);

        let data = PipelineData::new("propagation".into(), None, None, Uuid::new_v4());
        let result = pipeline.execute(&ctx, data).await;

        let err = match result {
            Ok(_) => panic!("should fail"),
            Err(e) => e,
        };
        let source = err.source_error();
        assert_eq!(source.category(), "validation");
        assert!(source.context().contains("testing"));
    }

    #[tokio::test]
    async fn pipeline_error_convertible_to_ramaria_error() {
        let ctx = test_context();
        let pipeline = SendMessagePipeline::new(vec![Box::new(FailStage {
            stage_name: "ConversionTest",
        })]);

        let data = PipelineData::new("conversion".into(), None, None, Uuid::new_v4());
        let result: Result<PipelineData, RamariaError> =
            pipeline.execute(&ctx, data).await.map_err(|e| e.into());

        assert!(result.is_err());
        let err = match result {
            Ok(_) => panic!("should convert to RamariaError"),
            Err(e) => e,
        };
        assert_eq!(err.category(), "validation");
    }
}
