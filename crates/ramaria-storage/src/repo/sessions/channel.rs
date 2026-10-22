//! crates/ramaria-storage/src/repo/sessions/channel.rs - 通道会话定位与概览
//!
//! 设计特点:
//! - 外部入口按 (channel, external_ref) 复用未关闭会话
//! - external_ref IS ? 为 NULL 安全比较
//! - 概览单条 SQL 完成活跃会话数与最近活动时间两个聚合
//! - 脏数据下按 started_at DESC 取最近开始的一条

use crate::repo::StorageResultExt;
use crate::retry::with_busy_retry;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::Session;
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

use super::SessionRow;

/// 创建带来源通道的 session（外部入口专用）。
///
/// 参数:
/// - `persona_uid`: 对话人格标识。
/// - `channel`: 来源通道（如 `mcp`）。
/// - `external_ref`: 外部对话标识（客户端 conversation id / 客户端名）。
pub async fn create_in_channel(
    pool: &SqlitePool,
    persona_uid: Option<&str>,
    channel: &str,
    external_ref: Option<&str>,
) -> RamariaResult<Session> {
    let now = ramaria_core::types::now_ms();
    let id = Uuid::new_v4();
    // 多进程写锁争用时有限重试，避免 database is locked 直接失败
    with_busy_retry("创建带通道 session", || async {
        sqlx::query(
            "INSERT INTO sessions (id, started_at, persona_uid, channel, external_ref) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(now)
        .bind(persona_uid)
        .bind(channel)
        .bind(external_ref)
        .execute(pool)
        .await
    })
    .await
    .storage_err("创建带通道 session 失败")?;
    Ok(Session {
        id,
        started_at: now,
        ended_at: None,
        persona_uid: persona_uid.map(|s| s.to_string()),
        channel: channel.to_string(),
        external_ref: external_ref.map(|s| s.to_string()),
    })
}

/// 按 `(channel, external_ref)` 查询活跃会话（最近开始的一条）。
///
/// 职责:
/// - 外部入口续写定位：同一外部对话标识优先复用未关闭的会话。
///
/// 参数:
/// - `channel`: 来源通道。
/// - `external_ref`: 外部对话标识；None 表示查询该通道下无标识的活跃会话。
///
/// 返回:
/// - `Ok(Some(session))`: 命中的活跃会话（数据异常存在多条时返回最近开始的一条）。
/// - `Ok(None)`: 无匹配活跃会话。
///
/// 说明:
/// - SQL 使用 `external_ref IS ?`（NULL 安全比较）：绑定 NULL 时等价 `IS NULL`，
///   绑定文本时等价 `=`；联合索引 `(channel, external_ref)` 可命中。
/// - `ORDER BY started_at DESC LIMIT 1` 为脏数据兜底（正常同标识至多一个活跃会话）。
pub async fn find_active_by_channel(
    pool: &SqlitePool,
    channel: &str,
    external_ref: Option<&str>,
) -> RamariaResult<Option<Session>> {
    let row = sqlx::query_as::<_, SessionRow>(
        "SELECT id, started_at, ended_at, persona_uid, channel, external_ref FROM sessions \
         WHERE channel = ? AND external_ref IS ? AND ended_at IS NULL \
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(channel)
    .bind(external_ref)
    .fetch_optional(pool)
    .await
    .storage_err("查询通道活跃 session 失败")?;
    row.map(SessionRow::into_session).transpose()
}

/// 通道会话概览（外部入口的可见性统计）。
///
/// 职责:
/// - 为入口层（如桌面「MCP 接入」面板）提供"该通道是否有客户端在用"的可见证据：
///   活跃会话数 + 最近一条消息时间。stdio MCP 服务由外部客户端按需拉起，
///   桌面侧无法直接观测其进程状态，只能以库内活动数据近似呈现。
///
/// 字段约定:
/// - `active_sessions`: 该通道下未关闭（`ended_at IS NULL`）的会话数。
/// - `last_activity_ms`: 该通道全部会话中最近一条消息的 `created_at`（Unix 毫秒）；
///   该通道无任何消息时为 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelOverview {
    pub active_sessions: i64,
    pub last_activity_ms: Option<i64>,
}

/// 统计指定通道的会话概览（活跃会话数 + 最近活动时间）。
///
/// 参数:
/// - `channel`: 来源通道（如 `mcp` / `local`）。
///
/// 返回:
/// - [`ChannelOverview`]；空通道返回 `active_sessions = 0` 且 `last_activity_ms = None`。
///
/// 说明:
/// - 单条 SQL 以子查询完成两个聚合，避免两次往返（面板每次进入设置页调用一次）；
/// - 只读查询，不参与写锁竞争。
pub async fn channel_overview(pool: &SqlitePool, channel: &str) -> RamariaResult<ChannelOverview> {
    let row = sqlx::query(
        "SELECT \
           (SELECT COUNT(*) FROM sessions WHERE channel = ? AND ended_at IS NULL) AS active_sessions, \
           (SELECT MAX(m.created_at) FROM messages m \
              JOIN sessions s ON m.session_id = s.id \
             WHERE s.channel = ?) AS last_activity_ms",
    )
    .bind(channel)
    .bind(channel)
    .fetch_one(pool)
    .await
    .storage_err("统计通道会话概览失败")?;

    let active_sessions: i64 = row
        .try_get("active_sessions")
        .storage_err("读取通道活跃会话数失败")?;
    let last_activity_ms: Option<i64> = row
        .try_get("last_activity_ms")
        .storage_err("读取通道最近活动时间失败")?;

    Ok(ChannelOverview {
        active_sessions: active_sessions.max(0),
        last_activity_ms,
    })
}
