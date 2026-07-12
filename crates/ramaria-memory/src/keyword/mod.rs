//! rust/crates/ramaria-memory/src/keyword/mod.rs - Ramaria 关键词处理模块入口
//!
//! 设计特点:
//! - 暴露关键词归一化器（BigramWithDictionaryNormalizer）和别名管理器（AliasManager）
//! - 供 L1 Summarizer、EventExtractor、BM25 分词器复用
//! - 所有纯函数逻辑可独立测试，不依赖数据库

pub mod alias;
pub mod normalizer;

// =========================================================
// 常用 re-export
// =========================================================

pub use alias::AliasManager;
pub use normalizer::BigramWithDictionaryNormalizer;
