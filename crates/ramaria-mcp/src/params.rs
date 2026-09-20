//! crates/ramaria-mcp/src/params.rs - 工具入参（MCP wire 类型）
//!
//! 设计特点:
//! - 入参结构即工具 schema（`schemars::JsonSchema` 派生），字段口径对齐服务层用例请求
//! - wire 类型与服务层类型分离：协议壳负责转换（角色 / 分层 / 分段的枚举映射）
//! - 字段全部可选或有明确默认：非法组合由工具实现给出可操作错误（见 `tools`）
//! - 不暴露 `conversation_id` 等未接线参数（避免"暴露了但无效"的误导）

use ramaria_service::{ChatRole, ChatTurn, PersonaSection, RecallLayer};
use schemars::JsonSchema;
use serde::Deserialize;

// =========================================================
// 对话片段
// =========================================================

/// 消息角色（wire）：仅 `user` / `assistant`。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RoleParam {
    /// 用户发言
    User,
    /// 助手（人格）发言
    Assistant,
}

impl From<RoleParam> for ChatRole {
    fn from(role: RoleParam) -> Self {
        match role {
            RoleParam::User => ChatRole::User,
            RoleParam::Assistant => ChatRole::Assistant,
        }
    }
}

/// 一轮对话消息（wire）。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct MessageParam {
    /// 发言角色：`user` / `assistant`
    pub role: RoleParam,
    /// 消息文本
    pub content: String,
}

impl MessageParam {
    /// 转为服务层对话片段类型。
    pub(crate) fn into_turn(self) -> ChatTurn {
        ChatTurn {
            role: self.role.into(),
            content: self.content,
        }
    }
}

// =========================================================
// 召回分层与人格分段
// =========================================================

/// 召回分层（wire）：与 `memory_recall.include` 契约一致。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LayerParam {
    /// L1 会话摘要
    L1,
    /// L2 事件
    L2,
    /// L3 性格画像
    L3,
    /// 知识事实
    Knowledge,
    /// 行为规则
    Behavior,
    /// 说话风格
    Style,
    /// 近期脉络 / 跨会话桥接
    Narrative,
    /// utt 原文块（受 `allow_raw_text` 约束，默认关闭）
    Raw,
}

impl From<LayerParam> for RecallLayer {
    fn from(layer: LayerParam) -> Self {
        match layer {
            LayerParam::L1 => RecallLayer::L1,
            LayerParam::L2 => RecallLayer::L2,
            LayerParam::L3 => RecallLayer::L3,
            LayerParam::Knowledge => RecallLayer::Knowledge,
            LayerParam::Behavior => RecallLayer::Behavior,
            LayerParam::Style => RecallLayer::Style,
            LayerParam::Narrative => RecallLayer::Narrative,
            LayerParam::Raw => RecallLayer::Raw,
        }
    }
}

/// 人格卡片分段（wire）：与 `persona_get.sections` 契约一致。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SectionParam {
    /// 性格画像
    Traits,
    /// 行为规则
    Behaviors,
    /// 表达风格
    Style,
    /// 知识事实
    Facts,
    /// 数据成熟度
    Maturity,
}

impl From<SectionParam> for PersonaSection {
    fn from(section: SectionParam) -> Self {
        match section {
            SectionParam::Traits => PersonaSection::Traits,
            SectionParam::Behaviors => PersonaSection::Behaviors,
            SectionParam::Style => PersonaSection::Style,
            SectionParam::Facts => PersonaSection::Facts,
            SectionParam::Maturity => PersonaSection::Maturity,
        }
    }
}

// =========================================================
// 各工具入参
// =========================================================

/// `memory_recall` 入参。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RecallParams {
    /// 最近对话片段（建议 3~5 轮），最后一条为当前输入；
    /// 传空数组且不传 `query` 时返回概览（按时间线返回最近记忆）
    pub messages: Vec<MessageParam>,
    /// 目标人格 uid（缺省 `rama-0001`）
    pub persona: Option<String>,
    /// 检索词；为空时按 `messages` 的最后一条用户消息检索
    pub query: Option<String>,
    /// 参与装配的分层；缺省为 `l1` + `l2` + `knowledge` + `narrative`
    pub include: Option<Vec<LayerParam>>,
    /// 条目上限（1~20；缺省取 `[mcp].max_items`）
    pub max_items: Option<u32>,
    /// 上下文文本预算（字符；缺省取 `[mcp].max_chars`）
    pub max_chars: Option<u32>,
}

/// `chat_send` 入参。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ChatSendParams {
    /// 本轮用户消息
    pub message: String,
    /// 回复方人格 uid（缺省 `rama-0001`）
    pub persona: Option<String>,
    /// 复用会话 id（UUID 字符串）；缺省按客户端身份名续写或新建
    pub session_id: Option<String>,
}

/// `chat_ingest` 入参。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct IngestParams {
    /// 一轮或多轮对话（`user` / `assistant`）
    pub messages: Vec<MessageParam>,
    /// 归属人格 uid（缺省 `rama-0001`）
    pub persona: Option<String>,
    /// 外部对话标识；缺省用客户端身份名（同一标识续写同一会话）
    pub conversation_id: Option<String>,
    /// `true` = 该段对话结束，立即触发封存与摘要（受 `[mcp].allow_seal` 约束）
    #[serde(default)]
    pub finalize: bool,
}

/// `persona_get` 入参。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PersonaGetParams {
    /// 目标人格 uid
    pub uid: String,
    /// 需要的分段；缺省返回全部分段
    pub sections: Option<Vec<SectionParam>>,
}

/// `chat_history` 入参。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HistoryParams {
    /// 会话 id（与 `persona` 二选一；同时提供时以本字段为准）
    pub session_id: Option<String>,
    /// 按人格取最近会话（与 `session_id` 二选一）
    pub persona: Option<String>,
    /// 每页条数（缺省 20）
    pub limit: Option<u32>,
    /// 分页偏移（第一页为 0）
    pub offset: Option<u32>,
}
