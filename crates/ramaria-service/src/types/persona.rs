//! crates/ramaria-service/src/types/persona.rs - Ramaria 人格读取与管理用例数据结构
//!
//! 设计特点:
//! - 摘要 / 卡片 / 完整信息视图对齐 persona_list / persona_get 契约
//! - 卡片分段选择器控制装配范围，缺省返回全部分段
//! - 管理视图字段沿用 snake_case 序列化口径（is_active 不输出 camelCase）
//! - 文件导入动作与结果以枚举 / 条目表达，单文件失败不影响其余文件
//! - 预设导入结果以预览 + 写入统计 + 拆解执行情况表达（导入失败降级如实回传）

use ramaria_core::types::{FactTier, PersonaKind, ProfileField, StyleStatsStatus, TraitLayer};
use ramaria_memory::preset::PresetPreview;
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

// =========================================================
// 预设导入用例（dry-run 预览 / 执行写入）
// =========================================================

/// 预设导入执行结果。
#[derive(Debug, Clone, Serialize)]
pub struct PersonaImportOutcome {
    /// 导入计划预览（与 dry-run 同一裁决数据源；含本次实际拆解产物）
    pub preview: PresetPreview,
    /// 实际写入统计
    pub written: ImportWriteStats,
    /// 拆解执行情况
    pub decompose: DecomposeExecution,
}

/// 导入写入统计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportWriteStats {
    pub traits: usize,
    pub facts: usize,
    pub examples: usize,
    pub behavior_rules: usize,
    /// 写入失败被跳过的条目数
    pub failed: usize,
}

/// 拆解执行情况。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecomposeExecution {
    /// 是否请求（--no-ai 的反面）
    pub requested: bool,
    /// 是否实际执行
    pub executed: bool,
    /// 跳过原因：privacy_unconfirmed / no_input / llm_failed（未跳过为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    pub fact_count: usize,
    pub trait_count: usize,
}

// =========================================================
// 预设导出用例（画像 → 预设文件）
// =========================================================

/// 预设导出结果（`persona export`）。
///
/// 字段约定:
/// - `toml`: 超集 TOML 文本（可被预设导入通道原样再导入）；
/// - `stats`: 导出内容计数（traits / facts / behavior_rules / blocks / 提示词补丁）；
/// - `hints`: 降级说明（如 `personas.config` 不可解析时未含自由文本块 / 提示词补丁）；
///   无降级时序列化省略该键。
///
/// 隐私:
/// - 不含证据链（trait / rule 证据与来源引用）、对话示例与会话原文，属画像级导出。
#[derive(Debug, Clone, Serialize)]
pub struct PersonaExportOutcome {
    pub uid: String,
    pub name: String,
    /// 超集 TOML 文本
    pub toml: String,
    pub stats: PersonaExportStats,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

/// 预设导出内容计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaExportStats {
    pub traits: usize,
    pub facts: usize,
    pub behavior_rules: usize,
    /// `[blocks]` 段数（自 `personas.config` 重放）
    pub blocks: usize,
    /// 是否包含 `[prompt]` 补丁（role_suffix / style_suffix 任一有值）
    pub prompt_patch: bool,
    /// 含入的对话示例条数（默认 0 = 不含示例）
    pub examples_included: usize,
}

// =========================================================
// 提示词预览用例（persona prompt 查看器）
// =========================================================

/// 提示词预览档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptPreviewTier {
    /// 静态段预览（不含检索内容）
    Static,
    /// 全量预览（给定输入，含记忆召回）
    Full,
}

/// 提示词预览结果（`persona prompt` 查看器）。
///
/// 字段约定:
/// - `system_prompt`: 完整渲染文本（静态档=无输入骨架；全量档=含检索注入）；
/// - `memory_context`: RAG 记忆上下文（随请求单独注入；静态档/无命中为 None，序列化时省略）；
/// - `chars` / `tokens`: 对 system_prompt 的度量（字符数 / token 估算）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaPromptPreview {
    pub uid: String,
    pub tier: PromptPreviewTier,
    pub system_prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_context: Option<String>,
    pub chars: usize,
    pub tokens: usize,
}
