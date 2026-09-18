//! crates/ramaria-service/src/types.rs - 服务层用例数据结构
//!
//! 设计特点:
//! - 与传输无关的纯数据：serde 可序列化，不出现 stdio / Tauri / HTTP 概念
//! - 字段口径对齐工具契约（memory_recall / chat_send / chat_ingest / persona_* / chat_history）
//! - 默认值与边界以常量集中声明，入口层（MCP schema）与用例层共用同一口径，避免双处定义漂移
//! - 时间字段对外统一 ISO-8601 UTC 字符串；毫秒时间戳由用例层在映射时转换
//! - 枚举序列化统一小写，与 MCP 客户端 JSON 约定一致
//! - 结构体仅承载数据，不含行为；业务语义由用例层（engine / recall / ingest 等）实现

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ramaria_core::types::{
    FactTier, MessageRole, PersonaKind, ProfileField, StyleStatsStatus, TraitLayer,
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
/// - `conversation_id`: 外部对话标识；提供时用于定位库内该对话历史参与检索去重，
///   不提供则仅按 persona 做记忆召回。
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
        assert_eq!(value["started_at"].as_str().is_some(), true);
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
}
