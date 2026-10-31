//! crates/ramaria-service/src/types/ingest.rs - Ramaria 回流写入用例数据结构
//!
//! 设计特点:
//! - 请求 / 结果对齐 chat_ingest 契约（写入条数 / 去重条数 / 封存标记）
//! - 会话归属由 conversation_id 与 channel 决定，落库目标由用例层解析
//! - finalize 语义：true 表示该段对话结束，立即触发封存与摘要
//! - 消息片段复用对话类型（角色仅开放 user / assistant）
//! - 历史写入类型为每条消息携带显式时间，供评测建库与历史回填还原真实时间线

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::chat::{ChatRole, ChatTurn};

// =========================================================
// 写入用例（chat_ingest）
// =========================================================

/// 回流写入请求（`chat_ingest` 入参）。
///
/// 字段约定:
/// - `messages`: 一轮或多轮对话（user / assistant）。
/// - `persona`: 归属人格 uid，缺省 [`DEFAULT_PERSONA_UID`](crate::types::DEFAULT_PERSONA_UID)。
/// - `conversation_id`: 外部对话标识；决定会话续写或另起。
/// - `channel`: 会话来源通道（如 [`CHANNEL_MCP`](crate::types::CHANNEL_MCP)）。
/// - `finalize`: true 表示该段对话结束，立即触发封存与摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestRequest {
    pub messages: Vec<ChatTurn>,
    pub persona: Option<String>,
    pub conversation_id: Option<String>,
    pub channel: String,
    pub finalize: bool,
}

/// 带显式时间的历史消息（评测建库 / 历史回填）。
///
/// 字段约定:
/// - `role`: 发言角色（仅 user / assistant）。
/// - `content`: 消息原始内容（落库文本为 trim 后的非空内容）。
/// - `created_at`: 消息发生时间（Unix 毫秒，必须 > 0）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimedTurn {
    pub role: ChatRole,
    pub content: String,
    pub created_at: i64,
}

/// 历史写入请求（评测建库 / 历史回填）。
///
/// 字段约定:
/// - `messages`: 按发生时间非递减排列的消息序列，逐条时间原样落库；
/// - `persona` / `conversation_id` / `channel`: 与 [`IngestRequest`] 同义；
/// - 无 `finalize`: 历史建库由调用方显式封存，批量写入不逐段触发摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoricalIngestRequest {
    pub messages: Vec<TimedTurn>,
    pub persona: Option<String>,
    pub conversation_id: Option<String>,
    pub channel: String,
}

/// 回流写入结果（`chat_ingest` 返回）。
///
/// 字段约定:
/// - `session_id`: 消息落库的目标会话。
/// - `written`: 实际写入条数。
/// - `deduplicated`: 因指纹去重跳过的条数。
/// - `finalized`: 是否已触发封存（`finalize=true` 且封存成功）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestOutcome {
    pub session_id: Uuid,
    pub written: usize,
    pub deduplicated: usize,
    pub finalized: bool,
}
