//! crates/ramaria-service/src/types/session.rs - Ramaria 会话读取与浏览用例数据结构
//!
//! 设计特点:
//! - 摘要 / 历史 / 列表 / 消息视图对齐 session 与 chat_history 契约
//! - 分页请求的归一化在 impl 内完成，缺省值取 defaults 常量
//! - 时间字段对外统一 ISO-8601 UTC 字符串；毫秒时间戳由用例层在映射时转换
//! - 通道概览按通道聚合活跃会话数与最近活动时间

use chrono::{DateTime, Utc};
use ramaria_core::types::{MessageRole, MessageSource};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::defaults::DEFAULT_HISTORY_LIMIT;

// =========================================================
// 会话读取用例（session 摘要 / chat_history）
// =========================================================

/// 会话摘要视图（会话列表条目，带来源通道）。
///
/// 字段约定:
/// - `channel` / `external_ref`: 会话来源通道与外部对话标识（用于来源标注与续写定位）。
/// - `message_count`: 会话消息条数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummaryView {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub persona_uid: Option<String>,
    pub channel: String,
    pub external_ref: Option<String>,
    pub message_count: u32,
}

/// 会话历史请求（`chat_history` 入参）。
///
/// 字段约定:
/// - `session_id` 与 `persona` 二选一；同时提供时以 `session_id` 优先。
/// - `limit`: 每页条数，缺省 [`DEFAULT_HISTORY_LIMIT`]。
/// - `offset`: 分页偏移（第一页为 0）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryRequest {
    pub session_id: Option<Uuid>,
    pub persona: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

impl HistoryRequest {
    /// 归一化分页条数：缺省或 0 时用 [`DEFAULT_HISTORY_LIMIT`]。
    pub fn effective_limit(&self) -> u32 {
        match self.limit {
            Some(n) if n > 0 => n,
            _ => DEFAULT_HISTORY_LIMIT,
        }
    }

    /// 归一化分页偏移（缺省 0）。
    pub fn effective_offset(&self) -> u32 {
        self.offset.unwrap_or(0)
    }
}

/// 历史消息视图（`chat_history` 条目）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryMessageView {
    pub role: MessageRole,
    pub content: String,
    pub time: DateTime<Utc>,
    /// 发言人标识（助手消息为对应人格；用户消息通常为 None）
    pub persona_uid: Option<String>,
}

/// 会话历史结果（`chat_history` 返回）。
///
/// 字段约定:
/// - `session_id`: 实际读取的会话（按 persona 查询时为该 persona 最近会话）；无数据时 None。
/// - `total`: 该会话 / 该 persona 的消息总条数（分页前的上限参考）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryResult {
    pub session_id: Option<Uuid>,
    pub messages: Vec<HistoryMessageView>,
    pub total: usize,
}

// =========================================================
// 会话浏览用例（会话列表 / 会话消息）
// =========================================================

/// 会话列表浏览请求。
///
/// 字段约定:
/// - `limit`: 返回条数上限（None = 全部；Some(0) 按下界 1 处理）。
/// - `offset`: 分页偏移（缺省 0）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionBrowseRequest {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// 会话列表浏览响应。
///
/// 字段约定:
/// - `items`: 会话摘要（按开始时间倒序；消息计数聚合失败时计数按 0 处理）。
/// - `total`: 分页前的会话数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionBrowsePage {
    pub items: Vec<SessionSummaryView>,
    pub total: usize,
}

/// 会话消息浏览请求。
///
/// 字段约定:
/// - `session_id`: 目标会话。
/// - `limit`: 每页条数（None = 全量加载，时间正序；Some 走最新在前分页后翻正）。
/// - `offset`: 分页偏移（仅分页路径生效，负数按 0 处理）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessagesRequest {
    pub session_id: Uuid,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// 会话消息浏览响应。
///
/// 字段约定:
/// - `total`: 会话消息总数（与分页无关）。
/// - `has_more`: 是否还有更早的消息未返回（仅分页路径有效；全量加载恒为 false）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessagesView {
    pub session_id: Uuid,
    pub total: u32,
    pub has_more: bool,
    pub messages: Vec<SessionMessageView>,
}

/// 会话详情视图（会话元数据 + 消息页）。
///
/// 字段约定:
/// - `started_at` / `ended_at`: 会话起止时间（UTC；未关闭时 `ended_at` 为 None）。
/// - `total_messages`: 会话消息总数（与分页无关）。
/// - `has_more`: 是否还有更早的消息未返回（全量加载恒为 false）。
/// - `messages`: 消息页（时间正序）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionDetailView {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub persona_uid: Option<String>,
    pub total_messages: u32,
    pub has_more: bool,
    pub messages: Vec<SessionMessageView>,
}

/// 消息浏览条目视图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessageView {
    pub id: Uuid,
    pub role: MessageRole,
    pub content: String,
    pub created_at: i64,
    pub source: MessageSource,
    pub persona_uid: Option<String>,
}

/// 通道会话概览视图（通道活动统计）。
///
/// 字段约定:
/// - `active_sessions`: 该通道未关闭会话数；
/// - `last_activity_ms`: 该通道最近一条消息时间（Unix 毫秒）；无活动为 None。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelOverviewView {
    pub active_sessions: i64,
    pub last_activity_ms: Option<i64>,
}
