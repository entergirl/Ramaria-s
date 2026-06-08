//! rust/crates/ramaria-storage/src/repo/background_jobs.rs - 后台任务管理

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

pub async fn create(
    pool: &SqlitePool,
    job_type: &str,
    payload: Option<&str>,
) -> RamariaResult<i64> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("INSERT INTO background_jobs (job_type, status, payload, created_at) VALUES (?, 'pending', ?, ?)")
        .bind(job_type).bind(payload).bind(now).execute(pool).await
        .map_err(|e| RamariaError::storage_with_source("创建后台任务失败", e))?;
    let id = sqlx::query_scalar::<_, i64>("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    Ok(id)
}

pub async fn update_status(
    pool: &SqlitePool,
    id: i64,
    status: &str,
    error: Option<&str>,
) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("UPDATE background_jobs SET status = ?, finished_at = ?, error = ? WHERE id = ?")
        .bind(status)
        .bind(now)
        .bind(error)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("更新后台任务失败", e))?;
    Ok(())
}

pub async fn list_pending(pool: &SqlitePool) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        job_type: String,
        payload: Option<String>,
    }
    let rows = sqlx::query_as::<_, Row>("SELECT id, job_type, payload FROM background_jobs WHERE status = 'pending' ORDER BY created_at")
        .fetch_all(pool).await
        .map_err(|e| RamariaError::storage_with_source("查询待处理任务失败", e))?;
    Ok(rows
        .into_iter()
        .map(|r| (r.id, r.job_type, r.payload))
        .collect())
}
