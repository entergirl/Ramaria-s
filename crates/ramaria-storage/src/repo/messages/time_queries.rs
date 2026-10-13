//! crates/ramaria-storage/src/repo/messages/time_queries.rs - L0 消息时序与存在性查询模块
//!
//! 设计特点:
//! - 按会话 / 人格的轻量时序查询：最后消息时间、用户消息时间窗口与存在性
//! - 聚合查询恒返回一行（无匹配行时 MAX 为 NULL，以 Option 解码）
//! - 所有数据库错误统一转换为 RamariaError::Storage

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use sqlx::SqlitePool;
use uuid::Uuid;

/// 获取指定 session 最后一条消息的时间。
///
/// 职责:
/// - 供空闲检测线程判断 session 是否超过空闲阈值。
/// - 对齐 Python `database.get_last_message_time`。
///
/// 返回:
/// - `Ok(Some(ms))`: 最后消息的 Unix 毫秒时间戳。
/// - `Ok(None)`: session 无消息。
pub async fn get_last_message_time(
    pool: &SqlitePool,
    session_id: Uuid,
) -> RamariaResult<Option<i64>> {
    // SQLite MAX 聚合在无行时返回 NULL，使用 Option<i64> 安全解码
    #[derive(sqlx::FromRow)]
    struct LastTimeRow {
        max_time: Option<i64>,
    }

    let row: Option<LastTimeRow> =
        sqlx::query_as("SELECT MAX(created_at) AS max_time FROM messages WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_optional(pool)
            .await
            .storage_err("查询最后消息时间失败")?;

    // SQLite 在无匹配行时也返回一行（含 NULL），所以 row 通常为 Some
    Ok(row.and_then(|r| r.max_time))
}

/// 查询指定 persona 会话中用户消息的最近时间（Unix 毫秒）。
///
/// 口径:
/// - 只计角色 `user` 的消息；主动消息（assistant 角色）不计入，避免主动投递
///   自身被误判为"用户回应"。
/// - 会话归属以 `sessions.persona_uid` 为准；该 persona 无用户消息时返回 None。
///
/// 参数:
/// - `persona_uid`: 人格标识。
///
/// 返回:
/// - `Ok(Some(ms))`: 最近一条用户消息的 Unix 毫秒时间戳。
/// - `Ok(None)`: 无用户消息历史。
pub async fn last_user_message_time_by_persona(
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
         WHERE s.persona_uid = ? AND m.role = 'user'",
    )
    .bind(persona_uid)
    .fetch_optional(pool)
    .await
    .storage_err("查询 persona 用户消息最近时间失败")?;

    Ok(row.and_then(|r| r.max_time))
}

/// 查询指定 persona 是否存在本地用户消息（"对话一次"存在性判定）。
///
/// 口径:
/// - 只计角色 `user` 且 `import_fingerprint IS NULL` 的消息：主动消息（assistant
///   角色）与导入消息均不计入。
/// - 会话归属以 `sessions.persona_uid` 为准；EXISTS 语义只判断存在性，不取时间。
///
/// 参数:
/// - `persona_uid`: 人格标识。
///
/// 返回:
/// - `Ok(true)`: 存在至少一条本地用户消息。
/// - `Ok(false)`: 不存在。
pub async fn has_local_user_message_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<bool> {
    // SQLite EXISTS 恒返回一行 0/1
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS( \
             SELECT 1 FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE s.persona_uid = ? AND m.role = 'user' AND m.import_fingerprint IS NULL \
         )",
    )
    .bind(persona_uid)
    .fetch_one(pool)
    .await
    .storage_err("查询 persona 本地用户消息存在性失败")?;

    Ok(exists != 0)
}

/// 查询指定会话最近一条消息的时间（Unix 毫秒，含全部角色）。
///
/// 口径:
/// - 会话内全部角色（user / assistant 等）的消息均计入；
/// - 会话无消息时返回 None。
///
/// 参数:
/// - `session_id`: 会话 ID。
///
/// 返回:
/// - `Ok(Some(ms))`: 最近一条消息的 Unix 毫秒时间戳。
/// - `Ok(None)`: 会话无消息。
pub async fn last_message_time_by_session(
    pool: &SqlitePool,
    session_id: Uuid,
) -> RamariaResult<Option<i64>> {
    // SQLite 聚合恒返回一行；无匹配行时 MAX 为 NULL，以 Option 解码
    let max_time = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(created_at) FROM messages WHERE session_id = ?",
    )
    .bind(session_id.to_string())
    .fetch_one(pool)
    .await
    .storage_err("查询会话最后消息时间失败")?;
    Ok(max_time)
}

/// 查询指定 persona 会话中时间窗口内的用户消息时间戳（升序）。
///
/// 口径:
/// - 只计角色 `user` 的消息；`since_ms` 为闭区间下界；结果按时间升序。
/// - 窗口由调用方按滚动天数给出；单次取回窗口内全部时间戳，供本地时区归桶。
///
/// 参数:
/// - `persona_uid`: 人格标识。
/// - `since_ms`: 窗口下界（Unix 毫秒，闭区间）。
///
/// 返回:
/// - 按 `created_at ASC` 排列的用户消息时间戳列表（窗口内无消息时为空）。
pub async fn list_user_message_times_since(
    pool: &SqlitePool,
    persona_uid: &str,
    since_ms: i64,
) -> RamariaResult<Vec<i64>> {
    let times = sqlx::query_scalar::<_, i64>(
        "SELECT m.created_at \
         FROM messages m JOIN sessions s ON s.id = m.session_id \
         WHERE s.persona_uid = ? AND m.role = 'user' AND m.created_at >= ? \
         ORDER BY m.created_at ASC",
    )
    .bind(persona_uid)
    .bind(since_ms)
    .fetch_all(pool)
    .await
    .storage_err("查询 persona 用户消息时间窗口失败")?;

    Ok(times)
}

/// 查询指定 persona 在时间窗口内的全部发言时间戳（升序）。
///
/// 口径:
/// - 归属口径为 `messages.persona_uid`（消息级归属）：不 JOIN 会话，含导入素材
///   与本地对话；不区分角色（人格自身发言无论以何角色入库均计入）。
/// - `since_ms` 为闭区间下界；结果按时间升序，供本地时区归桶分析。
///
/// 参数:
/// - `persona_uid`: 人格标识。
/// - `since_ms`: 窗口下界（Unix 毫秒，闭区间）。
///
/// 返回:
/// - 按 `created_at ASC` 排列的时间戳列表（窗口内无消息时为空）。
pub async fn list_persona_message_times_since(
    pool: &SqlitePool,
    persona_uid: &str,
    since_ms: i64,
) -> RamariaResult<Vec<i64>> {
    let times = sqlx::query_scalar::<_, i64>(
        "SELECT created_at FROM messages \
         WHERE persona_uid = ? AND created_at >= ? \
         ORDER BY created_at ASC",
    )
    .bind(persona_uid)
    .bind(since_ms)
    .fetch_all(pool)
    .await
    .storage_err("查询 persona 发言时间窗口失败")?;

    Ok(times)
}
