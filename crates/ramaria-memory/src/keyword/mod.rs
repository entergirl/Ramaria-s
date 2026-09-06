//! crates/ramaria-memory/src/keyword/mod.rs - Ramaria 关键词处理模块入口
//!
//! 设计特点:
//! - 暴露关键词别名管理器（AliasManager）
//! - 提供统一关键词标准化器（normalizer，keyword-design §4，M3 起收拢 5 处重复解析）
//! - 提供关键词子系统应用层服务（service）：KeywordPool + CompositeIndex 装载 / 增量维护
//! - 供 L1 Summarizer、EventExtractor、BM25、ExampleSelector、降级路径复用
//! - 所有纯函数逻辑可独立测试，不依赖数据库

pub mod alias;
pub mod composite;
pub mod index;
pub mod normalizer;
pub mod pool;
pub mod service;

// =========================================================
// 常用 re-export
// =========================================================

pub use alias::AliasManager;
pub use composite::{CompositeIndex, CompositeIndexConfig, FuzzyKeywordIndex};
pub use index::{DefaultScoringStrategy, KeywordIndex, ScoringStrategy};
pub use normalizer::{
    BigramNormalizer, BigramWithDictionaryNormalizer, CommaSeparatedNormalizer, KeywordNormalizer,
};
pub use pool::{AliasConflict, KeywordPool, PoolEntry};
pub use service::KeywordService;
