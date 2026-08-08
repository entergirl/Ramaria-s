//! rust/crates/ramaria-desktop/src/commands/session.rs - 会话管理 Tauri Commands
//!
//! 设计特点:
//! - list_sessions / get_session / delete_session / create_session: 委托 StorageBackend
//! - 所有返回值经过序列化，前端可直接解析 JSON
//! - 删除操作需要二次确认（前端处理），后端只执行删除
//! - 不保留业务逻辑，纯数据访问封装

use crate::DesktopState;
use serde::Serialize;
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

    let mut summaries = Vec::with_capacity(sorted.len());
    for s in sorted {
        let message_count = state.app.storage().count_messages(s.id).await.unwrap_or(0);
        summaries.push(SessionSummary {
            id: s.id.to_string(),
            started_at: s.started_at,
            ended_at: s.ended_at,
            message_count,
            persona_uid: s.persona_uid.clone(),
        });
    }

    tracing::debug!(count = summaries.len(), "list_sessions 完成");
    Ok(summaries)
}

// =========================================================
// get_session — 获取会话详情（含消息）
// =========================================================

/// 获取指定会话的详情，包含该会话下的所有消息。
///
/// 参数:
/// - `session_id`: 会话 UUID 字符串
///
/// 返回:
/// - SessionDetail（含消息列表）
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_session(
    state: State<'_, DesktopState>,
    session_id: String,
) -> Result<SessionDetail, String> {
    let sid = Uuid::parse_str(&session_id).map_err(|e| format!("无效的会话 ID: {}", e))?;

    let session = state
        .app
        .storage()
        .get_session(sid)
        .await
        .map_err(|e| format!("查询会话失败: {}", e))?
        .ok_or_else(|| format!("会话不存在: {}", session_id))?;

    let messages = state
        .app
        .storage()
        .list_messages(sid)
        .await
        .map_err(|e| format!("查询消息失败: {}", e))?;

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
        "get_session 完成"
    );

    Ok(SessionDetail {
        id: session.id.to_string(),
        started_at: session.started_at,
        ended_at: session.ended_at,
        persona_uid: session.persona_uid.clone(),
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
