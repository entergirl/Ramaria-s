//! rust/crates/ramaria-storage/src/repo/settings.rs - 全局运行配置存取模块
//!
//! 设计特点:
//! - key-value 结构，value 统一存储为 TEXT
//! - set 使用 INSERT OR REPLACE 确保幂等写入，自动刷新 updated_at
//! - 配置项包括 profile_mode、l2_trigger_count、push_enabled 等运行时参数
//! - 不存储敏感信息（API key 等），仅保存非敏感运行参数

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use sqlx::SqlitePool;

pub async fn get(pool: &SqlitePool, key: &str) -> RamariaResult<Option<String>> {
    let val: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .storage_err("查询设置失败")?;
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
        .storage_err("保存设置失败")?;
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
        .storage_err("查询设置列表失败")?;
    Ok(rows.into_iter().map(|r| (r.key, r.value)).collect())
}
