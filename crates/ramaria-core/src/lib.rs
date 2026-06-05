//! rust/crates/ramaria-core/src/lib.rs - Ramaria 核心 crate 入口模块
//!
//! 设计特点:
//! - 统一暴露核心能力: 配置、错误、trait、业务数据类型
//! - 提供常用 re-export，减少上层 crate 的导入路径噪音
//! - 保持核心层零 I/O，不依赖数据库、网络或异步运行时
//! - 作为 workspace 的类型边界，避免 CLI/Desktop 直接耦合底层实现
//! - 所有公共类型面向跨 crate 共享和长期演进设计

pub mod config;
pub mod error;
pub mod traits;
pub mod types;

// 常用 re-export
pub use config::RamariaConfig;
pub use error::{RamariaError, RamariaResult};
pub use traits::{
    ChatMessage, ChatRequest, Embedding, EmbeddingModelInfo, EmbeddingProvider,
    LlmProvider as LlmProviderTrait, StorageBackend, StreamDelta,
};
pub use types::{
    AppState, BackendConfig, L2Source, LlmProvider, MemoryL1, MemoryL2, Message, MessageRole,
    MessageSource, ModelCapability, PrivacyConsent, ProfileField, ProfileStatus, Session,
    TIME_PERIOD_OPTIONS, UserProfile, new_id, now_ms,
};
