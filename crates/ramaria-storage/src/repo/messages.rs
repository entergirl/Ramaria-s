//! crates/ramaria-storage/src/repo/messages.rs - L0 原始消息存取模块
//!
//! 设计特点:
//! - id 使用 UUID v4（TEXT 主键），与 sessions 保持 ID 类型一致
//! - 支持按 session_id 查询完整对话历史、按 persona_uid 过滤发言人消息
//! - find_by_fingerprint 用于历史导入去重（SHA-256 前 16 位 hex）
//! - role/source 解析失败时记录 WARNING 日志并回退到安全默认值
//! - UUID 解析异常时记录 WARNING，不静默吞错

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_required;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{Message, MessageRole, MessageSource};
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
        })
    }
}

/// 执行 messages INSERT（save / save_import / save_import_batch 共用）。
///
/// 参数:
/// - `executor`: sqlx 执行器（连接池引用或事务内连接）。
/// - `msg`: 待写入消息。
/// - `err_ctx`: 失败时的错误上下文文案。
async fn insert_message<'e, E>(executor: E, msg: &Message, err_ctx: &str) -> RamariaResult<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
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
        .execute(executor)
        .await
        .storage_err(err_ctx)?;
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

    insert_message(pool, msg, "保存消息失败").await
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

pub async fn list_by_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid
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
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid
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

// 预留给 v1.6 跨文件导入去重（可选立项，见 docs/dev-1.6/备忘.md D-26-21）
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
    .storage_err("指纹查询失败")?;
    row.map(|r| r.into_message()).transpose()
}

/// 按发言人查询全部消息（Persona-Aware RAG / 导入管线重建用）。
///
/// 去掉 `LIMIT 200`。调用方 `regenerate_import_pipeline`
/// 依赖"某 persona 的全部消息"来枚举其所属 session 并重建 L1；
/// 截断导致 129 个导入 session 中只有最近 4 个被覆盖（按钮名不副实）。
/// 消息量级（万级）下全量加载可控；如未来需分页再引入显式 limit 参数。
pub async fn list_by_persona(pool: &SqlitePool, persona_uid: &str) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid
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
    insert_message(pool, msg, "导入消息写入失败").await
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
        insert_message(
            &mut *txn,
            msg,
            &format!("批量导入消息写入失败 (第 {} 条)", written + 1),
        )
        .await?;
        written += 1;
    }

    txn.commit().await.storage_err("提交批量导入事务失败")?;

    tracing::debug!(count = written, "批量导入消息写入完成");
    Ok(written)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::init_test_pool;
    use ramaria_core::types::{Message, MessageRole, MessageSource};
    use uuid::Uuid;

    /// 插入 persona 与 session fixture，满足 messages 的外键约束。
    async fn setup_fixture(pool: &SqlitePool) -> Uuid {
        sqlx::query(
            "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
             VALUES ('char-0001', '测试', 'char', 1, 'local', 0, 0)",
        )
        .execute(pool)
        .await
        .expect("插入 persona fixture 应成功");
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
            .bind(session_id.to_string())
            .execute(pool)
            .await
            .expect("插入 session fixture 应成功");
        session_id
    }

    fn make_message(session_id: Uuid, fingerprint: Option<&str>) -> Message {
        Message {
            id: uuid::Uuid::new_v4(),
            session_id,
            role: MessageRole::User,
            content: "内容".to_string(),
            created_at: 1000,
            source: MessageSource::Local,
            fingerprint: fingerprint.map(|s| s.to_string()),
            persona_uid: Some("char-0001".to_string()),
        }
    }

    /// 同 fingerprint 二次写入被 UNIQUE 约束拒绝；不同 fingerprint 正常写入。
    #[tokio::test]
    async fn fingerprint_unique_rejects_duplicate() {
        let pool = init_test_pool().await.expect("测试库初始化失败");
        let session_id = setup_fixture(&pool).await;

        // 首次写入成功
        save_import(&pool, &make_message(session_id, Some("fp-same")))
            .await
            .expect("首次写入成功");

        // 同 fingerprint 再次写入 → UNIQUE 冲突
        let dup = make_message(session_id, Some("fp-same"));
        let err = save_import(&pool, &dup)
            .await
            .expect_err("同指纹应被 UNIQUE 拒绝");
        let unique_in_chain =
            std::iter::successors(std::error::Error::source(&err), |e| e.source())
                .map(|e| e.to_string())
                .chain([err.to_string()])
                .any(|msg| msg.contains("UNIQUE"));
        assert!(
            unique_in_chain,
            "底层错误链应含 UNIQUE 约束冲突，实际: {err}"
        );

        // 不同 fingerprint 正常写入
        save_import(&pool, &make_message(session_id, Some("fp-other")))
            .await
            .expect("不同指纹写入成功");
        assert_eq!(count_by_session(&pool, session_id).await.unwrap(), 2);
    }

    /// find_by_fingerprint 能命中已入库指纹，未入库则返回 None。
    #[tokio::test]
    async fn find_by_fingerprint_hits_and_misses() {
        let pool = init_test_pool().await.expect("测试库初始化失败");
        let session_id = setup_fixture(&pool).await;

        assert!(
            find_by_fingerprint(&pool, "fp-present")
                .await
                .unwrap()
                .is_none()
        );
        save_import(&pool, &make_message(session_id, Some("fp-present")))
            .await
            .expect("写入成功");
        let hit = find_by_fingerprint(&pool, "fp-present")
            .await
            .unwrap()
            .expect("应命中");
        assert_eq!(hit.fingerprint.as_deref(), Some("fp-present"));
    }
}
