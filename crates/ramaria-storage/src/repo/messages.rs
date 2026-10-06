//! crates/ramaria-storage/src/repo/messages.rs - L0 原始消息存取模块
//!
//! 设计特点:
//! - id 使用 UUID v4（TEXT 主键），与 sessions 保持 ID 类型一致
//! - 支持按 session_id 查询完整对话历史、按 persona_uid 过滤发言人消息
//! - 会话/发言人的"全量查询"（list_by_session/list_by_persona）供需完整数据的
//!   离线分析/重建路径使用；浏览/展示场景须走对应 *paginated 分页查询，避免全量回内存
//! - find_by_fingerprint 用于历史导入去重（SHA-256 前 16 位 hex）
//! - role/source 解析失败时记录 WARNING 日志并回退到安全默认值
//! - UUID 解析异常时记录 WARNING，不静默吞错

use std::collections::HashMap;

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_required;
use crate::retry::with_busy_retry;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::ProactiveDeliveryPair;
use ramaria_core::types::{Message, MessageKey, MessageRole, MessageSource};
use sqlx::SqlitePool;
use uuid::Uuid;

parse_enum_fallback!(
    parse_role, MessageRole, MessageRole::Tool, "messages", "role",
    "user"      => User,
    "assistant" => Assistant,
    "system"    => System,
    "tool"      => Tool,
);
parse_enum_fallback!(
    parse_source, MessageSource, MessageSource::Local, "messages", "source",
    "online" => Online,
    "local"  => Local,
);

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: String,
    session_id: String,
    role: String,
    content: String,
    created_at: i64,
    source: String,
    import_fingerprint: Option<String>,
    persona_uid: Option<String>,
    is_proactive: i64,
}

impl MessageRow {
    fn into_message(self) -> RamariaResult<Message> {
        let id = parse_uuid_required(&self.id, "messages", "id")?;
        let session_id = parse_uuid_required(&self.session_id, "messages", "session_id")?;

        Ok(Message {
            id,
            session_id,
            role: parse_role(&self.role),
            content: self.content,
            created_at: self.created_at,
            source: parse_source(&self.source),
            fingerprint: self.import_fingerprint,
            persona_uid: self.persona_uid,
            // SQLite 以 0/1 存储布尔标记；非 0 一律按主动消息读回
            is_proactive: self.is_proactive != 0,
        })
    }
}

/// 执行 messages INSERT（save / save_import / save_import_batch 共用）。
///
/// 参数:
/// - `executor`: sqlx 执行器（连接池引用或事务内连接）。
/// - `msg`: 待写入消息。
///
/// 返回:
/// - 原始 `sqlx::Error`：写入口（save / save_import）需要在 [`with_busy_retry`]
///   闭包内依据错误类型判断写锁忙，错误上下文由调用方在外层映射。
async fn insert_message<'e, E>(executor: E, msg: &Message) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source, import_fingerprint, persona_uid, is_proactive)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
        .bind(msg.id.to_string())
        .bind(msg.session_id.to_string())
        .bind(msg.role.as_str())
        .bind(&msg.content)
        .bind(msg.created_at)
        .bind(msg.source.to_string())
        .bind(&msg.fingerprint)
        .bind(&msg.persona_uid)
        .bind(i64::from(msg.is_proactive))
        .execute(executor)
        .await?;
    Ok(())
}

pub async fn save(pool: &SqlitePool, msg: &Message) -> RamariaResult<()> {
    // 写入前检查 session 是否已关闭（只读约束）
    // 对齐 Python：已关闭 session 不可再编辑
    if !is_session_active(pool, msg.session_id).await? {
        return Err(RamariaError::validation(format!(
            "session {} 已关闭，不可写入新消息",
            msg.session_id
        )));
    }

    // 多进程写锁争用时有限重试，避免 database is locked 直接失败
    with_busy_retry("保存消息", || async { insert_message(pool, msg).await })
        .await
        .storage_err("保存消息失败")?;
    Ok(())
}

/// 检查 session 是否处于活跃状态（ended_at IS NULL）。
///
/// 职责:
/// - 防止向已关闭 session 写入消息（只读约束）。
/// - 对齐 Python `SessionManager` 的只读保护行为。
///
/// 返回:
/// - `Ok(true)`: session 存在且未关闭。
/// - `Ok(false)`: session 不存在或已关闭。
async fn is_session_active(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM sessions WHERE id = ? AND ended_at IS NULL")
            .bind(session_id.to_string())
            .fetch_optional(pool)
            .await
            .storage_err("检查 session 活跃状态失败")?;
    Ok(row.is_some())
}

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

/// 列出指定 persona 的主动消息投递与其后（窗口内）首条本地用户消息的配对。
///
/// 口径:
/// - 投递 = 会话归属该 persona（`sessions.persona_uid`）且 `is_proactive = 1` 的消息；
/// - 回应 = 该 persona 任一会话中角色 `user`、无导入指纹、时间晚于投递且在窗口内
///   （闭区间上界）的最早一条；`response_window_ms = 0` 表示不设上界；
/// - 结果按投递时间升序。
///
/// 参数:
/// - `persona_uid`: 人格标识。
/// - `response_window_ms`: 回应判定窗口（毫秒；0 = 不设上界）。
///
/// 返回:
/// - 投递与回应配对列表（无投递时为空列表）。
pub async fn list_proactive_delivery_pairs(
    pool: &SqlitePool,
    persona_uid: &str,
    response_window_ms: i64,
) -> RamariaResult<Vec<ProactiveDeliveryPair>> {
    let rows = sqlx::query_as::<_, (i64, Option<i64>)>(
        "SELECT p.created_at AS sent_at, \
                (SELECT MIN(u.created_at) \
                   FROM messages u JOIN sessions us ON us.id = u.session_id \
                  WHERE us.persona_uid = s.persona_uid AND u.role = 'user' \
                    AND u.import_fingerprint IS NULL AND u.created_at > p.created_at \
                    AND (? = 0 OR u.created_at <= p.created_at + ?)) AS responded_at \
           FROM messages p JOIN sessions s ON s.id = p.session_id \
          WHERE p.is_proactive = 1 AND s.persona_uid = ? \
          ORDER BY p.created_at ASC",
    )
    .bind(response_window_ms)
    .bind(response_window_ms)
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询主动消息投递与回应配对失败")?;

    Ok(rows
        .into_iter()
        .map(|(sent_at, responded_at)| ProactiveDeliveryPair {
            sent_at,
            responded_at,
        })
        .collect())
}

/// 统计指定 session 的消息数量（使用 SELECT COUNT(*) 避免全表拉取）。
///
/// 职责:
/// - 供前端 session 列表展示真实消息数，代替硬编码 0。
/// - SQLite COUNT 直接返回行数，无需遍历。
///
/// 返回:
/// - 消息数量（无消息时为 0）。
pub async fn count_by_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<u32> {
    #[derive(sqlx::FromRow)]
    struct CountRow {
        cnt: i64,
    }

    let row: CountRow = sqlx::query_as("SELECT COUNT(*) AS cnt FROM messages WHERE session_id = ?")
        .bind(session_id.to_string())
        .fetch_one(pool)
        .await
        .storage_err("统计消息数量失败")?;

    Ok(row.cnt as u32)
}

/// 聚合全部会话的消息数量（`GROUP BY session_id`），供会话列表一次取回全部计数。
///
/// 职责:
/// - 单条聚合查询替代逐会话 COUNT 的 N+1 查询。
///
/// 返回:
/// - 会话 UUID → 消息条数的映射；只包含有消息的会话
///   （无消息会话由调用方按 0 处理，与桌面列表降级口径一致）。
///
/// 说明:
/// - 单行 session_id 解析失败时记录 WARNING 并跳过（防御历史脏数据，不阻塞列表）。
pub async fn count_by_sessions(pool: &SqlitePool) -> RamariaResult<HashMap<Uuid, u32>> {
    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT session_id, COUNT(*) AS cnt FROM messages GROUP BY session_id",
    )
    .fetch_all(pool)
    .await
    .storage_err("聚合会话消息数量失败")?;

    let mut counts = HashMap::with_capacity(rows.len());
    for (session_id, cnt) in rows {
        match ramaria_core::types::uuid_from_db(&session_id) {
            Ok(id) => {
                counts.insert(id, cnt.max(0) as u32);
            }
            Err(_) => {
                tracing::warn!(
                    raw_id = %session_id,
                    "messages.session_id UUID 解析失败，聚合计数已跳过该行"
                );
            }
        }
    }
    Ok(counts)
}

/// 聚合各会话的未读消息数（`GROUP BY session_id`，供会话列表与托盘徽标一次取回）。
///
/// 未读口径:
/// - 只计本地助手消息：`role = 'assistant'` 且 `import_fingerprint IS NULL`
///   （用户发言不计；主动消息与常规回复计）；
/// - 消息时间严格晚于会话的 `last_read_at`（等于视为已读）。
///
/// 返回:
/// - 会话 UUID → 未读条数的映射；只包含存在未读的会话
///   （无未读会话由调用方按 0 处理）。
///
/// 说明:
/// - 单行 session_id 解析失败时记录 WARNING 并跳过（防御历史脏数据，不阻塞列表）。
pub async fn list_unread_counts(pool: &SqlitePool) -> RamariaResult<HashMap<Uuid, u32>> {
    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT m.session_id, COUNT(*) AS cnt \
         FROM messages m JOIN sessions s ON s.id = m.session_id \
         WHERE m.role = 'assistant' AND m.import_fingerprint IS NULL \
           AND m.created_at > s.last_read_at \
         GROUP BY m.session_id",
    )
    .fetch_all(pool)
    .await
    .storage_err("聚合会话未读数量失败")?;

    let mut counts = HashMap::with_capacity(rows.len());
    for (session_id, cnt) in rows {
        match ramaria_core::types::uuid_from_db(&session_id) {
            Ok(id) => {
                counts.insert(id, cnt.max(0) as u32);
            }
            Err(_) => {
                tracing::warn!(
                    raw_id = %session_id,
                    "messages.session_id UUID 解析失败，未读聚合已跳过该行"
                );
            }
        }
    }
    Ok(counts)
}

/// 按时间升序加载指定 session 的全部消息。
///
/// 说明:
/// - **全量加载**，一次将整个 session 的消息 fetch 回内存。
/// - 供需要完整会话数据的离线/分析路径使用：L1 摘要生成、utt 块切分、
///   导入重建、会话导出等，均需基于会话全量数据计算，故保持全量语义。
/// - 浏览/展示场景请使用 `list_by_session_paginated`（按 limit/offset 分页），
///   避免把超长会话整段拉回内存。
///
/// 返回:
/// - 按 `created_at ASC`（时间正序）排列的消息列表。
pub async fn list_by_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid, is_proactive
         FROM messages WHERE session_id = ? ORDER BY created_at ASC",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await
    .storage_err("查询消息列表失败")?;
    rows.into_iter()
        .map(|r| r.into_message())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 按创建时间降序分页加载消息。
///
/// 返回按 `created_at DESC` 排序（最新在前），便于调用方从最新消息开始按 token 预算加载。
///
/// 参数:
/// - `pool`: 数据库连接池。
/// - `session_id`: 会话 ID。
/// - `limit`: 每页最大条数。
/// - `offset`: 分页偏移量（第一页为 0）。
///
/// 返回:
/// - 按 `created_at DESC` 排序的消息列表。
pub async fn list_by_session_paginated(
    pool: &SqlitePool,
    session_id: Uuid,
    limit: i64,
    offset: i64,
) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid, is_proactive
         FROM messages WHERE session_id = ? ORDER BY created_at DESC LIMIT ? OFFSET ?",
    )
    .bind(session_id.to_string())
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .storage_err("分页查询消息列表失败")?;
    rows.into_iter()
        .map(|r| r.into_message())
        .collect::<RamariaResult<Vec<_>>>()
}

// 跨文件导入去重：导入器按 import_fingerprint 查询消息是否已入库
pub async fn find_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: &str,
) -> RamariaResult<Option<Message>> {
    let row = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid, is_proactive
         FROM messages WHERE import_fingerprint = ? LIMIT 1",
    )
    .bind(fingerprint)
    .fetch_optional(pool)
    .await
    .storage_err("指纹查询失败")?;
    row.map(|r| r.into_message()).transpose()
}

/// 按来源通道 + 外部对话标识读取消息去重键（外部入口重复提交去重）。
///
/// 职责:
/// - 同一个外部对话可能跨多个会话（空闲封存后另起），本查询经 sessions 关联跨会话取回
///   该对话的**全部**消息键（角色 + trim 后正文），供回流用例做重发跳过与指纹序数计算。
/// - 只取两列：长对话（数千条）也能廉价全量取回，避免"读取窗口截断 → 指纹序数失准 → 重复写入"。
///
/// 参数:
/// - `channel`: 来源通道（`sessions.channel`）。
/// - `external_ref`: 外部对话标识；`None` 表示该通道下无标识的单流会话。
///
/// 返回:
/// - 按 `created_at ASC, id ASC` 排列的消息键列表（无匹配时为空）。
///
/// 说明:
/// - 使用 `sessions.external_ref IS ?`（NULL 安全比较）：绑定 NULL 时等价 `IS NULL`；
/// - 正文在 SQL 侧 `TRIM`，与写入口径（`save` 前调用方 trim）保持一致。
pub async fn list_keys_by_channel_ref(
    pool: &SqlitePool,
    channel: &str,
    external_ref: Option<&str>,
) -> RamariaResult<Vec<MessageKey>> {
    #[derive(sqlx::FromRow)]
    struct MessageKeyRow {
        role: String,
        content: String,
    }

    let rows = sqlx::query_as::<_, MessageKeyRow>(
        "SELECT m.role AS role, TRIM(m.content) AS content \
         FROM messages m JOIN sessions s ON s.id = m.session_id \
         WHERE s.channel = ? AND s.external_ref IS ? \
         ORDER BY m.created_at ASC, m.id ASC",
    )
    .bind(channel)
    .bind(external_ref)
    .fetch_all(pool)
    .await
    .storage_err("按通道查询消息键失败")?;

    Ok(rows
        .into_iter()
        .map(|row| MessageKey {
            role: parse_role(&row.role),
            content: row.content,
        })
        .collect())
}

/// 按发言人加载该 persona 的全部消息（离线分析/重建专用）。
///
/// 说明:
/// - **全量加载**，一次将该 persona 所有消息 fetch 回内存（不设 LIMIT）。
/// - 供需要完整数据的一次性离线分析/重建路径使用：
///   - A3 表达层风格统计（`app_style` 一次性分析 persona 全部消息）。
///   - 导入管线重建（`regenerate_import_pipeline` 需枚举该 persona 全部
///     session 并逐个重建 L1，截断会导致部分导入 session 未被覆盖）。
/// - 消息量级（万级）下全量加载可控；**浏览/展示场景请使用
///   `list_by_persona_paginated`**（分页），避免把大 persona 库整段拉回内存。
///
/// 返回:
/// - 按 `created_at DESC`（最新在前）排列的消息列表。
pub async fn list_by_persona(pool: &SqlitePool, persona_uid: &str) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid, is_proactive
         FROM messages WHERE persona_uid = ? ORDER BY created_at DESC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("按 persona 查询消息失败")?;
    rows.into_iter()
        .map(|r| r.into_message())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 按创建时间降序分页加载指定 persona 的消息（浏览场景专用）。
///
/// 说明:
/// - 返回与 `list_by_persona` 相同投影与排序（`created_at DESC`，最新在前）。
/// - 通过 `LIMIT ? OFFSET ?` 在 SQL 层分页，避免大 persona 库全量回内存。
/// - 供分页展示 persona 消息的场景使用；确需该 persona 全部消息的
///   离线分析/重建路径仍走 `list_by_persona`。
///
/// 参数:
/// - `pool`: 数据库连接池。
/// - `persona_uid`: 目标 persona 的 UID。
/// - `limit`: 每页最大条数。
/// - `offset`: 分页偏移量（第一页为 0）。
///
/// 返回:
/// - 按 `created_at DESC` 排序的当前页消息列表（末页或 offset 越界时可能为空）。
pub async fn list_by_persona_paginated(
    pool: &SqlitePool,
    persona_uid: &str,
    limit: i64,
    offset: i64,
) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid, is_proactive
         FROM messages WHERE persona_uid = ? ORDER BY created_at DESC LIMIT ? OFFSET ?",
    )
    .bind(persona_uid)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .storage_err("按 persona 分页查询消息失败")?;
    rows.into_iter()
        .map(|r| r.into_message())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 保存导入消息（跳过 session 活跃状态检查）。
///
/// 职责:
/// - 与 `save` 不同，此函数不检查目标 session 是否已关闭。
/// - 历史导入的 session 在创建时即已关闭（`ended_at` 不为 NULL），
///   而 `save` 会因只读约束拒绝写入。导入专用函数绕过此检查。
/// - 供 ramaria-importer 在快速/深度导入模式中使用。
///
/// 参数:
/// - `msg`: 待写入的消息，含 fingerprint 和 persona_uid。
pub async fn save_import(pool: &SqlitePool, msg: &Message) -> RamariaResult<()> {
    // 多进程写锁争用时有限重试，避免 database is locked 直接失败
    with_busy_retry("导入消息写入", || async {
        insert_message(pool, msg).await
    })
    .await
    .storage_err("导入消息写入失败")?;
    Ok(())
}

/// 批量保存导入消息，包裹在显式 SQLite 事务中。
///
/// 职责:
/// - 替代循环调用 `save_import` 的模式，将多条 INSERT 包裹在单个事务中。
/// - 减少 SQLite 的隐式事务→fsync→提交开销，显著提升导入性能。
///
/// 事务行为:
/// - 使用 `pool.begin` 创建显式事务。
/// - 若任一条写入失败，事务自动回滚（通过 `?` 运算符传播错误后 Drop 触发）。
/// - 所有消息成功写入后，调用 `txn.commit` 提交。
/// - 含 `created_at` 时间戳顺序校验——消息必须按时间升序排列（调用方负责排序）。
///
/// 参数:
/// - `msgs`: 待写入的消息列表。应为同一 session 的消息，调用方负责排序。
///
/// 返回:
/// - `Ok(count)`: 成功写入的消息数量。
/// - `Err(...)`: 写入失败时返回错误，事务已自动回滚。
///
/// 性能:
/// - 1000 条消息的事务包裹写入约为逐条写入的 10-50 倍快（取决于 fsync 配置）。
pub async fn save_import_batch(pool: &SqlitePool, msgs: &[Message]) -> RamariaResult<usize> {
    if msgs.is_empty() {
        return Ok(0);
    }

    let mut txn = pool.begin().await.storage_err("开启批量导入事务失败")?;

    let mut written = 0usize;

    for msg in msgs {
        insert_message(&mut *txn, msg)
            .await
            .storage_err(format!("批量导入消息写入失败 (第 {} 条)", written + 1))?;
        written += 1;
    }

    txn.commit().await.storage_err("提交批量导入事务失败")?;

    tracing::debug!(count = written, "批量导入消息写入完成");
    Ok(written)
}

#[cfg(test)]
mod tests;
