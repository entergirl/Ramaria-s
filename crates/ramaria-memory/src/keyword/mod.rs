//! crates/ramaria-memory/src/keyword/mod.rs - Ramaria 关键词处理模块入口
//!
//! 设计特点:
//! - 暴露关键词别名管理器（AliasManager）
//! - 供 L1 Summarizer、EventExtractor 复用
//! - 所有纯函数逻辑可独立测试，不依赖数据库

pub mod alias;

// =========================================================
// 常用 re-export
// =========================================================

pub use alias::AliasManager;
