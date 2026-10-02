//! crates/ramaria-service/src/types/memory_browse.rs - Ramaria 记忆浏览用例数据结构
//!
//! 设计特点:
//! - 覆盖 L1 / L2 / L3 与性格画像、证据链浏览（含分页与计数口径）
//! - 视图字段对齐浏览契约：时间与枚举沿用存储层口径
//! - 空结果以空集合表达，不作为错误
//! - 证据链按 trait 聚合：支撑 / 矛盾 / 中立计数与事件溯源

use ramaria_core::types::{Presentation, TraitLayer, TraitSource, TraitStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =========================================================
// 记忆浏览用例（L1 / L2 / L3 / 性格画像 / 证据链）
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
