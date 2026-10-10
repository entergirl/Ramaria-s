//! crates/ramaria-core/src/config/mod.rs - Ramaria 应用配置类型模块入口
//!
//! 设计特点:
//! - 按职责拆分配置域: 路径、后端、检索、衰减、Session、阈值、索引、日志、推断、
//!   事件提取、L1 摘要、utt 话语块、示例、桥接、缓存、行为、知识、嵌入、风格、反馈、
//!   注入协调预算、层间去重、主动对话、图片理解
//! - 每组配置提供稳定默认值，保证首次启动和测试环境有一致行为
//! - 支持 serde 序列化与反序列化，便于 CLI、GUI 和配置文件共享
//! - 非敏感配置才允许进入 config.toml，API key 始终由 OS keychain 管理
//! - 配置结构只描述数据，不负责读取文件、访问环境变量或写入磁盘
//! - 内建版本控制（version + schema_version），支持未来配置文件迁移

// =========================================================
// 版本控制常量
// =========================================================

/// 当前配置文件 schema 版本号。
///
/// 用途:
/// - 写入 config.toml 的 `schema_version` 字段。
/// - 当配置结构发生不兼容变更时递增此值，加载层据此触发迁移。
///
/// 版本历史:
/// - 1: 初始 schema
const CURRENT_SCHEMA_VERSION: u32 = 1;

/// 当前 Ramaria 应用版本号（与 workspace Cargo.toml 保持同步）。
const CURRENT_APP_VERSION: &str = "2.6.0";

mod channels;
mod core;
mod domains;
mod infra;
mod layers;
mod paths;
mod proactive;
mod retrieval;
mod runtime;
mod vision;

pub use channels::{BridgeConfig, McpConfig, UttConfig};
pub use core::RamariaConfig;
pub use domains::{
    BehaviorConfig, CalibrationConf, ConfidenceConf, DriftConf, ExamplesConfig, FeedbackConfig,
    InferenceConfig, InferenceUpgradeConfig, InferrerConf, KnowledgeConfig, StyleConfig,
};
pub use infra::{
    BackendSelection, CacheConfig, CacheEviction, EmbeddingConfig, EmbeddingDevice, LoggingConfig,
    MiscConfig,
};
pub use layers::{
    InjectionBudgetConfig, InjectionGate, InjectionSlot, L1Config, L1ProgressiveConfig,
    LayerDedupConfig,
};
pub use paths::PathConfig;
pub use proactive::ProactiveConfig;
pub use retrieval::{DecayConfig, RetrievalConfig};
pub use runtime::{EventExtractionConfig, IndexConfig, SessionConfig, ThresholdConfig};
pub use vision::VisionConfig;

#[cfg(test)]
mod tests;
