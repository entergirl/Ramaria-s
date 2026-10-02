//! crates/ramaria-service/src/types/persona.rs - Ramaria 人格读取与管理用例数据结构
//!
//! 设计特点:
//! - 摘要 / 卡片 / 完整信息视图对齐 persona_list / persona_get 契约
//! - 卡片分段选择器控制装配范围，缺省返回全部分段
//! - 管理视图字段沿用 snake_case 序列化口径（is_active 不输出 camelCase）
//! - 文件导入动作与结果以枚举 / 条目表达，单文件失败不影响其余文件

use ramaria_core::types::{FactTier, PersonaKind, ProfileField, StyleStatsStatus, TraitLayer};
use serde::{Deserialize, Serialize};

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
