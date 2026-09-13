//! crates/ramaria-desktop/src/commands/session.rs - 会话管理 Tauri Commands
//!
//! 设计特点:
//! - list_sessions / get_session / delete_session / create_session: 委托 StorageBackend
//! - 所有返回值经过序列化，前端可直接解析 JSON
//! - 删除操作需要二次确认（前端处理），后端只执行删除
//! - 不保留业务逻辑，纯数据访问封装

use crate::DesktopState;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::Message;
use serde::Serialize;
use sqlx::Row;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;
use uuid::Uuid;

// =========================================================
// 前端展示用结构体
// =========================================================

/// 会话摘要（列表展示用）。
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    /// 消息数量（通过 `SELECT COUNT(*)` 实时查询）
    pub message_count: u32,
    /// 会话绑定的人格 UID（NULL 表示存量旧数据）。
    /// 前端 SessionDrawer 据此按 persona 筛选会话列表。
    pub persona_uid: Option<String>,
}

/// 会话详情（含消息列表）。
#[derive(Debug, Clone, Serialize)]
pub struct SessionDetail {
    pub id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    /// 会话绑定的人格 UID。
    pub persona_uid: Option<String>,
    /// 会话消息总数（与分页无关，始终为真实总数）
    pub total_messages: u32,
    /// 是否还有更早的消息未返回（仅分页请求时有意义；全量加载恒为 false）
    pub has_more: bool,
    pub messages: Vec<MessageView>,
}

/// 消息视图（前端展示用）。
#[derive(Debug, Clone, Serialize)]
pub struct MessageView {
    pub id: String,
    pub role: String,
    pub content: String,
    pub persona_uid: Option<String>,
    pub created_at: i64,
}

// =========================================================
// list_sessions — 列出所有会话
// =========================================================

/// 列出所有会话，按开始时间倒序排列。
///
/// 返回:
/// - JSON 数组，每项为 SessionSummary
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_sessions(state: State<'_, DesktopState>) -> Result<Vec<SessionSummary>, String> {
    let sessions = state
        .app
        .storage()
        .list_sessions()
        .await
        .map_err(|e| format!("查询会话列表失败: {}", e))?;

    // 按 started_at 倒序排列
    let mut sorted = sessions;
    sorted.sort_by_key(|b| std::cmp::Reverse(b.started_at));

    // 单次聚合查询各会话消息数，替代逐会话 COUNT 的 N+1 查询
    let counts = message_counts_by_session(&state.pool).await;

    let mut summaries = Vec::with_capacity(sorted.len());
    for s in sorted {
        let id = s.id.to_string();
        let message_count = counts.get(&id).copied().unwrap_or(0);
        summaries.push(SessionSummary {
            id,
            started_at: s.started_at,
            ended_at: s.ended_at,
            message_count,
            persona_uid: s.persona_uid.clone(),
        });
    }

    tracing::debug!(count = summaries.len(), "list_sessions 完成");
    Ok(summaries)
}

/// 单查询聚合各会话消息数（`GROUP BY session_id`），替代逐会话 COUNT 的 N+1 查询。
///
/// 返回:
/// - 会话 UUID 文本 → 消息数；查询失败时仅告警并返回空表（列表仍可展示，
///   消息数降级为 0，不阻塞会话列表）。
async fn message_counts_by_session(pool: &SqlitePool) -> HashMap<String, u32> {
    // 本 crate 未启用 sqlx 的 macros feature，使用运行时查询配合 Row::try_get 解码
    let rows = sqlx::query("SELECT session_id, COUNT(*) AS cnt FROM messages GROUP BY session_id")
        .fetch_all(pool)
        .await;

    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "聚合会话消息数失败，消息数降级为 0");
            return HashMap::new();
        }
    };

    // GROUP BY 只返回有消息的会话；COUNT 解码异常时按 0 处理，不阻塞列表
    let mut counts = HashMap::with_capacity(rows.len());
    for row in rows {
        let session_id: String = match row.try_get("session_id") {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "聚合行缺少 session_id 字段，已跳过");
                continue;
            }
        };
        let cnt: i64 = row.try_get("cnt").unwrap_or(0);
        counts.insert(session_id, cnt.max(0) as u32);
    }
    counts
}

// =========================================================
// get_session — 获取会话详情（含消息）
// =========================================================

/// 单页消息条数上限（防御超大分页请求）。
const MAX_MESSAGE_PAGE: i64 = 1000;

/// 加载指定会话的消息（全量或按最新在前分页）。
///
/// 语义:
/// - `limit` 为 `None`: 走全量加载（时间正序），返回
///   `(全部消息, 总数 = 消息条数, false)`，保持前端不传参时的既有行为；
/// - `limit` 为 `Some(l)`: 按最新在前分页（`created_at DESC`），返回前反转为
///   时间正序，便于调用方直接按对话顺序渲染；`has_more` 表示是否还有更早
///   的消息未返回。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `session_id`: 会话 UUID。
/// - `limit`: 每页条数（`None` 表示全量加载）。
/// - `offset`: 分页偏移量（仅分页时生效，负数按 0 处理）。
///
/// 返回:
/// - `(消息列表, 消息总数, 是否还有更早消息)`。
///
/// 说明:
/// - 长会话应由调用方传 `limit`/`offset` 分页，避免一次性把全部消息拉回内存；
/// - 单页条数经 `MAX_MESSAGE_PAGE` 钳制，防御超大分页请求。
async fn load_session_messages(
    storage: &Arc<dyn StorageBackend>,
    session_id: Uuid,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<(Vec<Message>, u32, bool), String> {
    match limit {
        None => {
            let messages = storage
                .list_messages(session_id)
                .await
                .map_err(|e| format!("查询消息失败: {e}"))?;
            let total = messages.len() as u32;
            Ok((messages, total, false))
        }
        Some(l) => {
            let limit = l.clamp(1, MAX_MESSAGE_PAGE);
            let offset = offset.unwrap_or(0).max(0);

            // 分页按最新在前（created_at DESC）查询，返回前反转为时间正序
            let mut messages = storage
                .list_messages_paginated(session_id, limit, offset)
                .await
                .map_err(|e| format!("查询消息失败: {e}"))?;
            messages.reverse();

            let total = storage
                .count_messages(session_id)
                .await
                .map_err(|e| format!("统计消息数失败: {e}"))?;
            let has_more = (offset + limit) < total as i64;
            Ok((messages, total, has_more))
        }
    }
}

/// 获取指定会话的详情，包含该会话下的消息。
///
/// 参数:
/// - `session_id`: 会话 UUID 字符串
/// - `limit`: 可选，每页消息条数（`None` 表示全量加载）
/// - `offset`: 可选，分页偏移量（`None` 按 0 处理）
///
/// 返回:
/// - SessionDetail（含消息列表、消息总数与是否还有更早消息）
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_session(
    state: State<'_, DesktopState>,
    session_id: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<SessionDetail, String> {
    let sid = Uuid::parse_str(&session_id).map_err(|e| format!("无效的会话 ID: {}", e))?;

    let session = state
        .app
        .storage()
        .get_session(sid)
        .await
        .map_err(|e| format!("查询会话失败: {}", e))?
        .ok_or_else(|| format!("会话不存在: {}", session_id))?;

    let (messages, total_messages, has_more) =
        load_session_messages(state.app.storage(), sid, limit, offset).await?;

    let msg_views: Vec<MessageView> = messages
        .into_iter()
        .map(|m| MessageView {
            id: m.id.to_string(),
            role: m.role.as_str().to_string(),
            content: m.content,
            persona_uid: m.persona_uid,
            created_at: m.created_at,
        })
        .collect();

    tracing::debug!(
        session_id = %session_id,
        message_count = msg_views.len(),
        total_messages = total_messages,
        has_more = has_more,
        "get_session 完成"
    );

    Ok(SessionDetail {
        id: session.id.to_string(),
        started_at: session.started_at,
        ended_at: session.ended_at,
        persona_uid: session.persona_uid.clone(),
        total_messages,
        has_more,
        messages: msg_views,
    })
}

// =========================================================
// delete_session — 删除会话
// =========================================================

/// 删除指定会话及其关联的所有消息。
///
/// 参数:
/// - `session_id`: 会话 UUID 字符串
///
/// 返回:
/// - `"deleted"` 表示删除成功
///
/// 说明:
/// - 前端应先弹出确认对话框，确认后才调用此命令
///
/// 接线状态（未接线/预留）:
/// - 前端 `api.js` 已提供会话删除包装，但当前无视图调用（会话抽屉未提供删除入口）；
/// - 保留该命令以维持会话管理 API 完整性，是否接入 UI 或下线由负责人裁定。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_session(
    state: State<'_, DesktopState>,
    session_id: String,
) -> Result<String, String> {
    let sid = Uuid::parse_str(&session_id).map_err(|e| format!("无效的会话 ID: {}", e))?;

    state
        .app
        .storage()
        .delete_session(sid)
        .await
        .map_err(|e| format!("删除会话失败: {}", e))?;

    tracing::info!(session_id = %session_id, "会话已删除");
    Ok("deleted".to_string())
}

// =========================================================
// create_session — 创建新会话
// =========================================================

/// 创建一个新的空白会话。
///
/// 参数:
/// - `persona_uid`: 绑定的人格 UID（None 表示暂不绑定，发送消息时由
///   resolve_session 回写绑定）。
///
/// 返回:
/// - SessionSummary（新会话的摘要信息）
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn create_session(
    state: State<'_, DesktopState>,
    persona_uid: Option<String>,
) -> Result<SessionSummary, String> {
    let session = state
        .app
        .storage()
        .create_session(persona_uid.as_deref())
        .await
        .map_err(|e| format!("创建会话失败: {}", e))?;

    tracing::info!(session_id = %session.id, persona_uid = ?session.persona_uid, "新会话已创建");

    Ok(SessionSummary {
        id: session.id.to_string(),
        started_at: session.started_at,
        ended_at: session.ended_at,
        message_count: 0,
        persona_uid: session.persona_uid.clone(),
    })
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{Message, MessageRole, MessageSource};
    use ramaria_storage::SqliteStorage;
    use ramaria_storage::database::init_pool;

    /// 创建临时目录 + 真实 SQLite 库（已执行 migration），返回 (目录, 连接池)。
    async fn setup_pool() -> (std::path::PathBuf, SqlitePool) {
        let dir =
            std::env::temp_dir().join(format!("ramaria-desktop-session-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("创建测试临时目录失败");
        let pool = init_pool(Some(dir.join("test.db")))
            .await
            .expect("测试库初始化失败");
        (dir, pool)
    }

    /// 写入 `count` 条测试消息（created_at 从固定基准起逐条 +1，保证分页排序确定）。
    async fn insert_messages(storage: &Arc<dyn StorageBackend>, session_id: Uuid, count: i64) {
        for i in 0..count {
            let mut m = Message::new(
                session_id,
                MessageRole::User,
                format!("m{i}"),
                MessageSource::Local,
            );
            m.created_at = 1_700_000_000_000 + i;
            storage.save_message(&m).await.expect("写入测试消息失败");
        }
    }

    /// 分页语义：最新在前分页、返回时间正序、总数与 has_more 正确、全量与超限防御。
    #[tokio::test]
    async fn load_session_messages_paginates_latest_first() {
        let (dir, pool) = setup_pool().await;
        let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool.clone()));
        let session = storage
            .create_session(None)
            .await
            .expect("创建测试会话失败");
        insert_messages(&storage, session.id, 5).await;

        // 最新一页：limit 2 offset 0 → m3、m4（分页最新在前，返回前反转为时间正序）
        let (messages, total, has_more) =
            load_session_messages(&storage, session.id, Some(2), Some(0))
                .await
                .expect("分页查询失败");
        let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["m3", "m4"]);
        assert_eq!(total, 5);
        assert!(has_more);

        // 最后一页：limit 2 offset 4 → m0，无更早消息
        let (messages, total, has_more) =
            load_session_messages(&storage, session.id, Some(2), Some(4))
                .await
                .expect("末页查询失败");
        let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["m0"]);
        assert_eq!(total, 5);
        assert!(!has_more);

        // 全量：limit None → 5 条时间正序，has_more 恒为 false
        let (messages, total, has_more) = load_session_messages(&storage, session.id, None, None)
            .await
            .expect("全量查询失败");
        let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["m0", "m1", "m2", "m3", "m4"]);
        assert_eq!(total, 5);
        assert!(!has_more);

        // 超限防御：limit 0 被钳制到下界 1 → 仅最新 1 条
        let (messages, total, has_more) =
            load_session_messages(&storage, session.id, Some(0), None)
                .await
                .expect("下界钳制查询失败");
        let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["m4"]);
        assert_eq!(total, 5);
        assert!(has_more);

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 聚合查询：两个会话各 2 条 / 3 条，计数按会话正确归组。
    #[tokio::test]
    async fn message_counts_by_session_aggregates_per_session() {
        let (dir, pool) = setup_pool().await;
        let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool.clone()));
        let s1 = storage
            .create_session(None)
            .await
            .expect("创建测试会话失败");
        let s2 = storage
            .create_session(None)
            .await
            .expect("创建测试会话失败");
        insert_messages(&storage, s1.id, 2).await;
        insert_messages(&storage, s2.id, 3).await;

        let counts = message_counts_by_session(&pool).await;
        assert_eq!(counts.get(&s1.id.to_string()).copied(), Some(2));
        assert_eq!(counts.get(&s2.id.to_string()).copied(), Some(3));
        assert_eq!(counts.len(), 2, "聚合结果只应包含有消息的会话");

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
