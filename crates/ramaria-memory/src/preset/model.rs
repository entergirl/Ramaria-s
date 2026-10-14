//! crates/ramaria-memory/src/preset/model.rs - Ramaria 预设解析中间表示模块
//!
//! 设计特点:
//! - 预设文件解析结果的统一中间表示：注册段 / 对话身份 / 自由文本块 / L3 结构化段 / 提示词补丁
//! - 注册段对齐 personas 表可表达列；结构化段对齐 L3 直写目标（traits / facts / examples / rules）
//! - 跳过明细（SkippedEntry）与提示（PresetHint）分离：前者为结构化明细，后者为汇总文本
//! - 纯类型定义，零 I/O，不依赖数据库或异步运行时

use ramaria_core::behavior::BehaviorParams;
use ramaria_core::types::PersonaKind;
use ramaria_core::{ProfileField, TraitLayer};
use serde::{Deserialize, Serialize};

use super::PresetFormat;

// =========================================================
// 来源与提示枚举
// =========================================================

/// kind 来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KindSource {
    /// 显式声明
    Explicit,
    /// 由 uid 前缀推断
    Inferred,
    /// 无 uid 且无显式声明
    Default,
}

/// 跳过条目的所属类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipCategory {
    Traits,
    Facts,
    Examples,
    BehaviorRules,
}

/// 跳过原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// 同 label 跨层重复
    LabelDuplicate,
    /// 条目内容重复
    Duplicate,
    /// 条目非法（缺必填、类型/值域错）
    Invalid,
    /// 超出单层条数上限
    Overflow,
}

/// 提示级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HintLevel {
    Info,
    Warn,
}

/// 解析提示（汇总文本）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetHint {
    /// 提示级别
    pub level: HintLevel,
    /// 稳定提示码（snake_case）
    pub code: String,
    /// 面向用户的简短消息（不含原文全文）
    pub message: String,
}

/// 跳过的条目明细。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedEntry {
    /// 所属类别
    pub category: SkipCategory,
    /// 条目标识（trait label / fact 字段 / 规则首个关键词等，无则 None）
    pub label: Option<String>,
    /// 跳过原因
    pub reason: SkipReason,
}

// =========================================================
// 段落中间表示
// =========================================================

/// [persona] 注册段（解析结果；对齐 personas 表可表达列）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaSection {
    /// 显式（已过格式校验）> 文件名 stem；None = 待自动生成
    pub uid: Option<String>,
    /// 显式 > uid 前缀推断；仅当无 uid 且无显式时为 None
    pub kind: Option<PersonaKind>,
    /// kind 来源
    pub kind_source: KindSource,
    /// 注册名（缺省链兜底后必非空）
    pub name: String,
    /// 头像 URL 或本地路径
    pub avatar: Option<String>,
    /// 人格简要描述（[persona].description 优先，遗留 [identity].description 兜底）
    pub description: Option<String>,
    /// 来源渠道（显式 > 上下文缺省值）
    pub source: String,
}

/// [identity] 对话身份（解析结果）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentitySection {
    /// 对话自称（[identity] 优先 > [persona].name > 文件名 stem；必非空）
    pub assistant_name: String,
    /// 对话侧称呼（空串按未设置）
    pub user_name: Option<String>,
    /// Anim 类作品名
    pub work: Option<String>,
    /// Hist 类时代名
    pub era: Option<String>,
}

/// 单条 trait。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraitEntry {
    /// 该条所在层
    pub layer: TraitLayer,
    /// 标签（必非空）
    pub label: String,
    /// 含义（必非空；超长截断）
    pub meaning: String,
    /// 边界澄清（反向界定）
    pub not_meaning: Option<String>,
    /// 浮现条件
    pub trigger: Option<String>,
    /// 抑制条件
    pub suppress: Option<String>,
    /// 关联标签（逗号分隔）
    pub related: Option<String>,
    /// 动机维度附加语义（不落库，保留于中间表示）
    pub motives: Vec<String>,
}

/// [traits] 三层结构化段。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraitsSection {
    pub base: Vec<TraitEntry>,
    pub primary: Vec<TraitEntry>,
    pub accent: Vec<TraitEntry>,
}

/// 单条 fact。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactEntry {
    /// 画像字段（7 值枚举，解析期已归一校验）
    pub field: ProfileField,
    /// 事实内容（必非空；超长截断）
    pub content: String,
}

/// 单条对话示例。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExampleEntry {
    /// 对方所说（必非空）
    pub user: String,
    /// 人格回复（必非空）
    pub assistant: String,
}

/// 单条行为规则。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleEntry {
    /// 情境关键词（≥1，去重保首现）
    pub keywords: Vec<String>,
    /// 规则文本（None = 候选规则，仅参数注入）
    pub reaction: Option<String>,
    /// 禁忌主题词
    pub avoid: Vec<String>,
    /// 结构化参数（缺省键用默认值，显式键覆盖）
    pub params: BehaviorParams,
}

/// [prompt] 提示词补丁（追加语义）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptPatch {
    /// 角色层末尾追加文本
    pub role_suffix: Option<String>,
    /// 表达层末尾追加文本
    pub style_suffix: Option<String>,
}

/// 预设解析中间表示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedPreset {
    /// 源文件格式
    pub format: PresetFormat,
    /// 注册段
    pub persona: PersonaSection,
    /// 对话身份
    pub identity: IdentitySection,
    /// 自由文本块（保文件序）
    pub blocks: Vec<(String, String)>,
    /// 结构化性格标签
    pub traits: TraitsSection,
    /// 结构化事实
    pub facts: Vec<FactEntry>,
    /// 对话示例
    pub examples: Vec<ExampleEntry>,
    /// 行为规则
    pub rules: Vec<RuleEntry>,
    /// 提示词补丁
    pub prompt: PromptPatch,
    /// ST 卡 personality 原文（供后续批次消费）
    pub personality_raw: Option<String>,
    /// 解析提示（汇总文本）
    pub hints: Vec<PresetHint>,
    /// 跳过条目明细
    pub skipped: Vec<SkippedEntry>,
}
