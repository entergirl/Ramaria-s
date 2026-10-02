//! crates/ramaria-core/src/traits/llm.rs - Ramaria LLM Provider 抽象模块
//!
//! 设计特点:
//! - 定义流式片段与聊天请求/消息等 provider 交互数据结构
//! - 抽象 LM Studio、DeepSeek、OpenAI 等 provider 的统一聊天能力
//! - 统一 request_id、delta、done 与 metadata 的流式语义
//! - 提供非流式/流式入口及能力、配置、校验与健康检查默认实现
//! - 不记录 API key、完整 prompt 或完整用户消息

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use uuid::Uuid;

use crate::error::RamariaResult;
use crate::types::{BackendConfig, MessageRole, ModelCapability};

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
    /// Prompt 模板版本（参与精确缓存 key，变更需递增）。
    ///
    /// 来源: `ramaria_memory::prompt::PROMPT_TEMPLATE_VERSION` 常量，
    /// 随 `prompt/builder.rs`/`layers.rs` 变更递增。
    ///
    /// 用途: 参与精确缓存 key 构造（`sha256(model_id + template_version + prompt)`），
    /// 防止模板变更后跨版本误命中旧缓存。
    pub template_version: String,
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
