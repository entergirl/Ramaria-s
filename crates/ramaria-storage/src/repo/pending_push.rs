//! rust/crates/ramaria-storage/src/repo/pending_push.rs - 主动推送暂存

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

pub async fn create(pool: &SqlitePool, content: &str) -> RamariaResult<i64> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("INSERT INTO pending_push (content, created_at) VALUES (?, ?)")
        .bind(content)
        .bind(now)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("创建推送失败", e))?;
    let id = sqlx::query_scalar::<_, i64>("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    Ok(id)
}

pub async fn list_pending(pool: &SqlitePool) -> RamariaResult<Vec<(i64, String)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        content: String,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, content FROM pending_push WHERE status = 'pending' ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询待推送失败", e))?;
    Ok(rows.into_iter().map(|r| (r.id, r.content)).collect())
}

pub async fn mark_sent(pool: &SqlitePool, id: i64) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("UPDATE pending_push SET status = 'sent', sent_at = ? WHERE id = ?")
        .bind(now)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("标记推送已发送失败", e))?;
    Ok(())
}
