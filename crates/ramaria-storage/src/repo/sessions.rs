//! rust/crates/ramaria-storage/src/repo/sessions.rs - Session 生命周期 CRUD
//!
//! 设计特点:
//! - 创建、关闭、查询、列表、删除五项基础操作
//! - close_session 幂等：已关闭的 session 不刷新 ended_at
//! - delete_session 级联删除关联消息（应用层维护）

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::Session;
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

/// 创建新 session。
pub async fn create_session(pool: &SqlitePool, session: &Session) -> RamariaResult<()> {
    sqlx::query("INSERT INTO sessions (id, started_at, ended_at) VALUES (?, ?, ?)")
        .bind(session.id.to_string())
        .bind(session.started_at)
        .bind(session.ended_at)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("创建 session 失败", e))?;
    Ok(())
}

/// 关闭 session（幂等）。
///
/// 说明:
/// - 若 session 已关闭（ended_at IS NOT NULL），此操作不更新任何数据。
pub async fn close_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<()> {
    let now_ms = ramaria_core::types::now_ms();
    let rows = sqlx::query("UPDATE sessions SET ended_at = ? WHERE id = ? AND ended_at IS NULL")
        .bind(now_ms)
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("关闭 session 失败", e))?;

    if rows.rows_affected() == 0 {
        // session 不存在或已关闭，不视为错误
        return Ok(());
    }
    Ok(())
}

/// 获取单个 session。
pub async fn get_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Option<Session>> {
    let row = sqlx::query("SELECT id, started_at, ended_at FROM sessions WHERE id = ?")
        .bind(session_id.to_string())
        .fetch_optional(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("查询 session 失败", e))?;

    match row {
        Some(r) => Ok(Some(row_to_session(&r)?)),
        None => Ok(None),
    }
}

/// 列出活跃 session（ended_at IS NULL）。
pub async fn list_active_sessions(pool: &SqlitePool) -> RamariaResult<Vec<Session>> {
    let rows = sqlx::query(
        "SELECT id, started_at, ended_at FROM sessions WHERE ended_at IS NULL ORDER BY started_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("列出活跃 session 失败", e))?;

    rows.iter().map(row_to_session).collect()
}

/// 列出所有 session。
pub async fn list_sessions(pool: &SqlitePool) -> RamariaResult<Vec<Session>> {
    let rows =
        sqlx::query("SELECT id, started_at, ended_at FROM sessions ORDER BY started_at DESC")
            .fetch_all(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("列出 session 失败", e))?;

    rows.iter().map(row_to_session).collect()
}

/// 删除 session 及关联消息。
pub async fn delete_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<()> {
    let sid = session_id.to_string();

    // 先删除关联消息
    sqlx::query("DELETE FROM messages WHERE session_id = ?")
        .bind(&sid)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("删除 session 关联消息失败", e))?;

    // 再删除 session
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(&sid)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("删除 session 失败", e))?;

    Ok(())
}

// =========================================================
// 行映射
// =========================================================

fn row_to_session(row: &sqlx::sqlite::SqliteRow) -> RamariaResult<Session> {
    let id_str: String = row.get("id");
    let id = Uuid::parse_str(&id_str)
        .map_err(|e| RamariaError::storage_with_source("session ID 格式非法", e))?;
    Ok(Session {
        id,
        started_at: row.get("started_at"),
        ended_at: row.get("ended_at"),
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
    async fn create_and_get_session() {
        let pool = test_pool().await.unwrap();
        let session = Session::new();

        create_session(&pool, &session).await.unwrap();

        let fetched = get_session(&pool, session.id).await.unwrap();
        assert!(fetched.is_some());
        let s = fetched.unwrap();
        assert_eq!(s.id, session.id);
        assert_eq!(s.started_at, session.started_at);
        assert!(s.is_active());
    }

    #[tokio::test]
    async fn close_session_is_idempotent() {
        let pool = test_pool().await.unwrap();
        let session = Session::new();

        create_session(&pool, &session).await.unwrap();

        // 第一次关闭
        close_session(&pool, session.id).await.unwrap();
        let s1 = get_session(&pool, session.id).await.unwrap().unwrap();
        assert!(s1.ended_at.is_some());
        let first_close = s1.ended_at.unwrap();

        // 第二次关闭（幂等）
        close_session(&pool, session.id).await.unwrap();
        let s2 = get_session(&pool, session.id).await.unwrap().unwrap();
        assert_eq!(s2.ended_at, Some(first_close));
    }

    #[tokio::test]
    async fn list_active_sessions_filters_correctly() {
        let pool = test_pool().await.unwrap();

        let s1 = Session::new();
        let s2 = Session::new();
        create_session(&pool, &s1).await.unwrap();
        create_session(&pool, &s2).await.unwrap();

        // 关闭 s1
        close_session(&pool, s1.id).await.unwrap();

        let active = list_active_sessions(&pool).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, s2.id);
    }

    #[tokio::test]
    async fn delete_session_removes_messages() {
        let pool = test_pool().await.unwrap();
        let session = Session::new();
        create_session(&pool, &session).await.unwrap();

        // 插入一条消息
        let msg = ramaria_core::types::Message::new(
            session.id,
            ramaria_core::types::MessageRole::User,
            "测试消息".into(),
            ramaria_core::types::MessageSource::Local,
        );
        crate::repo::messages::save_message(&pool, &msg)
            .await
            .unwrap();

        // 删除 session
        delete_session(&pool, session.id).await.unwrap();

        // 确认 session 被删除
        assert!(get_session(&pool, session.id).await.unwrap().is_none());

        // 确认消息也被删除
        let msgs = crate::repo::messages::list_messages(&pool, session.id)
            .await
            .unwrap();
        assert!(msgs.is_empty());
    }
}
