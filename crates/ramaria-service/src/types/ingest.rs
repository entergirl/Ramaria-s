//! crates/ramaria-service/src/types/ingest.rs - Ramaria 回流写入用例数据结构
//!
//! 设计特点:
//! - 请求 / 结果对齐 chat_ingest 契约（写入条数 / 去重条数 / 封存标记）
//! - 会话归属由 conversation_id 与 channel 决定，落库目标由用例层解析
//! - finalize 语义：true 表示该段对话结束，立即触发封存与摘要
//! - 消息片段复用对话类型（角色仅开放 user / assistant）

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::chat::ChatTurn;

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
