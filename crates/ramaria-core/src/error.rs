//! 错误类型定义（骨架）

use thiserror::Error;

/// Ramaria 统一错误类型
#[derive(Error, Debug)]
pub enum RamariaError {
    #[error("配置错误: {0}")]
    Config(String),

    #[error("存储错误: {0}")]
    Storage(String),

    #[error("LLM 错误: {0}")]
    Llm(String),

    #[error("隐私确认错误: {0}")]
    Privacy(String),

    #[error("索引错误: {0}")]
    Index(String),

    #[error("校验错误: {0}")]
    Validation(String),

    #[error("IO 错误: {0}")]
    Io(String),

    #[error("不支持的操作: {0}")]
    Unsupported(String),
}

/// Ramaria 统一 Result 类型
pub type Result<T> = std::result::Result<T, RamariaError>;
