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
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{CHANNEL_LOCAL, Session};
use sqlx::SqlitePool;
use uuid::Uuid;

/// 列表中查询会话的列清单（与 [`SessionRow`] 字段顺序一致）。
const SESSION_COLUMNS: &str = "id, started_at, ended_at, persona_uid, channel, external_ref";

/// 创建新 session（本地通道），可选绑定 persona_uid。
///
/// 参数:
/// - `persona_uid`: 对话人格标识（None 兼容存量调用）。
pub async fn create(pool: &SqlitePool, persona_uid: Option<&str>) -> RamariaResult<Session> {
    let now = ramaria_core::types::now_ms();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at, persona_uid, channel) VALUES (?, ?, ?, ?)")
        .bind(id.to_string())
        .bind(now)
        .bind(persona_uid)
        .bind(CHANNEL_LOCAL)
        .execute(pool)
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
    let result = sqlx::query("UPDATE sessions SET ended_at = ? WHERE id = ? AND ended_at IS NULL")
        .bind(now)
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .storage_err("条件关闭 session 失败")?;
    Ok(result.rows_affected() > 0)
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

    // =========================================================
    // 通道支持（channel / external_ref）
    // =========================================================

    /// 新建带通道会话：通道与外部标识正确回读；本地会话取默认通道。
    #[tokio::test]
    async fn create_in_channel_roundtrip_and_local_default() {
        let pool = init_test_pool().await.expect("测试库初始化成功");

        let session = create_in_channel(&pool, Some("rama-0001"), "mcp", Some("client-A"))
            .await
            .expect("创建带通道 session 成功");
        assert_eq!(session.channel, "mcp");
        assert_eq!(session.external_ref.as_deref(), Some("client-A"));

        let fetched = get(&pool, session.id)
            .await
            .expect("查询成功")
            .expect("session 应存在");
        assert_eq!(fetched.channel, "mcp");
        assert_eq!(fetched.external_ref.as_deref(), Some("client-A"));

        // 本地创建走默认通道 'local'（存量行为不变）
        let local = create(&pool, Some("rama-0001"))
            .await
            .expect("创建本地 session 成功");
        assert_eq!(local.channel, "local");
        assert!(local.external_ref.is_none());
        let fetched_local = get(&pool, local.id)
            .await
            .expect("查询成功")
            .expect("本地 session 应存在");
        assert_eq!(fetched_local.channel, "local");
        assert!(fetched_local.external_ref.is_none());
    }

    /// 按 `(channel, external_ref)` 查询：命中活跃会话；无匹配返回 None；已关闭不返回。
    #[tokio::test]
    async fn find_active_by_channel_hits_and_misses() {
        let pool = init_test_pool().await.expect("测试库初始化成功");

        let session = create_in_channel(&pool, Some("rama-0001"), "mcp", Some("client-A"))
            .await
            .expect("创建带通道 session 成功");

        // 命中：同通道 + 同外部标识
        let hit = find_active_by_channel(&pool, "mcp", Some("client-A"))
            .await
            .expect("查询成功");
        assert_eq!(hit.map(|s| s.id), Some(session.id));

        // 无匹配：不同通道 / 不同标识 / 不同 NULL 形态
        assert!(
            find_active_by_channel(&pool, "telegram", Some("client-A"))
                .await
                .expect("查询成功")
                .is_none(),
            "不同通道不应命中"
        );
        assert!(
            find_active_by_channel(&pool, "mcp", Some("client-B"))
                .await
                .expect("查询成功")
                .is_none(),
            "不同外部标识不应命中"
        );
        assert!(
            find_active_by_channel(&pool, "mcp", None)
                .await
                .expect("查询成功")
                .is_none(),
            "无标识查询不应命中带标识会话"
        );

        // 关闭后不再命中（活跃过滤）
        close(&pool, session.id).await.expect("关闭 session 成功");
        assert!(
            find_active_by_channel(&pool, "mcp", Some("client-A"))
                .await
                .expect("查询成功")
                .is_none(),
            "已关闭会话不应命中"
        );
    }

    /// 无标识通道会话（external_ref = NULL）可被 `None` 查询命中。
    #[tokio::test]
    async fn find_active_by_channel_matches_null_ref() {
        let pool = init_test_pool().await.expect("测试库初始化成功");

        let session = create_in_channel(&pool, None, "mcp", None)
            .await
            .expect("创建无标识通道 session 成功");
        let hit = find_active_by_channel(&pool, "mcp", None)
            .await
            .expect("查询成功");
        assert_eq!(hit.map(|s| s.id), Some(session.id));
    }

    /// 抢占式条件关闭：首次抢到置 ended_at，二次调用返回 false（幂等，不重复关闭）。
    #[tokio::test]
    async fn close_if_active_is_single_winner() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let session = create(&pool, Some("rama-0001"))
            .await
            .expect("创建 session 成功");

        assert!(
            close_if_active(&pool, session.id)
                .await
                .expect("条件关闭成功"),
            "首次调用应抢到关闭权"
        );
        assert!(
            !close_if_active(&pool, session.id)
                .await
                .expect("条件关闭成功"),
            "二次调用不应再抢到（幂等）"
        );
        // 不存在的会话同样返回 false（不报错）
        assert!(
            !close_if_active(&pool, Uuid::new_v4())
                .await
                .expect("条件关闭成功"),
            "不存在的会话不应抢到"
        );

        let fetched = get(&pool, session.id)
            .await
            .expect("查询成功")
            .expect("session 应存在");
        assert!(fetched.ended_at.is_some(), "关闭后 ended_at 应落库");
    }
}
