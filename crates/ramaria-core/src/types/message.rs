//! crates/ramaria-core/src/types/message.rs - Ramaria 消息与来源数据类型模块
//!
//! 设计特点:
//! - 定义消息来源（本地/线上）与聊天角色枚举
//! - Message 携带来源通道、时间戳与会话归属
//! - MessageKey 用于跨批次/跨会话的导入去重标识
//! - 类型兼容 OpenAI Chat Completions role 语义
//! - 所有类型支持 serde，供 CLI、IPC、存储层共享

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{new_id, now_ms};

// =========================================================
// 消息来源
// =========================================================

/// 消息来源：本地模型或线上 API。
///
/// 职责:
/// - 标记一条消息来自本地 provider 还是线上 provider。
/// - 供隐私提示、日志脱敏和 UI 状态展示使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum MessageSource {
    #[default]
    Local,
    Online,
}

impl std::fmt::Display for MessageSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Online => write!(f, "online"),
        }
    }
}

// =========================================================
// 消息角色
// =========================================================

/// 消息角色枚举。
///
/// 职责:
/// - 表示一条对话消息在聊天协议中的角色。
/// - 与 OpenAI Chat Completions API 兼容。
/// - 预留 `tool` 用于未来工具调用和插件调用结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum MessageRole {
    User,
    Assistant,
    System,
    #[serde(rename = "tool")]
    Tool,
}

impl MessageRole {
    /// 返回 OpenAI API 兼容的小写字符串。
    ///
    /// 返回:
    /// - `user` / `assistant` / `system` / `tool`。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 消息去重键（角色 + 正文）。
///
/// 职责:
/// - 外部入口（MCP / 未来社交通道）回流时的**精确去重输入**：按
///   `(channel, external_ref)` 取回整段对话的键序列，用于
///   ① 重发前缀跳过（新提交头部与库内尾部比对）、② 指纹序数计算
///   （同一 `(role, content)` 在对话内的第几次出现）。
///
/// 语义约定:
/// - `content` 为入库时 trim 后的正文（与 `messages.content` 一致）；
/// - 不含 id / 时间戳：回流客户端不提供时间戳，去重不依赖时间；
///   需要完整消息行（含元数据）时按会话读取（`list_messages` / 分页查询）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageKey {
    /// 消息角色（用户 / 助手 / 系统 / 工具）
    pub role: MessageRole,
    /// 消息正文（已 trim）
    pub content: String,
}

/// L0 原始消息。
///
/// 职责:
/// - 保存用户、助手、系统或工具的原始消息。
/// - 作为 L1 摘要、检索索引和对话历史的事实源。
/// - `persona_uid` 标记发言人，用于 Persona-Aware RAG 的原话过滤。
///
/// 去重:
/// - `fingerprint` 为 SHA-256 前 16 位 hex，用于历史导入去重。
/// - 正常对话产生的消息此字段为 None。
///
/// 字段约定:
/// - `persona_uid`: 发言人标识。系统/助手消息填 None，导入消息填对应发言人的 uid。
/// - `is_proactive`: 主动生成标记。仅主动对话路径写入 true，常规消息恒 false；
///   与 `source` 语义无关。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub session_id: Uuid,
    pub role: MessageRole,
    pub content: String,
    /// 消息创建时间（Unix 毫秒）
    pub created_at: i64,
    pub source: MessageSource,
    /// 导入去重指纹，None 表示正常对话消息
    pub fingerprint: Option<String>,
    /// 发言人标识，系统/助手消息为 None
    pub persona_uid: Option<String>,
    /// 标记由主动对话路径生成的消息；常规消息恒 false；与 source 语义无关
    #[serde(default)]
    pub is_proactive: bool,
    /// 外部平台发送者 ID（导入消息）；本地 / MCP 消息为 None
    #[serde(default)]
    pub sender_ref: Option<String>,
    /// 外部平台发送者显示名（导入消息）；本地 / MCP 消息为 None
    #[serde(default)]
    pub sender_name: Option<String>,
}

impl Message {
    /// 创建一条新消息。
    ///
    /// 参数:
    /// - `session_id`: 消息所属 Session。
    /// - `role`: 消息角色。
    /// - `content`: 原始文本内容。
    /// - `source`: 本地或线上来源。
    ///
    /// 返回:
    /// - 带新 UUID、当前创建时间且无 fingerprint、persona_uid 的消息，`is_proactive` 为 false。
    pub fn new(
        session_id: Uuid,
        role: MessageRole,
        content: String,
        source: MessageSource,
    ) -> Self {
        Self {
            id: new_id(),
            session_id,
            role,
            content,
            created_at: now_ms(),
            source,
            fingerprint: None,
            persona_uid: None,
            is_proactive: false,
            sender_ref: None,
            sender_name: None,
        }
    }

    /// 设置发言人 persona_uid（链式调用）。
    ///
    /// 说明:
    /// - 用户消息通常不设（发言人是用户自己）。
    /// - 助手消息设为当前对话的人格 uid，用于前端显示"谁在回复"。
    pub fn with_persona_uid(mut self, uid: Option<String>) -> Self {
        self.persona_uid = uid;
        self
    }

    /// 设置主动生成标记（链式调用）。
    ///
    /// 说明:
    /// - 仅主动对话路径生成的助手消息设为 true，常规消息保持默认 false。
    /// - 与 `source`（本地/线上 provider）无关：本标记只表示产生方式，
    ///   供 UI 与回流路径区分主动消息，不改变记忆回流口径。
    pub fn with_proactive(mut self, is_proactive: bool) -> Self {
        self.is_proactive = is_proactive;
        self
    }
}
