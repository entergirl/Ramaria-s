//! crates/ramaria-core/src/traits/mod.rs - Ramaria 核心能力抽象模块入口
//!
//! 设计特点:
//! - 聚合三类核心边界: LLM Provider、Embedding Provider、Storage Backend
//! - 上层 crate 依赖 trait，不依赖具体数据库、模型服务或向量实现
//! - Storage 能力按业务 CRUD 与基础设施两组拆分，避免单一巨 trait
//! - 逐项 re-export 子模块公开条目，保持原有深层导入路径稳定
//! - 保持核心层零 I/O，不依赖数据库、网络或异步运行时

mod cache;
mod embedding;
mod llm;
mod store_backend;
mod store_crud;
mod store_version;

pub use cache::LlmResponseCache;
pub use embedding::{Embedding, EmbeddingModelInfo, EmbeddingProvider};
pub use llm::{ChatMessage, ChatRequest, LlmProvider, StreamDelta};
pub use store_backend::{StorageBackend, StoreInfrastructure};
pub use store_crud::{ProactiveDeliveryPair, StoreCrud};
pub use store_version::{
    BM25_INDEX_VERSION_CURRENT, BM25_INDEX_VERSION_LEGACY, IndexCorpusStamp,
    SETTING_BM25_INDEX_VERSION,
};

#[cfg(test)]
mod tests;
