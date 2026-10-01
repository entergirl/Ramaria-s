//! crates/ramaria-service/src/types.rs - 服务层用例数据结构
//!
//! 设计特点:
//! - 与传输无关的纯数据：不出现 stdio / Tauri / HTTP 概念（工具契约结构体均可 serde 序列化）
//! - 字段口径对齐工具契约（memory_recall / chat_send / chat_ingest / persona_* / chat_history）；
//!   交互入口的流式生成请求（`ChatStreamRequest`）承载配置覆盖与预置上文，不做 serde 序列化
//! - 默认值与边界以常量集中声明，入口层（MCP schema）与用例层共用同一口径，避免双处定义漂移
//! - 时间字段对外统一 ISO-8601 UTC 字符串；毫秒时间戳由用例层在映射时转换
//! - 枚举序列化统一小写，与 MCP 客户端 JSON 约定一致
//! - 结构体仅承载数据，不含行为；业务语义由用例层（engine / recall / ingest 等）实现

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::ChatMessage;
use ramaria_core::types::{
    FactSource, FactStatus, FactTier, LlmProvider, MessageRole, MessageSource, PersonaKind,
    Presentation, ProfileField, StyleStatsStatus, TraitLayer, TraitSource, TraitStatus,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =========================================================
// 默认值与边界常量
// =========================================================

/// 默认目标人格 uid（调用方未指定 persona 时使用）。
pub const DEFAULT_PERSONA_UID: &str = "rama-0001";

/// 召回条目默认上限（`max_items` 缺省值）。
pub const DEFAULT_MAX_ITEMS: u32 = 5;

/// 召回条目上限的硬边界（`max_items` 超过时按此截断）。
pub const MAX_ITEMS_LIMIT: u32 = 20;

/// 召回上下文文本默认预算（`max_chars` 缺省值，单位：字符）。
pub const DEFAULT_MAX_CHARS: u32 = 1200;

/// 会话历史分页默认条数（`chat_history.limit` 缺省值）。
pub const DEFAULT_HISTORY_LIMIT: u32 = 20;

/// MCP 入口的会话通道标识（外部 MCP 客户端产生的会话）。
pub const CHANNEL_MCP: &str = "mcp";

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
/// - `persona`: 目标人格 uid，缺省 [`DEFAULT_PERSONA_UID`]。
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

// =========================================================
// 写入用例（chat_ingest）
// =========================================================

/// 回流写入请求（`chat_ingest` 入参）。
///
/// 字段约定:
/// - `messages`: 一轮或多轮对话（user / assistant）。
/// - `persona`: 归属人格 uid，缺省 [`DEFAULT_PERSONA_UID`]。
/// - `conversation_id`: 外部对话标识；决定会话续写或另起。
/// - `channel`: 会话来源通道（如 [`CHANNEL_MCP`]）。
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

// =========================================================
// 生成用例（chat_send）
// =========================================================

/// 生成请求（`chat_send` 入参）。
///
/// 字段约定:
/// - `message`: 本轮用户消息（必填，空白视为非法）。
/// - `persona`: 回复方人格 uid，缺省 [`DEFAULT_PERSONA_UID`]。
/// - `session_id`: 复用会话；缺省按 `channel` + `conversation_id` 定位（无则新建）。
/// - `conversation_id`: 外部对话标识；决定同一外部对话续写哪个会话。
/// - `channel`: 会话来源通道（如 [`CHANNEL_MCP`]）。
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
/// - `persona`: 回复方人格 uid，缺省 [`DEFAULT_PERSONA_UID`]；会话已绑定人格时以会话归属为准。
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

// =========================================================
// 人格读取用例（persona_list / persona_get）
// =========================================================

/// 人格摘要视图（`persona_list` 条目）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaSummaryView {
    pub uid: String,
    pub name: String,
    pub kind: PersonaKind,
    /// 来源渠道（local / qq / wechat / telegram / manual / network）
    pub source: String,
    pub description: Option<String>,
    pub active: bool,
}

/// 人格卡片分段选择器（`persona_get.sections`）。
///
/// 格式:
/// - 序列化为小写字符串：`traits` / `behaviors` / `style` / `facts` / `maturity`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum PersonaSection {
    /// 性格画像（L3 三层标签）
    Traits,
    /// 行为规则
    Behaviors,
    /// 表达风格（自动风格规则文本）
    Style,
    /// 知识事实
    Facts,
    /// 数据成熟度（各层数据量计数）
    Maturity,
}

/// 人格卡片请求（`persona_get` 入参）。
///
/// 字段约定:
/// - `uid`: 目标人格 uid（必填）。
/// - `sections`: 需要的分段，缺省返回全部分段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaCardRequest {
    pub uid: String,
    pub sections: Option<Vec<PersonaSection>>,
}

impl PersonaCardRequest {
    /// 归一化分段选择：缺省或空列表时返回全部分段。
    pub fn effective_sections(&self) -> Vec<PersonaSection> {
        match &self.sections {
            Some(sections) if !sections.is_empty() => sections.clone(),
            _ => vec![
                PersonaSection::Traits,
                PersonaSection::Behaviors,
                PersonaSection::Style,
                PersonaSection::Facts,
                PersonaSection::Maturity,
            ],
        }
    }
}

/// 性格标签视图（人格卡片 · 性格画像分段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraitView {
    pub layer: TraitLayer,
    pub label: String,
    pub meaning: String,
    /// 浮现条件（Accent 层常用；无则为 None）
    pub trigger: Option<String>,
    pub confidence: f64,
}

/// 行为规则视图（人格卡片 · 行为规则分段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorRuleView {
    pub id: i64,
    /// 情境侧特征文本（由规则的情境特征渲染）
    pub situation: String,
    /// 规则文本（None = 候选规则，仅参数注入）
    pub reaction: Option<String>,
    /// 禁忌 / 注意列表
    pub avoid: Vec<String>,
    pub confidence: f64,
    pub enabled: bool,
}

/// 知识事实视图（人格卡片 · 知识事实分段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactView {
    pub field: ProfileField,
    pub content: String,
    pub tier: FactTier,
    pub confidence: f64,
}

/// 表达风格视图（人格卡片 · 风格分段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StyleView {
    /// 自动风格规则文本（None = 未生成：数据不足 / 无显著项 / 关闭）
    pub rule_text: Option<String>,
    pub status: StyleStatsStatus,
    /// 统计样本量 n_p（消息条数）
    pub sample_count: u32,
}

/// 数据成熟度视图（人格卡片 · 成熟度分段）。
///
/// 用途:
/// - 让外部调用方判断该人格的记忆数据积累程度（是否值得依赖召回结果）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DataMaturityView {
    /// L1 摘要条数
    pub l1_count: usize,
    /// L2 事件条数
    pub event_count: usize,
    /// L3 性格标签条数
    pub trait_count: usize,
    /// 知识事实条数（active）
    pub fact_count: usize,
    /// 对话示例条数
    pub example_count: usize,
}

/// 人格卡片视图（`persona_get` 返回）。
///
/// 字段约定:
/// - `sections` 未选中的分段对应字段为空集合 / None（maturity 为空结构）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaCardView {
    pub uid: String,
    pub name: String,
    pub kind: PersonaKind,
    pub source: String,
    pub description: Option<String>,
    pub active: bool,
    /// 性格画像（L3 三层标签）
    pub traits: Vec<TraitView>,
    /// 行为规则
    pub behaviors: Vec<BehaviorRuleView>,
    /// 表达风格（未选中分段或未统计时为 None）
    pub style: Option<StyleView>,
    /// 知识事实（active）
    pub facts: Vec<FactView>,
    /// 数据成熟度
    pub maturity: DataMaturityView,
}

// =========================================================
// 人格管理用例（全字段列表 / 信息更新 / 文件导入）
// =========================================================

/// 人格完整信息视图。
///
/// 职责:
/// - 供人格管理页展示与编辑使用；与 [`PersonaSummaryView`] 的区别是包含
///   `ref_id` / `avatar` / `config` / `description` / `updated_at` 等完整字段。
///
/// 字段约定:
/// - `kind`: 人格类型的稳定字符串标识（`user` / `rama` / `char` / `anim` / `oc` / `hist`）。
/// - `is_active`: 是否启用（契约字段名为 `is_active`，与摘要视图的 `active` 区分）。
/// - `created_at` / `updated_at`: Unix 毫秒时间戳。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaFullView {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub source: String,
    /// 来源方原始 ID（跨渠道去重用）
    pub ref_id: Option<String>,
    /// 头像 URL 或路径
    pub avatar: Option<String>,
    /// 人格配置内容（全量文本）
    pub config: Option<String>,
    /// 人格简要描述
    pub description: Option<String>,
    /// 是否启用
    pub is_active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 人格信息更新请求（仅用户可编辑字段）。
///
/// 字段约定:
/// - 各字段均为可选：`None` 表示不更新对应字段（沿用库中现值）。
/// - `description`: `Some("")` 表示清空描述（与 `None` 行为不同）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaUpdateRequest {
    pub name: Option<String>,
    pub avatar: Option<String>,
    pub description: Option<String>,
}

/// 人格文件导入动作（单文件结果）。
///
/// 格式:
/// - 序列化为小写字符串：`created` / `updated` / `skipped` / `failed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum PersonaFileAction {
    /// 新建（库中无该 uid 记录）
    Created,
    /// 更新（库中已有记录，按文件内容同步名称与配置）
    Updated,
    /// 跳过（仅创建缺失模式下记录已存在，未做任何写入）
    Skipped,
    /// 失败（读取 / 查询 / 写入错误；不影响其余文件）
    Failed,
}

/// 人格文件导入的单文件结果条目。
///
/// 字段约定:
/// - `uid`: 目标人格 uid（取自文件名 stem；文件名无法解析时为文件名的可读形态）。
/// - `action`: 本次导入对该文件执行的动作。
/// - `message`: 面向调用方的结果消息（成功为摘要，失败含具体原因）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaFileOutcome {
    pub uid: String,
    pub action: PersonaFileAction,
    pub message: String,
}

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
// 首次配置用例（状态机推进与缺项诊断）
// =========================================================

/// 设置检查结果——列出当前还缺哪些配置。
///
/// 职责:
/// - 供设置页 / 配置向导展示"还差什么才能对话"（缺项清单）；
/// - `is_complete` 表示核心配置就绪（可对话）；嵌入模型缺失不影响该结论
///   （向量通道降级，BM25 + 关键词镜像仍可用）。
///
/// 字段约定:
/// - `backend_configured`: `backend_config` 是否已有记录。
/// - `model_selected`: 线上 provider 已填 model_id；本地 provider 视为已选
///   （模型由本地推理服务侧决定，配置层不强制）。
/// - `needs_indexing`: 记忆索引尚未构建（`schema_meta.index_version == 0`；
///   该键缺失时按未构建口径返回 `0`）。
/// - `embedding_available`: 嵌入模型已加载且可用（向量通道就绪）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupStatus {
    pub backend_configured: bool,
    pub model_selected: bool,
    pub needs_indexing: bool,
    pub embedding_available: bool,
}

impl SetupStatus {
    /// 核心配置是否就绪（嵌入模型缺失不影响此结果）。
    pub fn is_complete(&self) -> bool {
        self.backend_configured && self.model_selected && !self.needs_indexing
    }

    /// 缺失项的人类可读描述列表（顺序与配置向导步骤一致）。
    pub fn missing_items(&self) -> Vec<&'static str> {
        let mut items = Vec::new();
        if !self.backend_configured {
            items.push("后端配置未完成（需选择 LLM provider）");
        }
        if !self.model_selected {
            items.push("模型未选择（需指定使用的模型）");
        }
        if self.needs_indexing {
            items.push("记忆索引待构建");
        }
        if !self.embedding_available {
            items.push("嵌入模型未配置（向量检索不可用，BM25+图谱仍可用）");
        }
        items
    }
}

/// 首次配置请求（配置向导提交的后端选择）。
///
/// 字段约定:
/// - `provider`: LLM 服务类型（本地 / 线上）。
/// - `model_id`: 模型标识（本地 provider 允许为空，由本地服务侧决定）。
/// - `base_url`: API 基础地址。
/// - `api_key`: 线上 provider 的 API key；本地 provider 忽略。
///   仅经 OS keychain 落盘，不写入配置表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupRequest {
    pub provider: LlmProvider,
    pub model_id: String,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

// =========================================================
// 模型管理用例（嵌入模型校验 / 读取 / 降级原因）
// =========================================================

/// 嵌入模型校验结果。
///
/// 职责:
/// - 承载"用户指定目录能否作为嵌入模型使用"的判定结果；
/// - 校验不通过（目录缺失 / 加载失败 / 推理失败）时不返回错误，
///   而是以 `valid=false` + `reason` 表达，便于设置页直接展示原因。
///
/// 字段约定:
/// - `valid`: 目录存在、模型可加载且推理可执行时为 true。
/// - `dimension`: 模型可加载时的向量维度；加载失败（无法读出维度）时为 None。
/// - `reason`: 未通过时的可读原因（模型文件缺失 / 推理失败的具体信息）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingValidation {
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl EmbeddingValidation {
    /// 构造失败结果（无维度，附原因）。
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self {
            valid: false,
            dimension: None,
            reason: Some(reason.into()),
        }
    }
}

/// 嵌入模型配置视图（设置页读取当前生效的嵌入模型）。
///
/// 字段约定:
/// - `model_path`: 仅在"未加载但配置中留有路径"时填充（供 UI 预填输入框）；
///   模型已加载时不暴露本地路径（只暴露维度与可用性）。
/// - `valid`: 模型已加载且自测可用；未加载时为 false。
/// - `dimension`: 模型已加载时的向量维度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingModelView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_path: Option<String>,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<usize>,
}

/// 应用处于降级状态时的原因分类。
///
/// 职责:
/// - 供设置页把"降级"翻译成可操作提示（缺嵌入模型 / LLM 不可达）；
/// - 非降级状态下用例返回 None，不产生本枚举值。
///
/// 格式:
/// - 序列化为小写蛇形字符串：`embedding_missing` / `llm_unavailable` /
///   `both_unavailable` / `unknown`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DegradedReason {
    /// 嵌入模型缺失或不可用（向量通道不可用，BM25 + 关键词镜像仍可用）。
    EmbeddingMissing,
    /// LLM provider 不可达。
    LlmUnavailable,
    /// LLM 与嵌入模型同时不可用。
    BothUnavailable,
    /// 其它未知原因（两者均可用但仍处于降级状态）。
    Unknown,
}

// =========================================================
// 记忆浏览用例（L1 / L2 / L3 / 性格画像 / 事实 / 证据链）
// =========================================================

/// L1 记忆浏览请求。
///
/// 字段约定:
/// - `persona`: 目标人格 uid（None = 不过滤；未吸收口径下必填）。
/// - `unabsorbed_only`: false = 按会话收集摘要的统一排序口径（桌面）；
///   true = 只读未吸收摘要（按 persona 全量取回后分页）。
/// - `limit`: 返回条数上限（缺省 200；桌面口径再按 1000 截断）。
/// - `offset`: 分页偏移（缺省 0）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct L1BrowseRequest {
    pub persona: Option<String>,
    pub unabsorbed_only: bool,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// L1 记忆摘要浏览视图。
///
/// 字段约定:
/// - `keywords` / `atmosphere` / `time_period` / `context_json`: 摘要伴随字段（原始可能为空）。
/// - `valence` / `salience`: 情绪效价与情感显著性；`created_at`: 创建时间（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L1MemoryView {
    pub id: Uuid,
    pub session_id: Uuid,
    pub summary: String,
    pub keywords: Option<String>,
    pub atmosphere: Option<String>,
    pub time_period: Option<String>,
    pub context_json: Option<String>,
    pub valence: f64,
    pub salience: f64,
    pub persona_uid: Option<String>,
    pub created_at: i64,
}

/// L1 记忆浏览响应。
///
/// 字段约定:
/// - `total`: 排序后、分页前的条数（调用方据此判断是否还有下一页）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L1BrowsePage {
    pub items: Vec<L1MemoryView>,
    pub total: usize,
}

/// L2 事件浏览请求。
///
/// 字段约定:
/// - `persona`: 目标人格 uid（None = 合并全部人格事件后统一排序）。
/// - `limit`: 返回条数上限（缺省 200，上限 1000）。
/// - `offset`: 分页偏移（缺省 0；仅 persona 口径生效）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct L2BrowseRequest {
    pub persona: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// L2 事件浏览视图。
///
/// 字段约定:
/// - `start` / `end`: 事件起止时间（Unix 毫秒，与 `MemoryEvent` 同口径）；
///   `created_at` 为事件写入时间，两者不同义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L2EventView {
    pub id: i64,
    pub persona_uid: String,
    pub title: String,
    pub summary: String,
    pub keywords: Option<String>,
    pub valence: f64,
    pub confidence: f64,
    pub presentation: Presentation,
    pub share: f64,
    pub attitude: Option<String>,
    pub salience: f64,
    pub created_at: i64,
    /// 事件开始时间（Unix 毫秒）
    pub start: i64,
    /// 事件结束时间（Unix 毫秒）
    pub end: i64,
}

/// L2 事件浏览响应。
///
/// 字段约定:
/// - `total`: 分页前的条数（persona 口径为全量计数；合并口径为各人格取回后的合并条数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L2BrowsePage {
    pub items: Vec<L2EventView>,
    pub total: usize,
}

/// L3 性格标签浏览视图（扁平列表条目）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L3TraitView {
    pub id: i64,
    pub persona_uid: String,
    pub layer: TraitLayer,
    pub label: String,
    pub meaning: String,
    pub confidence: f64,
    pub evidence: f64,
    pub consistency: f64,
    pub status: TraitStatus,
    pub created_at: i64,
}

/// L3 三层性格画像视图（按 base / primary / accent 分组）。
///
/// 字段约定:
/// - 每层仅含生效（Active）标签，层内按 `seq` 升序；无画像时三层均为空数组（非错误）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonalityProfileView {
    pub persona_uid: String,
    /// 底色层
    pub base: Vec<TraitDetailView>,
    /// 主色调层
    pub primary: Vec<TraitDetailView>,
    /// 点缀层
    pub accent: Vec<TraitDetailView>,
}

/// 单条性格标签的详细视图（三层画像展示用，含浮现 / 抑制等伴随字段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraitDetailView {
    pub id: i64,
    pub label: String,
    pub meaning: String,
    pub confidence: f64,
    pub evidence: f64,
    pub consistency: f64,
    pub layer: TraitLayer,
    pub not_meaning: Option<String>,
    pub trigger: Option<String>,
    pub suppress: Option<String>,
    pub related: Option<String>,
    pub seq: i32,
    pub source: TraitSource,
    pub status: TraitStatus,
    pub created_at: i64,
}

/// 画像数据状态视图（数据量指示器）。
///
/// 状态约定:
/// - `insufficient`: 有效样本量 < 5，画像不可信；
/// - `preliminary`: 5 ≤ 有效样本量 < 20，初步画像；
/// - `trusted`: 有效样本量 ≥ 20，画像相对稳定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileStatusView {
    pub persona_uid: String,
    /// 有效样本量（生效标签的 evidence 之和）
    pub n_total_eff: f64,
    /// 生效标签数量
    pub active_trait_count: usize,
    /// 状态标识: "insufficient" / "preliminary" / "trusted"
    pub status: String,
    /// 状态描述文本（供直接展示）
    pub status_text: String,
}

/// 性格标签证据链请求。
///
/// 字段约定:
/// - `persona`: 目标人格 uid（必填；事件与 L1 溯源按此人隔离查询）。
/// - `trait_id`: 目标性格标签 ID（须为正整数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraitEvidenceRequest {
    pub persona: String,
    pub trait_id: i64,
}

/// 完整证据链视图（一条 trait 与其全部支撑 / 矛盾事件的溯源）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraitEvidenceView {
    pub trait_id: i64,
    pub trait_label: String,
    /// 证据总数
    pub total_evidence: usize,
    pub support_count: usize,
    pub contradict_count: usize,
    pub neutral_count: usize,
    /// 证据事件链（按证据创建时间降序；单条查询失败的事件跳过）
    pub evidence_events: Vec<EvidenceEventView>,
}

/// 证据链中的事件视图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceEventView {
    pub event_id: i64,
    pub title: String,
    pub summary: String,
    pub confidence: f64,
    pub valence: f64,
    pub salience: f64,
    pub attitude: Option<String>,
    pub paraphrase: Option<String>,
    pub motives: Option<String>,
    /// 事件关联的 L1 溯源列表（引用不存在的 L1 跳过）
    pub l1_sources: Vec<EvidenceL1SourceView>,
}

/// 证据链中的 L1 溯源视图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceL1SourceView {
    pub l1_id: Uuid,
    pub summary: String,
    /// L1 证据片段（结构化证据线索的文本槽位）
    pub evidence_notes: Vec<String>,
    pub atmosphere: Option<String>,
    pub valence: f64,
    /// L1 对事件的贡献权重
    pub weight: f64,
}

/// 知识事实浏览请求。
///
/// 字段约定:
/// - `persona`: 目标人格 uid（必填）。
/// - `field`: 可选字段过滤（None = 全部字段）。
/// - `limit`: 返回条数上限（None = 全部；默认值由调用点决定）。
/// - `offset`: 分页偏移（缺省 0）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactBrowseRequest {
    pub persona: String,
    pub field: Option<ProfileField>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// 知识事实条目视图（全字段；内容为陈述句，非原文）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactEntryView {
    /// 事实 id
    pub id: i64,
    /// 字段归属人格
    pub persona_uid: String,
    pub field: ProfileField,
    pub content: String,
    /// 来源（event / manual / l1）
    pub source: FactSource,
    /// 生命周期状态（active / superseded / candidate）
    pub status: FactStatus,
    /// 分层（stable / volatile / historical）
    pub tier: FactTier,
    /// 覆盖链：被替换事实 id（沿此可展开历史版本）
    pub version_of: Option<i64>,
    pub confidence: f64,
    /// 关键词（判重 / 检索提示）
    pub keyword_hint: Option<String>,
    /// 来源事件 id
    pub ref_event_id: Option<i64>,
    /// 来源 L1 id
    pub ref_l1_id: Option<Uuid>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 知识事实浏览响应。
///
/// 字段约定:
/// - `total`: 分页前的条数（调用方据此判断是否还有下一页）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactBrowsePage {
    pub items: Vec<FactEntryView>,
    pub total: usize,
}

/// 单条事实详情（含完整版本链）。
///
/// 字段约定:
/// - `versions`: 含自身的完整版本链（链头最早在前）；单版本事实仅含自身。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactDetailView {
    pub fact: FactEntryView,
    pub versions: Vec<FactEntryView>,
}

/// 按字段分组的知识事实视图（含版本链折叠数据）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupedFactsView {
    pub persona_uid: String,
    /// 按字段展示名分组: { field_label: [活跃事实] }
    pub grouped: HashMap<String, Vec<FactEntryView>>,
    /// 版本链查找: { fact_id: [旧→新版本链] }（仅多版本事实入表）
    pub versions: HashMap<i64, Vec<FactEntryView>>,
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

// =========================================================
// 关键词用例（关键词池列表 / 待确认别名 / 别名裁决）
// =========================================================

/// 关键词池词条视图。
///
/// 字段约定:
/// - `status`: 三态字符串（canonical / alias / pending）。
/// - `canonical_id` / `canonical_keyword`: 指向的规范词（规范词自身为 None）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordEntryView {
    pub keyword: String,
    /// 使用次数（自然出现 +1；手工种子为 0）
    pub use_count: i64,
    pub status: String,
    pub canonical_id: Option<i64>,
    pub canonical_keyword: Option<String>,
    pub created_at: i64,
}

/// 关键词池列表视图（三态计数 + 全量词条）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordPoolView {
    pub total: usize,
    pub canonical_count: usize,
    pub alias_count: usize,
    pub pending_count: usize,
    pub keywords: Vec<KeywordEntryView>,
}

/// 待确认别名视图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingAliasView {
    /// 别名词条 rowid（裁决时定位行）
    pub alias_id: i64,
    /// 别名文本
    pub alias: String,
    /// 建议合并到的规范词文本
    pub canonical: String,
    pub created_at: i64,
}

/// 别名裁决动作。
///
/// 格式:
/// - 序列化为小写字符串：`confirm` / `reject`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AliasAction {
    /// 确认合并（pending → alias）
    Confirm,
    /// 驳回晋升（pending → canonical）
    Reject,
}

/// 别名裁决请求。
///
/// 字段约定:
/// - `alias`: 待处理的别名文本（标准化后比较）。
/// - `action`: 裁决动作（确认 / 驳回）。
/// - `already_applied_ok`: confirm 且词条已是 alias 时的处置——true = 幂等成功
///   （不写库，`already_applied` 置位）；false = 报业务校验错误。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AliasResolveRequest {
    pub alias: String,
    pub action: AliasAction,
    pub already_applied_ok: bool,
}

/// 别名裁决结果。
///
/// 字段约定:
/// - `alias`: 标准化后的别名文本。
/// - `canonical_keyword`: confirm 后指向的规范词文本（reject 后为 None）。
/// - `status`: 裁决后的状态（`alias` / `canonical`）。
/// - `already_applied`: true = 本次未写库（目标状态此前已达成，幂等路径）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AliasResolveOutcome {
    pub alias: String,
    pub canonical_keyword: Option<String>,
    pub status: String,
    pub already_applied: bool,
}

/// 关键词 seed 单条结果。
///
/// 字段约定:
/// - `inserted`: true = 本次新插入（use_count 从 0 起）；false = 词条已存在（保持现状）。
/// - `status`: 处理后的词条状态（canonical / alias / pending）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordSeedItem {
    pub keyword: String,
    pub inserted: bool,
    pub status: String,
}

/// 关键词 seed 结果。
///
/// 字段约定:
/// - `seeded` / `skipped`: 新插入 / 已存在跳过的条数之和恒等于 `results.len()`；
/// - `results`: 去重后的逐条结果（保留首次出现顺序）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordSeedOutcome {
    pub seeded: usize,
    pub skipped: usize,
    pub results: Vec<KeywordSeedItem>,
}

/// 关键词别名建议结果。
///
/// 字段约定:
/// - `scanned_tokens`: 参与分析的词条数（关键词池行与内存镜像的使用量按文本合并后）；
/// - `suggestions`: 相似度引擎产出的原始建议数（尚未过滤）；
/// - `inserted`: 本次新登记的待确认别名数；
/// - `skipped`: 因已存在词条 / 已建立状态 / 单条写入落败而跳过的建议数；
/// - `truncated`: 因单次运行登记上限而未处理的建议数；
/// - `message`: 面向入口的汇总提示（计数口径与上述字段一致）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordSuggestionOutcome {
    pub scanned_tokens: usize,
    pub suggestions: usize,
    pub inserted: usize,
    pub skipped: usize,
    pub truncated: usize,
    pub message: String,
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造固定时间（毫秒 → ISO UTC），避免测试依赖当前时钟。
    fn fixed_time(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(ms).expect("合法毫秒时间戳应可转换")
    }

    #[test]
    fn chat_role_serde_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&ChatRole::User).expect("序列化成功"),
            r#""user""#
        );
        assert_eq!(
            serde_json::to_string(&ChatRole::Assistant).expect("序列化成功"),
            r#""assistant""#
        );
        // 内核角色转换（外部两值 → 内核四值）
        assert_eq!(MessageRole::from(ChatRole::User), MessageRole::User);
        assert_eq!(
            MessageRole::from(ChatRole::Assistant),
            MessageRole::Assistant
        );
    }

    #[test]
    fn chat_turn_serde_roundtrip() {
        let turn = ChatTurn {
            role: ChatRole::User,
            content: "最近工作压力有点大".to_string(),
        };
        let json = serde_json::to_string(&turn).expect("序列化成功");
        let back: ChatTurn = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(turn, back);
        // 非法角色（system）应被拒绝：外部消息只允许 user / assistant
        let invalid = r#"{"role":"system","content":"x"}"#;
        assert!(
            serde_json::from_str::<ChatTurn>(invalid).is_err(),
            "system 角色不应通过外部入口类型"
        );
    }

    #[test]
    fn recall_layer_serde_matches_contract() {
        let cases = [
            (RecallLayer::L1, "l1"),
            (RecallLayer::L2, "l2"),
            (RecallLayer::L3, "l3"),
            (RecallLayer::Knowledge, "knowledge"),
            (RecallLayer::Behavior, "behavior"),
            (RecallLayer::Style, "style"),
            (RecallLayer::Narrative, "narrative"),
            (RecallLayer::Raw, "raw"),
        ];
        for (layer, expected) in cases {
            let json = serde_json::to_string(&layer).expect("序列化成功");
            assert_eq!(
                json,
                format!("\"{expected}\""),
                "{layer:?} 应序列化为 {expected}"
            );
            let back: RecallLayer = serde_json::from_str(&json).expect("反序列化成功");
            assert_eq!(layer, back);
            assert_eq!(layer.as_str(), expected);
        }
    }

    #[test]
    fn recall_request_defaults_follow_decisions() {
        let req = RecallRequest::default();
        assert_eq!(req.effective_max_items(), DEFAULT_MAX_ITEMS);
        assert_eq!(req.effective_max_chars(), DEFAULT_MAX_CHARS);
        let include = req.effective_include();
        assert_eq!(
            include,
            vec![
                RecallLayer::L1,
                RecallLayer::L2,
                RecallLayer::Knowledge,
                RecallLayer::Narrative,
            ],
            "默认分层 = 记忆类 + 知识 + 脉络（行为/风格/原文默认关）"
        );
        assert!(!include.contains(&RecallLayer::Behavior));
        assert!(!include.contains(&RecallLayer::Style));
        assert!(!include.contains(&RecallLayer::Raw));
    }

    #[test]
    fn recall_request_clamps_boundaries() {
        let mut req = RecallRequest {
            max_items: Some(0),
            max_chars: Some(0),
            include: Some(Vec::new()),
            ..RecallRequest::default()
        };
        // 0 视为缺省
        assert_eq!(req.effective_max_items(), DEFAULT_MAX_ITEMS);
        assert_eq!(req.effective_max_chars(), DEFAULT_MAX_CHARS);
        assert_eq!(
            req.effective_include(),
            RecallRequest::DEFAULT_INCLUDE.to_vec()
        );

        // 超上限截断
        req.max_items = Some(999);
        assert_eq!(req.effective_max_items(), MAX_ITEMS_LIMIT);

        // 合法值透传
        req.max_items = Some(3);
        req.max_chars = Some(500);
        assert_eq!(req.effective_max_items(), 3);
        assert_eq!(req.effective_max_chars(), 500);
    }

    #[test]
    fn recall_request_serde_roundtrip_full() {
        let req = RecallRequest {
            messages: vec![
                ChatTurn {
                    role: ChatRole::User,
                    content: "昨天说的那个项目怎么样了".to_string(),
                },
                ChatTurn {
                    role: ChatRole::Assistant,
                    content: "还在等需求确认".to_string(),
                },
            ],
            persona: Some("char-0001".to_string()),
            query: Some("项目进度".to_string()),
            include: Some(vec![RecallLayer::L1, RecallLayer::Raw]),
            max_items: Some(8),
            max_chars: Some(2048),
            conversation_id: Some("conv-42".to_string()),
        };
        let json = serde_json::to_string(&req).expect("序列化成功");
        let back: RecallRequest = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(req, back);
    }

    #[test]
    fn recall_result_serde_uses_iso_time_and_lowercase_mode() {
        let result = RecallResult {
            context: "## 相关记忆\n- 用户最近在赶项目".to_string(),
            items: vec![RecallItem {
                layer: RecallLayer::L1,
                id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
                text: "用户最近在赶项目".to_string(),
                score: Some(0.71),
                time: Some(fixed_time(1_756_000_000_000)),
            }],
            stats: RecallStats {
                mode: RecallMode::Search,
                channels: BTreeMap::from([
                    ("vector".to_string(), 4usize),
                    ("bm25".to_string(), 3usize),
                ]),
                truncated: false,
            },
        };
        let json = serde_json::to_string(&result).expect("序列化成功");
        let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
        // 契约口径：layer / mode 小写，time 为 ISO-8601 字符串
        assert_eq!(value["items"][0]["layer"], "l1");
        assert_eq!(value["stats"]["mode"], "search");
        let time = value["items"][0]["time"].as_str().expect("time 应为字符串");
        assert!(
            time.starts_with("2025-"),
            "time 应为 ISO 字符串，实际 {time}"
        );
        assert!(time.ends_with('Z'), "time 应为 UTC 后缀 Z，实际 {time}");
        // 往返一致
        let back: RecallResult = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(result, back);
    }

    #[test]
    fn recall_result_empty_is_serializable() {
        // 空结果（无命中 / 空库）应输出结构完整的空骨架，而非 None
        let result = RecallResult::default();
        let json = serde_json::to_string(&result).expect("序列化成功");
        let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
        assert_eq!(value["context"], "");
        assert_eq!(value["items"].as_array().map(Vec::len), Some(0));
        assert_eq!(value["stats"]["mode"], "search");
        let back: RecallResult = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(result, back);
    }

    #[test]
    fn ingest_types_serde_roundtrip() {
        let req = IngestRequest {
            messages: vec![ChatTurn {
                role: ChatRole::Assistant,
                content: "好的，明天见".to_string(),
            }],
            persona: Some(DEFAULT_PERSONA_UID.to_string()),
            conversation_id: Some("client-A".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: true,
        };
        let json = serde_json::to_string(&req).expect("序列化成功");
        let back: IngestRequest = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(req, back);

        let outcome = IngestOutcome {
            session_id: Uuid::nil(),
            written: 4,
            deduplicated: 1,
            finalized: true,
        };
        let json = serde_json::to_string(&outcome).expect("序列化成功");
        let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
        assert!(
            value["session_id"].is_string(),
            "session_id 应序列化为字符串，实际 {}",
            value["session_id"]
        );
        let back: IngestOutcome = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(outcome, back);
    }

    #[test]
    fn seal_outcome_serde_roundtrip() {
        let outcome = SealOutcome {
            session_id: Uuid::nil(),
            sealed: true,
            l1_count: 2,
        };
        let json = serde_json::to_string(&outcome).expect("序列化成功");
        let back: SealOutcome = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(outcome, back);
        // 未抢到场景：sealed=false 且 l1_count=0（不重复生成）
        let skipped = SealOutcome {
            session_id: Uuid::nil(),
            sealed: false,
            l1_count: 0,
        };
        let json = serde_json::to_string(&skipped).expect("序列化成功");
        let back: SealOutcome = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(skipped, back);
    }

    #[test]
    fn persona_card_sections_normalization() {
        let req = PersonaCardRequest {
            uid: "rama-0001".to_string(),
            sections: None,
        };
        assert_eq!(req.effective_sections().len(), 5, "缺省返回全部分段");

        let req = PersonaCardRequest {
            uid: "rama-0001".to_string(),
            sections: Some(Vec::new()),
        };
        assert_eq!(req.effective_sections().len(), 5, "空列表视为缺省");

        let req = PersonaCardRequest {
            uid: "rama-0001".to_string(),
            sections: Some(vec![PersonaSection::Traits]),
        };
        assert_eq!(req.effective_sections(), vec![PersonaSection::Traits]);
    }

    #[test]
    fn persona_card_view_serde_roundtrip() {
        let card = PersonaCardView {
            uid: "char-0001".to_string(),
            name: "小林".to_string(),
            kind: PersonaKind::Char,
            source: "local".to_string(),
            description: Some("大学同学".to_string()),
            active: true,
            traits: vec![TraitView {
                layer: TraitLayer::Base,
                label: "温和".to_string(),
                meaning: "说话节奏慢，很少打断别人".to_string(),
                trigger: None,
                confidence: 0.82,
            }],
            behaviors: vec![BehaviorRuleView {
                id: 7,
                situation: "被问到工作压力".to_string(),
                reaction: Some("先自嘲一句再聊具体事".to_string()),
                avoid: vec!["直接说教".to_string()],
                confidence: 0.7,
                enabled: true,
            }],
            style: Some(StyleView {
                rule_text: Some("句尾常用「啦」".to_string()),
                status: StyleStatsStatus::Ready,
                sample_count: 320,
            }),
            facts: vec![FactView {
                field: ProfileField::Interests,
                content: "喜欢露营".to_string(),
                tier: FactTier::Stable,
                confidence: 0.9,
            }],
            maturity: DataMaturityView {
                l1_count: 42,
                event_count: 12,
                trait_count: 5,
                fact_count: 8,
                example_count: 20,
            },
        };
        let json = serde_json::to_string(&card).expect("序列化成功");
        let back: PersonaCardView = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(card, back);
        // 嵌套枚举序列化口径
        let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
        assert_eq!(value["kind"], "char");
        assert_eq!(value["traits"][0]["layer"], "base");
        assert_eq!(value["style"]["status"], "ready");
        assert_eq!(value["facts"][0]["tier"], "stable");
        assert_eq!(value["facts"][0]["field"], "interests");
    }

    #[test]
    fn session_summary_view_serde_roundtrip() {
        let view = SessionSummaryView {
            id: Uuid::nil(),
            started_at: fixed_time(1_756_000_000_000),
            ended_at: None,
            persona_uid: Some("rama-0001".to_string()),
            channel: CHANNEL_MCP.to_string(),
            external_ref: Some("client-A".to_string()),
            message_count: 6,
        };
        let json = serde_json::to_string(&view).expect("序列化成功");
        let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
        assert_eq!(value["channel"], "mcp");
        assert!(value["started_at"].as_str().is_some());
        let back: SessionSummaryView = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(view, back);
    }

    #[test]
    fn history_types_serde_roundtrip_and_paging_defaults() {
        let req = HistoryRequest {
            session_id: Some(Uuid::nil()),
            persona: None,
            limit: None,
            offset: None,
        };
        assert_eq!(req.effective_limit(), DEFAULT_HISTORY_LIMIT);
        assert_eq!(req.effective_offset(), 0);
        let json = serde_json::to_string(&req).expect("序列化成功");
        let back: HistoryRequest = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(req, back);

        let result = HistoryResult {
            session_id: Some(Uuid::nil()),
            messages: vec![HistoryMessageView {
                role: MessageRole::Assistant,
                content: "嗯，我在".to_string(),
                time: fixed_time(1_756_000_000_000),
                persona_uid: Some("char-0001".to_string()),
            }],
            total: 1,
        };
        let json = serde_json::to_string(&result).expect("序列化成功");
        let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
        assert_eq!(value["messages"][0]["role"], "assistant");
        let back: HistoryResult = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(result, back);
    }

    #[test]
    fn persona_summary_view_serde_roundtrip() {
        let view = PersonaSummaryView {
            uid: "rama-0001".to_string(),
            name: "Ramaria".to_string(),
            kind: PersonaKind::Rama,
            source: "local".to_string(),
            description: None,
            active: true,
        };
        let json = serde_json::to_string(&view).expect("序列化成功");
        let back: PersonaSummaryView = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(view, back);
    }

    /// 人格管理视图：JSON 字段名与桌面契约逐字对齐（snake_case，`is_active`）。
    #[test]
    fn persona_management_views_serde() {
        let view = PersonaFullView {
            uid: "char-0001".to_string(),
            name: "小林".to_string(),
            kind: "char".to_string(),
            source: "file".to_string(),
            ref_id: Some("qq-123456".to_string()),
            avatar: Some("avatar.png".to_string()),
            config: Some("assistant_name = \"小林\"".to_string()),
            description: Some("大学同学".to_string()),
            is_active: true,
            created_at: 1_000,
            updated_at: 2_000,
        };
        let json = serde_json::to_string(&view).expect("序列化成功");
        let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
        for key in [
            "uid",
            "name",
            "kind",
            "source",
            "ref_id",
            "avatar",
            "config",
            "description",
            "is_active",
            "created_at",
            "updated_at",
        ] {
            assert!(value.get(key).is_some(), "字段 {key} 应存在于 JSON: {json}");
        }
        assert!(value.get("isActive").is_none(), "不应输出 camelCase 字段名");
        let back: PersonaFullView = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(view, back);

        // 更新请求：三个可选字段缺省为 None
        let req = PersonaUpdateRequest::default();
        assert!(req.name.is_none() && req.avatar.is_none() && req.description.is_none());
        let json = serde_json::to_string(&PersonaUpdateRequest {
            name: Some("新名字".to_string()),
            avatar: None,
            description: Some(String::new()),
        })
        .expect("序列化成功");
        assert!(json.contains("\"description\":\"\""), "空描述应可表达清空");

        // 文件导入动作：小写序列化口径
        for (action, expected) in [
            (PersonaFileAction::Created, "created"),
            (PersonaFileAction::Updated, "updated"),
            (PersonaFileAction::Skipped, "skipped"),
            (PersonaFileAction::Failed, "failed"),
        ] {
            let json = serde_json::to_string(&action).expect("序列化成功");
            assert_eq!(json, format!("\"{expected}\""));
            let back: PersonaFileAction = serde_json::from_str(&json).expect("反序列化成功");
            assert_eq!(action, back);
        }

        // 结果条目往返
        let outcome = PersonaFileOutcome {
            uid: "char-0001".to_string(),
            action: PersonaFileAction::Updated,
            message: "已更新 persona: char-0001 (小林)".to_string(),
        };
        let json = serde_json::to_string(&outcome).expect("序列化成功");
        let back: PersonaFileOutcome = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(outcome, back);
    }

    /// 模型管理视图：可选字段缺省时不出现，降级原因按蛇形小写序列化。
    #[test]
    fn model_management_types_serde() {
        // 校验失败：只有 valid + reason，dimension 字段不出现
        let invalid = EmbeddingValidation::invalid("模型目录不存在: /tmp/x");
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&invalid).expect("序列化成功"))
                .expect("JSON 解析成功");
        assert!(value.get("dimension").is_none());
        assert_eq!(value["valid"], false);

        // 校验成功：valid + dimension，reason 字段不出现
        let valid = EmbeddingValidation {
            valid: true,
            dimension: Some(384),
            reason: None,
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&valid).expect("序列化成功"))
                .expect("JSON 解析成功");
        assert!(value.get("reason").is_none());
        assert_eq!(value["dimension"], 384);

        // 已加载模型视图：不暴露本地路径
        let loaded = EmbeddingModelView {
            model_path: None,
            valid: true,
            dimension: Some(1024),
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&loaded).expect("序列化成功"))
                .expect("JSON 解析成功");
        assert!(value.get("model_path").is_none());

        // 降级原因四态序列化口径
        let cases = [
            (DegradedReason::EmbeddingMissing, "embedding_missing"),
            (DegradedReason::LlmUnavailable, "llm_unavailable"),
            (DegradedReason::BothUnavailable, "both_unavailable"),
            (DegradedReason::Unknown, "unknown"),
        ];
        for (reason, expected) in cases {
            let json = serde_json::to_string(&reason).expect("序列化成功");
            assert_eq!(json, format!("\"{expected}\""));
        }
    }

    /// 浏览 / 关键词用例：请求默认值与分页字段形态。
    #[test]
    fn browse_request_defaults() {
        let l1 = L1BrowseRequest::default();
        assert!(l1.persona.is_none());
        assert!(!l1.unabsorbed_only, "L1 默认走按会话收集口径");
        assert_eq!(l1.limit, None);
        assert_eq!(l1.offset, None);

        let l2 = L2BrowseRequest::default();
        assert!(l2.persona.is_none());
        assert_eq!(l2.limit, None);

        let sessions = SessionBrowseRequest::default();
        assert_eq!(sessions.limit, None, "会话列表缺省返回全部");
        assert_eq!(sessions.offset, None);

        let messages = SessionMessagesRequest {
            session_id: Uuid::nil(),
            limit: None,
            offset: None,
        };
        assert!(messages.limit.is_none(), "limit=None 表示全量加载");
    }

    /// 浏览 / 关键词用例：代表性视图 serde 往返与枚举口径。
    #[test]
    fn browse_and_keyword_views_serde_roundtrip() {
        // 别名裁决动作枚举口径（confirm / reject）
        for (action, expected) in [
            (AliasAction::Confirm, "confirm"),
            (AliasAction::Reject, "reject"),
        ] {
            let json = serde_json::to_string(&action).expect("序列化成功");
            assert_eq!(json, format!("\"{expected}\""));
            let back: AliasAction = serde_json::from_str(&json).expect("反序列化成功");
            assert_eq!(action, back);
        }

        // L1 摘要视图往返
        let l1 = L1MemoryView {
            id: Uuid::nil(),
            session_id: Uuid::nil(),
            summary: "用户最近在准备考试".to_string(),
            keywords: Some("考试".to_string()),
            atmosphere: Some("专注".to_string()),
            time_period: Some("夜间".to_string()),
            context_json: None,
            valence: 0.2,
            salience: 0.7,
            persona_uid: Some("char-0001".to_string()),
            created_at: 1_756_000_000_000,
        };
        let json = serde_json::to_string(&l1).expect("序列化成功");
        let back: L1MemoryView = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(l1, back);

        // 会话详情视图往返（UTC 时间与消息条目）
        let detail = SessionDetailView {
            id: Uuid::nil(),
            started_at: fixed_time(1_756_000_000_000),
            ended_at: None,
            persona_uid: Some("char-0001".to_string()),
            total_messages: 1,
            has_more: false,
            messages: vec![SessionMessageView {
                id: Uuid::nil(),
                role: MessageRole::User,
                content: "你好".to_string(),
                created_at: 1_756_000_000_001,
                source: MessageSource::Local,
                persona_uid: Some("char-0001".to_string()),
            }],
        };
        let json = serde_json::to_string(&detail).expect("序列化成功");
        assert!(
            json.contains("\"role\":\"user\""),
            "role 应小写序列化: {json}"
        );
        let back: SessionDetailView = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(detail, back);

        // L2 事件视图往返（presentation 小写序列化）
        let event = L2EventView {
            id: 7,
            persona_uid: "char-0001".to_string(),
            title: "备考冲刺".to_string(),
            summary: "连续几天复习到深夜".to_string(),
            keywords: Some("考试,复习".to_string()),
            valence: -0.1,
            confidence: 0.8,
            presentation: Presentation::Subjective,
            share: 0.5,
            attitude: Some("有点紧张但坚持".to_string()),
            salience: 0.6,
            created_at: 1_756_000_000_000,
            start: 1_756_000_000_000,
            end: 1_756_001_800_000,
        };
        let json = serde_json::to_string(&event).expect("序列化成功");
        assert!(
            json.contains("\"presentation\":\"subjective\""),
            "presentation 应小写序列化: {json}"
        );
        assert!(
            json.contains("\"start\":1756000000000") && json.contains("\"end\":1756001800000"),
            "事件起止时间应随视图序列化: {json}"
        );
        let back: L2EventView = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(event, back);

        // 事实条目视图往返（枚举字段口径）
        let fact = FactEntryView {
            id: 3,
            persona_uid: "char-0001".to_string(),
            field: ProfileField::Interests,
            content: "喜欢露营".to_string(),
            source: FactSource::Manual,
            status: FactStatus::Active,
            tier: FactTier::Stable,
            version_of: None,
            confidence: 0.9,
            keyword_hint: Some("露营".to_string()),
            ref_event_id: None,
            ref_l1_id: None,
            created_at: 1_000,
            updated_at: 2_000,
        };
        let json = serde_json::to_string(&fact).expect("序列化成功");
        assert!(json.contains("\"field\":\"interests\""));
        assert!(json.contains("\"status\":\"active\""));
        assert!(json.contains("\"source\":\"manual\""));
        let back: FactEntryView = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(fact, back);

        // 别名裁决结果（reject 后 canonical_keyword 为 null）
        let outcome = AliasResolveOutcome {
            alias: "职场焦虑".to_string(),
            canonical_keyword: None,
            status: "canonical".to_string(),
            already_applied: false,
        };
        let json = serde_json::to_string(&outcome).expect("序列化成功");
        assert!(json.contains("\"canonical_keyword\":null"));
        let back: AliasResolveOutcome = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(outcome, back);

        // 关键词 seed 结果（逐条 inserted / status 口径）
        let seed = KeywordSeedOutcome {
            seeded: 1,
            skipped: 1,
            results: vec![
                KeywordSeedItem {
                    keyword: "工作压力".to_string(),
                    inserted: true,
                    status: "canonical".to_string(),
                },
                KeywordSeedItem {
                    keyword: "职场焦虑".to_string(),
                    inserted: false,
                    status: "pending".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&seed).expect("序列化成功");
        let back: KeywordSeedOutcome = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(seed, back);
    }
}
