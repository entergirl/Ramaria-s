//! rust/crates/ramaria-storage/src/repo/sessions.rs - Session CRUD

use ramaria_core::error::RamariaResult;
use ramaria_core::types::Session;
use sqlx::SqlitePool;
use uuid::Uuid;

pub async fn create(pool: &SqlitePool) -> RamariaResult<Session> {
    let now = ramaria_core::types::now_ms();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, ?)")
        .bind(id.to_string())
        .bind(now)
        .execute(pool)
        .await
        .map_err(|e| {
            ramaria_core::error::RamariaError::storage_with_source("创建 session 失败", e)
        })?;
    Ok(Session {
        id,
        started_at: now,
        ended_at: None,
    })
}

pub async fn close(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("UPDATE sessions SET ended_at = ? WHERE id = ?")
        .bind(now)
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .map_err(|e| {
            ramaria_core::error::RamariaError::storage_with_source("关闭 session 失败", e)
        })?;
    Ok(())
}

pub async fn get(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Option<Session>> {
    let row = sqlx::query_as::<_, SessionRow>(
        "SELECT id, started_at, ended_at FROM sessions WHERE id = ?",
    )
    .bind(session_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(|e| ramaria_core::error::RamariaError::storage_with_source("查询 session 失败", e))?;
    row.map(|r| r.into_session()).transpose()
}

pub async fn list_active(pool: &SqlitePool) -> RamariaResult<Vec<Session>> {
    let rows = sqlx::query_as::<_, SessionRow>("SELECT id, started_at, ended_at FROM sessions WHERE ended_at IS NULL ORDER BY started_at DESC")
        .fetch_all(pool)
        .await
        .map_err(|e| ramaria_core::error::RamariaError::storage_with_source("查询活跃 session 失败", e))?;
    rows.into_iter()
        .map(|r| r.into_session())
        .collect::<Result<Vec<_>, _>>()
}

pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<Session>> {
    let rows = sqlx::query_as::<_, SessionRow>(
        "SELECT id, started_at, ended_at FROM sessions ORDER BY started_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| {
        ramaria_core::error::RamariaError::storage_with_source("查询全部 session 失败", e)
    })?;
    rows.into_iter()
        .map(|r| r.into_session())
        .collect::<Result<Vec<_>, _>>()
}

pub async fn delete(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<()> {
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .map_err(|e| {
            ramaria_core::error::RamariaError::storage_with_source("删除 session 失败", e)
        })?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    started_at: i64,
    ended_at: Option<i64>,
}

impl SessionRow {
    fn into_session(self) -> RamariaResult<Session> {
        Ok(Session {
            id: ramaria_core::types::uuid_from_db(&self.id),
            started_at: self.started_at,
            ended_at: self.ended_at,
        })
    }
}
