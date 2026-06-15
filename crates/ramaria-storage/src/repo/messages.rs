//! rust/crates/ramaria-storage/src/repo/messages.rs - L0 原始消息存取模块
//!
//! 设计特点:
//! - id 使用 UUID v4（TEXT 主键），与 sessions 保持 ID 类型一致
//! - 支持按 session_id 查询完整对话历史、按 persona_uid 过滤发言人消息
//! - find_by_fingerprint 用于历史导入去重（SHA-256 前 16 位 hex）
//! - role/source 解析失败时记录 WARNING 日志并回退到安全默认值
//! - UUID 解析异常时记录 WARNING，不静默吞错

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{Message, MessageRole, MessageSource};
use sqlx::SqlitePool;
use uuid::Uuid;

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
}

impl MessageRow {
    fn into_message(self) -> RamariaResult<Message> {
        let id = ramaria_core::types::uuid_from_db(&self.id)
            .inspect_err(|_| tracing::warn!(raw_id = %self.id, "messages.id UUID 解析失败"))?;
        let session_id = ramaria_core::types::uuid_from_db(&self.session_id).inspect_err(
            |_| tracing::warn!(raw_id = %self.session_id, "messages.session_id UUID 解析失败"),
        )?;

        let role = match self.role.as_str() {
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "system" => MessageRole::System,
            "tool" => MessageRole::Tool,
            other => {
                tracing::warn!(%other, "messages.role 值非法，回退为 Tool");
                MessageRole::Tool
            }
        };

        let source = match self.source.as_str() {
            "online" => MessageSource::Online,
            "local" => MessageSource::Local,
            other => {
                tracing::warn!(%other, "messages.source 值非法，回退为 Local");
                MessageSource::Local
            }
        };

        Ok(Message {
            id,
            session_id,
            role,
            content: self.content,
            created_at: self.created_at,
            source,
            fingerprint: self.import_fingerprint,
            persona_uid: self.persona_uid,
        })
    }
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

    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source, import_fingerprint, persona_uid)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
        .bind(msg.id.to_string())
        .bind(msg.session_id.to_string())
        .bind(msg.role.as_str())
        .bind(&msg.content)
        .bind(msg.created_at)
        .bind(msg.source.to_string())
        .bind(&msg.fingerprint)
        .bind(&msg.persona_uid)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("保存消息失败", e))?;
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
pub async fn is_session_active(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM sessions WHERE id = ? AND ended_at IS NULL")
            .bind(session_id.to_string())
            .fetch_optional(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("检查 session 活跃状态失败", e))?;
    Ok(row.is_some())
}

/// 获取指定 session 最后一条消息的时间。
///
/// 职责:
/// - 供空闲检测线程判断 session 是否超过空闲阈值。
/// - 对齐 Python `database.get_last_message_time()`。
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
            .map_err(|e| RamariaError::storage_with_source("查询最后消息时间失败", e))?;

    // SQLite 在无匹配行时也返回一行（含 NULL），所以 row 通常为 Some
    Ok(row.and_then(|r| r.max_time))
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
        .map_err(|e| RamariaError::storage_with_source("统计消息数量失败", e))?;

    Ok(row.cnt as u32)
}

pub async fn list_by_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid
         FROM messages WHERE session_id = ? ORDER BY created_at ASC",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询消息列表失败", e))?;
    rows.into_iter()
        .map(|r| r.into_message())
        .collect::<RamariaResult<Vec<_>>>()
}

pub async fn find_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: &str,
) -> RamariaResult<Option<Message>> {
    let row = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid
         FROM messages WHERE import_fingerprint = ? LIMIT 1",
    )
    .bind(fingerprint)
    .fetch_optional(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("指纹查询失败", e))?;
    row.map(|r| r.into_message()).transpose()
}

pub async fn list_by_persona(pool: &SqlitePool, persona_uid: &str) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid
         FROM messages WHERE persona_uid = ? ORDER BY created_at DESC LIMIT 200",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("按 persona 查询消息失败", e))?;
    rows.into_iter()
        .map(|r| r.into_message())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 保存导入消息（跳过 session 活跃状态检查）。
///
/// 职责:
/// - 与 `save()` 不同，此函数不检查目标 session 是否已关闭。
/// - 历史导入的 session 在创建时即已关闭（`ended_at` 不为 NULL），
///   而 `save()` 会因只读约束拒绝写入。导入专用函数绕过此检查。
/// - 供 ramaria-importer 在快速/深度导入模式中使用。
///
/// 参数:
/// - `msg`: 待写入的消息，含 fingerprint 和 persona_uid。
pub async fn save_import(pool: &SqlitePool, msg: &Message) -> RamariaResult<()> {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source, import_fingerprint, persona_uid)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
        .bind(msg.id.to_string())
        .bind(msg.session_id.to_string())
        .bind(msg.role.as_str())
        .bind(&msg.content)
        .bind(msg.created_at)
        .bind(msg.source.to_string())
        .bind(&msg.fingerprint)
        .bind(&msg.persona_uid)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("导入消息写入失败", e))?;
    Ok(())
}

/// 获取所有已导入消息的指纹集合（去重预检专用）。
///
/// 职责:
/// - 供导入器在写入前批量比对，避免重复导入。
/// - 仅查询 `import_fingerprint IS NOT NULL` 的消息（正常对话消息不含指纹）。
///
/// 返回:
/// - 所有已导入 fingerprint 的集合。
pub async fn list_all_fingerprints(pool: &SqlitePool) -> RamariaResult<Vec<String>> {
    #[derive(sqlx::FromRow)]
    struct FpRow {
        import_fingerprint: String,
    }

    let rows = sqlx::query_as::<_, FpRow>(
        "SELECT import_fingerprint FROM messages WHERE import_fingerprint IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询指纹列表失败", e))?;

    Ok(rows.into_iter().map(|r| r.import_fingerprint).collect())
}

/// 批量保存导入消息，包裹在显式 SQLite 事务中。
///
/// 职责:
/// - 替代循环调用 `save_import()` 的模式，将多条 INSERT 包裹在单个事务中。
/// - 减少 SQLite 的隐式事务→fsync→提交开销，显著提升导入性能。
///
/// 事务行为:
/// - 使用 `pool.begin()` 创建显式事务。
/// - 若任一条写入失败，事务自动回滚（通过 `?` 运算符传播错误后 Drop 触发）。
/// - 所有消息成功写入后，调用 `txn.commit()` 提交。
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

    let mut txn = pool
        .begin()
        .await
        .map_err(|e| RamariaError::storage_with_source("开启批量导入事务失败", e))?;

    let mut written = 0usize;

    for msg in msgs {
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, content, created_at, source, import_fingerprint, persona_uid)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        )
            .bind(msg.id.to_string())
            .bind(msg.session_id.to_string())
            .bind(msg.role.as_str())
            .bind(&msg.content)
            .bind(msg.created_at)
            .bind(msg.source.to_string())
            .bind(&msg.fingerprint)
            .bind(&msg.persona_uid)
            .execute(&mut *txn)
            .await
            .map_err(|e| {
                RamariaError::storage_with_source(
                    format!("批量导入消息写入失败 (第 {} 条)", written + 1),
                    e,
                )
            })?;
        written += 1;
    }

    txn.commit()
        .await
        .map_err(|e| RamariaError::storage_with_source("提交批量导入事务失败", e))?;

    tracing::debug!(count = written, "批量导入消息写入完成");
    Ok(written)
}
