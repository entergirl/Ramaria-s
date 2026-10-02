//! crates/ramaria-service/src/types/recall.rs - Ramaria 召回用例数据结构
//!
//! 设计特点:
//! - 请求 / 条目 / 统计 / 结果对齐 memory_recall 契约
//! - 分层选择与条目上限、文本预算的归一化在 impl 内完成，缺省值取 defaults 常量
//! - 条目分值与时间可缺省：概览模式（时间线排序）无分值时输出 None
//! - 统计按检索通道记录命中数（vector / bm25 / keyword / graph），供诊断与调试

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::chat::ChatTurn;
use super::defaults::{DEFAULT_MAX_CHARS, DEFAULT_MAX_ITEMS, MAX_ITEMS_LIMIT};

// =========================================================
// 召回用例（memory_recall）
// =========================================================

/// 召回分层选择器（`memory_recall.include`）。
///
/// 职责:
/// - 控制哪些记忆层参与 `context` 装配与 `items` 返回。
/// - 与注入优先级对应：行为 > 知识 > 表达（风格/原文） > 脉络 > 记忆。
///
/// 格式:
/// - 序列化为小写字符串：`l1` / `l2` / `l3` / `knowledge` / `behavior` / `style` /
///   `narrative` / `raw`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RecallLayer {
    /// L1 会话摘要（记忆类）
    L1,
    /// L2 事件（记忆类）
    L2,
    /// L3 性格画像（记忆类）
    L3,
    /// 知识事实卡片
    Knowledge,
    /// 行为规则
    Behavior,
    /// 说话风格规则（表达类）
    Style,
    /// 近期对话脉络 / 跨会话桥接
    Narrative,
    /// utt 原文块（最高敏感层；受 `allow_raw_text` 约束，默认关闭）
    Raw,
}

impl RecallLayer {
    /// 返回分层的稳定字符串标识（小写，与 JSON 约定一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::L1 => "l1",
            Self::L2 => "l2",
            Self::L3 => "l3",
            Self::Knowledge => "knowledge",
            Self::Behavior => "behavior",
            Self::Style => "style",
            Self::Narrative => "narrative",
            Self::Raw => "raw",
        }
    }
}

/// 召回模式。
///
/// 状态:
/// - `Search`: 按 `query` / 对话片段检索（默认路径）。
/// - `Overview`: 未提供 `query` 时按时间线返回最近记忆（概览路径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RecallMode {
    #[default]
    Search,
    Overview,
}

impl RecallMode {
    /// 返回模式的稳定字符串标识（小写，与 JSON 约定一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Overview => "overview",
        }
    }
}

/// 召回请求（`memory_recall` 入参）。
///
/// 字段约定:
/// - `messages`: 最近对话片段（建议 3~5 轮），最后一条为当前输入。
/// - `persona`: 目标人格 uid，缺省 [`DEFAULT_PERSONA_UID`](crate::types::DEFAULT_PERSONA_UID)。
/// - `query`: 检索词；为空时进入概览模式（见 [`RecallMode::Overview`]）。
/// - `include`: 分层选择，缺省 [`RecallRequest::DEFAULT_INCLUDE`]。
/// - `max_items`: 条目上限（1~[`MAX_ITEMS_LIMIT`]），缺省 [`DEFAULT_MAX_ITEMS`]。
/// - `max_chars`: `context` 文本预算（字符），缺省 [`DEFAULT_MAX_CHARS`]。
/// - `conversation_id`: 外部对话标识（数据属性）；**当前仅透传，未参与检索去重**——
///   "当前对话库内历史不重复返回"的接线点见 `ramaria-service` 的 recall 模块说明。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecallRequest {
    pub messages: Vec<ChatTurn>,
    pub persona: Option<String>,
    pub query: Option<String>,
    pub include: Option<Vec<RecallLayer>>,
    pub max_items: Option<u32>,
    pub max_chars: Option<u32>,
    pub conversation_id: Option<String>,
}

impl RecallRequest {
    /// 默认召回分层：记忆类（l1/l2）+ 知识 + 脉络。
    ///
    /// 说明:
    /// - 行为与风格默认关闭：外部前端多自带人设，避免与外部 system prompt 冲突。
    /// - 原文块（raw）固定默认关闭：最高敏感层，需显式开启且受配置约束。
    pub const DEFAULT_INCLUDE: &'static [RecallLayer] = &[
        RecallLayer::L1,
        RecallLayer::L2,
        RecallLayer::Knowledge,
        RecallLayer::Narrative,
    ];

    /// 归一化 `max_items`：缺省用默认值，超上限按 [`MAX_ITEMS_LIMIT`] 截断。
    ///
    /// 返回:
    /// - 落入 `1..=MAX_ITEMS_LIMIT` 的条目上限（0 视为缺省）。
    pub fn effective_max_items(&self) -> u32 {
        match self.max_items {
            Some(n) if n > 0 => n.min(MAX_ITEMS_LIMIT),
            _ => DEFAULT_MAX_ITEMS,
        }
    }

    /// 归一化 `max_chars`：缺省或 0 时用 [`DEFAULT_MAX_CHARS`]。
    pub fn effective_max_chars(&self) -> u32 {
        match self.max_chars {
            Some(n) if n > 0 => n,
            _ => DEFAULT_MAX_CHARS,
        }
    }

    /// 归一化分层选择：缺省时用 [`Self::DEFAULT_INCLUDE`]。
    pub fn effective_include(&self) -> Vec<RecallLayer> {
        match &self.include {
            Some(layers) if !layers.is_empty() => layers.clone(),
            _ => Self::DEFAULT_INCLUDE.to_vec(),
        }
    }
}

/// 召回条目（结构化明细）。
///
/// 字段约定:
/// - `layer`: 条目所属分层。
/// - `id`: 条目主键字符串（UUID 表的 uuid / 自增表的十进制 id）。
/// - `text`: 条目文本（概览模式下为摘要 / 事件描述）。
/// - `score`: 融合排序分；概览模式（时间线排序）无分值为 None。
/// - `time`: 条目时间（ISO-8601 UTC）；无时间的条目为 None。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallItem {
    pub layer: RecallLayer,
    pub id: String,
    pub text: String,
    pub score: Option<f64>,
    pub time: Option<DateTime<Utc>>,
}

/// 召回统计（诊断与调试用）。
///
/// 字段约定:
/// - `mode`: 实际生效的召回模式。
/// - `channels`: 各检索通道命中数（键为通道名：vector / bm25 / keyword / graph）。
/// - `truncated`: 是否发生预算截断（`max_items` 或 `max_chars` 触发）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecallStats {
    pub mode: RecallMode,
    pub channels: BTreeMap<String, usize>,
    pub truncated: bool,
}

/// 召回结果（`memory_recall` 返回）。
///
/// 字段约定:
/// - `context`: 按注入优先级装配好的记忆段落文本，可直接拼入外部 system prompt。
/// - `items`: 结构化明细（便于外部自行再加工）。
/// - `stats`: 召回统计。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecallResult {
    pub context: String,
    pub items: Vec<RecallItem>,
    pub stats: RecallStats,
}
