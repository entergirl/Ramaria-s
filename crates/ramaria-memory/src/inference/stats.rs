//! crates/ramaria-memory/src/inference/stats.rs - 统计特征提取
//!
//! 设计特点:
//! - A1 三轨动态准入: confirmed/tentative/discarded 替代置信度硬截断
//! - 校准权重链: w_i = salience_cal × confidence_factor × situation_multiplier × source_support
//! - salience 校准: 基于主题复现次数、情绪强度和用户提及频率的指数校准
//! - A3 按领域分类聚合: 按 keywords 主分类分组，计算校准加权均值/方差/有效样本量
//! - 情境强度加权: 弱情境(1-2)→×1.5，中性(3)/None→×1.0，强情境(4-5)→×0.5
//! - A6 跨分类高阶指标: 情绪稳定性、叙事一致性、态度矛盾检测、社交开放性
//! - A7 代表性事件选取: 每分类取 salience 最高的 2-3 条事件
//! - 纯数值计算，零 I/O，不依赖数据库或异步运行时，所有输入由调用方传入
//! - 可独立单元测试，无需 mock StorageBackend
//! - 向后兼容: prefilter_events 保留但委托给三轨分类；StatsConfig::default() 行为不变

use ramaria_core::types::{MemoryEvent, Presentation};

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

// =========================================================
// A1: 准入轨道分类
// =========================================================

/// 将单个事件分类到准入轨道。
///
/// 参数:
/// - `event`: 待分类的事件。
///
/// 返回:
/// - `AdmissionTrack` 枚举值。
///
/// 说明:
/// - 边界值处理: confidence == 0.6 → Confirmed, confidence == 0.45 → Tentative。
/// - 负值/NaN 防御: confidence < 0.0 或 NaN → Discarded。
pub fn classify_event(event: &MemoryEvent) -> AdmissionTrack {
    if event.confidence.is_nan() || event.confidence < 0.0 {
        return AdmissionTrack::Discarded;
    }
    if event.confidence >= 0.6 {
        AdmissionTrack::Confirmed
    } else if event.confidence >= 0.45 {
        AdmissionTrack::Tentative
    } else {
        AdmissionTrack::Discarded
    }
}

/// 将事件列表分类到三个准入轨道。
///
/// 参数:
/// - `events`: 完整的事件列表。
///
/// 返回:
/// - `ClassifiedEvents`，包含三个轨道的分类结果。
pub fn classify_events(events: &[MemoryEvent]) -> ClassifiedEvents {
    let mut confirmed = Vec::new();
    let mut tentative = Vec::new();
    let mut discarded_count = 0usize;

    for event in events {
        match classify_event(event) {
            AdmissionTrack::Confirmed => confirmed.push(event.clone()),
            AdmissionTrack::Tentative => tentative.push(event.clone()),
            AdmissionTrack::Discarded => discarded_count += 1,
        }
    }

    ClassifiedEvents {
        confirmed,
        tentative,
        discarded_count,
    }
}

// =========================================================
// A1 兼容: 预过滤（向后兼容，委托给三轨分类）
// =========================================================

/// 预过滤事件：排除 confidence 低于阈值的推测性事件。
///
/// 说明:
/// - 内部委托给 `classify_event`，使用配置中的 `confidence_threshold` 做硬截断。
/// - 当 `use_calibrated_weights=true` 时，调用方应优先使用 `run_phase_a_stats`，
///   它会自动使用三轨分类。
///
/// 参数:
/// - `events`: 完整事件列表。
/// - `config`: 统计配置（使用其中的 confidence_threshold）。
///
/// 返回:
/// - 两次结果：(通过过滤的事件列表, 被排除的事件数)。
pub fn prefilter_events(events: &[MemoryEvent], config: &StatsConfig) -> (Vec<MemoryEvent>, usize) {
    let total = events.len();
    let filtered: Vec<MemoryEvent> = events
        .iter()
        .filter(|e| classify_event(e) != AdmissionTrack::Discarded)
        // 当 use_calibrated_weights=false 时，额外按旧阈值硬截断以保持完全兼容
        .filter(|e| {
            if config.use_calibrated_weights {
                true
            } else {
                e.confidence >= config.confidence_threshold
            }
        })
        .cloned()
        .collect();
    let excluded = total - filtered.len();
    (filtered, excluded)
}

// =========================================================
// A1 扩展: Tentative 跨批次复现自动提升
// =========================================================

/// Tentative 事件跨批次复现自动提升配置。
///
/// 职责:
/// - 控制 tentative 事件自动提升为 confirmed 的条件阈值。
///
/// 字段约定:
/// - `min_cluster_size`: 同一关键词簇中至少需 N 条 tentative 事件才考虑提升，默认 2。
/// - `min_batch_interval_hours`: 判定为"不同批次"的最小时间间隔（小时），默认 6.0。
/// - `keyword_similarity_threshold`: 簇内事件间关键词 Jaccard 相似度阈值，默认 0.4。
/// - `promoted_confidence`: 提升后的置信度值，默认 0.6（刚好进入 confirmed 轨道）。
#[derive(Debug, Clone)]
pub struct TentativePromotionConfig {
    /// 最小簇大小（至少 N 条 tentative 事件在同一关键词簇中）
    pub min_cluster_size: usize,
    /// 不同批次的最小时间间隔（小时）
    pub min_batch_interval_hours: f64,
    /// 关键词 Jaccard 相似度阈值（用于簇内互证）
    pub keyword_similarity_threshold: f64,
    /// 提升后的置信度值
    pub promoted_confidence: f64,
}

impl Default for TentativePromotionConfig {
    fn default() -> Self {
        Self {
            min_cluster_size: 2,
            min_batch_interval_hours: 6.0,
            keyword_similarity_threshold: 0.4,
            promoted_confidence: 0.6,
        }
    }
}

/// Tentative 事件提升结果。
///
/// 职责:
/// - 返回提升后的 confirmed 事件列表和未提升的 tentative 事件列表。
/// - 调用方应将 promoted 事件合并到 confirmed 轨道参与后续 Phase A 统计。
#[derive(Debug, Clone)]
pub struct TentativePromotionResult {
    /// 提升为 confirmed 的事件列表（confidence 已设为 promoted_confidence）
    pub promoted: Vec<MemoryEvent>,
    /// 未提升的 tentative 事件（保持原 confidence，继续以半权重参与统计）
    pub remaining_tentative: Vec<MemoryEvent>,
    /// 被提升的事件数
    pub promoted_count: usize,
    /// 未提升的事件数
    pub remaining_count: usize,
}

/// 计算两个事件的关键词 Jaccard 相似度。
///
/// 公式: J(A,B) = |A ∩ B| / |A ∪ B|
///
/// 参数:
/// - `a_keywords`: 事件 A 的关键词集合。
/// - `b_keywords`: 事件 B 的关键词集合。
///
/// 返回:
/// - Jaccard 相似度 [0.0, 1.0]。任一方关键词为空时返回 0.0。
///
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::jaccard_similarity`。
fn keyword_jaccard(a_keywords: &str, b_keywords: &str) -> f64 {
    let a_set: std::collections::HashSet<&str> = a_keywords
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let b_set: std::collections::HashSet<&str> = b_keywords
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    crate::similarity::jaccard_similarity(a_set, b_set)
}

/// 判断两个事件是否来自不同批次。
///
/// 策略:
/// - 比较 `created_at` 时间戳（Unix 毫秒），差值超过 `min_batch_interval_hours` 小时视为不同批次。
/// - 若任一事件的 `created_at` 为 0（未初始化），保守视为同批次。
///
/// 参数:
/// - `a`: 事件 A。
/// - `b`: 事件 B。
/// - `config`: 提升配置。
///
/// 返回:
/// - true 表示来自不同批次。
fn are_different_batches(
    a: &MemoryEvent,
    b: &MemoryEvent,
    config: &TentativePromotionConfig,
) -> bool {
    if a.created_at == 0 || b.created_at == 0 {
        return false;
    }
    let diff_ms = (a.created_at - b.created_at).abs() as f64;
    let diff_hours = diff_ms / (1000.0 * 3600.0);
    diff_hours >= config.min_batch_interval_hours
}

/// 判断一个 tentative 事件簇是否满足互证条件并应提升。
///
/// 判断标准（所有条件必须同时满足）:
/// 1. 簇大小 ≥ `min_cluster_size`。
/// 2. 至少存在一对事件来自不同批次。
/// 3. 簇内事件对的关键词 Jaccard 相似度均值 ≥ `keyword_similarity_threshold`。
///
/// 参数:
/// - `cluster`: 同一关键词簇中的 tentative 事件。
/// - `config`: 提升配置。
///
/// 返回:
/// - true 表示该簇应被提升。
fn should_promote_cluster(cluster: &[MemoryEvent], config: &TentativePromotionConfig) -> bool {
    if cluster.len() < config.min_cluster_size {
        return false;
    }

    // 条件 2: 至少一对事件来自不同批次
    let has_cross_batch = (0..cluster.len()).any(|i| {
        ((i + 1)..cluster.len()).any(|j| are_different_batches(&cluster[i], &cluster[j], config))
    });

    if !has_cross_batch {
        return false;
    }

    // 条件 3: 簇内关键词 Jaccard 相似度均值 ≥ 阈值
    let mut total_sim = 0.0f64;
    let mut pair_count = 0usize;
    for i in 0..cluster.len() {
        for j in (i + 1)..cluster.len() {
            let a_kw = cluster[i].keywords.as_deref().unwrap_or("");
            let b_kw = cluster[j].keywords.as_deref().unwrap_or("");
            total_sim += keyword_jaccard(a_kw, b_kw);
            pair_count += 1;
        }
    }

    if pair_count == 0 {
        return false;
    }

    let avg_sim = total_sim / pair_count as f64;
    avg_sim >= config.keyword_similarity_threshold
}

/// 对 tentative 事件执行跨批次复现自动提升。
///
/// 算法:
/// 1. 按主分类将 tentative 事件分组为关键词簇。
/// 2. 对每个簇调用 `should_promote_cluster` 判断是否应提升。
/// 3. 满足条件的簇内所有事件 confidence 设为 `promoted_confidence`，归入 promoted。
/// 4. 不满足条件的簇内事件保持原 confidence，归入 remaining_tentative。
///
/// 说明:
/// - 本函数为纯数值逻辑，不执行 I/O。调用方负责将提升后的事件写入存储。
/// - `confirmed` 参数保留以供未来扩展（如与 confirmed 事件做交叉验证），当前版本仅用于签名兼容。
/// - 当 embedding 可用时，调用方可在提升前额外过滤：对 `should_promote_cluster` 返回 true 的簇，
///   使用 `paraphrase` 或 `summary` 字段的 embedding 做余弦相似度验证（> 0.7）。
///
/// 参数:
/// - `tentative`: tentative 轨道的事件列表。
/// - `confirmed`: confirmed 轨道的事件列表（保留供扩展，当前仅用于签名兼容）。
/// - `config`: 提升配置。
///
/// 返回:
/// - `TentativePromotionResult`，包含 promoted 和 remaining 两部分。
pub fn promote_tentative_events(
    tentative: &[MemoryEvent],
    confirmed: &[MemoryEvent],
    config: &TentativePromotionConfig,
) -> TentativePromotionResult {
    // 允许 unused 参数以保持签名扩展性
    let _ = confirmed;

    if tentative.is_empty() {
        return TentativePromotionResult {
            promoted: Vec::new(),
            remaining_tentative: Vec::new(),
            promoted_count: 0,
            remaining_count: 0,
        };
    }

    // Step 1: 按主分类分组（关键词簇）
    let grouped = group_by_category(tentative);

    let mut promoted = Vec::new();
    let mut remaining_tentative = Vec::new();

    // Step 2: 对每个簇判断是否应提升
    for (_category, cluster) in grouped {
        if should_promote_cluster(&cluster, config) {
            // 提升: 将簇内所有事件的 confidence 设为 promoted_confidence
            for mut event in cluster {
                event.confidence = config.promoted_confidence;
                promoted.push(event);
            }
        } else {
            // 不满足条件: 保持原样
            remaining_tentative.extend(cluster);
        }
    }

    let promoted_count = promoted.len();
    let remaining_count = remaining_tentative.len();

    TentativePromotionResult {
        promoted,
        remaining_tentative,
        promoted_count,
        remaining_count,
    }
}

// =========================================================
// 情境强度加权
// =========================================================

/// 根据情境强度计算 salience 乘数。
///
/// 公式（对齐决策列表 §5）:
/// - 弱情境（1-2）: ×1.5 — 日常琐事中流露的性格信号更强
/// - 中性（3 或 None）: ×1.0 — 常规权重
/// - 强情境（4-5）: ×0.5 — 强情境中行为更多由环境驱动，非性格
///
/// 参数:
/// - `strength`: 情境强度 1-5 或 None（等效 3）。
///
/// 返回:
/// - salience 权重乘数（0.5 / 1.0 / 1.5）。
pub fn situation_multiplier(strength: Option<i32>) -> f64 {
    match strength {
        Some(1) | Some(2) => 1.5,
        Some(4) | Some(5) => 0.5,
        _ => 1.0, // None 或 3 均为中性
    }
}

// =========================================================
// 校准权重链核心函数
// =========================================================

/// salience 校准函数。
///
/// 公式: `salience_cal = raw_salience^exp × (1 + α_rec × recurrence + α_int × intensity + α_men × mention)`
///
/// 其中 α_rec/int/men 分别是复现次数、情绪强度、提及频率的最大加成比例。
///
/// 说明:
/// - 原始 salience 通过指数变换调整凸性（exp=1.0 时线性，exp<1.0 时压缩高值差异）。
/// - 三个加成因子独立叠加，上限由配置控制。
/// - 结果 clamp 到 [0.01, 1.0] 以避免零权重。
///
/// 参数:
/// - `raw_salience`: 事件的原始显著性 [0.0, 1.0]。
/// - `recurrence_count`: 同主题复现次数归一化值 [0.0, 1.0]。
/// - `emotional_intensity`: 情绪强度 [0.0, 1.0]。
/// - `mention_frequency`: 用户提及频率归一化值 [0.0, 1.0]。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 校准后的 salience 值 [0.01, 1.0]。
pub fn calibrate_salience(
    raw_salience: f64,
    recurrence_count: f64,
    emotional_intensity: f64,
    mention_frequency: f64,
    config: &CalibratedWeightConfig,
) -> f64 {
    // 指数变换: 调整 salience 的凸性
    let base = raw_salience.clamp(0.0, 1.0).powf(config.salience_exponent);

    // 三个加成因子，各自归一化后乘以最大加成比例
    let rec_boost = recurrence_count.clamp(0.0, 1.0) * config.recurrence_boost_max;
    let intensity_boost = emotional_intensity.clamp(0.0, 1.0) * config.intensity_boost_max;
    let men_boost = mention_frequency.clamp(0.0, 1.0) * config.mention_boost_max;

    let calibrated = base * (1.0 + rec_boost + intensity_boost + men_boost);
    // 保底 0.01，避免零权重导致事件完全消失
    calibrated.clamp(0.01, 1.0)
}

/// 计算四因子校准权重。
///
/// 公式: `w_i = salience_cal × confidence_factor × situation_multiplier × source_support`
///
/// 其中:
/// - `salience_cal`: 由 `calibrate_salience` 计算的校准后显著性。
/// - `confidence_factor`: 由事件置信度决定（Confirmed→1.0, Tentative→半权重, Discarded→0.0）。
/// - `situation_multiplier`: 情境强度乘数（1.5 / 1.0 / 0.5）。
/// - `source_support`: 多源互证因子 = min(1.0, source_count / min_sources)。
///
/// 说明:
/// - 四因子相乘意味着任一因子为零则整体权重为零。
/// - 这是相对于 `salience × situation_multiplier` 的核心升级。
///
/// 参数:
/// - `event`: 待计算权重的事件。
/// - `enrichment`: 事件的增强统计数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 校准后的综合权重 [0.0, 1.0]。
pub fn compute_calibrated_weight(
    event: &MemoryEvent,
    enrichment: &EventEnrichment,
    config: &CalibratedWeightConfig,
) -> f64 {
    // Step 1: salience 校准
    let salience_cal = calibrate_salience(
        event.salience,
        enrichment.topic_recurrence_count,
        enrichment.emotional_intensity,
        enrichment.mention_frequency,
        config,
    );

    // Step 2: confidence_factor（基于三轨分类）
    let track = classify_event(event);
    let confidence_factor = track.confidence_factor(config.tentative_weight_factor);

    // Step 3: situation_multiplier
    let sit_mult = situation_multiplier(event.situation_strength);

    // Step 4: source_support（多源互证）
    let source_support = if enrichment.source_count == 0 {
        0.5 // 无来源时给半权重（防御性处理）
    } else {
        let ratio = enrichment.source_count as f64 / config.min_sources_for_full_support as f64;
        ratio.min(1.0)
    };

    salience_cal * confidence_factor * sit_mult * source_support
}

/// 为事件列表批量计算校准权重。
///
/// 参数:
/// - `events`: 事件列表。
/// - `enrichments`: 与 events 一一对应的增强数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 与 events 一一对应的校准权重向量。
///
/// # Panics
/// - 当 events 和 enrichments 长度不一致时 panic。
pub fn compute_calibrated_weights_batch(
    events: &[MemoryEvent],
    enrichments: &[EventEnrichment],
    config: &CalibratedWeightConfig,
) -> Vec<f64> {
    assert_eq!(
        events.len(),
        enrichments.len(),
        "events 与 enrichments 长度必须一致"
    );
    events
        .iter()
        .zip(enrichments)
        .map(|(event, enrichment)| compute_calibrated_weight(event, enrichment, config))
        .collect()
}

/// 使用简单权重（兼容路径）。
///
/// 公式: `w_i = salience × situation_multiplier(situation_strength)`
///
/// 说明:
/// - 这是旧权重公式，保留以支持 `use_calibrated_weights=false`。
///
/// 参数:
/// - `event`: 待计算权重的事件。
///
/// 返回:
/// - 简单权重 [0.0, 1.5]。
pub fn compute_simple_weight(event: &MemoryEvent) -> f64 {
    event.salience * situation_multiplier(event.situation_strength)
}

/// 为事件列表批量计算简单权重。
pub fn compute_simple_weights_batch(events: &[MemoryEvent]) -> Vec<f64> {
    events.iter().map(compute_simple_weight).collect()
}

// =========================================================
// A3: 按分类聚合
// =========================================================

/// 从事件的关键词中提取主分类标签。
///
/// 策略:
/// - 取 keywords 逗号分隔后的第一个非空标签作为主分类。
/// - 若 keywords 为 None 或为空串，返回 "未分类"。
///
/// 参数:
/// - `event`: 待提取分类的事件。
///
/// 返回:
/// - 主分类标签字符串。
pub fn extract_primary_category(event: &MemoryEvent) -> String {
    event
        .keywords
        .as_ref()
        .and_then(|kw| {
            let first = kw.split(',').next().unwrap_or("").trim();
            if first.is_empty() {
                None
            } else {
                Some(first.to_string())
            }
        })
        .unwrap_or_else(|| "未分类".to_string())
}

/// 按主分类分组事件。
///
/// 参数:
/// - `events`: 预过滤后的事件列表。
///
/// 返回:
/// - 分类标签 → 事件列表的映射。按分类标签字典序排列以保证确定性和可复现。
pub fn group_by_category(events: &[MemoryEvent]) -> Vec<(String, Vec<MemoryEvent>)> {
    let mut map: std::collections::BTreeMap<String, Vec<MemoryEvent>> =
        std::collections::BTreeMap::new();
    for event in events {
        let category = extract_primary_category(event);
        map.entry(category).or_default().push(event.clone());
    }
    map.into_iter().collect()
}

// =========================================================
// E: 动机维度二次分组统计
// =========================================================

/// 从事件的 motives 字段中提取动机标签列表。
///
/// 策略:
/// - motives 字段为逗号分隔的字符串（如 "地位维护,自主性"）。
/// - 拆分后 trim 每个标签，过滤空白和空字符串。
/// - 若 `motives` 为 None 或全部标签过滤后为空，返回空 Vec。
///
/// 参数:
/// - `event`: 待提取动机标签的事件。
///
/// 返回:
/// - 去空白后的动机标签列表。无动机时返回空 Vec。
pub fn extract_motive_tags(event: &MemoryEvent) -> Vec<String> {
    match &event.motives {
        Some(s) => {
            let tags: Vec<String> = s
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            tags
        }
        None => Vec::new(),
    }
}

/// 按动机标签分组事件。
///
/// 说明:
/// - 一个事件可能包含多个动机标签，会同时出现在多个分组中。
/// - 这是"二次分组"——在主分类（keywords）之下，按动机标签做二级聚合。
/// - 分组按动机标签字典序排列以保证确定性。
///
/// 参数:
/// - `events`: 预过滤后的事件列表。
///
/// 返回:
/// - 动机标签 → 事件列表的映射。仅包含至少 1 个事件的动机标签。
pub fn group_by_motive(events: &[MemoryEvent]) -> Vec<(String, Vec<MemoryEvent>)> {
    let mut map: std::collections::BTreeMap<String, Vec<MemoryEvent>> =
        std::collections::BTreeMap::new();
    for event in events {
        let tags = extract_motive_tags(event);
        for tag in tags {
            map.entry(tag).or_default().push(event.clone());
        }
    }
    map.into_iter().collect()
}

/// 计算全部动机标签的聚合统计。
///
/// 策略:
/// - 对每个动机标签，调用 `compute_category_stats` 复用已有的加权统计算法。
/// - 结果按 `n_eff` 降序排列（有效样本量大的动机优先展示）。
/// - 仅对 confirmed + tentative 事件进行统计，discarded 已在上游排除。
/// - 若所有事件均无 motives 数据，返回空 Vec。
///
/// 参数:
/// - `events`: 活跃事件列表（confirmed + tentative，不含 discarded）。
/// - `enrichments`: 与 events 一一对应的增强数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 按 n_eff 降序排列的动机统计列表。无动机数据时为空。
pub fn compute_motive_stats(
    events: &[MemoryEvent],
    enrichments: &[EventEnrichment],
    config: &CalibratedWeightConfig,
) -> Vec<MotiveStats> {
    if events.is_empty() {
        return Vec::new();
    }

    let grouped = group_by_motive(events);
    if grouped.is_empty() {
        return Vec::new();
    }

    // 按 n_eff 降序排列
    let mut stats: Vec<MotiveStats> = grouped
        .iter()
        .map(|(motive, motive_events)| {
            // 为每个动机分组构造对应的 enrichments 子集
            let motive_enrichments: Vec<EventEnrichment> = motive_events
                .iter()
                .map(|e| {
                    let idx = events.iter().position(|ae| ae.id == e.id).unwrap_or(0);
                    enrichments.get(idx).cloned().unwrap_or_default()
                })
                .collect();

            // 复用 compute_category_stats 计算加权统计量
            let cat_stats =
                compute_category_stats(motive, motive_events, Some(&motive_enrichments), config);

            MotiveStats {
                motive: motive.clone(),
                event_count: cat_stats.event_count,
                n_eff: cat_stats.n_eff,
                valence_mean: cat_stats.valence_mean,
                valence_std: cat_stats.valence_std,
                valence_positive_ratio: cat_stats.valence_positive_ratio,
                share_mean: cat_stats.share_mean,
                share_std: cat_stats.share_std,
                presentation_objective_ratio: cat_stats.presentation_objective_ratio,
                presentation_subjective_ratio: cat_stats.presentation_subjective_ratio,
                presentation_mixed_ratio: cat_stats.presentation_mixed_ratio,
                avg_salience: if motive_events.is_empty() {
                    0.0
                } else {
                    motive_events.iter().map(|e| e.salience).sum::<f64>()
                        / motive_events.len() as f64
                },
            }
        })
        .collect();

    // 按 n_eff 降序排列
    stats.sort_by(|a, b| {
        b.n_eff
            .partial_cmp(&a.n_eff)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    stats
}

/// 计算加权均值。
///
/// 公式: x̄_w = Σ(w_i · x_i) / Σ w_i
///
/// 参数:
/// - `values`: 各事件的指标取值。
/// - `weights`: 各事件的权重（需与 values 一一对应）。
///
/// 返回:
/// - 加权均值。若总权重为 0，返回 0.0。
pub fn weighted_mean(values: &[f64], weights: &[f64]) -> f64 {
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let weighted_sum: f64 = values.iter().zip(weights).map(|(v, w)| v * w).sum();
    weighted_sum / total_weight
}

/// 计算加权方差（总体方差，非样本方差）。
///
/// 公式: σ²_w = Σ(w_i · (x_i - x̄_w)²) / Σ w_i
///
/// 参数:
/// - `values`: 各事件的指标取值。
/// - `weights`: 各事件的权重。
/// - `mean`: 已计算的加权均值。
///
/// 返回:
/// - 加权方差。若总权重为 0 或仅 1 个有效样本，返回 0.0。
pub fn weighted_variance(values: &[f64], weights: &[f64], mean: f64) -> f64 {
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let weighted_sq_diff: f64 = values
        .iter()
        .zip(weights)
        .map(|(v, w)| w * (v - mean).powi(2))
        .sum();
    weighted_sq_diff / total_weight
}

/// 计算加权占比（用于正面事件比例和 presentation 分布）。
///
/// 公式: ratio = Σ(indicator_i · w_i) / Σ w_i
///
/// 参数:
/// - `indicators`: 各事件的指示器值（0.0 或 1.0）。
/// - `weights`: 各事件的权重。
///
/// 返回:
/// - 加权占比。若总权重为 0，返回 0.0。
pub fn weighted_ratio(indicators: &[f64], weights: &[f64]) -> f64 {
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let weighted_sum: f64 = indicators.iter().zip(weights).map(|(i, w)| i * w).sum();
    weighted_sum / total_weight
}

/// 计算单个分类的全部统计量（使用校准权重）。
///
/// 参数:
/// - `category`: 分类标签。
/// - `events`: 该分类下的全部事件。
/// - `enrichments`: 与 events 一一对应的增强数据。若为 None，使用简单权重（向后兼容）。
/// - `config`: 校准权重链配置（仅在 enrichments 不为 None 时使用）。
///
/// 返回:
/// - 包含所有加权统计量的 CategoryStats。
pub fn compute_category_stats(
    category: &str,
    events: &[MemoryEvent],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> CategoryStats {
    let event_count = events.len();

    // 根据是否提供增强数据选择权重计算方式
    let weights: Vec<f64> = match enrichments {
        Some(enr) => {
            assert_eq!(
                events.len(),
                enr.len(),
                "events 与 enrichments 长度必须一致"
            );
            events
                .iter()
                .zip(enr.iter())
                .map(|(event, enrichment)| compute_calibrated_weight(event, enrichment, config))
                .collect()
        }
        None => compute_simple_weights_batch(events),
    };

    let n_eff: f64 = weights.iter().sum();

    // 效价特征
    let valences: Vec<f64> = events.iter().map(|e| e.valence).collect();
    let valence_mean = weighted_mean(&valences, &weights);
    let valence_std = weighted_variance(&valences, &weights, valence_mean).sqrt();
    let valence_positive: Vec<f64> = events
        .iter()
        .map(|e| if e.valence > 0.0 { 1.0 } else { 0.0 })
        .collect();
    let valence_positive_ratio = weighted_ratio(&valence_positive, &weights);

    // 分享意愿特征
    let shares: Vec<f64> = events.iter().map(|e| e.share).collect();
    let share_mean = weighted_mean(&shares, &weights);
    let share_std = weighted_variance(&shares, &weights, share_mean).sqrt();

    // 表达特征 —— 单次遍历收集三种 presentation 指示器
    let mut is_objective = Vec::with_capacity(event_count);
    let mut is_subjective = Vec::with_capacity(event_count);
    let mut is_mixed = Vec::with_capacity(event_count);
    for e in events {
        is_objective.push(if matches!(e.presentation, Presentation::Objective) {
            1.0
        } else {
            0.0
        });
        is_subjective.push(if matches!(e.presentation, Presentation::Subjective) {
            1.0
        } else {
            0.0
        });
        is_mixed.push(if matches!(e.presentation, Presentation::Mixed) {
            1.0
        } else {
            0.0
        });
    }

    let presentation_objective_ratio = weighted_ratio(&is_objective, &weights);
    let presentation_subjective_ratio = weighted_ratio(&is_subjective, &weights);
    let presentation_mixed_ratio = weighted_ratio(&is_mixed, &weights);

    // 平均 salience（供后续 group_weight 计算）
    let avg_salience = if event_count > 0 {
        events.iter().map(|e| e.salience).sum::<f64>() / event_count as f64
    } else {
        0.0
    };

    CategoryStats {
        category: category.to_string(),
        event_count,
        n_eff,
        valence_mean,
        valence_std,
        valence_positive_ratio,
        share_mean,
        share_std,
        presentation_objective_ratio,
        presentation_subjective_ratio,
        presentation_mixed_ratio,
        group_weight: n_eff * avg_salience, // 临时值，后续按全局归一化
    }
}

/// 归一化所有分类的组权重。
///
/// 说明:
/// - 将各分类的 group_weight 除以总和，使所有分类权重之和为 1。
/// - 若仅有 1 个分类，其权重完全保留。
///
/// 参数:
/// - `categories`: 可变引用的分类统计列表。
fn normalize_group_weights(categories: &mut [CategoryStats]) {
    let total_weight: f64 = categories.iter().map(|c| c.group_weight).sum();
    if total_weight > 0.0 {
        for cat in categories.iter_mut() {
            cat.group_weight /= total_weight;
        }
    }
    // 按 group_weight 降序排列
    categories.sort_by(|a, b| {
        b.group_weight
            .partial_cmp(&a.group_weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

// =========================================================
// A6: 跨分类高阶指标
// =========================================================

/// 计算事件列表的权重向量（根据配置选择校准或简单权重）。
fn compute_weights_for_events(
    events: &[MemoryEvent],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> Vec<f64> {
    match enrichments {
        Some(enr) => compute_calibrated_weights_batch(events, enr, config),
        None => compute_simple_weights_batch(events),
    }
}

/// 计算情绪稳定性（全局 valence 加权标准差）。
///
/// 说明:
/// - 不按分类分组，直接对全部事件的 valence 做加权标准差。
/// - 标准差小 → 情绪平稳；标准差大 → 情绪波动剧烈。
///
/// 参数:
/// - `events`: 事件列表。
/// - `enrichments`: 可选的增强数据（None 时使用简单权重）。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 全局加权 valence 标准差。
pub fn compute_emotional_stability(
    events: &[MemoryEvent],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> f64 {
    let valences: Vec<f64> = events.iter().map(|e| e.valence).collect();
    let weights = compute_weights_for_events(events, enrichments, config);
    let mean = weighted_mean(&valences, &weights);
    weighted_variance(&valences, &weights, mean).sqrt()
}

/// 计算叙事一致性（跨分类 presentation 分布的相似度）。
///
/// 策略:
/// - 对每对分类计算其 presentation 分布（三元素向量）的余弦相似度。
/// - 取所有分类对的均值作为一致性指标。
/// - 仅 1 个分类时返回 1.0（完全一致）。
///
/// 参数:
/// - `categories`: 所有分类的统计摘要。
///
/// 返回:
/// - 归一化一致性指标 0.0..1.0。值越高表示跨分类表达风格越一致。
pub fn compute_narrative_consistency(categories: &[CategoryStats]) -> f64 {
    if categories.len() <= 1 {
        return 1.0;
    }

    let mut similarities = Vec::new();
    for i in 0..categories.len() {
        for j in (i + 1)..categories.len() {
            let a = &categories[i];
            let b = &categories[j];
            let dot = a.presentation_objective_ratio * b.presentation_objective_ratio
                + a.presentation_subjective_ratio * b.presentation_subjective_ratio
                + a.presentation_mixed_ratio * b.presentation_mixed_ratio;
            let norm_a = (a.presentation_objective_ratio.powi(2)
                + a.presentation_subjective_ratio.powi(2)
                + a.presentation_mixed_ratio.powi(2))
            .sqrt();
            let norm_b = (b.presentation_objective_ratio.powi(2)
                + b.presentation_subjective_ratio.powi(2)
                + b.presentation_mixed_ratio.powi(2))
            .sqrt();
            if norm_a > 0.0 && norm_b > 0.0 {
                similarities.push((dot / (norm_a * norm_b)).clamp(0.0, 1.0));
            }
        }
    }

    if similarities.is_empty() {
        0.0
    } else {
        similarities.iter().sum::<f64>() / similarities.len() as f64
    }
}

/// 计算 share 分布的偏度（基于加权）。
///
/// 公式: skew = Σ(w_i · (x_i - x̄)³) / (σ³ · Σ w_i)
///
/// 参数:
/// - `events`: 事件列表。
/// - `enrichments`: 可选的增强数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 偏度系数。正值=右偏（少数事件 share 很高），负值=左偏。
pub fn compute_share_skewness(
    events: &[MemoryEvent],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> f64 {
    let shares: Vec<f64> = events.iter().map(|e| e.share).collect();
    let weights = compute_weights_for_events(events, enrichments, config);
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let mean = weighted_mean(&shares, &weights);
    let variance = weighted_variance(&shares, &weights, mean);
    let std = variance.sqrt();
    if std < 1e-10 {
        return 0.0;
    }
    let m3: f64 = shares
        .iter()
        .zip(&weights)
        .map(|(s, w)| w * (s - mean).powi(3))
        .sum::<f64>()
        / total_weight;
    m3 / std.powi(3)
}

/// 计算 share 分布的峰度（基于加权）。
///
/// 公式: kurt = Σ(w_i · (x_i - x̄)⁴) / (σ⁴ · Σ w_i)
///
/// 参数:
/// - `events`: 事件列表。
/// - `enrichments`: 可选的增强数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - 峰度系数。正值=尖峰分布，负值=扁平分布。
pub fn compute_share_kurtosis(
    events: &[MemoryEvent],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> f64 {
    let shares: Vec<f64> = events.iter().map(|e| e.share).collect();
    let weights = compute_weights_for_events(events, enrichments, config);
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let mean = weighted_mean(&shares, &weights);
    let variance = weighted_variance(&shares, &weights, mean);
    let std = variance.sqrt();
    if std < 1e-10 {
        return 0.0;
    }
    let m4: f64 = shares
        .iter()
        .zip(&weights)
        .map(|(s, w)| w * (s - mean).powi(4))
        .sum::<f64>()
        / total_weight;
    m4 / std.powi(4)
}

/// 计算完整的跨分类高阶指标。
///
/// 参数:
/// - `events`: 事件列表。
/// - `categories`: 所有分类的统计摘要。
/// - `enrichments`: 可选的增强数据。
/// - `config`: 校准权重链配置。
///
/// 返回:
/// - CrossCategoryMetrics 结构体。
pub fn compute_cross_category_metrics(
    events: &[MemoryEvent],
    categories: &[CategoryStats],
    enrichments: Option<&[EventEnrichment]>,
    config: &CalibratedWeightConfig,
) -> CrossCategoryMetrics {
    let emotional_stability = compute_emotional_stability(events, enrichments, config);
    let narrative_consistency = compute_narrative_consistency(categories);
    // 态度矛盾检测在 LLM 推断阶段基于分类对做标记，具体计数由语义判断
    // 此处预留基础指标：分类数 >= 2 时标记可能存在矛盾
    let attitude_contradiction_count = if categories.len() >= 2 { 1 } else { 0 };
    let share_skewness = compute_share_skewness(events, enrichments, config);
    let share_kurtosis = compute_share_kurtosis(events, enrichments, config);

    CrossCategoryMetrics {
        emotional_stability,
        narrative_consistency,
        attitude_contradiction_count,
        share_skewness,
        share_kurtosis,
    }
}

// =========================================================
// A7: 代表性事件选取
// =========================================================

/// 选取每分类的代表性事件（A7）。
///
/// 策略:
/// - 每分类取 salience 最高的 `max_representative_events` 条。
/// - 保留原始 attitude 文本而非 paraphrase——LLM 推断阶段需要看到具体语境。
/// - 按分类的 group_weight 降序输出，每分类内按 salience 降序。
///
/// 参数:
/// - `events`: 预过滤后的事件列表（需与 categories 对应）。
/// - `categories`: 所有分类的统计摘要（用于确定分类顺序）。
/// - `config`: 统计配置。
///
/// 返回:
/// - 代表性事件列表，按分类权重降序、分类内按 salience 降序。
pub fn select_representative_events(
    events: &[MemoryEvent],
    categories: &[CategoryStats],
    config: &StatsConfig,
) -> Vec<RepresentativeEvent> {
    // 按分类分组原始事件
    let grouped = group_by_category(events);
    let category_order: std::collections::HashMap<&str, usize> = categories
        .iter()
        .enumerate()
        .map(|(i, c)| (c.category.as_str(), i))
        .collect();

    let mut results = Vec::new();

    for (category, mut cat_events) in grouped {
        // 按 salience 降序排列
        cat_events.sort_by(|a, b| {
            b.salience
                .partial_cmp(&a.salience)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let take_n = config.max_representative_events.min(cat_events.len());
        for event in cat_events.iter().take(take_n) {
            results.push(RepresentativeEvent {
                title: event.title.clone(),
                summary: event.summary.clone(),
                attitude: event.attitude.clone(),
                valence: event.valence,
                salience: event.salience,
                category: category.clone(),
            });
        }
    }

    // 按分类权重排序（使用 category_order）
    results.sort_by(|a, b| {
        let order_a = category_order
            .get(a.category.as_str())
            .copied()
            .unwrap_or(usize::MAX);
        let order_b = category_order
            .get(b.category.as_str())
            .copied()
            .unwrap_or(usize::MAX);
        match order_a.cmp(&order_b) {
            std::cmp::Ordering::Equal => {
                // 同分类内按 salience 降序
                b.salience
                    .partial_cmp(&a.salience)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
            other => other,
        }
    });

    results
}

// =========================================================
// 主编排函数
// =========================================================

/// 执行完整的 Phase A 统计管线。
///
/// 管线步骤:
/// 1. A1: 三轨分类（confirmed/tentative/discarded）
/// 2. 自动派生增强数据（主题复现、情绪强度、提及频率）
/// 3. A3: 按 keywords 主分类分组，计算校准加权统计量
/// 4. A6: 计算跨分类高阶指标
/// 5. A7: 选取代表性事件
///
/// 参数:
/// - `events`: 完整的 L2 事件列表（从 StorageBackend 读取）。
/// - `config`: 统计配置。
///
/// 返回:
/// - `StatsSummary`: 包含三轨分布、分类统计、跨分类指标和代表性事件的完整摘要。
///
/// 说明:
/// - 当 `config.use_calibrated_weights = true`（默认）时，使用三轨准入 + 校准权重链。
/// - 当 `config.use_calibrated_weights = false` 时，回退到旧行为（硬截断 + 简单权重）。
/// - 若所有事件都被 discarded，返回空的 StatsSummary。
/// - 日志在调用方记录，本函数不执行 I/O。
pub fn run_phase_a_stats(events: &[MemoryEvent], config: &StatsConfig) -> StatsSummary {
    let total_events_in = events.len();

    if events.is_empty() {
        return StatsSummary {
            total_events_in: 0,
            total_events_filtered: 0,
            confirmed_count: 0,
            tentative_count: 0,
            discarded_count: 0,
            category_count: 0,
            categories: Vec::new(),
            cross_category: CrossCategoryMetrics {
                emotional_stability: 0.0,
                narrative_consistency: 1.0,
                attitude_contradiction_count: 0,
                share_skewness: 0.0,
                share_kurtosis: 0.0,
            },
            representative_events: Vec::new(),
            motive_stats: Vec::new(),
        };
    }

    if config.use_calibrated_weights {
        // ---- 三轨准入 + 校准权重链 ----

        // A1: 三轨分类
        let classified = classify_events(events);
        let active = classified.active_events();

        if active.is_empty() {
            return StatsSummary {
                total_events_in,
                total_events_filtered: 0,
                confirmed_count: classified.confirmed.len(),
                tentative_count: classified.tentative.len(),
                discarded_count: classified.discarded_count,
                category_count: 0,
                categories: Vec::new(),
                cross_category: CrossCategoryMetrics {
                    emotional_stability: 0.0,
                    narrative_consistency: 1.0,
                    attitude_contradiction_count: 0,
                    share_skewness: 0.0,
                    share_kurtosis: 0.0,
                },
                representative_events: Vec::new(),
                motive_stats: Vec::new(),
            };
        }

        // 派生增强数据
        let enrichments = EventEnrichment::derive_batch(&active);

        // A3: 按分类聚合（使用校准权重）
        let grouped = group_by_category(&active);
        let mut categories: Vec<CategoryStats> = grouped
            .iter()
            .map(|(cat, evts)| {
                // 为每个分类的事件构建对应的增强数据子集
                let cat_enrichments: Vec<EventEnrichment> = evts
                    .iter()
                    .map(|e| {
                        // 在 active 中找到此事件的索引来获取对应的增强数据
                        let idx = active.iter().position(|ae| ae.id == e.id).unwrap_or(0);
                        enrichments.get(idx).cloned().unwrap_or_default()
                    })
                    .collect();
                compute_category_stats(
                    cat,
                    evts,
                    Some(&cat_enrichments),
                    &config.calibrated_weight_config,
                )
            })
            .collect();
        normalize_group_weights(&mut categories);
        let category_count = categories.len();

        // A6: 跨分类指标（使用校准权重）
        let cross_category = compute_cross_category_metrics(
            &active,
            &categories,
            Some(&enrichments),
            &config.calibrated_weight_config,
        );

        // A7: 代表性事件
        let representative_events = select_representative_events(&active, &categories, config);

        // E: 动机维度二次分组统计
        let motive_stats =
            compute_motive_stats(&active, &enrichments, &config.calibrated_weight_config);

        StatsSummary {
            total_events_in,
            total_events_filtered: active.len(),
            confirmed_count: classified.confirmed.len(),
            tentative_count: classified.tentative.len(),
            discarded_count: classified.discarded_count,
            category_count,
            categories,
            cross_category,
            representative_events,
            motive_stats,
        }
    } else {
        // ---- 兼容路径: 硬截断 + 简单权重 ----

        let (filtered, _excluded) = prefilter_events(events, config);
        let total_events_filtered = filtered.len();

        if filtered.is_empty() {
            return StatsSummary {
                total_events_in,
                total_events_filtered: 0,
                confirmed_count: 0,
                tentative_count: 0,
                discarded_count: total_events_in,
                category_count: 0,
                categories: Vec::new(),
                cross_category: CrossCategoryMetrics {
                    emotional_stability: 0.0,
                    narrative_consistency: 1.0,
                    attitude_contradiction_count: 0,
                    share_skewness: 0.0,
                    share_kurtosis: 0.0,
                },
                representative_events: Vec::new(),
                motive_stats: Vec::new(),
            };
        }

        let grouped = group_by_category(&filtered);
        let mut categories: Vec<CategoryStats> = grouped
            .iter()
            .map(|(cat, evts)| {
                compute_category_stats(cat, evts, None, &config.calibrated_weight_config)
            })
            .collect();
        normalize_group_weights(&mut categories);
        let category_count = categories.len();

        let cross_category = compute_cross_category_metrics(
            &filtered,
            &categories,
            None,
            &config.calibrated_weight_config,
        );

        let representative_events = select_representative_events(&filtered, &categories, config);

        StatsSummary {
            total_events_in,
            total_events_filtered,
            confirmed_count: total_events_filtered,
            tentative_count: 0,
            discarded_count: total_events_in - total_events_filtered,
            category_count,
            categories,
            cross_category,
            representative_events,
            motive_stats: Vec::new(),
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{Presentation, now_ms};

    /// 构造测试用 MemoryEvent。
    #[allow(clippy::too_many_arguments)]
    fn make_event(
        title: &str,
        summary: &str,
        keywords: Option<&str>,
        confidence: f64,
        salience: f64,
        valence: f64,
        share: f64,
        presentation: Presentation,
        attitude: Option<&str>,
    ) -> MemoryEvent {
        let now = now_ms();
        let mut ev = MemoryEvent::new(
            "user-0001".into(),
            title.into(),
            summary.into(),
            now - 1000,
            now,
        );
        ev.keywords = keywords.map(|s| s.into());
        ev.confidence = confidence;
        ev.salience = salience;
        ev.valence = valence;
        ev.share = share;
        ev.presentation = presentation;
        ev.attitude = attitude.map(|s| s.into());
        ev
    }

    /// 构造测试用 MemoryEvent（含 situation_strength）。
    fn make_event_with_situation(
        title: &str,
        summary: &str,
        keywords: Option<&str>,
        confidence: f64,
        salience: f64,
        valence: f64,
        share: f64,
        presentation: Presentation,
        attitude: Option<&str>,
        situation_strength: Option<i32>,
    ) -> MemoryEvent {
        let mut ev = make_event(
            title,
            summary,
            keywords,
            confidence,
            salience,
            valence,
            share,
            presentation,
            attitude,
        );
        ev.situation_strength = situation_strength;
        ev
    }

    // =========================================================
    // 情境强度乘数
    // =========================================================

    /// situation_multiplier 全分支参数化验证：
    /// - None / 3 / 非法值(0,6,100) → 中性 1.0
    /// - 弱情境 (1,2) → 放大 1.5
    /// - 强情境 (4,5) → 抑制 0.5
    #[test]
    fn situation_multiplier_cases() {
        let cases = [
            (None, 1.0),
            (Some(3), 1.0),
            (Some(1), 1.5),
            (Some(2), 1.5),
            (Some(4), 0.5),
            (Some(5), 0.5),
            (Some(0), 1.0),
            (Some(6), 1.0),
            (Some(100), 1.0),
        ];
        for (strength, expected) in cases {
            assert!(
                (situation_multiplier(strength) - expected).abs() < 1e-10,
                "strength={strength:?} 期望 {expected}",
            );
        }
    }

    // =========================================================
    // 准入轨道分类
    // =========================================================

    /// classify_event 各置信度分支参数化验证（含边界值与 NaN/负值防御）。
    #[test]
    fn classify_event_cases() {
        let cases = [
            (0.9, AdmissionTrack::Confirmed),
            (0.6, AdmissionTrack::Confirmed), // 边界值
            (0.5, AdmissionTrack::Tentative),
            (0.45, AdmissionTrack::Tentative),   // 边界值
            (0.5999, AdmissionTrack::Tentative), // 刚好低于 confirmed
            (0.3, AdmissionTrack::Discarded),
            (0.4499, AdmissionTrack::Discarded), // 刚好低于 tentative
            (f64::NAN, AdmissionTrack::Discarded), // NaN 防御
            (-0.1, AdmissionTrack::Discarded),   // 负值防御
        ];
        for (confidence, expected) in cases {
            let ev = make_event(
                "E",
                "s",
                None,
                confidence,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            );
            assert_eq!(classify_event(&ev), expected, "confidence={confidence}");
        }
    }

    #[test]
    fn classify_events_mixed() {
        let events = vec![
            make_event(
                "E1",
                "s",
                None,
                0.9,
                0.8,
                0.5,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "s",
                None,
                0.5,
                0.6,
                -0.2,
                0.3,
                Presentation::Subjective,
                None,
            ),
            make_event(
                "E3",
                "s",
                None,
                0.3,
                0.5,
                0.6,
                0.9,
                Presentation::Mixed,
                None,
            ),
        ];
        let classified = classify_events(&events);
        assert_eq!(classified.confirmed.len(), 1);
        assert_eq!(classified.tentative.len(), 1);
        assert_eq!(classified.discarded_count, 1);
        assert_eq!(classified.active_count(), 2);
    }

    #[test]
    fn classify_events_empty() {
        let classified = classify_events(&[]);
        assert_eq!(classified.confirmed.len(), 0);
        assert_eq!(classified.tentative.len(), 0);
        assert_eq!(classified.discarded_count, 0);
    }

    #[test]
    fn admission_track_confidence_factor() {
        assert!((AdmissionTrack::Confirmed.confidence_factor(0.5) - 1.0).abs() < 1e-10);
        assert!((AdmissionTrack::Tentative.confidence_factor(0.5) - 0.5).abs() < 1e-10);
        assert!((AdmissionTrack::Discarded.confidence_factor(0.5) - 0.0).abs() < 1e-10);

        // 自定义 tentative_factor
        assert!((AdmissionTrack::Tentative.confidence_factor(0.3) - 0.3).abs() < 1e-10);
    }

    #[test]
    fn admission_track_as_str() {
        assert_eq!(AdmissionTrack::Confirmed.as_str(), "confirmed");
        assert_eq!(AdmissionTrack::Tentative.as_str(), "tentative");
        assert_eq!(AdmissionTrack::Discarded.as_str(), "discarded");
    }

    // =========================================================
    // 校准权重链核心
    // =========================================================

    /// calibrate_salience 各 (raw, rec, int, men) 组合参数化验证（含 floor/ceiling）。
    #[test]
    fn calibrate_salience_cases() {
        let config = CalibratedWeightConfig::default();
        let cases = [
            // (raw, recurrence, intensity, mention, expected)
            (0.8, 0.0, 0.0, 0.0, 0.8),    // 无加成 → 保持不变
            (0.8, 1.0, 0.0, 0.0, 1.0),    // rec=1.0 → boost 0.30 → clamp 1.0
            (0.5, 0.5, 0.5, 0.5, 0.6625), // rec=0.15 + int=0.10 + men=0.075
            (0.0, 0.0, 0.0, 0.0, 0.01),   // 极低 → 保底 0.01
            (1.0, 1.0, 1.0, 1.0, 1.0),    // 全加成 → clamp 1.0
        ];
        for (raw, rec, int, men, expected) in cases {
            let cal = calibrate_salience(raw, rec, int, men, &config);
            assert!((cal - expected).abs() < 1e-6, "raw={raw} 期望 {expected}");
        }
    }

    #[test]
    fn compute_calibrated_weight_confirmed() {
        let config = CalibratedWeightConfig::default();
        let event = make_event(
            "E",
            "s",
            None,
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
        );
        let enrichment = EventEnrichment {
            topic_recurrence_count: 0.5,
            emotional_intensity: 0.5,
            mention_frequency: 0.5,
            source_count: 3,
        };
        // salience_cal = 0.8 * (1 + 0.15 + 0.10 + 0.075) = 0.8 * 1.325 = 1.06 → clamp 1.0
        // confidence_factor = 1.0 (confirmed)
        // situation_multiplier = 1.0 (None → 中性)
        // source_support = min(1.0, 3/3) = 1.0
        // w = 1.0 * 1.0 * 1.0 * 1.0 = 1.0
        let w = compute_calibrated_weight(&event, &enrichment, &config);
        assert!((w - 1.0).abs() < 1e-6);
    }

    #[test]
    fn compute_calibrated_weight_tentative_half() {
        let config = CalibratedWeightConfig::default();
        let event = make_event(
            "E",
            "s",
            None,
            0.5,
            0.8,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        let enrichment = EventEnrichment {
            source_count: 1,
            ..Default::default()
        };
        // salience_cal = 0.8 (no boosts)
        // confidence_factor = 0.5 (tentative)
        // situation_multiplier = 1.0
        // source_support = min(1.0, 1/3) = 0.333...
        // w = 0.8 * 0.5 * 1.0 * 0.333... = 0.1333...
        let w = compute_calibrated_weight(&event, &enrichment, &config);
        assert!((w - 0.8 * 0.5 * (1.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn compute_calibrated_weight_discarded_zero() {
        let config = CalibratedWeightConfig::default();
        let event = make_event(
            "E",
            "s",
            None,
            0.3,
            0.8,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        let enrichment = EventEnrichment::default();
        let w = compute_calibrated_weight(&event, &enrichment, &config);
        assert!((w - 0.0).abs() < 1e-10);
    }

    #[test]
    fn compute_calibrated_weight_weak_situation_boost() {
        let config = CalibratedWeightConfig::default();
        let event = make_event_with_situation(
            "E",
            "s",
            None,
            0.9,
            0.8,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            Some(2),
        );
        let enrichment = EventEnrichment {
            source_count: 1,
            ..Default::default()
        };
        // salience_cal = 0.8, conf_factor=1.0, sit_mult=1.5, source=1/3=0.333
        // w = 0.8 * 1.0 * 1.5 * 0.333 = 0.4
        let w = compute_calibrated_weight(&event, &enrichment, &config);
        assert!((w - 0.8 * 1.5 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn compute_calibrated_weight_strong_situation_dampen() {
        let config = CalibratedWeightConfig::default();
        let event = make_event_with_situation(
            "E",
            "s",
            None,
            0.9,
            0.8,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            Some(5),
        );
        let enrichment = EventEnrichment {
            source_count: 3,
            ..Default::default()
        };
        // salience_cal = 0.8, conf_factor=1.0, sit_mult=0.5, source=1.0
        // w = 0.8 * 0.5 = 0.4
        let w = compute_calibrated_weight(&event, &enrichment, &config);
        assert!((w - 0.4).abs() < 1e-6);
    }

    #[test]
    fn compute_calibrated_weight_full_source_support() {
        let config = CalibratedWeightConfig::default();
        let event = make_event(
            "E",
            "s",
            None,
            0.9,
            1.0,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        let enrichment = EventEnrichment {
            source_count: 5, // > min_sources_for_full_support (3)
            ..Default::default()
        };
        let w = compute_calibrated_weight(&event, &enrichment, &config);
        // source_support = 1.0 (capped)
        assert!(w > 0.9);
    }

    #[test]
    fn compute_simple_weight_vs_calibrated() {
        // 对比: 简单权重 vs 校准权重（无加成时）
        let config = CalibratedWeightConfig::default();
        let event = make_event(
            "E",
            "s",
            None,
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
        );
        let enrichment = EventEnrichment::default();

        let simple = compute_simple_weight(&event);
        let calibrated = compute_calibrated_weight(&event, &enrichment, &config);

        // simple = 0.8 * 1.0 = 0.8
        assert!((simple - 0.8).abs() < 1e-10);

        // calibrated 应该与简单权重有差异（因为 source_support < 1.0）
        assert!(
            calibrated < simple,
            "校准权重应因 source_support < 1.0 而降低"
        );
    }

    // =========================================================
    // EventEnrichment 派生
    // =========================================================

    #[test]
    fn enrichment_from_event() {
        let event = make_event(
            "E",
            "s",
            None,
            0.9,
            0.8,
            -0.5,
            0.5,
            Presentation::Mixed,
            None,
        );
        let enrichment = EventEnrichment::from_event(&event);
        assert!((enrichment.emotional_intensity - 0.5).abs() < 1e-10);
        assert!((enrichment.mention_frequency - 0.8).abs() < 1e-10);
        assert_eq!(enrichment.source_count, 1);
    }

    #[test]
    fn enrichment_derive_batch_recurrence() {
        let events = vec![
            make_event(
                "E1",
                "s",
                Some("工作"),
                0.9,
                0.8,
                0.5,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "s",
                Some("工作"),
                0.9,
                0.6,
                -0.2,
                0.3,
                Presentation::Subjective,
                None,
            ),
            make_event(
                "E3",
                "s",
                Some("社交"),
                0.8,
                0.5,
                0.6,
                0.9,
                Presentation::Mixed,
                None,
            ),
        ];
        let enrichments = EventEnrichment::derive_batch(&events);
        assert_eq!(enrichments.len(), 3);

        // 工作 ×2, 社交 ×1 → max=2
        // 工作 recurrence = 2/2 = 1.0
        assert!((enrichments[0].topic_recurrence_count - 1.0).abs() < 1e-10);
        assert!((enrichments[1].topic_recurrence_count - 1.0).abs() < 1e-10);
        // 社交 recurrence = 1/2 = 0.5
        assert!((enrichments[2].topic_recurrence_count - 0.5).abs() < 1e-10);
    }

    #[test]
    fn enrichment_derive_batch_empty() {
        let enrichments = EventEnrichment::derive_batch(&[]);
        assert!(enrichments.is_empty());
    }

    // =========================================================
    // 向后兼容: prefilter_events
    // =========================================================

    #[test]
    fn prefilter_excludes_low_confidence() {
        let config = StatsConfig::default();
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作,会议"),
                0.9,
                0.8,
                0.5,
                0.7,
                Presentation::Objective,
                None,
            ),
            make_event(
                "E2",
                "s2",
                Some("社交,聚会"),
                0.3,
                0.6,
                -0.2,
                0.3,
                Presentation::Subjective,
                None,
            ),
            make_event(
                "E3",
                "s3",
                Some("工作,项目"),
                0.8,
                0.5,
                0.6,
                0.9,
                Presentation::Mixed,
                None,
            ),
        ];
        let (filtered, excluded) = prefilter_events(&events, &config);
        assert_eq!(excluded, 1, "应排除 1 条低置信度事件");
        assert_eq!(filtered.len(), 2);
        let titles: Vec<&str> = filtered.iter().map(|e| e.title.as_str()).collect();
        assert!(titles.contains(&"E1"));
        assert!(titles.contains(&"E3"));
        assert!(!titles.contains(&"E2"));
    }

    #[test]
    fn prefilter_all_pass_when_high_confidence() {
        let config = StatsConfig::default();
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作"),
                0.9,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "s2",
                Some("社交"),
                0.8,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
        ];
        let (filtered, excluded) = prefilter_events(&events, &config);
        assert_eq!(excluded, 0);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn prefilter_empty_input() {
        let config = StatsConfig::default();
        let (filtered, excluded) = prefilter_events(&[], &config);
        assert_eq!(excluded, 0);
        assert!(filtered.is_empty());
    }

    // =========================================================
    // Tentative 跨批次复现自动提升
    // =========================================================

    /// 创建一个带有自定义 created_at 的事件（用于批次检测）。
    fn make_event_with_time(
        title: &str,
        keywords: Option<&str>,
        confidence: f64,
        salience: f64,
        created_at: i64,
    ) -> MemoryEvent {
        let mut ev = make_event(
            title,
            "摘要",
            keywords,
            confidence,
            salience,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        ev.created_at = created_at;
        ev
    }

    /// are_different_batches 各分支参数化验证：跨批次 / 间隔内 / created_at 为 0 保守同批次。
    #[test]
    fn are_different_batches_cases() {
        let config = TentativePromotionConfig::default(); // min_batch_interval_hours = 6.0
        let base_time = 1700000000000i64; // 某个 Unix 毫秒时间戳
        let a = make_event_with_time("E1", Some("工作"), 0.5, 0.6, base_time);
        // 8 小时后 → 不同批次
        let b = make_event_with_time("E2", Some("工作"), 0.5, 0.6, base_time + 8 * 3600 * 1000);
        assert!(are_different_batches(&a, &b, &config));
        // 3 小时后 → 同批次
        let b = make_event_with_time("E2", Some("工作"), 0.5, 0.6, base_time + 3 * 3600 * 1000);
        assert!(!are_different_batches(&a, &b, &config));
        // created_at == 0 → 保守视为同批次
        let c = make_event_with_time("E3", Some("工作"), 0.5, 0.6, 0);
        let d = make_event_with_time("E4", Some("工作"), 0.5, 0.6, base_time);
        assert!(!are_different_batches(&c, &d, &config));
    }

    #[test]
    fn promote_tentative_cross_batch_promotes() {
        let config = TentativePromotionConfig::default();
        let base_time = 1700000000000i64;
        // 两条同簇（工作）tentative 事件，来自不同批次，关键词相似度高 → 应提升
        let tentative = vec![
            make_event_with_time("E1", Some("工作, 会议, 压力"), 0.5, 0.6, base_time),
            make_event_with_time(
                "E2",
                Some("工作, 会议, 项目"),
                0.55,
                0.5,
                base_time + 8 * 3600 * 1000,
            ),
        ];
        let confirmed: Vec<MemoryEvent> = vec![];
        let result = promote_tentative_events(&tentative, &confirmed, &config);

        assert_eq!(result.promoted_count, 2);
        assert_eq!(result.remaining_count, 0);
        // 提升后的置信度应为 0.6
        for event in &result.promoted {
            assert!(
                (event.confidence - 0.6).abs() < 1e-10,
                "提升后 confidence 应为 0.6，实际为 {}",
                event.confidence
            );
        }
    }

    #[test]
    fn promote_tentative_single_event_not_promoted() {
        let config = TentativePromotionConfig::default();
        // 单条 tentative 事件 → 簇大小不足，不提升
        let tentative = vec![make_event_with_time(
            "E1",
            Some("工作, 会议"),
            0.5,
            0.6,
            1700000000000i64,
        )];
        let confirmed: Vec<MemoryEvent> = vec![];
        let result = promote_tentative_events(&tentative, &confirmed, &config);

        assert_eq!(result.promoted_count, 0);
        assert_eq!(result.remaining_count, 1);
    }

    #[test]
    fn promote_tentative_same_batch_not_promoted() {
        let config = TentativePromotionConfig::default();
        let base_time = 1700000000000i64;
        // 两条同簇事件，但来自同一批次（时间间隔不足）→ 不提升
        let tentative = vec![
            make_event_with_time("E1", Some("工作, 会议"), 0.5, 0.6, base_time),
            make_event_with_time(
                "E2",
                Some("工作, 项目"),
                0.55,
                0.5,
                base_time + 3600 * 1000, // 仅 1 小时后
            ),
        ];
        let confirmed: Vec<MemoryEvent> = vec![];
        let result = promote_tentative_events(&tentative, &confirmed, &config);

        assert_eq!(result.promoted_count, 0);
        assert_eq!(result.remaining_count, 2);
    }

    #[test]
    fn promote_tentative_low_keyword_similarity_not_promoted() {
        let config = TentativePromotionConfig::default();
        let base_time = 1700000000000i64;
        // 两条事件来自不同批次，但关键词无交集 → Jaccard=0，不提升
        let tentative = vec![
            make_event_with_time("E1", Some("工作, 会议"), 0.5, 0.6, base_time),
            make_event_with_time(
                "E2",
                Some("社交, 聚会"),
                0.55,
                0.5,
                base_time + 8 * 3600 * 1000,
            ),
        ];
        let confirmed: Vec<MemoryEvent> = vec![];
        let result = promote_tentative_events(&tentative, &confirmed, &config);

        assert_eq!(result.promoted_count, 0);
        assert_eq!(result.remaining_count, 2);
    }

    #[test]
    fn promote_tentative_mixed_clusters() {
        let config = TentativePromotionConfig::default();
        let base_time = 1700000000000i64;
        // 工作簇：2 条，跨批次，关键词相似 → 应提升
        // 社交簇：1 条 → 不提升
        let tentative = vec![
            make_event_with_time("E1", Some("工作, 会议, 压力"), 0.5, 0.6, base_time),
            make_event_with_time(
                "E2",
                Some("工作, 会议, 项目"),
                0.55,
                0.5,
                base_time + 8 * 3600 * 1000,
            ),
            make_event_with_time("E3", Some("社交, 聚会"), 0.5, 0.4, base_time),
        ];
        let confirmed: Vec<MemoryEvent> = vec![];
        let result = promote_tentative_events(&tentative, &confirmed, &config);

        assert_eq!(result.promoted_count, 2, "工作簇应提升");
        assert_eq!(result.remaining_count, 1, "社交簇不提升");
        // 验证提升的是工作簇
        let promoted_titles: Vec<&str> = result.promoted.iter().map(|e| e.title.as_str()).collect();
        assert!(promoted_titles.contains(&"E1"));
        assert!(promoted_titles.contains(&"E2"));
        assert_eq!(result.remaining_tentative[0].title, "E3");
    }

    #[test]
    fn promote_tentative_empty_input() {
        let config = TentativePromotionConfig::default();
        let tentative: Vec<MemoryEvent> = vec![];
        let confirmed: Vec<MemoryEvent> = vec![];
        let result = promote_tentative_events(&tentative, &confirmed, &config);

        assert_eq!(result.promoted_count, 0);
        assert_eq!(result.remaining_count, 0);
        assert!(result.promoted.is_empty());
        assert!(result.remaining_tentative.is_empty());
    }

    #[test]
    fn promote_tentative_custom_min_cluster_size() {
        let config = TentativePromotionConfig {
            min_cluster_size: 3,
            ..Default::default()
        };
        let base_time = 1700000000000i64;
        // 3 条同簇事件，跨批次 → 满足 min_cluster_size=3
        let tentative = vec![
            make_event_with_time("E1", Some("工作, 会议, 压力"), 0.5, 0.6, base_time),
            make_event_with_time(
                "E2",
                Some("工作, 会议, 项目"),
                0.55,
                0.5,
                base_time + 8 * 3600 * 1000,
            ),
            make_event_with_time(
                "E3",
                Some("工作, 会议, 汇报"),
                0.5,
                0.4,
                base_time + 16 * 3600 * 1000,
            ),
        ];
        let confirmed: Vec<MemoryEvent> = vec![];
        let result = promote_tentative_events(&tentative, &confirmed, &config);

        assert_eq!(result.promoted_count, 3);
        assert_eq!(result.remaining_count, 0);
    }

    #[test]
    fn promote_tentative_respects_confirmed_list() {
        // confirmed 列表存在但不应影响提升逻辑（当前为签名保留参数）
        let config = TentativePromotionConfig::default();
        let base_time = 1700000000000i64;
        let tentative = vec![
            make_event_with_time("E1", Some("工作, 会议"), 0.5, 0.6, base_time),
            make_event_with_time(
                "E2",
                Some("工作, 会议, 项目"),
                0.55,
                0.5,
                base_time + 8 * 3600 * 1000,
            ),
        ];
        let confirmed = vec![make_event(
            "E0_confirmed",
            "已有确认事件",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
        )];
        let result = promote_tentative_events(&tentative, &confirmed, &config);

        // confirmed 列表存在时提升逻辑不受影响
        assert_eq!(result.promoted_count, 2);
    }

    // =========================================================
    // 主分类提取
    // =========================================================

    /// extract_primary_category 各分支参数化验证：多关键词取首个 / 单关键词 / None / 空串。
    #[test]
    fn extract_primary_category_cases() {
        let cases = [
            (Some("工作, 会议, 紧张"), "工作"),
            (Some("家庭"), "家庭"),
            (None, "未分类"),
            (Some(""), "未分类"),
        ];
        for (keywords, expected) in cases {
            let ev = make_event(
                "E1",
                "摘要",
                keywords,
                0.8,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            );
            assert_eq!(extract_primary_category(&ev), expected);
        }
    }

    // =========================================================
    // 加权统计
    // =========================================================

    /// weighted_mean 各分支参数化验证：等权重 / 非等权重 / 全零权重。
    #[test]
    fn weighted_mean_cases() {
        // 等权重 → 算术平均
        let values = vec![1.0, 2.0, 3.0];
        let weights = vec![1.0, 1.0, 1.0];
        let mean = weighted_mean(&values, &weights);
        assert!((mean - 2.0).abs() < 1e-10);
        // 非等权重 → 加权平均
        let values = vec![0.5, 0.5, 1.0];
        let weights = vec![0.2, 0.5, 0.8];
        let mean = weighted_mean(&values, &weights);
        assert!((mean - 0.7666).abs() < 0.001);
        // 全零权重 → 0.0
        let values = vec![1.0, 2.0];
        let weights = vec![0.0, 0.0];
        let mean = weighted_mean(&values, &weights);
        assert!((mean - 0.0).abs() < 1e-10);
    }

    #[test]
    fn weighted_variance_basic() {
        let values = vec![1.0, 2.0, 3.0];
        let weights = vec![1.0, 1.0, 1.0];
        let var = weighted_variance(&values, &weights, 2.0);
        assert!((var - 2.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn weighted_ratio_basic() {
        let indicators = vec![1.0, 0.0, 1.0];
        let weights = vec![0.4, 0.6, 1.0];
        let ratio = weighted_ratio(&indicators, &weights);
        assert!((ratio - 0.7).abs() < 1e-10);
    }

    // =========================================================
    // 单分类统计（校准权重 + 向后兼容）
    // =========================================================

    #[test]
    fn category_stats_with_calibrated_weights() {
        let config = CalibratedWeightConfig::default();
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作,会议"),
                0.9,
                0.8,
                0.5,
                0.7,
                Presentation::Objective,
                Some("满意"),
            ),
            make_event(
                "E2",
                "s2",
                Some("工作,项目"),
                0.8,
                0.6,
                -0.3,
                0.4,
                Presentation::Subjective,
                Some("焦虑"),
            ),
            make_event(
                "E3",
                "s3",
                Some("工作,汇报"),
                0.7,
                0.9,
                0.2,
                0.6,
                Presentation::Mixed,
                Some("一般"),
            ),
        ];
        let enrichments = EventEnrichment::derive_batch(&events);
        let stats = compute_category_stats("工作", &events, Some(&enrichments), &config);

        assert_eq!(stats.category, "工作");
        assert_eq!(stats.event_count, 3);
        // n_eff 应该由于校准权重而略小于简单加权（source_support < 1.0）
        let simple_stats = compute_category_stats("工作", &events, None, &config);
        assert!(
            stats.n_eff < simple_stats.n_eff,
            "校准 n_eff({}) 应小于简单加权 n_eff({})",
            stats.n_eff,
            simple_stats.n_eff
        );
        assert!(stats.n_eff > 0.0, "n_eff 应大于 0");
    }

    #[test]
    fn category_stats_with_simple_weights_backward_compat() {
        let config = CalibratedWeightConfig::default();
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作,会议"),
                0.9,
                0.8,
                0.5,
                0.7,
                Presentation::Objective,
                Some("满意"),
            ),
            make_event(
                "E2",
                "s2",
                Some("工作,项目"),
                0.8,
                0.6,
                -0.3,
                0.4,
                Presentation::Subjective,
                Some("焦虑"),
            ),
            make_event(
                "E3",
                "s3",
                Some("工作,汇报"),
                0.7,
                0.9,
                0.2,
                0.6,
                Presentation::Mixed,
                Some("一般"),
            ),
        ];
        // None enrichments → 简单权重
        let stats = compute_category_stats("工作", &events, None, &config);

        assert_eq!(stats.category, "工作");
        assert_eq!(stats.event_count, 3);
        // n_eff = 0.8 + 0.6 + 0.9 = 2.3
        assert!((stats.n_eff - 2.3).abs() < 1e-10);
    }

    #[test]
    fn category_stats_single_event() {
        let config = CalibratedWeightConfig::default();
        let events = vec![make_event(
            "E1",
            "摘要",
            Some("家庭"),
            0.9,
            0.5,
            0.8,
            0.6,
            Presentation::Subjective,
            None,
        )];
        let stats = compute_category_stats("家庭", &events, None, &config);
        assert_eq!(stats.event_count, 1);
        assert!((stats.n_eff - 0.5).abs() < 1e-10);
        assert!((stats.valence_mean - 0.8).abs() < 1e-10);
        assert!((stats.valence_std - 0.0).abs() < 1e-10);
        assert!((stats.presentation_subjective_ratio - 1.0).abs() < 1e-10);
    }

    #[test]
    fn category_stats_respects_situation_multiplier() {
        let config = CalibratedWeightConfig::default();
        let events = vec![
            make_event_with_situation(
                "弱情境事件",
                "摘要",
                Some("工作"),
                0.9,
                0.8,
                0.5,
                0.5,
                Presentation::Mixed,
                None,
                Some(2),
            ),
            make_event_with_situation(
                "强情境事件",
                "摘要",
                Some("工作"),
                0.9,
                0.8,
                0.5,
                0.5,
                Presentation::Mixed,
                None,
                Some(5),
            ),
        ];
        let stats = compute_category_stats("工作", &events, None, &config);
        // 简单权重: n_eff = 0.8*1.5 + 0.8*0.5 = 1.2 + 0.4 = 1.6
        assert!((stats.n_eff - 1.6).abs() < 1e-10);
    }

    // =========================================================
    // 分组 + 权重归一化
    // =========================================================

    #[test]
    fn group_by_category_works() {
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作"),
                0.9,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "s2",
                Some("社交"),
                0.8,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E3",
                "s3",
                Some("工作"),
                0.7,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
        ];
        let grouped = group_by_category(&events);
        assert_eq!(grouped.len(), 2);
        let social_group = grouped.iter().find(|(k, _)| k == "社交").unwrap();
        assert_eq!(social_group.1.len(), 1);
        let work_group = grouped.iter().find(|(k, _)| k == "工作").unwrap();
        assert_eq!(work_group.1.len(), 2);
    }

    #[test]
    fn group_weights_normalize() {
        let config = CalibratedWeightConfig::default();
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作"),
                0.9,
                0.8,
                0.5,
                0.7,
                Presentation::Objective,
                None,
            ),
            make_event(
                "E2",
                "s2",
                Some("社交"),
                0.8,
                0.6,
                0.3,
                0.5,
                Presentation::Subjective,
                None,
            ),
        ];
        let grouped = group_by_category(&events);
        let mut cats: Vec<CategoryStats> = grouped
            .iter()
            .map(|(cat, evts)| compute_category_stats(cat, evts, None, &config))
            .collect();
        normalize_group_weights(&mut cats);
        let total: f64 = cats.iter().map(|c| c.group_weight).sum();
        assert!(
            (total - 1.0).abs() < 1e-10,
            "权重应归一化为和为1，实际为{}",
            total
        );
    }

    // =========================================================
    // 跨分类指标
    // =========================================================

    #[test]
    fn emotional_stability_with_calibrated_weights() {
        let config = CalibratedWeightConfig::default();
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作"),
                0.9,
                0.5,
                0.5,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "s2",
                Some("社交"),
                0.8,
                0.5,
                -0.3,
                0.5,
                Presentation::Mixed,
                None,
            ),
        ];
        let enrichments = EventEnrichment::derive_batch(&events);
        let stability = compute_emotional_stability(&events, Some(&enrichments), &config);
        assert!(stability > 0.0, "方差应大于零");
    }

    #[test]
    fn narrative_consistency_perfect() {
        let cats = vec![
            CategoryStats {
                category: "工作".into(),
                event_count: 1,
                n_eff: 1.0,
                valence_mean: 0.0,
                valence_std: 0.0,
                valence_positive_ratio: 0.5,
                share_mean: 0.5,
                share_std: 0.0,
                presentation_objective_ratio: 0.6,
                presentation_subjective_ratio: 0.3,
                presentation_mixed_ratio: 0.1,
                group_weight: 0.5,
            },
            CategoryStats {
                category: "社交".into(),
                event_count: 1,
                n_eff: 1.0,
                valence_mean: 0.0,
                valence_std: 0.0,
                valence_positive_ratio: 0.5,
                share_mean: 0.5,
                share_std: 0.0,
                presentation_objective_ratio: 0.6,
                presentation_subjective_ratio: 0.3,
                presentation_mixed_ratio: 0.1,
                group_weight: 0.5,
            },
        ];
        let consistency = compute_narrative_consistency(&cats);
        assert!((consistency - 1.0).abs() < 1e-10);
    }

    #[test]
    fn narrative_consistency_single_category() {
        let cats = vec![CategoryStats {
            category: "工作".into(),
            event_count: 1,
            n_eff: 1.0,
            valence_mean: 0.0,
            valence_std: 0.0,
            valence_positive_ratio: 0.5,
            share_mean: 0.5,
            share_std: 0.0,
            presentation_objective_ratio: 0.5,
            presentation_subjective_ratio: 0.3,
            presentation_mixed_ratio: 0.2,
            group_weight: 1.0,
        }];
        let consistency = compute_narrative_consistency(&cats);
        assert!((consistency - 1.0).abs() < 1e-10, "单个分类一致性为 1.0");
    }

    /// compute_share_skewness 各事件分布参数化验证。
    #[test]
    fn share_skewness_cases() {
        let config = CalibratedWeightConfig::default();
        // 对称分布（share 0.3/0.5/0.7）→ 偏度接近 0
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作"),
                0.9,
                0.5,
                0.0,
                0.3,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "s2",
                Some("工作"),
                0.9,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E3",
                "s3",
                Some("工作"),
                0.9,
                0.5,
                0.0,
                0.7,
                Presentation::Mixed,
                None,
            ),
        ];
        let skew = compute_share_skewness(&events, None, &config);
        assert!(skew.abs() < 0.1, "对称分布偏度应接近0，实际={skew}");
        // 单事件 → 偏度 0
        let events = vec![make_event(
            "E1",
            "s1",
            Some("工作"),
            0.9,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        )];
        let skew = compute_share_skewness(&events, None, &config);
        assert!((skew - 0.0).abs() < 1e-10);
    }

    #[test]
    fn share_kurtosis_uniform() {
        let config = CalibratedWeightConfig::default();
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作"),
                0.9,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "s2",
                Some("工作"),
                0.9,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
        ];
        let kurt = compute_share_kurtosis(&events, None, &config);
        assert!((kurt - 0.0).abs() < 1e-10);
    }

    // =========================================================
    // 代表性事件选取
    // =========================================================

    #[test]
    fn representative_events_limit() {
        let config = StatsConfig {
            max_representative_events: 2,
            ..Default::default()
        };
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作"),
                0.9,
                0.9,
                0.5,
                0.5,
                Presentation::Mixed,
                Some("态度1"),
            ),
            make_event(
                "E2",
                "s2",
                Some("工作"),
                0.9,
                0.5,
                0.3,
                0.5,
                Presentation::Mixed,
                Some("态度2"),
            ),
            make_event(
                "E3",
                "s3",
                Some("工作"),
                0.9,
                0.8,
                0.6,
                0.5,
                Presentation::Mixed,
                Some("态度3"),
            ),
            make_event(
                "E4",
                "s4",
                Some("工作"),
                0.9,
                0.3,
                0.1,
                0.5,
                Presentation::Mixed,
                Some("态度4"),
            ),
        ];
        let cfg = CalibratedWeightConfig::default();
        let grouped = group_by_category(&events);
        let cats: Vec<CategoryStats> = grouped
            .iter()
            .map(|(cat, evts)| compute_category_stats(cat, evts, None, &cfg))
            .collect();
        let representatives = select_representative_events(&events, &cats, &config);
        assert_eq!(representatives.len(), 2, "最多 2 条");
        let saliences: Vec<f64> = representatives.iter().map(|r| r.salience).collect();
        assert!(saliences.contains(&0.9));
        assert!(saliences.contains(&0.8));
    }

    #[test]
    fn representative_events_preserves_attitude_original() {
        let config = StatsConfig::default();
        let events = vec![make_event(
            "E1",
            "摘要",
            Some("工作"),
            0.9,
            0.9,
            0.5,
            0.5,
            Presentation::Mixed,
            Some("对项目进展感到满意"),
        )];
        let cfg = CalibratedWeightConfig::default();
        let grouped = group_by_category(&events);
        let cats: Vec<CategoryStats> = grouped
            .iter()
            .map(|(cat, evts)| compute_category_stats(cat, evts, None, &cfg))
            .collect();
        let reps = select_representative_events(&events, &cats, &config);
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].attitude.as_deref(), Some("对项目进展感到满意"));
    }

    // =========================================================
    // 完整管线: 三轨 + 校准权重
    // =========================================================

    #[test]
    fn run_phase_a_stats_v13_calibrated() {
        let config = StatsConfig::default(); // use_calibrated_weights = true
        let events = vec![
            make_event(
                "E1",
                "工作会议摘要",
                Some("工作,会议"),
                0.9,
                0.8,
                0.7,
                0.8,
                Presentation::Objective,
                Some("对成果满意"),
            ),
            make_event(
                "E2",
                "低置信事件",
                Some("工作,闲聊"),
                0.3,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E3",
                "社交聚会摘要",
                Some("社交,聚会"),
                0.8,
                0.7,
                0.5,
                0.9,
                Presentation::Subjective,
                Some("聚会很愉快"),
            ),
            make_event(
                "E4",
                "家庭事件摘要",
                Some("家庭,晚餐"),
                0.7,
                0.6,
                -0.2,
                0.3,
                Presentation::Mixed,
                Some("家庭小摩擦"),
            ),
        ];
        let summary = run_phase_a_stats(&events, &config);

        assert_eq!(summary.total_events_in, 4);
        // E2 被 discarded (conf=0.3)，其余 3 条为 active
        assert_eq!(summary.total_events_filtered, 3);
        assert_eq!(summary.discarded_count, 1);
        assert!(summary.confirmed_count > 0);
        assert_eq!(summary.category_count, 3); // 工作、社交、家庭
        assert!(!summary.categories.is_empty());
        // 分类按 group_weight 降序
        assert!(
            summary.categories[0].group_weight >= summary.categories.last().unwrap().group_weight
        );
        assert!(summary.cross_category.emotional_stability >= 0.0);
        assert!(!summary.representative_events.is_empty());
    }

    #[test]
    fn run_phase_a_stats_v13_includes_tentative() {
        let config = StatsConfig::default();
        // 一个 tentative 事件（conf=0.5）+ 一个 confirmed 事件
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作"),
                0.9,
                0.8,
                0.5,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "待定事件",
                Some("工作"),
                0.5,
                0.6,
                -0.3,
                0.3,
                Presentation::Subjective,
                None,
            ),
        ];
        let summary = run_phase_a_stats(&events, &config);

        assert_eq!(summary.total_events_in, 2);
        // tentative 事件应在 active 中（以半权重参与统计）
        assert_eq!(summary.total_events_filtered, 2);
        assert_eq!(summary.confirmed_count, 1);
        assert_eq!(summary.tentative_count, 1);
        assert_eq!(summary.discarded_count, 0);
        assert_eq!(summary.category_count, 1);
    }

    #[test]
    fn run_phase_a_stats_v13_all_discarded() {
        let config = StatsConfig::default();
        let events = vec![
            make_event(
                "E1",
                "低置信",
                Some("工作"),
                0.3,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "也低置信",
                Some("社交"),
                0.2,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
        ];
        let summary = run_phase_a_stats(&events, &config);
        assert_eq!(summary.total_events_in, 2);
        assert_eq!(summary.total_events_filtered, 0);
        assert_eq!(summary.discarded_count, 2);
        assert_eq!(summary.category_count, 0);
        assert!(summary.categories.is_empty());
    }

    #[test]
    fn run_phase_a_stats_v13_empty_input() {
        let config = StatsConfig::default();
        let summary = run_phase_a_stats(&[], &config);
        assert_eq!(summary.total_events_in, 0);
        assert_eq!(summary.total_events_filtered, 0);
    }

    #[test]
    fn run_phase_a_stats_v12_compat_path() {
        // 使用 use_calibrated_weights=false 回退到旧行为
        let config = StatsConfig {
            use_calibrated_weights: false,
            ..Default::default()
        };
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作,会议"),
                0.9,
                0.8,
                0.7,
                0.8,
                Presentation::Objective,
                Some("满意"),
            ),
            make_event(
                "E2",
                "低置信事件",
                Some("工作,闲聊"),
                0.3,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E3",
                "s3",
                Some("社交,聚会"),
                0.8,
                0.7,
                0.5,
                0.9,
                Presentation::Subjective,
                Some("愉快"),
            ),
            make_event(
                "E4",
                "s4",
                Some("家庭,晚餐"),
                0.7,
                0.6,
                -0.2,
                0.3,
                Presentation::Mixed,
                Some("小摩擦"),
            ),
        ];
        let summary = run_phase_a_stats(&events, &config);

        assert_eq!(summary.total_events_in, 4);
        // 旧路径: E2 被硬截断排除
        assert_eq!(summary.total_events_filtered, 3);
        assert_eq!(summary.category_count, 3);
        // 旧路径将所有通过的事件视为 confirmed
        assert!(summary.confirmed_count > 0);
        assert_eq!(summary.tentative_count, 0);
    }

    // =========================================================
    // 跨分类指标（校准权重路径）
    // =========================================================

    #[test]
    fn cross_category_metrics_with_calibrated_weights() {
        let config = CalibratedWeightConfig::default();
        let events = vec![
            make_event(
                "E1",
                "s1",
                Some("工作"),
                0.9,
                0.8,
                0.5,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "s2",
                Some("社交"),
                0.8,
                0.6,
                -0.3,
                0.5,
                Presentation::Mixed,
                None,
            ),
        ];
        let enrichments = EventEnrichment::derive_batch(&events);
        let cat_cfg = CalibratedWeightConfig::default();
        let grouped = group_by_category(&events);
        let mut cats: Vec<CategoryStats> = grouped
            .iter()
            .map(|(cat, evts)| {
                let cat_enr: Vec<EventEnrichment> = evts
                    .iter()
                    .map(|e| {
                        let idx = events.iter().position(|ae| ae.id == e.id).unwrap_or(0);
                        enrichments.get(idx).cloned().unwrap_or_default()
                    })
                    .collect();
                compute_category_stats(cat, evts, Some(&cat_enr), &cat_cfg)
            })
            .collect();
        normalize_group_weights(&mut cats);

        let metrics = compute_cross_category_metrics(&events, &cats, Some(&enrichments), &config);
        assert!(metrics.emotional_stability >= 0.0);
        assert!(metrics.narrative_consistency >= 0.0);
    }

    // =========================================================
    // 动机维度统计（MotivesStats）
    // =========================================================

    /// 构造带 motives 字段的测试事件。
    fn make_event_with_motives(
        title: &str,
        summary: &str,
        keywords: Option<&str>,
        confidence: f64,
        salience: f64,
        valence: f64,
        share: f64,
        presentation: Presentation,
        attitude: Option<&str>,
        motives: Option<&str>,
    ) -> MemoryEvent {
        let now = now_ms();
        let mut ev = MemoryEvent::new(
            "test-persona".into(),
            title.into(),
            summary.into(),
            now - 1000,
            now,
        );
        ev.keywords = keywords.map(|k| k.into());
        ev.confidence = confidence;
        ev.salience = salience;
        ev.valence = valence;
        ev.share = share;
        ev.presentation = presentation;
        ev.attitude = attitude.map(|a| a.into());
        ev.motives = motives.map(|m| m.into());
        ev.situation_strength = Some(3);
        ev
    }

    /// extract_motive_tags 各分支参数化验证：多标签 / None / 纯空白 / 单标签 / 去除空白。
    #[test]
    fn extract_motive_tags_cases() {
        // 逗号分隔多标签
        let event = make_event_with_motives(
            "E1",
            "s",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
            Some("地位维护,自主性,归属"),
        );
        let tags = extract_motive_tags(&event);
        assert_eq!(tags.len(), 3);
        assert_eq!(tags[0], "地位维护");
        assert_eq!(tags[1], "自主性");
        assert_eq!(tags[2], "归属");
        // None → 空
        let event = make_event_with_motives(
            "E2",
            "s",
            Some("社交"),
            0.8,
            0.6,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            None,
        );
        assert!(extract_motive_tags(&event).is_empty());
        // 纯空白 → 空
        let event = make_event_with_motives(
            "E3",
            "s",
            Some("社交"),
            0.8,
            0.6,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            Some("  ,  ,  "),
        );
        assert!(extract_motive_tags(&event).is_empty());
        // 单标签
        let event = make_event_with_motives(
            "E4",
            "s",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
            Some("自主性"),
        );
        let tags = extract_motive_tags(&event);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0], "自主性");
        // 去除首尾空白
        let event = make_event_with_motives(
            "E5",
            "s",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
            Some(" 地位维护 , 自主性 ,  归属 "),
        );
        let tags = extract_motive_tags(&event);
        assert_eq!(tags.len(), 3);
        assert_eq!(tags[0], "地位维护");
        assert_eq!(tags[1], "自主性");
        assert_eq!(tags[2], "归属");
    }

    #[test]
    fn group_by_motive_handles_multi_tag_events() {
        let e1 = make_event_with_motives(
            "E1",
            "s",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
            Some("地位维护,自主性"),
        );
        let e2 = make_event_with_motives(
            "E2",
            "s",
            Some("社交"),
            0.8,
            0.6,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            Some("归属"),
        );
        let e3 = make_event_with_motives(
            "E3",
            "s",
            Some("工作"),
            0.7,
            0.7,
            0.3,
            0.5,
            Presentation::Mixed,
            None,
            Some("地位维护"),
        );
        let events = vec![e1, e2, e3];
        let grouped = group_by_motive(&events);

        // 应该有 3 个动机标签: 地位维护, 自主性, 归属
        assert_eq!(grouped.len(), 3);

        // 地位维护应该有 2 个事件 (E1, E3)
        let status_group: Vec<_> = grouped.iter().filter(|(k, _)| k == "地位维护").collect();
        assert_eq!(status_group.len(), 1);
        assert_eq!(status_group[0].1.len(), 2);

        // 自主性应该有 1 个事件 (E1)
        let autonomy_group: Vec<_> = grouped.iter().filter(|(k, _)| k == "自主性").collect();
        assert_eq!(autonomy_group.len(), 1);
        assert_eq!(autonomy_group[0].1.len(), 1);

        // 归属应该有 1 个事件 (E2)
        let belonging_group: Vec<_> = grouped.iter().filter(|(k, _)| k == "归属").collect();
        assert_eq!(belonging_group.len(), 1);
        assert_eq!(belonging_group[0].1.len(), 1);
    }

    #[test]
    fn group_by_motive_empty_when_no_motives() {
        let e1 = make_event_with_motives(
            "E1",
            "s",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
            None,
        );
        let e2 = make_event_with_motives(
            "E2",
            "s",
            Some("社交"),
            0.8,
            0.6,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            None,
        );
        let grouped = group_by_motive(&[e1, e2]);
        assert!(grouped.is_empty());
    }

    #[test]
    fn compute_motive_stats_basic() {
        let events = vec![
            make_event_with_motives(
                "E1",
                "s1",
                Some("工作"),
                0.9,
                0.8,
                0.6,
                0.5,
                Presentation::Mixed,
                None,
                Some("地位维护,自主性"),
            ),
            make_event_with_motives(
                "E2",
                "s2",
                Some("社交"),
                0.8,
                0.6,
                -0.3,
                0.7,
                Presentation::Subjective,
                None,
                Some("归属"),
            ),
            make_event_with_motives(
                "E3",
                "s3",
                Some("工作"),
                0.7,
                0.7,
                0.2,
                0.4,
                Presentation::Objective,
                None,
                Some("地位维护,公平"),
            ),
        ];

        let enrichments = EventEnrichment::derive_batch(&events);
        let config = CalibratedWeightConfig::default();
        let stats = compute_motive_stats(&events, &enrichments, &config);

        // 应该有 4 个动机标签: 地位维护, 自主性, 归属, 公平
        assert_eq!(stats.len(), 4);

        // 按 n_eff 降序排列，地位维护应该排第一（2个事件）
        assert_eq!(stats[0].motive, "地位维护");
        assert_eq!(stats[0].event_count, 2);
        assert!(stats[0].n_eff > 0.0);
        assert!(stats[0].valence_mean > 0.0); // 两个事件都正值

        // 归属只有1个事件，负效价
        let belonging = stats.iter().find(|s| s.motive == "归属").unwrap();
        assert_eq!(belonging.event_count, 1);
        assert!(belonging.valence_mean < 0.0);
        assert!(belonging.valence_positive_ratio < 0.5);
    }

    /// compute_motive_stats 空结果各分支参数化验证：事件无 motives / 空事件列表。
    #[test]
    fn compute_motive_stats_empty_cases() {
        // 事件存在但无 motives → 空
        let events = vec![make_event_with_motives(
            "E1",
            "s",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
            None,
        )];
        let enrichments = EventEnrichment::derive_batch(&events);
        let config = CalibratedWeightConfig::default();
        let stats = compute_motive_stats(&events, &enrichments, &config);
        assert!(stats.is_empty());
        // 空事件列表 → 空
        let enrichments: Vec<EventEnrichment> = Vec::new();
        let stats = compute_motive_stats(&[], &enrichments, &config);
        assert!(stats.is_empty());
    }

    // =========================================================
    // CalibratedWeightConfig::default() 验证
    // =========================================================

    #[test]
    fn calibrated_weight_config_defaults() {
        let config = CalibratedWeightConfig::default();
        assert!((config.salience_exponent - 1.0).abs() < 1e-10);
        assert!((config.recurrence_boost_max - 0.30).abs() < 1e-10);
        assert!((config.intensity_boost_max - 0.20).abs() < 1e-10);
        assert!((config.mention_boost_max - 0.15).abs() < 1e-10);
        assert_eq!(config.min_sources_for_full_support, 3);
        assert!((config.tentative_weight_factor - 0.5).abs() < 1e-10);
    }

    // =========================================================
    // 批量权重计算
    // =========================================================

    #[test]
    fn test_compute_calibrated_weights_batch() {
        let config = CalibratedWeightConfig::default();
        let events = vec![
            make_event(
                "E1",
                "s1",
                None,
                0.9,
                0.8,
                0.5,
                0.5,
                Presentation::Mixed,
                None,
            ),
            make_event(
                "E2",
                "s2",
                None,
                0.5,
                0.6,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
            ),
        ];
        let enrichments = vec![
            EventEnrichment::from_event(&events[0]),
            EventEnrichment::from_event(&events[1]),
        ];
        let weights = compute_calibrated_weights_batch(&events, &enrichments, &config);
        assert_eq!(weights.len(), 2);
        // E1 (confirmed) 应比 E2 (tentative) 权重更高
        assert!(
            weights[0] > weights[1],
            "confirmed 事件权重应高于 tentative"
        );
    }

    #[test]
    #[should_panic(expected = "events 与 enrichments 长度必须一致")]
    fn test_compute_calibrated_weights_batch_mismatch() {
        let config = CalibratedWeightConfig::default();
        let events = vec![make_event(
            "E1",
            "s",
            None,
            0.9,
            0.8,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        )];
        let enrichments = vec![EventEnrichment::default(), EventEnrichment::default()];
        compute_calibrated_weights_batch(&events, &enrichments, &config);
    }
}
