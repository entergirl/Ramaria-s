//! rust/crates/ramaria-storage/src/repo/background_jobs.rs - 后台任务管理
//!
//! 设计特点:
//! - 支持 pending/running/failed/done 四种状态
//! - 支持重试计数和最大重试次数
//! - 按状态和类型查询，供后台任务调度器使用

use ramaria_core::error::{RamariaError, RamariaResult};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

/// 后台任务状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    Pending,
    Running,
    Failed,
    Done,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Failed => "failed",
            Self::Done => "done",
        }
    }
}

/// 后台任务记录。
#[derive(Debug, Clone)]
pub struct BackgroundJob {
    pub id: Uuid,
    pub job_type: String,
    pub status: JobStatus,
    pub payload_json: Option<String>,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub retry_count: i32,
    pub max_retries: i32,
}

impl BackgroundJob {
    /// 创建一条新的 pending 任务。
    pub fn new(job_type: impl Into<String>, payload_json: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            job_type: job_type.into(),
            status: JobStatus::Pending,
            payload_json,
            error_message: None,
            created_at: ramaria_core::types::now_ms(),
            started_at: None,
            finished_at: None,
            retry_count: 0,
            max_retries: 3,
        }
    }
}

// =========================================================
// CRUD
// =========================================================

/// 保存后台任务。
pub async fn save_job(pool: &SqlitePool, job: &BackgroundJob) -> RamariaResult<()> {
    sqlx::query(
        "INSERT INTO background_jobs (id, job_type, status, payload_json, error_message, \
         created_at, started_at, finished_at, retry_count, max_retries) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(job.id.to_string())
    .bind(&job.job_type)
    .bind(job.status.as_str())
    .bind(&job.payload_json)
    .bind(&job.error_message)
    .bind(job.created_at)
    .bind(job.started_at)
    .bind(job.finished_at)
    .bind(job.retry_count)
    .bind(job.max_retries)
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("保存后台任务失败", e))?;
    Ok(())
}

/// 查询某状态的 pending 任务列表。
pub async fn list_pending_jobs(
    pool: &SqlitePool,
    job_type: &str,
) -> RamariaResult<Vec<BackgroundJob>> {
    let rows = sqlx::query(
        "SELECT id, job_type, status, payload_json, error_message, \
         created_at, started_at, finished_at, retry_count, max_retries \
         FROM background_jobs WHERE job_type = ? AND status = 'pending' ORDER BY created_at ASC",
    )
    .bind(job_type)
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("列出 pending 任务失败", e))?;

    rows.iter().map(row_to_job).collect()
}

/// 更新任务状态。
pub async fn update_job_status(
    pool: &SqlitePool,
    job_id: Uuid,
    status: JobStatus,
    error_message: Option<&str>,
) -> RamariaResult<()> {
    let now_ms = ramaria_core::types::now_ms();
    match status {
        JobStatus::Running => {
            sqlx::query(
                "UPDATE background_jobs SET status = ?, started_at = ?, error_message = ? WHERE id = ?",
            )
            .bind(status.as_str())
            .bind(now_ms)
            .bind(error_message)
            .bind(job_id.to_string())
            .execute(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("更新任务状态为 running 失败", e))?;
        }
        JobStatus::Done | JobStatus::Failed => {
            sqlx::query(
                "UPDATE background_jobs SET status = ?, finished_at = ?, error_message = ? WHERE id = ?",
            )
            .bind(status.as_str())
            .bind(now_ms)
            .bind(error_message)
            .bind(job_id.to_string())
            .execute(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("更新任务状态为 done/failed 失败", e))?;
        }
        JobStatus::Pending => {
            // 重置为 pending
            sqlx::query(
                "UPDATE background_jobs SET status = 'pending', started_at = NULL, \
                 finished_at = NULL, error_message = ? WHERE id = ?",
            )
            .bind(error_message)
            .bind(job_id.to_string())
            .execute(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("重置任务状态失败", e))?;
        }
    }
    Ok(())
}

/// 递增重试次数。
pub async fn increment_retry(pool: &SqlitePool, job_id: Uuid) -> RamariaResult<()> {
    sqlx::query("UPDATE background_jobs SET retry_count = retry_count + 1 WHERE id = ?")
        .bind(job_id.to_string())
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("增加重试次数失败", e))?;
    Ok(())
}

// =========================================================
// 行映射
// =========================================================

fn row_to_job(row: &sqlx::sqlite::SqliteRow) -> RamariaResult<BackgroundJob> {
    let id_str: String = row.get("id");
    let status_str: String = row.get("status");

    let id = Uuid::parse_str(&id_str)
        .map_err(|e| RamariaError::storage_with_source("job ID 格式非法", e))?;

    let status = match status_str.as_str() {
        "pending" => JobStatus::Pending,
        "running" => JobStatus::Running,
        "failed" => JobStatus::Failed,
        "done" => JobStatus::Done,
        other => {
            return Err(RamariaError::storage(format!("未知 job 状态: {other}")));
        }
    };

    Ok(BackgroundJob {
        id,
        job_type: row.get("job_type"),
        status,
        payload_json: row.get("payload_json"),
        error_message: row.get("error_message"),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
        retry_count: row.get("retry_count"),
        max_retries: row.get("max_retries"),
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::test_pool;

    #[tokio::test]
    async fn create_and_list_pending_jobs() {
        let pool = test_pool().await.unwrap();

        let job = BackgroundJob::new("l1_summary", None);
        save_job(&pool, &job).await.unwrap();

        let pending = list_pending_jobs(&pool, "l1_summary").await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].job_type, "l1_summary");
    }

    #[tokio::test]
    async fn job_lifecycle() {
        let pool = test_pool().await.unwrap();

        let job = BackgroundJob::new("index_rebuild", None);
        save_job(&pool, &job).await.unwrap();

        // pending -> running
        update_job_status(&pool, job.id, JobStatus::Running, None)
            .await
            .unwrap();

        // running -> done
        update_job_status(&pool, job.id, JobStatus::Done, None)
            .await
            .unwrap();

        // 不应再有 pending 任务
        let pending = list_pending_jobs(&pool, "index_rebuild").await.unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn job_with_error_message() {
        let pool = test_pool().await.unwrap();

        let job = BackgroundJob::new("l1_summary", None);
        save_job(&pool, &job).await.unwrap();

        update_job_status(&pool, job.id, JobStatus::Failed, Some("LLM 调用超时"))
            .await
            .unwrap();

        let pending = list_pending_jobs(&pool, "l1_summary").await.unwrap();
        // 失败的任务不在 pending 列表中
        assert!(pending.is_empty());
    }
}
