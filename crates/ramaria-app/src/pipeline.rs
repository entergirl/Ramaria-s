//! crates/ramaria-app/src/pipeline.rs - Pipeline + Stage 核心基础设施
//!
//! 设计特点:
//! - PipelineStage trait 定义统一 Stage 接口，每个 Stage 独立可测试
//! - PipelineContext 全 Arc 引用，Stage 间零拷贝传递共享依赖
//! - PipelineData 贯穿整个管线，承载各阶段中间结果
//! - PipelineError 区分 Retryable（可重试）和 Fatal（不可恢复），编排器统一处理
//! - SendMessagePipeline 编排器按顺序执行 Stage 序列，任一失败即中止
//! - 向后兼容：App::send_message 对外接口不变，内部委托 SendMessagePipeline::execute

use std::sync::{Arc, RwLock};

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
    /// 内存检索器（RwLock 替代 Mutex，允许多读并发）
    pub retriever: Arc<RwLock<Retriever>>,
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
    /// - `retriever`: 检索器（Arc<RwLock> 包裹，支持并发读写）。
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
        retriever: Arc<RwLock<Retriever>>,
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

    // === Stage 5.5: utt 原文块检索（v1.4） ===
    /// utt 原文片段（已按预算裁剪渲染；白名单外/未命中为 None，等同 v1.3）
    pub utt_context: Option<String>,

    /// 桥接内容（上一会话尾部原文，已按预算渲染；None 表示不注入，等同 v1.3）
    pub bridge_context: Option<String>,

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
            utt_context: None,
            bridge_context: None,
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
mod tests;
