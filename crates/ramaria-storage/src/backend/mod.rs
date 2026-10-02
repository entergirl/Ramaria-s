//! crates/ramaria-storage/src/backend/mod.rs - Ramaria SQLite 后端装配模块
//!
//! 设计特点:
//! - 定义后端门面 `SqliteStorage`（持有连接池，供各能力实现共享）
//! - `crud` / `infrastructure` 子模块分别承载 `StoreCrud` / `StoreInfrastructure` 实现
//! - `llm_cache` 子模块承载 `SqliteLlmCache`（`LlmResponseCache` 实现）
//! - 由 crate 根统一 re-export，保持 `ramaria_storage::SqliteStorage` 等外部路径稳定

pub(crate) mod crud;
pub(crate) mod infrastructure;
pub(crate) mod llm_cache;

use sqlx::SqlitePool;

pub use llm_cache::SqliteLlmCache;

/// SQLite 存储后端。
pub struct SqliteStorage {
    pub(crate) pool: SqlitePool,
}

impl SqliteStorage {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}
