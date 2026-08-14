//! crates/ramaria-core/src/error.rs - Ramaria 统一错误管理模块
//!
//! 设计特点:
//! - 标准化错误分类: Config / Storage / Llm / Privacy / Index / Validation / Io / Unsupported
//! - 统一公共 API 返回类型: `RamariaResult<T>`
//! - 支持 trace_id 贯穿请求、检索、LLM 调用和后台任务生命周期
//! - 支持 source 错误链，保留底层错误上下文，便于日志和 UI 诊断
//! - 提供便捷构造器和常用 From 实现，减少上层 crate 的重复样板代码

use thiserror::Error;

// =========================================================
// 统一错误分类
// =========================================================

/// Ramaria 统一错误类型。
///
/// 设计目标:
/// - 所有公共 API 使用同一种错误枚举，便于 CLI/Desktop 统一展示。
/// - 每类错误保留上下文文本和可选 source，日志层可以串联完整错误链。
/// - 每类错误都可携带 trace_id，方便从 UI 操作追踪到存储、检索和 LLM 调用。
///
/// 分类语义:
/// - `Config`: 配置解析、配置值非法或缺少必需字段。
/// - `Storage`: SQLite 连接、migration、查询和事务错误。
/// - `Llm`: provider 连接、HTTP 状态码、流式解析和模型响应错误。
/// - `Serialization`: JSON/二进制序列化与反序列化错误。
/// - `Privacy`: 隐私确认未完成、keychain 存取失败或线上调用被阻止。
/// - `Index`: 向量、BM25、图谱索引读写和重建错误。
/// - `Validation`: 用户输入、模型输出或内部数据结构校验失败。
/// - `Io`: 文件系统读写错误。
/// - `Unsupported`: 当前版本明确不支持的功能调用。
#[derive(Error, Debug)]
pub enum RamariaError {
    /// 配置相关错误。
    #[error("config error: {context}")]
    Config {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
        trace_id: Option<String>,
    },

    /// 存储（数据库）相关错误。
    #[error("storage error: {context}")]
    Storage {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
        trace_id: Option<String>,
    },

    /// LLM 相关错误。
    #[error("llm error: {context}")]
    Llm {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
        trace_id: Option<String>,
    },

    /// 序列化/反序列化错误（JSON、MessagePack 等）。
    ///
    /// 与 `Config` 不同：`Config` 指配置语义层面的错误，
    /// 而 `Serialization` 专指 serde 序列化/反序列化技术层面的错误。
    #[error("serialization error: {context}")]
    Serialization {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
        trace_id: Option<String>,
    },

    /// 隐私确认相关错误。
    #[error("privacy error: {context}")]
    Privacy {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
        trace_id: Option<String>,
    },

    /// 索引操作相关错误。
    #[error("index error: {context}")]
    Index {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
        trace_id: Option<String>,
    },

    /// 输入校验失败。
    #[error("validation error: {context}")]
    Validation {
        context: String,
        trace_id: Option<String>,
    },

    /// 文件系统 I/O 错误。
    #[error("io error: {context}")]
    Io {
        context: String,
        #[source]
        source: Option<std::io::Error>,
        trace_id: Option<String>,
    },

    /// 嵌入模型相关错误（模型加载、推理、架构检测等）。
    #[error("embedding error: {context}")]
    Embedding {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
        trace_id: Option<String>,
    },

    /// 不支持的功能。
    #[error("unsupported: {context}")]
    Unsupported {
        context: String,
        trace_id: Option<String>,
    },
}

/// 便捷类型别名。
pub type RamariaResult<T> = std::result::Result<T, RamariaError>;

// =========================================================
// 错误构造器
// =========================================================

impl RamariaError {
    // -- Config --

    /// 创建配置错误。
    ///
    /// 参数:
    /// - `context`: 面向开发者和用户提示的错误上下文。
    ///
    /// 返回:
    /// - `RamariaError::Config`，不包含 source 和 trace_id。
    pub fn config(context: impl Into<String>) -> Self {
        Self::Config {
            context: context.into(),
            source: None,
            trace_id: None,
        }
    }

    // -- Storage --

    /// 创建存储错误。
    ///
    /// 参数:
    /// - `context`: SQLite、migration 或 repository 操作的错误上下文。
    pub fn storage(context: impl Into<String>) -> Self {
        Self::Storage {
            context: context.into(),
            source: None,
            trace_id: None,
        }
    }

    /// 创建带 source 的存储错误。
    ///
    /// 参数:
    /// - `context`: 当前存储操作的错误描述。
    /// - `source`: sqlx、I/O 或其他底层错误。
    pub fn storage_with_source(
        context: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::Storage {
            context: context.into(),
            source: Some(source.into()),
            trace_id: None,
        }
    }

    // -- Llm --

    /// 创建 LLM 错误。
    ///
    /// 参数:
    /// - `context`: provider、模型、请求或响应解析相关的错误上下文。
    pub fn llm(context: impl Into<String>) -> Self {
        Self::Llm {
            context: context.into(),
            source: None,
            trace_id: None,
        }
    }

    /// 创建带 source 的 LLM 错误。
    ///
    /// 参数:
    /// - `context`: 当前 LLM 操作的错误描述。
    /// - `source`: HTTP、JSON 或流式解析底层错误。
    pub fn llm_with_source(
        context: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::Llm {
            context: context.into(),
            source: Some(source.into()),
            trace_id: None,
        }
    }

    // -- Serialization --

    /// 创建序列化/反序列化错误。
    ///
    /// 参数:
    /// - `context`: serde 或二进制序列化操作的错误上下文。
    pub fn serialization(context: impl Into<String>) -> Self {
        Self::Serialization {
            context: context.into(),
            source: None,
            trace_id: None,
        }
    }

    // -- Privacy --

    /// 创建隐私错误。
    ///
    /// 参数:
    /// - `context`: 隐私确认、线上调用许可或 keychain 操作的错误上下文。
    pub fn privacy(context: impl Into<String>) -> Self {
        Self::Privacy {
            context: context.into(),
            source: None,
            trace_id: None,
        }
    }

    // -- Index --

    /// 创建索引错误。
    ///
    /// 参数:
    /// - `context`: 向量、BM25、图谱索引操作的错误上下文。
    pub fn index(context: impl Into<String>) -> Self {
        Self::Index {
            context: context.into(),
            source: None,
            trace_id: None,
        }
    }

    /// 创建带 source 的索引错误。
    ///
    /// 参数:
    /// - `context`: 当前索引操作的错误描述。
    /// - `source`: 向量库、序列化或文件系统底层错误。
    pub fn index_with_source(
        context: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::Index {
            context: context.into(),
            source: Some(source.into()),
            trace_id: None,
        }
    }

    // -- Validation --

    /// 创建校验错误。
    ///
    /// 参数:
    /// - `context`: 校验失败原因，例如字段缺失、范围非法或模型输出格式错误。
    pub fn validation(context: impl Into<String>) -> Self {
        Self::Validation {
            context: context.into(),
            trace_id: None,
        }
    }

    // -- Io --

    /// 创建文件系统 I/O 错误。
    ///
    /// 参数:
    /// - `context`: 当前读写操作的错误描述。
    /// - `source`: 可选的标准库 I/O 错误。
    pub fn io(context: impl Into<String>, source: Option<std::io::Error>) -> Self {
        Self::Io {
            context: context.into(),
            source,
            trace_id: None,
        }
    }

    // -- Embedding --

    /// 创建嵌入模型错误。
    ///
    /// 参数:
    /// - `context`: 模型加载、推理或架构检测相关的错误上下文。
    pub fn embedding(context: impl Into<String>) -> Self {
        Self::Embedding {
            context: context.into(),
            source: None,
            trace_id: None,
        }
    }

    // -- Unsupported --

    /// 创建不支持功能错误。
    ///
    /// 参数:
    /// - `context`: 被调用但当前版本不支持的功能描述。
    pub fn unsupported(context: impl Into<String>) -> Self {
        Self::Unsupported {
            context: context.into(),
            trace_id: None,
        }
    }

    // -- 通用方法 --

    /// 为错误附加 trace_id。
    ///
    /// 用法:
    /// - 在请求入口、后台任务或流式输出创建 trace_id 后调用。
    /// - UI、CLI、日志系统可通过同一 trace_id 串联完整生命周期。
    ///
    /// 参数:
    /// - `trace_id`: 当前请求或任务的追踪 ID。
    ///
    /// 返回:
    /// - 带 trace_id 的错误自身。
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        let tid = Some(trace_id.into());
        match &mut self {
            Self::Config { trace_id, .. } => *trace_id = tid,
            Self::Storage { trace_id, .. } => *trace_id = tid,
            Self::Llm { trace_id, .. } => *trace_id = tid,
            Self::Serialization { trace_id, .. } => *trace_id = tid,
            Self::Privacy { trace_id, .. } => *trace_id = tid,
            Self::Index { trace_id, .. } => *trace_id = tid,
            Self::Embedding { trace_id, .. } => *trace_id = tid,
            Self::Validation { trace_id, .. } => *trace_id = tid,
            Self::Io { trace_id, .. } => *trace_id = tid,
            Self::Unsupported { trace_id, .. } => *trace_id = tid,
        }
        self
    }

    /// 获取错误的 trace_id。
    ///
    /// 返回:
    /// - `Some(&str)`: 错误已绑定追踪 ID。
    /// - `None`: 错误未绑定追踪 ID。
    pub fn trace_id(&self) -> Option<&str> {
        match self {
            Self::Config { trace_id, .. }
            | Self::Storage { trace_id, .. }
            | Self::Llm { trace_id, .. }
            | Self::Serialization { trace_id, .. }
            | Self::Privacy { trace_id, .. }
            | Self::Index { trace_id, .. }
            | Self::Embedding { trace_id, .. }
            | Self::Validation { trace_id, .. }
            | Self::Io { trace_id, .. }
            | Self::Unsupported { trace_id, .. } => trace_id.as_deref(),
        }
    }

    /// 获取错误分类标签。
    ///
    /// 返回:
    /// - 稳定小写字符串，可用于日志字段、UI 映射和测试断言。
    pub fn category(&self) -> &'static str {
        match self {
            Self::Config { .. } => "config",
            Self::Storage { .. } => "storage",
            Self::Llm { .. } => "llm",
            Self::Serialization { .. } => "serialization",
            Self::Privacy { .. } => "privacy",
            Self::Index { .. } => "index",
            Self::Embedding { .. } => "embedding",
            Self::Validation { .. } => "validation",
            Self::Io { .. } => "io",
            Self::Unsupported { .. } => "unsupported",
        }
    }

    /// 获取错误上下文描述。
    ///
    /// 返回:
    /// - 创建错误时传入的上下文文本。
    pub fn context(&self) -> &str {
        match self {
            Self::Config { context, .. }
            | Self::Storage { context, .. }
            | Self::Llm { context, .. }
            | Self::Serialization { context, .. }
            | Self::Privacy { context, .. }
            | Self::Index { context, .. }
            | Self::Embedding { context, .. }
            | Self::Validation { context, .. }
            | Self::Io { context, .. }
            | Self::Unsupported { context, .. } => context.as_str(),
        }
    }
}

// =========================================================
// 常用 From 实现
// =========================================================

impl From<std::io::Error> for RamariaError {
    fn from(err: std::io::Error) -> Self {
        Self::Io {
            context: err.to_string(),
            source: Some(err),
            trace_id: None,
        }
    }
}

impl From<serde_json::Error> for RamariaError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization {
            context: format!("JSON 序列化/反序列化失败: {err}"),
            source: Some(Box::new(err)),
            trace_id: None,
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let err = RamariaError::config("缺少必需配置字段");
        assert!(err.to_string().contains("config error"));
        assert!(err.to_string().contains("缺少必需配置字段"));
    }

    #[test]
    fn error_category() {
        assert_eq!(RamariaError::config("x").category(), "config");
        assert_eq!(RamariaError::storage("x").category(), "storage");
        assert_eq!(RamariaError::llm("x").category(), "llm");
        assert_eq!(RamariaError::serialization("x").category(), "serialization");
        assert_eq!(RamariaError::privacy("x").category(), "privacy");
        assert_eq!(RamariaError::index("x").category(), "index");
        assert_eq!(RamariaError::embedding("x").category(), "embedding");
        assert_eq!(RamariaError::validation("x").category(), "validation");
        assert_eq!(RamariaError::io("x", None).category(), "io");
        assert_eq!(RamariaError::unsupported("x").category(), "unsupported");
    }

    #[test]
    fn error_with_trace_id() {
        let err = RamariaError::storage("数据库连接失败").with_trace_id("trace-abc-123");
        assert_eq!(err.trace_id(), Some("trace-abc-123"));
    }

    #[test]
    fn error_without_trace_id() {
        let err = RamariaError::validation("role 值非法");
        assert_eq!(err.trace_id(), None);
    }

    #[test]
    fn error_context() {
        let err = RamariaError::llm("LM Studio 连接超时");
        assert_eq!(err.context(), "LM Studio 连接超时");
    }

    #[test]
    fn error_with_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "文件未找到");
        let err = RamariaError::storage_with_source("读取数据库失败", io_err);
        assert!(err.to_string().contains("读取数据库失败"));
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "权限不足");
        let err: RamariaError = io_err.into();
        assert_eq!(err.category(), "io");
        assert!(err.to_string().contains("权限不足"));
    }

    #[test]
    fn from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let err: RamariaError = json_err.into();
        assert_eq!(err.category(), "serialization");
        assert!(err.to_string().contains("JSON"));
    }

    #[test]
    fn serialization_error_display() {
        let err = RamariaError::serialization("反序列化 MessageFormat 失败");
        assert!(err.to_string().contains("serialization error"));
        assert!(err.to_string().contains("反序列化 MessageFormat 失败"));
        assert_eq!(err.category(), "serialization");
    }
}
