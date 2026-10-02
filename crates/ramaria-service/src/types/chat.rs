//! crates/ramaria-service/src/types/chat.rs - Ramaria 对话与生成用例数据结构
//!
//! 设计特点:
//! - 外部消息角色仅 user / assistant：system / tool 为内部管线概念，不经外部入口
//! - 生成请求与结果口径对齐 chat_send 契约；流式请求供交互入口使用，不做 serde 序列化
//! - 流式请求承载配置覆盖与预置上文（seed_history 不落库，仅进入本轮 prompt 历史段）
//! - 封存结果供 `Engine::seal` 与 `tick_idle` 内部使用

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::ChatMessage;
use ramaria_core::types::MessageRole;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =========================================================
// 对话片段（入口传入 / 返回的消息）
// =========================================================

/// 对话消息角色（外部入口可见范围）。
///
/// 职责:
/// - 限定外部消息只允许 `user` / `assistant` 两种角色。
/// - 与内核 `MessageRole` 区分的边界：`system` / `tool` 是内部管线概念，
///   不通过外部入口传入或回流（避免外部消息污染系统提示与工具调用语义）。
///
/// 格式:
/// - 序列化为小写字符串：`"user"` / `"assistant"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ChatRole {
    User,
    Assistant,
}

impl ChatRole {
    /// 返回角色的稳定字符串标识（小写，与 JSON 约定一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

impl From<ChatRole> for MessageRole {
    /// 转换为内核消息角色（写入 L0 / 组装历史时使用）。
    fn from(role: ChatRole) -> Self {
        match role {
            ChatRole::User => MessageRole::User,
            ChatRole::Assistant => MessageRole::Assistant,
        }
    }
}

/// 一轮对话消息（外部入口传入的对话片段）。
///
/// 字段约定:
/// - `role`: 发言角色（仅 user / assistant）。
/// - `content`: 消息文本（原始内容，不参与日志输出）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
}

// =========================================================
// 生成用例（chat_send）
// =========================================================

/// 生成请求（`chat_send` 入参）。
///
/// 字段约定:
/// - `message`: 本轮用户消息（必填，空白视为非法）。
/// - `persona`: 回复方人格 uid，缺省 [`DEFAULT_PERSONA_UID`](crate::types::DEFAULT_PERSONA_UID)。
/// - `session_id`: 复用会话；缺省按 `channel` + `conversation_id` 定位（无则新建）。
/// - `conversation_id`: 外部对话标识；决定同一外部对话续写哪个会话。
/// - `channel`: 会话来源通道（如 [`CHANNEL_MCP`](crate::types::CHANNEL_MCP)）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatSendRequest {
    pub message: String,
    pub persona: Option<String>,
    pub session_id: Option<Uuid>,
    pub conversation_id: Option<String>,
    pub channel: String,
}

/// 生成结果（`chat_send` 返回）。
///
/// 字段约定:
/// - `reply`: 人格回复全文（未做截断，原样返回）。
/// - `session_id`: 本轮对话所属会话（复用或新建）。
/// - `chars`: 回复字符数（诊断与分页参考）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatSendOutcome {
    pub reply: String,
    pub session_id: Uuid,
    pub chars: usize,
}

/// 流式生成请求（交互入口使用，返回增量事件流）。
///
/// 字段约定:
/// - `message`: 本轮用户消息（必填，空白视为非法）。
/// - `persona`: 回复方人格 uid，缺省 [`DEFAULT_PERSONA_UID`](crate::types::DEFAULT_PERSONA_UID)；会话已绑定人格时以会话归属为准。
/// - `session_id`: 复用会话；缺省时新建会话（交互入口语义，含新会话桥接）。
/// - `seed_history`: 调用方预置上文（时间正序）：不落库、仅进入本轮 prompt 历史段
///   （与库内历史拼接，seed 在前）。
/// - `config_override`: 配置覆盖（档位实验用）；`None` = 引擎生效配置。
#[derive(Debug, Clone)]
pub struct ChatStreamRequest {
    pub message: String,
    pub persona: Option<String>,
    pub session_id: Option<Uuid>,
    pub seed_history: Vec<ChatMessage>,
    pub config_override: Option<Arc<RamariaConfig>>,
}

// =========================================================
// 封存用例（seal / tick_idle）
// =========================================================

/// 封存结果（`Engine::seal` 与 `tick_idle` 内部使用）。
///
/// 字段约定:
/// - `session_id`: 目标会话。
/// - `sealed`: 是否由本次调用抢占并完成封存（false = 已被其他进程/线程封存，或会话不存在）。
/// - `l1_count`: 本次生成的 L1 摘要条数（未抢到时为 0）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SealOutcome {
    pub session_id: Uuid,
    pub sealed: bool,
    pub l1_count: usize,
}
