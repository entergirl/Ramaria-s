//! rust/crates/ramaria-storage/src/repo/schema_meta.rs - Schema/Index 版本管理

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

pub async fn get_schema_version(pool: &SqlitePool) -> RamariaResult<i32> {
    let val: String =
        sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = 'schema_version'")
            .fetch_optional(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("查询 schema 版本失败", e))?
            .unwrap_or_else(|| "1".to_string());
    val.parse()
        .map_err(|_| RamariaError::storage("schema_version 值非法"))
}

pub async fn get_index_version(pool: &SqlitePool) -> RamariaResult<i32> {
    let val: String =
        sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = 'index_version'")
            .fetch_optional(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("查询索引版本失败", e))?
            .unwrap_or_else(|| "1".to_string());
    val.parse()
        .map_err(|_| RamariaError::storage("index_version 值非法"))
}

pub async fn set_index_version(pool: &SqlitePool, version: i32) -> RamariaResult<()> {
    sqlx::query("INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('index_version', ?)")
        .bind(version.to_string())
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("更新索引版本失败", e))?;
    Ok(())
}
