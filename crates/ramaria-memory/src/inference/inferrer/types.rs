//! crates/ramaria-memory/src/inference/inferrer/types.rs - 性格推断配置与中间输出类型
//!
//! 设计特点:
//! - `InferrerConfig` 集中推断温度 / token 上限 / 小样本阈值
//! - Step 1/2/3 的结构化中间输出类型：分类信号 / 一致性分析 / 最终画像
//! - 输出后处理的差异类型（DiffAction）与差异记录（TraitDiff）
//! - 纯数据结构，可独立序列化与测试

use ramaria_core::PersonalityTrait;

/// 推断器配置。
#[derive(Debug, Clone)]
pub struct InferrerConfig {
    /// LLM 生成温度
    pub temperature: f64,
    /// LLM 最大输出 tokens
    pub max_tokens: u32,
    /// 小样本分类的 n_eff 阈值（低于此值附降低确信度声明）
    pub low_evidence_threshold: f64,
    /// 每步最多 token 数
    pub step_max_tokens: u32,
}

impl Default for InferrerConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 2048,
            low_evidence_threshold: 5.0,
            step_max_tokens: 2048,
        }
    }
}

impl From<ramaria_core::config::InferrerConf> for InferrerConfig {
    fn from(conf: ramaria_core::config::InferrerConf) -> Self {
        Self {
            temperature: conf.temperature,
            max_tokens: conf.max_tokens,
            low_evidence_threshold: conf.low_evidence_threshold,
            step_max_tokens: conf.step_max_tokens,
        }
    }
}

// =========================================================
// 结构化中间输出类型
// =========================================================

/// Step 1 输出：单个分类的性格信号。
#[derive(Debug, Clone)]
pub struct CategorySignal {
    /// 分类名
    pub category: String,
    /// 性格信号标签（如"尽责""社交回避"）
    pub signal_label: String,
    /// 支持性证据引用（统计指标摘要）
    pub evidence_citation: String,
    /// 跨领域稳定性预判（"stable"/"contextual"/"uncertain"）
    pub stability_judgment: String,
    /// 有效样本量是否充足
    pub sufficient_evidence: bool,
}

/// Step 2 输出：跨分类一致性分析。
#[derive(Debug, Clone)]
pub struct ConsistencyAnalysis {
    /// 底色候选（跨分类一致的信号标签列表）
    pub base_candidates: Vec<String>,
    /// 主色调候选（最高权重分类的信号标签）
    pub primary_candidates: Vec<String>,
    /// 点缀候选（条件性信号标签列表）
    pub accent_candidates: Vec<String>,
    /// 分析说明
    pub notes: String,
}

/// Step 3 输出：最终性格画像（在解析为 Vec<PersonalityTrait> 前的中间形态）。
#[derive(Debug, Clone)]
pub struct InferredTrait {
    pub layer: String,
    pub trait_label: String,
    pub meaning: String,
    pub not_meaning: Option<String>,
    pub trigger: Option<String>,
    pub suppress: Option<String>,
    pub related: Option<String>,
    pub seq: i32,
    /// LLM 推断的置信度（0.0..1.0）。
    /// 从 LLM JSON 输出中解析，不再统一硬编码 0.5。
    /// 若 LLM 未提供此字段，默认回退为 None（由后处理校准）。
    pub confidence: Option<f64>,
}

/// 完整推断结果。
#[derive(Debug, Clone)]
pub struct InferenceResult {
    /// Step 1 逐分类信号
    pub category_signals: Vec<CategorySignal>,
    /// Step 2 一致性分析
    pub consistency: ConsistencyAnalysis,
    /// Step 3 推断的性格标签
    pub traits: Vec<PersonalityTrait>,
}

// =========================================================
// 输出后处理类型
// =========================================================

/// 推断后处理的差异类型。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DiffAction {
    /// 新增——旧画像中不存在
    Add,
    /// 更新——语义等价但含义变化
    Update,
    /// 废弃——旧 accent 不再有事件支撑
    Deprecate,
    /// 保留——无变化
    Keep,
}

/// 单条 trait 的差异记录。
#[derive(Debug, Clone)]
pub struct TraitDiff {
    /// 差异动作
    pub action: DiffAction,
    /// 新推断的 trait（Add/Update 时有值）
    pub new_trait: Option<PersonalityTrait>,
    /// 被替换的旧 trait ID（Update/Deprecate/Keep 时有值）
    pub old_trait_id: Option<i64>,
    /// 旧 trait 标签（供日志）
    pub old_label: Option<String>,
}

/// 推断后处理结果。
#[derive(Debug, Clone)]
pub struct PostProcessResult {
    /// 需要新增的 trait
    pub to_add: Vec<PersonalityTrait>,
    /// 需要更新的 trait（附带旧 ID）
    pub to_update: Vec<(i64, PersonalityTrait)>,
    /// 需要标记为废弃的 trait ID 列表
    pub to_deprecate: Vec<i64>,
    /// 差异详情
    pub diffs: Vec<TraitDiff>,
}
