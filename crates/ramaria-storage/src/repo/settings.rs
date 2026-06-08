//! rust/crates/ramaria-storage/src/repo/settings.rs - 全局设置 key-value

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

pub async fn get(pool: &SqlitePool, key: &str) -> RamariaResult<Option<String>> {
    let val: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("查询设置失败", e))?;
    Ok(val)
}

pub async fn set(pool: &SqlitePool, key: &str, value: &str) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("INSERT OR REPLACE INTO settings (key, value, updated_at) VALUES (?, ?, ?)")
        .bind(key)
        .bind(value)
        .bind(now)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("保存设置失败", e))?;
    Ok(())
}

pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<(String, String)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        key: String,
        value: String,
    }
    let rows = sqlx::query_as::<_, Row>("SELECT key, value FROM settings ORDER BY key")
        .fetch_all(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("查询设置列表失败", e))?;
    Ok(rows.into_iter().map(|r| (r.key, r.value)).collect())
}
