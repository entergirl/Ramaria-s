//! rust/crates/ramaria-storage/src/repo/schema_meta.rs - Schema 元信息读写
//!
//! 设计特点:
//! - 管理 schema_version 和 index_version 两个元数据键
//! - 使用 key-value 模式，支持未来扩展更多元信息键
//! - 版本号使用 i32 存储，与 sqlx migrate 版本号一致

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

/// 获取 schema 版本号。
///
/// 返回:
/// - schema_version 整数，不存在时返回错误。
pub async fn get_schema_version(pool: &SqlitePool) -> RamariaResult<i32> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM schema_meta WHERE key = 'schema_version'")
            .fetch_optional(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("读取 schema_version 失败", e))?;

    let value = row.ok_or_else(|| RamariaError::storage("schema_version 条目缺失"))?;
    value
        .0
        .parse::<i32>()
        .map_err(|e| RamariaError::storage_with_source("schema_version 值非法", e))
}

/// 获取索引版本号。
///
/// 返回:
/// - index_version 整数，不存在时返回 0。
pub async fn get_index_version(pool: &SqlitePool) -> RamariaResult<i32> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM schema_meta WHERE key = 'index_version'")
            .fetch_optional(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("读取 index_version 失败", e))?;

    match row {
        Some((v,)) => v
            .parse::<i32>()
            .map_err(|e| RamariaError::storage_with_source("index_version 值非法", e)),
        None => Ok(0),
    }
}

/// 更新索引版本号。
///
/// 参数:
/// - `version`: 新索引版本号。
pub async fn set_index_version(pool: &SqlitePool, version: i32) -> RamariaResult<()> {
    sqlx::query("INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('index_version', ?)")
        .bind(version.to_string())
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("更新 index_version 失败", e))?;
    Ok(())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::test_pool;

    #[tokio::test]
    async fn schema_version_is_readable() {
        let pool = test_pool().await.unwrap();
        let version = get_schema_version(&pool).await.unwrap();
        assert_eq!(version, 1);
    }

    #[tokio::test]
    async fn index_version_defaults_to_zero() {
        let pool = test_pool().await.unwrap();
        let version = get_index_version(&pool).await.unwrap();
        assert_eq!(version, 0);
    }

    #[tokio::test]
    async fn set_and_get_index_version() {
        let pool = test_pool().await.unwrap();

        set_index_version(&pool, 5).await.unwrap();
        assert_eq!(get_index_version(&pool).await.unwrap(), 5);

        set_index_version(&pool, 42).await.unwrap();
        assert_eq!(get_index_version(&pool).await.unwrap(), 42);
    }
}
