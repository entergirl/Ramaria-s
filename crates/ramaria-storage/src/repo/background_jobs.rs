//! rust/crates/ramaria-storage/src/repo/background_jobs.rs - 后台任务管理
//!
//! 设计特点:
//! - 管理 L1→L2 事件提取、索引重建等后台异步任务的状态
//! - status 默认 'pending'，完成时更新为 'done'/'failed'
//! - 支持重试计数（DDL 层控制，max_retries=3）

use crate::repo::StorageResultExt;
use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

pub async fn create(
    pool: &SqlitePool,
    job_type: &str,
    payload: Option<&str>,
) -> RamariaResult<i64> {
    let now = ramaria_core::types::now_ms();
    // 使用 RETURNING 子句替代 last_insert_rowid：
    // last_insert_rowid 是 per-connection 的，连接池中不同连接可能拿到 0。
    // RETURNING 在同一条 SQL 中返回自增 ID，不受连接池影响。
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO background_jobs (job_type, status, payload, created_at) VALUES (?, 'pending', ?, ?) RETURNING id"
    )
        .bind(job_type)
        .bind(payload)
        .bind(now)
        .fetch_one(pool)
        .await
        .storage_err("创建后台任务失败")
}

pub async fn update_status(
    pool: &SqlitePool,
    id: i64,
    status: &str,
    error: Option<&str>,
) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    let result = sqlx::query(
        "UPDATE background_jobs SET status = ?, finished_at = ?, error = ? WHERE id = ?",
    )
    .bind(status)
    .bind(now)
    .bind(error)
    .bind(id)
    .execute(pool)
    .await
    .storage_err("更新后台任务失败")?;

    // 防御性检查：确保目标 job 确实存在
    if result.rows_affected() == 0 {
        return Err(RamariaError::storage(format!(
            "后台任务不存在 (id={id})，无法更新状态为 {status}"
        )));
    }

    tracing::info!(
        job_id = id,
        %status,
        error = error.unwrap_or(""),
        "后台任务状态已更新"
    );

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
        .storage_err("查询待处理任务失败")?;
    Ok(rows
        .into_iter()
        .map(|r| (r.id, r.job_type, r.payload))
        .collect())
}
