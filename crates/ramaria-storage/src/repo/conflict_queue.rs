//! rust/crates/ramaria-storage/src/repo/conflict_queue.rs - 冲突检测队列

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

pub async fn create(
    pool: &SqlitePool,
    field: &str,
    conflict_type: &str,
    old_content: Option<&str>,
    new_content: Option<&str>,
    desc: Option<&str>,
) -> RamariaResult<i64> {
    let now = ramaria_core::types::now_ms();
    sqlx::query(
        "INSERT INTO conflict_queue (field, conflict_type, old_content, new_content, conflict_desc, created_at)
         VALUES (?, ?, ?, ?, ?, ?)"
    ).bind(field).bind(conflict_type).bind(old_content).bind(new_content).bind(desc).bind(now)
        .execute(pool).await
        .map_err(|e| RamariaError::storage_with_source("创建冲突记录失败", e))?;
    let id = sqlx::query_scalar::<_, i64>("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    Ok(id)
}

pub async fn list_pending(pool: &SqlitePool) -> RamariaResult<Vec<(i64, String, String, String)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        field: String,
        conflict_type: String,
        conflict_desc: String,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, field, conflict_type, COALESCE(conflict_desc, '') as conflict_desc FROM conflict_queue WHERE status = 'pending'"
    ).fetch_all(pool).await
        .map_err(|e| RamariaError::storage_with_source("查询待解决冲突失败", e))?;
    Ok(rows
        .into_iter()
        .map(|r| (r.id, r.field, r.conflict_type, r.conflict_desc))
        .collect())
}

pub async fn resolve(pool: &SqlitePool, id: i64) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("UPDATE conflict_queue SET status = 'resolved', resolved_at = ? WHERE id = ?")
        .bind(now)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("解决冲突失败", e))?;
    Ok(())
}
