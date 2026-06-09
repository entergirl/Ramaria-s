//! rust/crates/ramaria-storage/src/repo/mod.rs - Repository 模块入口
//!
//! 设计特点:
//! - 每个子模块负责一类实体的 SQL 操作和行映射
//! - 提供共享辅助函数，减少各模块的重复代码
//! - 所有数据库错误统一转换为 RamariaError::Storage

pub mod backend_config;
pub mod background_jobs;
pub mod bm25_index;
pub mod cluster;
pub mod conflict_queue;
pub mod events;
pub mod examples;
pub mod facts;
pub mod graph;
pub mod keyword;
pub mod memory_l1;
pub mod messages;
pub mod pending_push;
pub mod personas;
pub mod privacy_consent;
pub mod schema_meta;
pub mod sessions;
pub mod settings;
pub mod traits;

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

/// 获取最近一次 INSERT 产生的自增 ID。
///
/// 用法:
/// - INSERT 语句执行成功后调用，获取 AUTOINCREMENT 主键值。
///
/// 返回:
/// - 成功时返回自增 ID（>= 1）。
/// - 失败时返回 Storage 错误（非 `unwrap_or(0)` 静默吞错）。
///
/// 说明:
/// - 必须在同一连接上紧接 INSERT 之后调用。
/// - 不适用 TEXT 主键表（sessions/messages/memory_l1）。
pub(crate) async fn last_insert_id(pool: &SqlitePool) -> RamariaResult<i64> {
    sqlx::query_scalar::<_, i64>("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("获取自增 ID 失败——INSERT 可能未生效", e))
}
