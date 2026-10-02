//! crates/ramaria-desktop/src/commands/memory_view.rs - 记忆页面视图类型与映射
//!
//! 设计特点:
//! - 承载记忆页面全部前端视图类型（L1/L2 列表、L3 画像、证据链、知识事实、状态）
//! - 视图只暴露前端展示字段，隐藏内部 id 与非展示字段
//! - 服务层视图到前端视图的映射集中在本模块（`TraitDetailView` / `PersonaFactView`）
//! - 类型经 `commands::memory` 重导出，保持 `memory::<View>` 原路径可解析

use serde::Serialize;
use std::collections::HashMap;

// =========================================================
// 前端展示用结构体
// =========================================================

/// L1 记忆摘要视图。
#[derive(Debug, Clone, Serialize)]
pub struct MemoryL1View {
    pub id: String,
    pub session_id: String,
    pub summary: String,
    pub keywords: String,
    pub atmosphere: String,
    pub valence: f64,
    pub salience: f64,
    pub persona_uid: Option<String>,
    pub created_at: i64,
    /// 时间段（清晨/上午/下午/傍晚/夜间/深夜）
    pub time_period: Option<String>,
    /// 分组上下文 JSON，含 chat_partners / message_count 等
    pub context_json: Option<String>,
}

/// L2 事件视图。
#[derive(Debug, Clone, Serialize)]
pub struct MemoryEventView {
    pub id: i64,
    pub persona_uid: String,
    pub title: String,
    pub summary: String,
    pub keywords: String,
    pub valence: f64,
    pub confidence: f64,
    pub presentation: String,
    pub share: f64,
    pub attitude: String,
    pub salience: f64,
    pub created_at: i64,
    /// 事件开始时间（Unix 毫秒）
    pub start: i64,
    /// 事件结束时间（Unix 毫秒）
    pub end: i64,
}

/// Persona 摘要视图。
#[derive(Debug, Clone, Serialize)]
pub struct PersonaView {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub source: String,
    pub is_active: bool,
    pub created_at: i64,
}

/// 知识事实视图（只读展示）。
#[derive(Debug, Clone, Serialize)]
pub struct PersonaFactView {
    /// 事实 id
    pub id: i64,
    /// 字段归属
    pub field: String,
    /// 事实内容（陈述句，非原文）
    pub content: String,
    /// 生命周期状态（active/superseded/candidate）
    pub status: String,
    /// 分层（stable/volatile/historical）
    pub tier: String,
    /// 置信度 0.0..1.0
    pub confidence: f64,
    /// 来源（event/manual/l1）
    pub source: String,
    /// 关键词（判重/检索提示）
    pub keyword_hint: Option<String>,
    /// 覆盖链：被替换事实 id（沿此可展开历史版本）
    pub version_of: Option<i64>,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
}

/// 知识事实查询响应（按 ProfileField 分组的 active 事实 + 版本链）。
#[derive(Debug, Clone, Serialize)]
pub struct FactListView {
    pub persona_uid: String,
    /// 按 field 分组：{ field_label: [PersonaFactView] }
    pub grouped: HashMap<String, Vec<PersonaFactView>>,
    /// 版本链查找：{ fact_id: [旧→新版本链] }（供历史版本折叠展示）
    pub versions: HashMap<i64, Vec<PersonaFactView>>,
}

/// L3 性格画像完整视图——按 base/primary/accent 三层分组。
///
/// 职责:
/// - 供前端 MemoryView L3 Tab 渲染三层分层展示。
/// - 每层包含该层的所有活跃 trait，含完整字段（trigger/suppress 等）。
///
/// 字段约定:
/// - `base`: 底色层 trait 列表（跨情境稳定，2-3 条）
/// - `primary`: 主色调层 trait 列表（日常最突出，1-2 条）
/// - `accent`: 点缀层 trait 列表（特定条件浮现，2-4 条）
#[derive(Debug, Clone, Serialize)]
pub struct PersonalityProfileView {
    /// 所属人格标识
    pub persona_uid: String,
    /// 底色层
    pub base: Vec<TraitDetailView>,
    /// 主色调层
    pub primary: Vec<TraitDetailView>,
    /// 点缀层
    pub accent: Vec<TraitDetailView>,
}

/// 单条性格标签的详细视图——用于三层分层展示。
///
/// 字段约定:
/// - 含 trigger/suppress/not_meaning/related 等前端三层展示所需字段。
/// - 含 evidence 字段（有效证据量），供前端渲染置信度条。
#[derive(Debug, Clone, Serialize)]
pub struct TraitDetailView {
    /// 内部 ID（用于后续 get_trait_evidence 查询）
    pub id: i64,
    /// 标签词，如"温和""幽默"
    pub label: String,
    /// 在此人身上的具体含义
    pub meaning: String,
    /// 聚合置信度 0..1
    pub confidence: f64,
    /// 有效证据量
    pub evidence: f64,
    /// 一致度
    pub consistency: f64,
    /// 所属分层: base / primary / accent
    pub layer: String,
    /// 反向界定——它不是什么
    pub not_meaning: Option<String>,
    /// 浮现条件
    pub trigger: Option<String>,
    /// 抑制条件
    pub suppress: Option<String>,
    /// 与其他性格的关系
    pub related: Option<String>,
    /// 层内排序
    pub seq: i32,
    /// 性格来源
    pub source: String,
    /// 性格状态
    pub status: String,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
}

impl From<ramaria_service::TraitDetailView> for TraitDetailView {
    fn from(t: ramaria_service::TraitDetailView) -> Self {
        Self {
            id: t.id,
            label: t.label,
            meaning: t.meaning,
            confidence: t.confidence,
            evidence: t.evidence,
            consistency: t.consistency,
            layer: t.layer.as_str().to_string(),
            not_meaning: t.not_meaning,
            trigger: t.trigger,
            suppress: t.suppress,
            related: t.related,
            seq: t.seq,
            source: t.source.as_str().to_string(),
            status: t.status.as_str().to_string(),
            created_at: t.created_at,
        }
    }
}

/// 证据链中的 L1 摘要引用视图。
///
/// 职责:
/// - 承载事件溯源链中的 L1 层证据片段。
/// - 包含 evidence_notes（双层摘要中的证据片段层），供前端"展开证据"渲染。
#[derive(Debug, Clone, Serialize)]
pub struct L1SourceView {
    /// L1 摘要 ID（UUID）
    pub l1_id: String,
    /// L1 摘要文本
    pub summary: String,
    /// L1 证据片段（evidence_notes），可能为空数组
    pub evidence_notes: Vec<String>,
    /// L1 会话氛围
    pub atmosphere: Option<String>,
    /// 情绪效价
    pub valence: f64,
    /// L1 对事件的贡献权重
    pub weight: f64,
}

/// 证据链中的事件视图。
///
/// 职责:
/// - 承载 trait→event 证据链中单个事件的详细信息。
/// - 包含事件的完整推断信号（confidence/valence/salience/attitude/paraphrase）。
#[derive(Debug, Clone, Serialize)]
pub struct EventInEvidenceView {
    /// 事件 ID
    pub event_id: i64,
    /// 事件标题
    pub title: String,
    /// 事件摘要
    pub summary: String,
    /// 事实确凿度
    pub confidence: f64,
    /// 情绪效价
    pub valence: f64,
    /// 显著性
    pub salience: f64,
    /// 态度描述
    pub attitude: Option<String>,
    /// 态度的去情境化重述
    pub paraphrase: Option<String>,
    /// 底层动机标注
    pub motives: Option<String>,
    /// 事件所关联的 L1 溯源列表
    pub l1_sources: Vec<L1SourceView>,
}

/// 完整证据链视图——一条 trait 与其所有支撑/矛盾事件的完整溯源。
///
/// 职责:
/// - 供前端"展开证据"按钮渲染完整溯源链。
/// - 链结构: trait → 该 trait 的所有证据记录 → 每条证据的事件 → 事件的所有 L1 溯源 → L1 的 evidence_notes。
#[derive(Debug, Clone, Serialize)]
pub struct TraitEvidenceChainView {
    /// 性格标签 ID
    pub trait_id: i64,
    /// 标签词
    pub trait_label: String,
    /// 证据总数
    pub total_evidence: usize,
    /// 支持性证据数
    pub support_count: usize,
    /// 矛盾性证据数
    pub contradict_count: usize,
    /// 中性证据数
    pub neutral_count: usize,
    /// 按创建时间降序排列的证据事件链
    pub evidence_events: Vec<EventInEvidenceView>,
}

/// 人格画像数据状态视图。
///
/// 职责:
/// - 供前端 MemoryView L3 Tab 顶部渲染数据状态指示器。
/// - 基于有效样本量判定当前画像的可信程度。
///
/// 状态约定:
/// - `insufficient`: 数据不足（n_total_eff < 5），画像不可信，建议继续对话积累数据。
/// - `preliminary`: 初步画像（5 ≤ n_total_eff < 20），画像有一定参考价值但需谨慎。
/// - `trusted`: 可信画像（n_total_eff ≥ 20），画像相对稳定可靠。
#[derive(Debug, Clone, Serialize)]
pub struct ProfileStatusView {
    /// 所属人格标识
    pub persona_uid: String,
    /// 有效样本总量（所有活跃 trait 的 evidence 字段之和）
    pub n_total_eff: f64,
    /// 活跃 trait 数量
    pub active_trait_count: usize,
    /// 状态标识: "insufficient" / "preliminary" / "trusted"
    pub status: String,
    /// 状态描述文本（中文，供前端直接展示）
    pub status_text: String,
}

// =========================================================
// 视图映射
// =========================================================

/// 将服务层事实视图转换为前端视图（不含内部字段）。
pub(super) fn fact_to_view(f: ramaria_service::FactEntryView) -> PersonaFactView {
    PersonaFactView {
        id: f.id,
        field: f.field.as_str().to_string(),
        content: f.content,
        status: f.status.as_str().to_string(),
        tier: f.tier.as_str().to_string(),
        confidence: f.confidence,
        source: f.source.as_str().to_string(),
        keyword_hint: f.keyword_hint,
        version_of: f.version_of,
        created_at: f.created_at,
    }
}
