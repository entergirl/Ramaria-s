//! crates/ramaria-memory/src/inference/stats/config.rs - Ramaria 统计特征提取配置与输出类型模块
//!
//! 设计特点:
//! - 集中定义统计管线所需的全部配置与数据输出载体（纯 struct/enum，不含统计运算）。
//! - StatsConfig: 预过滤策略、代表性事件数量、分组策略与校准权重链开关。
//! - CalibratedWeightConfig: 四因子校准权重链 w_i 各因子行为参数。
//! - EventEnrichment: 校准权重计算的外部增强输入（主题复现/情绪强度/提及频率/来源数）。
//! - CategoryStats / MotiveStats / RepresentativeEvent / StatsSummary / CrossCategoryMetrics: A3/E/A6/A7 输出载体。
//! - AdmissionTrack / ClassifiedEvents: A1 三轨准入的类型与分类结果容器。
//! - EventEnrichment::derive_batch 依赖分类归属提取，故引用 category 模块的 extract_primary_category。
//!
//! 安全约束:
//! - 本文件为零 I/O、零异步的纯类型定义，输入由调用方传入，无隐私数据记录。

use super::category::extract_primary_category;
use ramaria_core::types::MemoryEvent;

// =========================================================
// 配置类型
// =========================================================

/// 统计配置。
///
/// 职责:
/// - 集中管理预过滤策略、代表性事件数量和分组策略参数。
/// - 新增: 校准权重链开关与参数。
///
/// 字段约定:
/// - `confidence_threshold`: 事件置信度门槛，默认 0.6。在 `use_calibrated_weights=true` 时仅用于
///   `prefilter_events` 兼容路径；三轨模式使用 `classify_events`。
/// - `max_representative_events`: 每分类最多选取的代表性事件数，默认 3。
/// - `use_calibrated_weights`: 是否启用四因子校准权重链，默认 true。
/// - `calibrated_weight_config`: 校准权重链参数（仅在 use_calibrated_weights=true 时生效）。
#[derive(Debug, Clone)]
pub struct StatsConfig {
    /// 事件置信度门槛（硬截断），默认 0.6
    pub confidence_threshold: f64,
    /// 每分类最多选取的代表性事件数，默认 3
    pub max_representative_events: usize,
    /// 是否启用校准权重链（默认 true）
    pub use_calibrated_weights: bool,
    /// 校准权重链参数
    pub calibrated_weight_config: CalibratedWeightConfig,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.6,
            max_representative_events: 3,
            use_calibrated_weights: true,
            calibrated_weight_config: CalibratedWeightConfig::default(),
        }
    }
}

// =========================================================
// 校准权重链配置
// =========================================================

/// 校准权重链配置。
///
/// 职责:
/// - 控制四因子校准权重 `w_i = salience_cal × confidence_factor × situation_multiplier × source_support`
///   中各因子的行为参数。
///
/// 字段约定:
/// - `salience_exponent`: salience 校准的指数基底，默认 1.0（不改变 salience 的凸性）。
/// - `recurrence_boost_max`: 主题复现次数带来的最大加成比例，默认 0.30。
/// - `intensity_boost_max`: 情绪强度带来的最大加成比例，默认 0.20。
/// - `mention_boost_max`: 用户提及频率带来的最大加成比例，默认 0.15。
/// - `min_sources_for_full_support`: source_support 达到 1.0 所需的最小独立来源数，默认 3。
/// - `tentative_weight_factor`: tentative 轨道的半权重因子，默认 0.5。
#[derive(Debug, Clone)]
pub struct CalibratedWeightConfig {
    /// salience 校准指数基底
    pub salience_exponent: f64,
    /// 主题复现最大加成
    pub recurrence_boost_max: f64,
    /// 情绪强度最大加成
    pub intensity_boost_max: f64,
    /// 提及频率最大加成
    pub mention_boost_max: f64,
    /// source_support 满额所需来源数
    pub min_sources_for_full_support: usize,
    /// tentative 轨道权重因子
    pub tentative_weight_factor: f64,
}

impl Default for CalibratedWeightConfig {
    fn default() -> Self {
        Self {
            salience_exponent: 1.0,
            recurrence_boost_max: 0.30,
            intensity_boost_max: 0.20,
            mention_boost_max: 0.15,
            min_sources_for_full_support: 3,
            tentative_weight_factor: 0.5,
        }
    }
}

// =========================================================
// 事件增强数据（校准权重计算的外部输入）
// =========================================================

/// 事件增强统计，用于校准权重计算。
///
/// 职责:
/// - 封装从事件元数据或外部查询中提取的增强特征。
/// - 作为 `compute_calibrated_weight` 的参数，与 `MemoryEvent` 配对使用。
///
/// 字段约定:
/// - `topic_recurrence_count`: 同主题复现次数归一化值 [0.0, 1.0]。
/// - `emotional_intensity`: 情绪强度 [0.0, 1.0]，可从 |valence| 或情绪标签推导。
/// - `mention_frequency`: 用户主动提及频率归一化值 [0.0, 1.0]。
/// - `source_count`: 支持该事件的独立 L1 来源数。
#[derive(Debug, Clone, Default)]
pub struct EventEnrichment {
    /// 同主题复现次数归一化值
    pub topic_recurrence_count: f64,
    /// 情绪强度 0.0..1.0
    pub emotional_intensity: f64,
    /// 用户主动提及频率归一化值
    pub mention_frequency: f64,
    /// 独立来源数
    pub source_count: usize,
}

impl EventEnrichment {
    /// 从事件列表中的单个事件派生基础增强数据。
    ///
    /// 派生策略:
    /// - `emotional_intensity = abs(valence)` 作为情绪强度的代理。
    /// - `topic_recurrence_count`、`mention_frequency`、`source_count` 使用默认值。
    ///   外部调用方应覆盖这些字段以提供更精确的值。
    pub fn from_event(event: &MemoryEvent) -> Self {
        Self {
            topic_recurrence_count: 0.0,
            emotional_intensity: event.valence.abs().min(1.0),
            mention_frequency: event.salience, // salience 作为提及频率的代理
            source_count: 1,                   // 默认至少 1 个来源
        }
    }

    /// 为事件批次派生增强数据，按分类统计复现次数。
    ///
    /// 参数:
    /// - `events`: 预过滤后的事件列表。
    ///
    /// 返回:
    /// - 与 events 一一对应的 EventEnrichment 列表。
    pub fn derive_batch(events: &[MemoryEvent]) -> Vec<Self> {
        if events.is_empty() {
            return Vec::new();
        }

        // 按主分类统计事件数
        use std::collections::HashMap;
        let mut category_counts: HashMap<String, usize> = HashMap::new();
        for event in events {
            let cat = extract_primary_category(event);
            *category_counts.entry(cat).or_default() += 1;
        }

        let max_count = category_counts.values().copied().max().unwrap_or(1) as f64;

        events
            .iter()
            .map(|event| {
                let cat = extract_primary_category(event);
                let count = *category_counts.get(&cat).unwrap_or(&1) as f64;
                Self {
                    topic_recurrence_count: (count / max_count).min(1.0),
                    emotional_intensity: event.valence.abs().min(1.0),
                    mention_frequency: event.salience,
                    source_count: 1,
                }
            })
            .collect()
    }
}

// =========================================================
// 统计输出类型
// =========================================================

/// 单分类统计摘要（A3 输出）。
///
/// 职责:
/// - 封装一个关键词分类下的全部加权统计量。
/// - 作为 LLM 推断的逐分类输入。
///
/// 字段约定:
/// - `category`: 主分类标签，如"工作""社交""家庭"，从事件 keywords 的第一个标签提取。
/// - `event_count`: 该分类的原始事件数（仅用于诊断，不参与计算）。
/// - `n_eff`: 加权有效样本量 = Σ w_i（使用校准权重）。
/// - `valence_mean`: 加权平均效价。
/// - `valence_std`: 加权效价标准差。
/// - `valence_positive_ratio`: 正面事件（valence > 0）的加权占比。
/// - `share_mean`: 加权平均分享意愿。
/// - `share_std`: 加权分享意愿标准差。
/// - `presentation_objective_ratio / subjective_ratio / mixed_ratio`: 三种陈述方式的加权占比，和为 1。
/// - `group_weight`: 该分类在全局画像中的相对权重 = (n_eff / 总 n_eff) × 该分类平均 salience。
#[derive(Debug, Clone)]
pub struct CategoryStats {
    /// 主分类标签
    pub category: String,
    /// 原始事件数（仅诊断）
    pub event_count: usize,
    /// 加权有效样本量
    pub n_eff: f64,
    /// 加权平均效价
    pub valence_mean: f64,
    /// 加权效价标准差
    pub valence_std: f64,
    /// 正面事件加权占比（valence > 0）
    pub valence_positive_ratio: f64,
    /// 加权平均分享意愿
    pub share_mean: f64,
    /// 加权分享意愿标准差
    pub share_std: f64,
    /// 客观型加权占比
    pub presentation_objective_ratio: f64,
    /// 主观型加权占比
    pub presentation_subjective_ratio: f64,
    /// 混合型加权占比
    pub presentation_mixed_ratio: f64,
    /// 该分类的全局权重
    pub group_weight: f64,
}

/// 跨分类高阶指标（A6 输出）。
///
/// 职责:
/// - 汇总跨分类的比较性统计特征。
/// - 为 LLM 推断的"区分底色/点缀"提供数值依据。
///
/// 字段约定:
/// - `emotional_stability`: 全局 valence 加权标准差。值越小情绪越平稳。
/// - `narrative_consistency`: 跨分类 presentation 分布相似度的均值（余弦相似度）。
/// - `attitude_contradiction_count`: 态度矛盾指示器。
/// - `share_skewness`: 全局 share 分布的偏度。正值=右偏，负值=左偏。
/// - `share_kurtosis`: 全局 share 分布的峰度。正值=尖峰分布，负值=扁平分布。
#[derive(Debug, Clone)]
pub struct CrossCategoryMetrics {
    /// 全局 valence 加权标准差（情绪稳定性指标）
    pub emotional_stability: f64,
    /// 跨分类 presentation 分布相似度（叙事一致性指标）
    pub narrative_consistency: f64,
    /// 态度矛盾对数量
    pub attitude_contradiction_count: usize,
    /// 全局 share 偏度
    pub share_skewness: f64,
    /// 全局 share 峰度
    pub share_kurtosis: f64,
}

/// 单个动机标签的聚合统计（E 模块——动机维度统计）。
///
/// 职责:
/// - 对同一动机标签下的事件进行加权聚合，产出与 CategoryStats 同构的统计指标。
/// - 为 Phase B Prompt 提供动机维度的量化信息，辅助 LLM 推断动机驱动的性格模式。
///
/// 字段约定:
/// - `motive`: 动机标签名（如 "地位维护"、"自主性"、"归属"）。
/// - `event_count`: 该动机标签出现的原始事件总数。
/// - `n_eff`: 加权有效样本量 = Σ w_i。
/// - `valence_mean`: 加权平均效价。
/// - `valence_std`: 加权效价标准差。
/// - `valence_positive_ratio`: 正面事件（valence > 0）的加权占比。
/// - `share_mean`: 加权平均分享意愿。
/// - `share_std`: 加权分享意愿标准差。
/// - `presentation_objective_ratio / subjective_ratio / mixed_ratio`: 三种陈述方式的加权占比。
/// - `avg_salience`: 该动机标签下事件的平均显著性（算术均值，供排序使用）。
#[derive(Debug, Clone)]
pub struct MotiveStats {
    /// 动机标签
    pub motive: String,
    /// 原始事件数
    pub event_count: usize,
    /// 加权有效样本量
    pub n_eff: f64,
    /// 加权平均效价
    pub valence_mean: f64,
    /// 加权效价标准差
    pub valence_std: f64,
    /// 正面事件加权占比
    pub valence_positive_ratio: f64,
    /// 加权平均分享意愿
    pub share_mean: f64,
    /// 加权分享意愿标准差
    pub share_std: f64,
    /// 客观型加权占比
    pub presentation_objective_ratio: f64,
    /// 主观型加权占比
    pub presentation_subjective_ratio: f64,
    /// 混合型加权占比
    pub presentation_mixed_ratio: f64,
    /// 算术平均显著性（供排序参考）
    pub avg_salience: f64,
}

/// 代表性事件的精简视图（A7 输出）。
///
/// 职责:
/// - 保留事件的核心字段，供 LLM 推断时注入 Prompt 作为具体示例。
/// - 只包含对性格推断有信息价值的字段，不泄露内部 ID。
#[derive(Debug, Clone)]
pub struct RepresentativeEvent {
    /// 事件标题（≤20 字）
    pub title: String,
    /// 事件摘要（2-3 句）
    pub summary: String,
    /// 态度的自然语言原文
    pub attitude: Option<String>,
    /// 情绪效价
    pub valence: f64,
    /// 事件显著性
    pub salience: f64,
    /// 所属分类
    pub category: String,
}

/// 完整统计摘要（新增三轨分布字段）。
///
/// 职责:
/// - 聚合 A1/A3/A6/A7 的全部输出，作为 LLM 推断的完整输入。
/// - 所有字段由 `run_phase_a_stats` 一次性计算。
#[derive(Debug, Clone)]
pub struct StatsSummary {
    /// 输入事件总数（分类前）
    pub total_events_in: usize,
    /// 预过滤后事件数（即 confirmed + tentative，不含 discarded）
    pub total_events_filtered: usize,
    /// confirmed 轨道事件数（confidence ≥ 0.6）
    pub confirmed_count: usize,
    /// tentative 轨道事件数（0.45 ≤ confidence < 0.6）
    pub tentative_count: usize,
    /// discarded 轨道事件数（confidence < 0.45）
    pub discarded_count: usize,
    /// 分类数
    pub category_count: usize,
    /// 按组权重降序排列的逐分类统计
    pub categories: Vec<CategoryStats>,
    /// 跨分类高阶指标
    pub cross_category: CrossCategoryMetrics,
    /// 每分类的代表性事件（按 salience 降序，最多 max_representative_events 条）
    pub representative_events: Vec<RepresentativeEvent>,
    /// 按动机标签的二次分组统计（主分类关键词分组之下的二级聚合）。
    /// 若所有事件的 motives 均为 None 或为空，此字段为空。
    pub motive_stats: Vec<MotiveStats>,
}

// =========================================================
// 准入轨道类型
// =========================================================

/// 事件准入轨道。
///
/// 职责:
/// - 替代单一的 confidence ≥ 0.6 硬截断。
/// - 将事件按置信度分入三个轨道，各轨道有不同的统计参与权重。
///
/// 状态:
/// - `Confirmed`: confidence ≥ 0.6，完整参与 L3 统计，confidence_factor = 1.0。
/// - `Tentative`: 0.45 ≤ confidence < 0.6，以半权重参与候选统计，可跨批次复现提升。
/// - `Discarded`: confidence < 0.45，不参与 L3 统计，但保留在存储中。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionTrack {
    /// 确认事件，完整权重
    Confirmed,
    /// 待定事件，半权重
    Tentative,
    /// 丢弃事件，不参与
    Discarded,
}

impl AdmissionTrack {
    /// 返回该轨道对应的 confidence_factor（用于校准权重链）。
    ///
    /// 返回:
    /// - Confirmed → 1.0
    /// - Tentative → `tentative_factor`（默认 0.5）
    /// - Discarded → 0.0
    pub fn confidence_factor(self, tentative_factor: f64) -> f64 {
        match self {
            Self::Confirmed => 1.0,
            Self::Tentative => tentative_factor,
            Self::Discarded => 0.0,
        }
    }

    /// 返回轨道的简短描述标签。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Tentative => "tentative",
            Self::Discarded => "discarded",
        }
    }
}

/// 三轨分类结果。
///
/// 职责:
/// - 替代 `prefilter_events` 的单一 (filtered, excluded) 返回。
/// - 返回三个独立的轨道列表，供调用方按需组合。
#[derive(Debug, Clone)]
pub struct ClassifiedEvents {
    /// confirmed 轨道（confidence ≥ 0.6）
    pub confirmed: Vec<MemoryEvent>,
    /// tentative 轨道（0.45 ≤ confidence < 0.6）
    pub tentative: Vec<MemoryEvent>,
    /// discarded 事件数（confidence < 0.45，不返回事件本身以节省内存）
    pub discarded_count: usize,
}

impl ClassifiedEvents {
    /// 获取所有参与统计的事件（confirmed + tentative）。
    pub fn active_events(&self) -> Vec<MemoryEvent> {
        let mut all = Vec::with_capacity(self.confirmed.len() + self.tentative.len());
        all.extend(self.confirmed.iter().cloned());
        all.extend(self.tentative.iter().cloned());
        all
    }

    /// 统计参与事件的各类别总数。
    pub fn active_count(&self) -> usize {
        self.confirmed.len() + self.tentative.len()
    }
}
