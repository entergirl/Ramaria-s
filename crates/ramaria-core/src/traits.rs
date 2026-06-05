//! rust/crates/ramaria-core/src/traits.rs - Ramaria 核心能力抽象模块
//!
//! 设计特点:
//! - 定义三类核心边界: LLM Provider、Embedding Provider、Storage Backend
//! - 上层 crate 依赖 trait，不依赖具体数据库、模型服务或向量实现
//! - 支持流式 LLM 响应，统一 request_id、delta、done 和 metadata 语义
//! - 支持 embedding 模型下载、进度查询、可用性校验和批量向量化
//! - Storage trait 暴露业务级 CRUD，不泄露 sqlx 连接池或具体表结构

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use uuid::Uuid;

use crate::error::RamariaResult;
use crate::types::{
    BackendConfig, MemoryL1, MemoryL2, Message, MessageRole, ModelCapability, PrivacyConsent,
    Session, UserProfile,
};

// =========================================================
// LLM Provider 抽象
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
    /// - v1.0 必需的 streaming 能力是否可用。
    async fn validate(&self) -> RamariaResult<()>;

    /// 返回 provider 名称。
    ///
    /// 返回:
    /// - 静态名称，用于日志、诊断和 UI 展示。
    fn name(&self) -> &'static str;
}

// =========================================================
// Embedding Provider 抽象
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
// Storage Backend 抽象
// =========================================================

/// 存储后端抽象 trait。
///
/// 职责:
/// - 定义 app 和 memory 层需要的业务级 CRUD。
/// - 隔离 SQLite/sqlx 细节，避免上层持有连接池或拼接 SQL。
/// - 统一 Session、L0 消息、L1/L2/L3 记忆、隐私确认和索引版本的存取边界。
///
/// 实现要求:
/// - 具体实现位于 `ramaria-storage`。
/// - 所有可恢复错误应转换为 `RamariaError::Storage` 或更精确分类。
/// - 删除操作必须明确处理关联数据，避免悬挂引用。
#[async_trait]
pub trait StorageBackend: Send + Sync {
    // -- Session --

    /// 创建新 session。
    async fn create_session(&self) -> RamariaResult<Session>;

    /// 关闭 session。
    async fn close_session(&self, session_id: Uuid) -> RamariaResult<()>;

    /// 获取单个 session。
    async fn get_session(&self, session_id: Uuid) -> RamariaResult<Option<Session>>;

    /// 列出活跃 session（未关闭的）。
    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>>;

    /// 列出所有 session。
    async fn list_sessions(&self) -> RamariaResult<Vec<Session>>;

    /// 删除 session 及其关联消息。
    async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()>;

    // -- Message (L0) --

    /// 保存消息。
    async fn save_message(&self, message: &Message) -> RamariaResult<()>;

    /// 获取 session 的所有消息（按时间升序）。
    async fn list_messages(&self, session_id: Uuid) -> RamariaResult<Vec<Message>>;

    /// 按指纹检查消息是否已存在（去重用）。
    async fn find_message_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> RamariaResult<Option<Message>>;

    // -- Memory L1 --

    /// 保存 L1 记忆。
    async fn save_memory_l1(&self, memory: &MemoryL1) -> RamariaResult<()>;

    /// 获取指定 session 的 L1 记忆列表。
    async fn list_memory_l1(&self, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>>;

    /// 获取单个 L1 记忆。
    async fn get_memory_l1(&self, id: Uuid) -> RamariaResult<Option<MemoryL1>>;

    /// 标记 L1 已被 L2 吸收。
    async fn mark_l1_absorbed(&self, l1_ids: &[Uuid]) -> RamariaResult<()>;

    /// 查询所有未吸收的 L1 记忆。
    async fn list_unabsorbed_l1(&self) -> RamariaResult<Vec<MemoryL1>>;

    // -- Memory L2 --

    /// 保存 L2 记忆。
    async fn save_memory_l2(&self, memory: &MemoryL2) -> RamariaResult<()>;

    /// 保存 L2 → L1 溯源关系。
    async fn save_l2_sources(&self, l2_id: Uuid, l1_ids: &[Uuid]) -> RamariaResult<()>;

    /// 列出所有 L2 记忆。
    async fn list_memory_l2(&self) -> RamariaResult<Vec<MemoryL2>>;

    /// 获取 L2 的来源 L1 列表。
    async fn get_l2_sources(&self, l2_id: Uuid) -> RamariaResult<Vec<Uuid>>;

    // -- User Profile (L3) --

    /// 保存画像条目。
    async fn save_user_profile(&self, profile: &UserProfile) -> RamariaResult<()>;

    /// 获取当前生效的画像（is_current = true）。
    async fn get_current_profile(&self) -> RamariaResult<Vec<UserProfile>>;

    /// 将旧版本标记为非 current。
    async fn mark_profile_historical(&self, field: &str) -> RamariaResult<()>;

    // -- Privacy Consent --

    /// 保存隐私确认记录。
    async fn save_privacy_consent(&self, consent: &PrivacyConsent) -> RamariaResult<()>;

    /// 获取某 provider + base_url 的确认记录。
    async fn get_privacy_consent(
        &self,
        provider: &str,
        base_url: &str,
    ) -> RamariaResult<Option<PrivacyConsent>>;

    // -- Backend Config --

    /// 保存非敏感后端配置。
    async fn save_backend_config(&self, config: &BackendConfig) -> RamariaResult<()>;

    /// 获取当前后端配置。
    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>>;

    // -- 索引一致性 --

    /// 获取 schema 版本。
    async fn get_schema_version(&self) -> RamariaResult<i32>;

    /// 获取索引版本。
    async fn get_index_version(&self) -> RamariaResult<i32>;

    /// 更新索引版本。
    async fn set_index_version(&self, version: i32) -> RamariaResult<()>;
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
