//! rust/crates/ramaria-storage/src/repo/keyword.rs - KeywordPool CRUD

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

pub async fn upsert(pool: &SqlitePool, keyword: &str) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query(
        "INSERT INTO keyword_pool (keyword, use_count, last_used_at, created_at) VALUES (?, 1, ?, ?)
         ON CONFLICT(keyword) DO UPDATE SET use_count = use_count + 1, last_used_at = ?"
    ).bind(keyword).bind(now).bind(now).bind(now)
        .execute(pool).await
        .map_err(|e| RamariaError::storage_with_source("upsert 关键词失败", e))?;
    Ok(())
}

pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<String>> {
    let rows =
        sqlx::query_scalar::<_, String>("SELECT keyword FROM keyword_pool ORDER BY use_count DESC")
            .fetch_all(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("查询关键词列表失败", e))?;
    Ok(rows)
}
