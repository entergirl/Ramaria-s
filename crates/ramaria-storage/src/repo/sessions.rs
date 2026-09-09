//! crates/ramaria-storage/src/repo/sessions.rs - Session CRUD
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

/// 回写绑定会话的 persona_uid（存量 NULL 会话归属修复）。
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

/// 级联删除 session 及其全部关联数据（消息 / utt 块 / L1 / 反馈日志 / 示例）。
///
/// 职责:
/// - 在单个 SQLite 事务内按外键依赖顺序删除，保证整体成功或整体回滚：
///   1. utt_blocks（引用 messages 与 sessions）
///   2. messages（引用 sessions；显式删除保证外键开关状态下均幂等）
///   3. memory_l1（引用 sessions）
///   4. persona_examples（引用 sessions）
///   5. feedback_log（无外键，仅清理该 session 的审计残留）
///   6. sessions 本体
/// - 供 `StoreCrud::delete_session_cascade`（一次性合成会话清理）使用；
///   普通会话删除仍走 [`delete`]，不触碰子表。
pub async fn delete_cascade(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<()> {
    let sid = session_id.to_string();
    let mut txn = pool.begin().await.storage_err("开启级联删除事务失败")?;
    sqlx::query("DELETE FROM utt_blocks WHERE session_id = ?")
        .bind(&sid)
        .execute(&mut *txn)
        .await
        .storage_err("级联删除 utt_blocks 失败")?;
    sqlx::query("DELETE FROM messages WHERE session_id = ?")
        .bind(&sid)
        .execute(&mut *txn)
        .await
        .storage_err("级联删除 messages 失败")?;
    sqlx::query("DELETE FROM memory_l1 WHERE session_id = ?")
        .bind(&sid)
        .execute(&mut *txn)
        .await
        .storage_err("级联删除 memory_l1 失败")?;
    sqlx::query("DELETE FROM persona_examples WHERE session_id = ?")
        .bind(&sid)
        .execute(&mut *txn)
        .await
        .storage_err("级联删除 persona_examples 失败")?;
    sqlx::query("DELETE FROM feedback_log WHERE session_id = ?")
        .bind(&sid)
        .execute(&mut *txn)
        .await
        .storage_err("级联删除 feedback_log 失败")?;
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(&sid)
        .execute(&mut *txn)
        .await
        .storage_err("删除 session 失败")?;
    txn.commit().await.storage_err("提交级联删除事务失败")?;
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

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::init_test_pool;
    use ramaria_core::types::{Message, MessageRole, MessageSource};

    /// 插入 persona + session + 消息 + utt 块 fixture（验证级联删除的外键依赖顺序）。
    ///
    /// 返回 session_id。utt_blocks 同时引用 messages（start/end_msg_id）与 sessions，
    /// 若不先删 utt_blocks 直接删 session 会被外键约束拒绝。
    async fn setup_fixture(pool: &SqlitePool) -> Uuid {
        sqlx::query(
            "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
             VALUES ('char-0001', '测试', 'char', 1, 'local', 0, 0)",
        )
        .execute(pool)
        .await
        .expect("插入 persona fixture 应成功");

        let session = create(pool, Some("char-0001"))
            .await
            .expect("创建 session 成功");
        let m1 = Message::new(
            session.id,
            MessageRole::User,
            "问题一".to_string(),
            MessageSource::Local,
        );
        let m2 = Message::new(
            session.id,
            MessageRole::Assistant,
            "回复一".to_string(),
            MessageSource::Online,
        )
        .with_persona_uid(Some("char-0001".to_string()));
        crate::repo::messages::save_import(pool, &m1)
            .await
            .expect("插入消息 1 成功");
        crate::repo::messages::save_import(pool, &m2)
            .await
            .expect("插入消息 2 成功");

        sqlx::query(
            "INSERT INTO utt_blocks \
             (persona_uid, session_id, start_msg_id, end_msg_id, block_text, msg_count, \
              time_span_ms, embedding, created_at) \
             VALUES ('char-0001', ?, ?, ?, '测试话语块', 2, 1000, NULL, 0)",
        )
        .bind(session.id.to_string())
        .bind(m1.id.to_string())
        .bind(m2.id.to_string())
        .execute(pool)
        .await
        .expect("插入 utt_blocks fixture 应成功");

        session.id
    }

    /// 级联删除应移除 session 及其消息与 utt 块（utt 块引用顺序正确、无外键残留）。
    #[tokio::test]
    async fn delete_cascade_removes_session_and_dependents() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let session_id = setup_fixture(&pool).await;

        // 前置：session、2 条消息、1 个 utt 块均存在
        assert!(get(&pool, session_id).await.unwrap().is_some());
        assert_eq!(
            crate::repo::messages::count_by_session(&pool, session_id)
                .await
                .unwrap(),
            2
        );

        delete_cascade(&pool, session_id)
            .await
            .expect("级联删除成功");

        assert!(
            get(&pool, session_id).await.unwrap().is_none(),
            "session 应已删除"
        );
        assert_eq!(
            crate::repo::messages::count_by_session(&pool, session_id)
                .await
                .unwrap(),
            0,
            "session 消息应已级联删除"
        );
        let utt_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM utt_blocks WHERE session_id = ?")
                .bind(session_id.to_string())
                .fetch_one(&pool)
                .await
                .expect("查询 utt_blocks 数量成功");
        assert_eq!(utt_count.0, 0, "session 的 utt 块应已删除");
    }

    /// 删除不存在的 session 幂等成功（不报错）。
    #[tokio::test]
    async fn delete_cascade_missing_session_is_idempotent() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        delete_cascade(&pool, Uuid::new_v4())
            .await
            .expect("删除不存在的 session 应幂等成功");
    }
}
