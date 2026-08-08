//! rust/crates/ramaria-storage/src/repo/sessions.rs - Session CRUD
//!
//! 设计特点:
//! - 管理对话会话生命周期：创建、关闭、查询、删除
//! - id 使用 UUID v4（TEXT 主键），时间字段为 Unix 毫秒
//! - 新增 persona_uid 字段，支持 Session-Persona 绑定
//! - UUID 解析失败时记录 WARNING 日志

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_required;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::Session;
use sqlx::SqlitePool;
use uuid::Uuid;

/// 创建新 session，可选绑定 persona_uid。
///
/// 参数:
/// - `persona_uid`: 对话人格标识（None 兼容存量调用）。
pub async fn create(pool: &SqlitePool, persona_uid: Option<&str>) -> RamariaResult<Session> {
    let now = ramaria_core::types::now_ms();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at, persona_uid) VALUES (?, ?, ?)")
        .bind(id.to_string())
        .bind(now)
        .bind(persona_uid)
        .execute(pool)
        .await
        .storage_err("创建 session 失败")?;
    Ok(Session {
        id,
        started_at: now,
        ended_at: None,
        persona_uid: persona_uid.map(|s| s.to_string()),
    })
}

pub async fn close(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("UPDATE sessions SET ended_at = ? WHERE id = ?")
        .bind(now)
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .storage_err("关闭 session 失败")?;
    Ok(())
}

/// 回写绑定会话的 persona_uid（存量 NULL 会话归属修复，P0-1）。
///
/// 职责:
/// - 会话创建时未绑定（`persona_uid=NULL`）时，由 resolve_session
///   在发送消息阶段用前端传入的 persona_uid 补绑。
/// - 幂等：已绑定同 uid 时 UPDATE 无副作用；会话不存在时静默成功
///   （调用方不依赖返回行数，防御优先）。
///
/// 参数:
/// - `session_id`: 目标会话 UUID。
/// - `persona_uid`: 要绑定的对话人格 UID。
pub async fn bind_persona_uid(
    pool: &SqlitePool,
    session_id: Uuid,
    persona_uid: &str,
) -> RamariaResult<()> {
    sqlx::query("UPDATE sessions SET persona_uid = ? WHERE id = ?")
        .bind(persona_uid)
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .storage_err("回写 session persona_uid 失败")?;
    Ok(())
}

pub async fn get(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Option<Session>> {
    let row = sqlx::query_as::<_, SessionRow>(
        "SELECT id, started_at, ended_at, persona_uid FROM sessions WHERE id = ?",
    )
    .bind(session_id.to_string())
    .fetch_optional(pool)
    .await
    .storage_err("查询 session 失败")?;
    row.map(|r| r.into_session()).transpose()
}

pub async fn list_active(pool: &SqlitePool) -> RamariaResult<Vec<Session>> {
    let rows = sqlx::query_as::<_, SessionRow>(
        "SELECT id, started_at, ended_at, persona_uid FROM sessions WHERE ended_at IS NULL ORDER BY started_at DESC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询活跃 session 失败")?;
    rows.into_iter()
        .map(|r| r.into_session())
        .collect::<Result<Vec<_>, _>>()
}

pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<Session>> {
    let rows = sqlx::query_as::<_, SessionRow>(
        "SELECT id, started_at, ended_at, persona_uid FROM sessions ORDER BY started_at DESC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询全部 session 失败")?;
    rows.into_iter()
        .map(|r| r.into_session())
        .collect::<Result<Vec<_>, _>>()
}

pub async fn delete(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<()> {
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .storage_err("删除 session 失败")?;
    Ok(())
}

/// 创建一条历史 session（导入专用）。
///
/// 职责:
/// - 与 `create` 不同，此函数使用外部提供的时间戳，而非当前时间。
/// - 创建时即设置 `ended_at`，表示这是一个已完成的历史会话。
/// - 供 ramaria-importer 在快速/深度导入模式中使用。
///
/// 参数:
/// - `started_at`: Session 开始时间（Unix 毫秒）。
/// - `ended_at`: Session 结束时间（Unix 毫秒）。
/// - `persona_uid`: 导入会话必须绑定人格，否则 SessionDrawer
///   按 persona 筛选时 NULL 会话被错误归类到默认人格 rama-0001。
///
/// 返回:
/// - 带指定时间范围、已关闭的 Session。
pub async fn create_historical(
    pool: &SqlitePool,
    started_at: i64,
    ended_at: i64,
    persona_uid: &str,
) -> RamariaResult<Session> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at, ended_at, persona_uid) VALUES (?, ?, ?, ?)")
        .bind(id.to_string())
        .bind(started_at)
        .bind(ended_at)
        .bind(persona_uid)
        .execute(pool)
        .await
        .storage_err("创建历史 session 失败")?;
    Ok(Session {
        id,
        started_at,
        ended_at: Some(ended_at),
        persona_uid: Some(persona_uid.to_string()),
    })
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    started_at: i64,
    ended_at: Option<i64>,
    persona_uid: Option<String>,
}

impl SessionRow {
    fn into_session(self) -> RamariaResult<Session> {
        let id = parse_uuid_required(&self.id, "sessions", "id")?;
        Ok(Session {
            id,
            started_at: self.started_at,
            ended_at: self.ended_at,
            persona_uid: self.persona_uid,
        })
    }
}
