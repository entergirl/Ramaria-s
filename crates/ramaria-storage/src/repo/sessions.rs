//! crates/ramaria-storage/src/repo/sessions.rs - Session CRUD
//!
//! 设计特点:
//! - 管理对话会话生命周期：创建、关闭（含抢占式条件关闭）、查询、删除
//! - id 使用 UUID v4（TEXT 主键），时间字段为 Unix 毫秒
//! - persona_uid 支持 Session-Persona 绑定；channel / external_ref 记录来源通道与外部对话标识
//! - 通道查询按 `(channel, external_ref)` 联合索引定位活跃会话（外部入口续写）
//! - UUID 解析失败时记录 WARNING 日志

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_required;
use crate::retry::with_busy_retry;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{CHANNEL_LOCAL, Session};
use sqlx::SqlitePool;
use uuid::Uuid;

mod channel;

pub use channel::{ChannelOverview, channel_overview, create_in_channel, find_active_by_channel};

/// 列表中查询会话的列清单（与 [`SessionRow`] 字段顺序一致）。
const SESSION_COLUMNS: &str = "id, started_at, ended_at, persona_uid, channel, external_ref";

/// 创建新 session（本地通道），可选绑定 persona_uid。
///
/// 参数:
/// - `persona_uid`: 对话人格标识（None 兼容存量调用）。
pub async fn create(pool: &SqlitePool, persona_uid: Option<&str>) -> RamariaResult<Session> {
    let now = ramaria_core::types::now_ms();
    let id = Uuid::new_v4();
    // 多进程写锁争用时有限重试，避免 database is locked 直接失败
    with_busy_retry("创建 session", || async {
        sqlx::query(
            "INSERT INTO sessions (id, started_at, persona_uid, channel) VALUES (?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(now)
        .bind(persona_uid)
        .bind(CHANNEL_LOCAL)
        .execute(pool)
        .await
    })
    .await
    .storage_err("创建 session 失败")?;
    Ok(Session {
        id,
        started_at: now,
        ended_at: None,
        persona_uid: persona_uid.map(|s| s.to_string()),
        channel: CHANNEL_LOCAL.to_string(),
        external_ref: None,
    })
}

/// 查询指定 persona 的最近对话时间（该 persona 会话中的最大消息时间）。
///
/// 口径:
/// - 会话归属以 `sessions.persona_uid` 为准（会话创建时绑定），取会话内消息
///   `MAX(created_at)`；用户消息（`messages.persona_uid IS NULL`）同样计入
///   "最近对话"。
/// - 会话存在但无消息时不计入；该 persona 无任何消息时返回 None（不视为错误）。
///
/// 参数:
/// - `persona_uid`: 人格标识。
///
/// 返回:
/// - `Ok(Some(ms))`: 最近一条消息的 Unix 毫秒时间戳。
/// - `Ok(None)`: 无对话历史。
pub async fn last_message_time_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Option<i64>> {
    // SQLite MAX 聚合在无匹配行时返回 NULL，使用 Option<i64> 安全解码
    #[derive(sqlx::FromRow)]
    struct LastTimeRow {
        max_time: Option<i64>,
    }

    let row: Option<LastTimeRow> = sqlx::query_as(
        "SELECT MAX(m.created_at) AS max_time \
         FROM messages m JOIN sessions s ON s.id = m.session_id \
         WHERE s.persona_uid = ?",
    )
    .bind(persona_uid)
    .fetch_optional(pool)
    .await
    .storage_err("查询 persona 最近对话时间失败")?;

    Ok(row.and_then(|r| r.max_time))
}

/// 条件更新抢占式关闭 session（幂等封存入口）。
///
/// 职责:
/// - 多进程 / 多线程同时封存同一会话时，仅一个调用方"抢到"关闭权；
///   抢到者继续生成 L1 摘要，未抢到者直接返回。
///
/// 返回:
/// - `Ok(true)`: 本次调用完成了关闭（`ended_at` 由 NULL 变为当前时间）。
/// - `Ok(false)`: 会话已关闭或不存在（未抢占到）。
pub async fn close_if_active(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<bool> {
    let now = ramaria_core::types::now_ms();
    // 多进程写锁争用时有限重试，避免 database is locked 直接失败
    let result = with_busy_retry("条件关闭 session", || async {
        sqlx::query("UPDATE sessions SET ended_at = ? WHERE id = ? AND ended_at IS NULL")
            .bind(now)
            .bind(session_id.to_string())
            .execute(pool)
            .await
    })
    .await
    .storage_err("条件关闭 session 失败")?;
    Ok(result.rows_affected() > 0)
}

pub async fn close(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    // 多进程写锁争用时有限重试，避免 database is locked 直接失败
    with_busy_retry("关闭 session", || async {
        sqlx::query("UPDATE sessions SET ended_at = ? WHERE id = ?")
            .bind(now)
            .bind(session_id.to_string())
            .execute(pool)
            .await
    })
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
    // 多进程写锁争用时有限重试，避免 database is locked 直接失败
    with_busy_retry("回写 session persona_uid", || async {
        sqlx::query("UPDATE sessions SET persona_uid = ? WHERE id = ?")
            .bind(persona_uid)
            .bind(session_id.to_string())
            .execute(pool)
            .await
    })
    .await
    .storage_err("回写 session persona_uid 失败")?;
    Ok(())
}

/// 标记会话已读（把 `last_read_at` 推进到指定时间）。
///
/// 职责:
/// - 会话浏览侧的"已读"写入口：推进后该会话中早于该时间的本地助手消息
///   不再计入未读。
///
/// 参数:
/// - `session_id`: 目标会话 UUID。
/// - `at_ms`: 已读时间戳（Unix 毫秒），由调用方统一取当前时间提供。
///
/// 说明:
/// - 幂等：会话不存在（0 行命中）或重复标记均静默成功。
pub async fn mark_read(pool: &SqlitePool, session_id: Uuid, at_ms: i64) -> RamariaResult<()> {
    // 多进程写锁争用时有限重试，避免 database is locked 直接失败
    with_busy_retry("标记 session 已读", || async {
        sqlx::query("UPDATE sessions SET last_read_at = ? WHERE id = ?")
            .bind(at_ms)
            .bind(session_id.to_string())
            .execute(pool)
            .await
    })
    .await
    .storage_err("标记 session 已读失败")?;
    Ok(())
}

pub async fn get(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Option<Session>> {
    let row = sqlx::query_as::<_, SessionRow>(&format!(
        "SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?"
    ))
    .bind(session_id.to_string())
    .fetch_optional(pool)
    .await
    .storage_err("查询 session 失败")?;
    row.map(|r| r.into_session()).transpose()
}

pub async fn list_active(pool: &SqlitePool) -> RamariaResult<Vec<Session>> {
    let rows = sqlx::query_as::<_, SessionRow>(&format!(
        "SELECT {SESSION_COLUMNS} FROM sessions WHERE ended_at IS NULL ORDER BY started_at DESC"
    ))
    .fetch_all(pool)
    .await
    .storage_err("查询活跃 session 失败")?;
    rows.into_iter()
        .map(|r| r.into_session())
        .collect::<Result<Vec<_>, _>>()
}

pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<Session>> {
    let rows = sqlx::query_as::<_, SessionRow>(&format!(
        "SELECT {SESSION_COLUMNS} FROM sessions ORDER BY started_at DESC"
    ))
    .fetch_all(pool)
    .await
    .storage_err("查询全部 session 失败")?;
    rows.into_iter()
        .map(|r| r.into_session())
        .collect::<Result<Vec<_>, _>>()
}

/// 查询"已关闭且缺 L1 摘要"的本地会话 id 列表（补跑候选）。
///
/// 口径:
/// - 仅统计 `channel = 'local'` 的会话；
/// - 会话须已关闭（`ended_at IS NOT NULL`）且存在至少一条消息；
/// - 会话无任何 `memory_l1` 行（含无归属行）才算缺失；
/// - 按 `started_at ASC, id ASC` 排序（先发生的先补）；
/// - `limit = 0` 表示不限；`limit > 0` 时取前 limit 条。
///
/// 参数:
/// - `limit`: 返回条数上限（0 = 不限）。
pub async fn list_closed_missing_l1(pool: &SqlitePool, limit: u32) -> RamariaResult<Vec<Uuid>> {
    let base = "SELECT s.id FROM sessions s \
         WHERE s.channel = ? \
           AND s.ended_at IS NOT NULL \
           AND EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id) \
           AND NOT EXISTS (SELECT 1 FROM memory_l1 l WHERE l.session_id = s.id) \
         ORDER BY s.started_at ASC, s.id ASC";
    let rows: Vec<String> = if limit == 0 {
        sqlx::query_scalar(base)
            .bind(CHANNEL_LOCAL)
            .fetch_all(pool)
            .await
    } else {
        sqlx::query_scalar(&format!("{base} LIMIT ?"))
            .bind(CHANNEL_LOCAL)
            .bind(limit)
            .fetch_all(pool)
            .await
    }
    .storage_err("查询缺失 L1 的会话失败")?;

    rows.iter()
        .map(|id| parse_uuid_required(id, "sessions", "id"))
        .collect()
}

/// 统计"已关闭且缺 L1 摘要"的本地会话数量。
///
/// 口径:
/// - 与 [`list_closed_missing_l1`] 完全一致（`channel = 'local'`、已关闭、有消息、无 L1）；
/// - 仅计数，不取 id 列表。
pub async fn count_closed_missing_l1(pool: &SqlitePool) -> RamariaResult<u64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions s \
         WHERE s.channel = ? \
           AND s.ended_at IS NOT NULL \
           AND EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id) \
           AND NOT EXISTS (SELECT 1 FROM memory_l1 l WHERE l.session_id = s.id)",
    )
    .bind(CHANNEL_LOCAL)
    .fetch_one(pool)
    .await
    .storage_err("统计缺失 L1 的会话数量失败")?;
    Ok(count.max(0) as u64)
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
    sqlx::query(
        "INSERT INTO sessions (id, started_at, ended_at, persona_uid, channel) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(started_at)
    .bind(ended_at)
    .bind(persona_uid)
    .bind(CHANNEL_LOCAL)
    .execute(pool)
    .await
    .storage_err("创建历史 session 失败")?;
    Ok(Session {
        id,
        started_at,
        ended_at: Some(ended_at),
        persona_uid: Some(persona_uid.to_string()),
        channel: CHANNEL_LOCAL.to_string(),
        external_ref: None,
    })
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    started_at: i64,
    ended_at: Option<i64>,
    persona_uid: Option<String>,
    channel: String,
    external_ref: Option<String>,
}

impl SessionRow {
    fn into_session(self) -> RamariaResult<Session> {
        let id = parse_uuid_required(&self.id, "sessions", "id")?;
        Ok(Session {
            id,
            started_at: self.started_at,
            ended_at: self.ended_at,
            persona_uid: self.persona_uid,
            channel: self.channel,
            external_ref: self.external_ref,
        })
    }
}
#[cfg(test)]
mod tests;
