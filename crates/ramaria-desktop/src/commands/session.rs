//! crates/ramaria-desktop/src/commands/session.rs - 会话管理 Tauri Commands
//!
//! 设计特点:
//! - list_sessions / get_session / create_session / mark_session_read: 委托服务层会话用例
//! - 所有返回值经过序列化，前端可直接解析 JSON（字段结构保持既有契约）
//! - 服务层的时间类型转换为毫秒时间戳，与前端既有展示口径一致

use crate::DesktopState;
use serde::Serialize;
use tauri::{AppHandle, State};
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
    /// 消息数量（服务层聚合查询结果）
    pub message_count: u32,
    /// 未读消息数（本地助手消息中晚于会话已读时间的条数；用户发言与导入历史不计）。
    pub unread: u32,
    /// 会话绑定的人格 UID（NULL 表示存量旧数据）。
    /// 前端 SessionDrawer 据此按 persona 筛选会话列表。
    pub persona_uid: Option<String>,
    /// 会话来源通道（`local` / `mcp`；v2.1 会话来源标注）。
    ///
    /// 用途:
    /// - 前端会话抽屉据此标注外部来源（MCP 回流会话），完成定义要求
    ///   「外部对话回流后桌面可见并标注来源」。
    pub channel: String,
    /// 外部对话标识（客户端 conversation id / 客户端身份名；本地会话为 None）。
    pub external_ref: Option<String>,
}

/// 由存储层会话与消息数聚合构造前端摘要（新建会话路径的字段映射入口）。
///
/// 参数:
/// - `session`: 存储层会话记录。
/// - `message_count`: 该会话消息数（`None` 表示聚合缺失，按 0 处理）。
fn summary_from(
    session: &ramaria_core::types::Session,
    message_count: Option<u32>,
) -> SessionSummary {
    SessionSummary {
        id: session.id.to_string(),
        started_at: session.started_at,
        ended_at: session.ended_at,
        message_count: message_count.unwrap_or(0),
        unread: 0,
        persona_uid: session.persona_uid.clone(),
        channel: session.channel.clone(),
        external_ref: session.external_ref.clone(),
    }
}

/// 由服务层会话摘要视图构造前端摘要（列表路径的字段映射入口，便于单测锁定）。
///
/// 参数:
/// - `view`: 服务层会话列表条目（时间类型为 UTC，转换毫秒时间戳透出）。
fn summary_from_view(view: &ramaria_service::SessionSummaryView) -> SessionSummary {
    SessionSummary {
        id: view.id.to_string(),
        started_at: view.started_at.timestamp_millis(),
        ended_at: view.ended_at.as_ref().map(|dt| dt.timestamp_millis()),
        message_count: view.message_count,
        unread: view.unread,
        persona_uid: view.persona_uid.clone(),
        channel: view.channel.clone(),
        external_ref: view.external_ref.clone(),
    }
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
    /// 主动生成标记（后端主动对话路径写入 true；常规消息恒 false）
    pub is_proactive: bool,
    /// 外部平台发送者 ID（导入消息）；本地 / MCP 消息为 None。
    pub sender_ref: Option<String>,
    /// 外部平台发送者显示名（导入消息）；本地 / MCP 消息为 None。
    pub sender_name: Option<String>,
}

/// 由服务层会话详情视图构造前端详情（字段映射的唯一入口，便于单测锁定）。
///
/// 参数:
/// - `view`: 服务层会话详情视图（时间类型为 UTC，转换毫秒时间戳透出）。
fn detail_from_view(view: &ramaria_service::SessionDetailView) -> SessionDetail {
    SessionDetail {
        id: view.id.to_string(),
        started_at: view.started_at.timestamp_millis(),
        ended_at: view.ended_at.as_ref().map(|dt| dt.timestamp_millis()),
        persona_uid: view.persona_uid.clone(),
        total_messages: view.total_messages,
        has_more: view.has_more,
        messages: view.messages.iter().map(message_view).collect(),
    }
}

/// 由服务层消息视图构造前端消息视图。
fn message_view(m: &ramaria_service::SessionMessageView) -> MessageView {
    MessageView {
        id: m.id.to_string(),
        role: m.role.as_str().to_string(),
        content: m.content.clone(),
        persona_uid: m.persona_uid.clone(),
        created_at: m.created_at,
        is_proactive: m.is_proactive,
        sender_ref: m.sender_ref.clone(),
        sender_name: m.sender_name.clone(),
    }
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
    let page = state
        .engine
        .session_list(ramaria_service::SessionBrowseRequest {
            limit: None,
            offset: None,
        })
        .await
        .map_err(|e| format!("查询会话列表失败: {}", e))?;

    let summaries: Vec<SessionSummary> = page.items.iter().map(summary_from_view).collect();

    tracing::debug!(count = summaries.len(), "list_sessions 完成");
    Ok(summaries)
}

// =========================================================
// get_session — 获取会话详情（含消息）
// =========================================================

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

    // 会话元数据与消息页由服务层一次读取（存在性校验含在内）
    let detail = state
        .engine
        .session_detail(sid, limit, offset)
        .await
        .map_err(|e| {
            if e.category() == "validation" {
                format!("会话不存在: {}", session_id)
            } else {
                format!("查询会话失败: {}", e)
            }
        })?;

    tracing::debug!(
        session_id = %session_id,
        message_count = detail.messages.len(),
        total_messages = detail.total_messages,
        has_more = detail.has_more,
        "get_session 完成"
    );

    Ok(detail_from_view(&detail))
}

// =========================================================
// create_session — 创建新会话
// =========================================================

/// 创建一个新的空白会话。
///
/// 参数:
/// - `persona_uid`: 绑定的人格 UID（None 表示暂不绑定，发送消息时由
///   会话定位回写绑定）。
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
        .engine
        .create_session(persona_uid.as_deref())
        .await
        .map_err(|e| format!("创建会话失败: {}", e))?;

    tracing::info!(session_id = %session.id, persona_uid = ?session.persona_uid, "新会话已创建");

    Ok(summary_from(&session, Some(0)))
}

// =========================================================
// mark_session_read — 标记会话已读
// =========================================================

/// 标记会话已读（把该会话的未读计数清零）。
///
/// 参数:
/// - `app_handle`: Tauri AppHandle（成功后刷新托盘未读徽标）。
/// - `session_id`: 会话 UUID 字符串。
///
/// 返回:
/// - 成功返回 "ok"。
#[tauri::command]
#[tracing::instrument(skip(app_handle, state))]
pub async fn mark_session_read(
    app_handle: AppHandle,
    state: State<'_, DesktopState>,
    session_id: String,
) -> Result<String, String> {
    let sid = Uuid::parse_str(&session_id).map_err(|e| format!("无效的会话 ID: {}", e))?;

    state
        .engine
        .session_mark_read(sid)
        .await
        .map_err(|e| format!("标记会话已读失败: {}", e))?;

    tracing::debug!(session_id = %session_id, "mark_session_read 完成");
    crate::tray::spawn_tray_refresh(app_handle);
    Ok("ok".to_string())
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 摘要映射：来源通道与外部标识透传（v2.1 桌面来源标注的数据源）。
    #[test]
    fn summary_maps_channel_and_external_ref() {
        let mut session = ramaria_core::types::Session::new();
        session.persona_uid = Some("rama-0001".to_string());
        session.channel = "mcp".to_string();
        session.external_ref = Some("client-A".to_string());

        let summary = summary_from(&session, Some(3));
        assert_eq!(summary.channel, "mcp", "通道应透传（前端据此标注来源）");
        assert_eq!(summary.external_ref.as_deref(), Some("client-A"));
        assert_eq!(summary.message_count, 3);
        assert_eq!(summary.unread, 0, "新建会话路径未读应恒为 0");
        assert_eq!(summary.persona_uid.as_deref(), Some("rama-0001"));

        // 本地会话：通道 local、无外部标识；消息数缺失按 0 处理
        let local = ramaria_core::types::Session::new();
        let summary = summary_from(&local, None);
        assert_eq!(summary.channel, "local");
        assert!(summary.external_ref.is_none());
        assert_eq!(summary.message_count, 0);
    }

    /// 摘要映射：服务层视图（UTC 时间）转换为毫秒时间戳，字段逐项透传。
    #[test]
    fn summary_from_view_maps_fields_and_timestamps() {
        use chrono::{DateTime, Utc};

        let session_id = Uuid::new_v4();
        let started =
            DateTime::<Utc>::from_timestamp_millis(1_700_000_000_000).expect("合法毫秒时间戳");
        let ended =
            DateTime::<Utc>::from_timestamp_millis(1_700_000_060_000).expect("合法毫秒时间戳");
        let view = ramaria_service::SessionSummaryView {
            id: session_id,
            started_at: started,
            ended_at: Some(ended),
            persona_uid: Some("char-0001".to_string()),
            channel: "local".to_string(),
            external_ref: None,
            message_count: 7,
            unread: 4,
        };

        let summary = summary_from_view(&view);
        assert_eq!(summary.id, session_id.to_string());
        assert_eq!(summary.started_at, 1_700_000_000_000);
        assert_eq!(summary.ended_at, Some(1_700_000_060_000));
        assert_eq!(summary.message_count, 7);
        assert_eq!(summary.unread, 4, "未读数应透传");
        assert_eq!(summary.persona_uid.as_deref(), Some("char-0001"));
        assert_eq!(summary.channel, "local");
        assert!(summary.external_ref.is_none());

        // 未关闭会话：ended_at 为 None 保持
        let open = ramaria_service::SessionSummaryView {
            ended_at: None,
            ..view
        };
        assert!(summary_from_view(&open).ended_at.is_none());
    }

    /// 详情映射：服务层视图（UTC 时间）转换为毫秒时间戳，消息字段逐项透传。
    #[test]
    fn detail_from_view_maps_fields_and_messages() {
        use chrono::{DateTime, Utc};
        use ramaria_core::types::{MessageRole, MessageSource};

        let session_id = Uuid::new_v4();
        let started =
            DateTime::<Utc>::from_timestamp_millis(1_700_000_000_000).expect("合法毫秒时间戳");
        let ended =
            DateTime::<Utc>::from_timestamp_millis(1_700_000_060_000).expect("合法毫秒时间戳");
        let view = ramaria_service::SessionDetailView {
            id: session_id,
            started_at: started,
            ended_at: Some(ended),
            persona_uid: Some("char-0001".to_string()),
            total_messages: 9,
            has_more: true,
            messages: vec![ramaria_service::SessionMessageView {
                id: Uuid::new_v4(),
                role: MessageRole::User,
                content: "你好".to_string(),
                created_at: 1_700_000_030_000,
                source: MessageSource::Local,
                persona_uid: Some("char-0001".to_string()),
                is_proactive: true,
                sender_ref: None,
                sender_name: None,
            }],
        };

        let detail = detail_from_view(&view);
        assert_eq!(detail.id, session_id.to_string());
        assert_eq!(detail.started_at, 1_700_000_000_000);
        assert_eq!(detail.ended_at, Some(1_700_000_060_000));
        assert_eq!(detail.persona_uid.as_deref(), Some("char-0001"));
        assert_eq!(detail.total_messages, 9);
        assert!(detail.has_more);
        assert_eq!(detail.messages.len(), 1);
        let msg = &detail.messages[0];
        assert_eq!(msg.id, view.messages[0].id.to_string());
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "你好");
        assert_eq!(msg.persona_uid.as_deref(), Some("char-0001"));
        assert_eq!(msg.created_at, 1_700_000_030_000);
        assert!(msg.is_proactive, "主动标记应透传");

        // 未关闭会话：ended_at 为 None 保持；全量加载 has_more false
        let open = ramaria_service::SessionDetailView {
            ended_at: None,
            has_more: false,
            total_messages: 0,
            messages: Vec::new(),
            ..view
        };
        let detail = detail_from_view(&open);
        assert!(detail.ended_at.is_none());
        assert!(!detail.has_more);
        assert!(detail.messages.is_empty());
    }
}
