//! crates/ramaria-storage/src/lib.rs - Ramaria SQLite 存储层
//!
//! 设计特点:
//! - 存储层入口：装配 `SqliteStorage`（`StoreCrud` + `StoreInfrastructure` = `StorageBackend`，对应 schema 27 张表；`pending_push` 预留未接线）与 `SqliteLlmCache`
//! - Repository 模式：每个子模块负责一类实体的 SQL 操作与行映射
//! - 所有可恢复错误统一转换为 RamariaError::Storage
//! - 手动行映射避免 sqlx derive 侵入 core 层，保持零 I/O 约束
//! - 公共 API 与 `StorageBackend` 聚合 trait 一致，供 app/memory 层依赖注入使用
//! - ID 类型对齐: TEXT 主键表用 Uuid，INTEGER AUTOINCREMENT 表用 i64

pub mod database;
pub mod repo;
pub mod retry;

mod backend;

pub use backend::{SqliteLlmCache, SqliteStorage};

#[cfg(test)]
mod tests;
