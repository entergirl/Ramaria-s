//! rust/crates/ramaria-memory/src/inference/stats.rs - 统计特征提取
//!
//! 设计特点:
//! - A1 预过滤: confidence < 0.6 的事件排除（唯一硬截断），salience 作为全链路连续权重
//! - A3 按领域分类聚合: 按 keywords 主分类分组，计算 salience 加权均值/方差/有效样本量
//! - 情境强度加权: 弱情境(1-2)→×1.5，中性(3)/None→×1.0，强情境(4-5)→×0.5
//! - A6 跨分类高阶指标: 情绪稳定性、叙事一致性、态度矛盾检测、社交开放性
//! - A7 代表性事件选取: 每分类取 salience 最高的 2-3 条事件
//! - 纯数值计算，零 I/O，不依赖数据库或异步运行时，所有输入由调用方传入
//! - 可独立单元测试，无需 mock StorageBackend

use ramaria_core::types::{MemoryEvent, Presentation};

// =========================================================
// 配置类型
// =========================================================

/// 统计配置。
///
/// 职责:
/// - 集中管理预过滤阈值、代表性事件数量和分组策略参数。
///
/// 字段约定:
/// - `confidence_threshold`: 事件置信度门槛，默认 0.6。低于此值的事件不参与任何统计。
/// - `max_representative_events`: 每分类最多选取的代表性事件数，默认 3。
#[derive(Debug, Clone)]
pub struct StatsConfig {
    /// 事件置信度门槛（唯一硬截断），默认 0.6
    pub confidence_threshold: f64,
    /// 每分类最多选取的代表性事件数，默认 3
    pub max_representative_events: usize,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.6,
            max_representative_events: 3,
        }
    }
}

// =========================================================
// 统计输出类型
// =========================================================

/// 单分类统计摘要（A3 输出）。
///
/// 职责:
/// - 封装一个关键词分类下的全部 salience 加权统计量。
/// - 作为 LLM 推断的逐分类输入。
///
/// 字段约定:
/// - `category`: 主分类标签，如"工作""社交""家庭"，从事件 keywords 的第一个标签提取。
/// - `event_count`: 该分类的原始事件数（仅用于诊断，不参与计算）。
/// - `n_eff`: salience 加权有效样本量 = Σ salience_i。
/// - `valence_mean`: salience 加权平均效价。
/// - `valence_std`: salience 加权效价标准差。
/// - `valence_positive_ratio`: 正面事件（valence > 0）的 salience 加权占比。
/// - `share_mean`: salience 加权平均分享意愿。
/// - `share_std`: salience 加权分享意愿标准差。
/// - `presentation_objective_ratio / subjective_ratio / mixed_ratio`: 三种陈述方式的加权占比，和为 1。
/// - `group_weight`: 该分类在全局画像中的相对权重 = (n_eff / 总 n_eff) × 该分类平均 salience。
#[derive(Debug, Clone)]
pub struct CategoryStats {
    /// 主分类标签
    pub category: String,
    /// 原始事件数（仅诊断）
    pub event_count: usize,
    /// salience 加权有效样本量
    pub n_eff: f64,
    /// salience 加权平均效价
    pub valence_mean: f64,
    /// salience 加权效价标准差
    pub valence_std: f64,
    /// 正面事件加权占比（valence > 0）
    pub valence_positive_ratio: f64,
    /// salience 加权平均分享意愿
    pub share_mean: f64,
    /// salience 加权分享意愿标准差
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
/// - 为 的"区分底色/点缀"提供数值依据。
///
/// 字段约定:
/// - `emotional_stability`: 全局 valence 加权标准差。值越小情绪越平稳。
/// - `narrative_consistency`: 跨分类 presentation 分布相似度的均值（Jensen-Shannon 散度的补数）。
/// - `attitude_contradiction_count`: 态度矛盾指示器（当前为占位：≥2 分类时标记 1，精确计数待 接入真实 embedding 后通过 cross-cluster centroid cosine similarity 计算）。
/// - `share_skewness`: 全局 share 分布的偏度。正值=右偏（少数事件 share 很高），负值=左偏。
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

/// 完整统计摘要。
///
/// 职责:
/// - 聚合 A1/A3/A6/A7 的全部输出，作为 LLM 推断的完整输入。
/// - 所有字段由 `run_phase_a_stats` 一次性计算。
#[derive(Debug, Clone)]
pub struct StatsSummary {
    /// 输入事件总数（预过滤前）
    pub total_events_in: usize,
    /// 预过滤后事件数
    pub total_events_filtered: usize,
    /// 分类数
    pub category_count: usize,
    /// 按组权重降序排列的逐分类统计
    pub categories: Vec<CategoryStats>,
    /// 跨分类高阶指标
    pub cross_category: CrossCategoryMetrics,
    /// 每分类的代表性事件（按 salience 降序，最多 max_representative_events 条）
    pub representative_events: Vec<RepresentativeEvent>,
}

// =========================================================
// A1: 预过滤
// =========================================================

/// 预过滤事件：排除 confidence 低于阈值的推测性事件。
///
/// 说明:
/// - 这是 中唯一的硬截断。
/// - salience 不做截断，作为连续权重贯穿后续全部计算。
/// - 被排除的事件保留在存储中，待未来交叉验证提升置信度后重新吸收。
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
        .filter(|e| e.confidence >= config.confidence_threshold)
        .cloned()
        .collect();
    let excluded = total - filtered.len();
    (filtered, excluded)
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

/// 计算 salience 加权均值。
///
/// 公式: x̄_w = Σ(w_i · x_i) / Σ w_i
///
/// 参数:
/// - `values`: 各事件的指标取值。
/// - `weights`: 各事件的 salience 权重（需与 values 一一对应）。
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

/// 计算 salience 加权方差（总体方差，非样本方差）。
///
/// 公式: σ²_w = Σ(w_i · (x_i - x̄_w)²) / Σ w_i
///
/// 参数:
/// - `values`: 各事件的指标取值。
/// - `weights`: 各事件的 salience 权重。
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

/// 计算 salience 加权占比（用于正面事件比例和 presentation 分布）。
///
/// 公式: ratio = Σ(indicator_i · w_i) / Σ w_i
///
/// 参数:
/// - `indicators`: 各事件的指示器值（0.0 或 1.0）。
/// - `weights`: 各事件的 salience 权重。
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

/// 计算单个分类的全部统计量。
///
/// 参数:
/// - `category`: 分类标签。
/// - `events`: 该分类下的全部事件。
///
/// 返回:
/// - 包含所有 salience 加权统计量的 CategoryStats。
pub fn compute_category_stats(category: &str, events: &[MemoryEvent]) -> CategoryStats {
    let event_count = events.len();
    // salience × situation_multiplier → 有效权重
    let weights: Vec<f64> = events
        .iter()
        .map(|e| e.salience * situation_multiplier(e.situation_strength))
        .collect();
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

/// 计算情绪稳定性（全局 valence 加权标准差）。
///
/// 说明:
/// - 不按分类分组，直接对全部事件的 valence 做 salience 加权标准差。
/// - 标准差小 → 情绪平稳；标准差大 → 情绪波动剧烈。
///
/// 参数:
/// - `events`: 预过滤后的事件列表。
///
/// 返回:
/// - 全局加权 valence 标准差。
pub fn compute_emotional_stability(events: &[MemoryEvent]) -> f64 {
    let valences: Vec<f64> = events.iter().map(|e| e.valence).collect();
    let weights: Vec<f64> = events
        .iter()
        .map(|e| e.salience * situation_multiplier(e.situation_strength))
        .collect();
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

/// 计算 share 分布的偏度（基于 salience 加权）。
///
/// 公式: skew = Σ(w_i · (x_i - x̄)³) / (σ³ · Σ w_i)
///
/// 参数:
/// - `events`: 预过滤后的事件列表。
///
/// 返回:
/// - 偏度系数。正值=右偏（少数事件 share 很高），负值=左偏。
pub fn compute_share_skewness(events: &[MemoryEvent]) -> f64 {
    let shares: Vec<f64> = events.iter().map(|e| e.share).collect();
    let weights: Vec<f64> = events
        .iter()
        .map(|e| e.salience * situation_multiplier(e.situation_strength))
        .collect();
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

/// 计算 share 分布的峰度（基于 salience 加权）。
///
/// 公式: kurt = Σ(w_i · (x_i - x̄)⁴) / (σ⁴ · Σ w_i)
///
/// 参数:
/// - `events`: 预过滤后的事件列表。
///
/// 返回:
/// - 峰度系数。正值=尖峰分布，负值=扁平分布。
pub fn compute_share_kurtosis(events: &[MemoryEvent]) -> f64 {
    let shares: Vec<f64> = events.iter().map(|e| e.share).collect();
    let weights: Vec<f64> = events
        .iter()
        .map(|e| e.salience * situation_multiplier(e.situation_strength))
        .collect();
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
/// - `events`: 预过滤后的事件列表。
/// - `categories`: 所有分类的统计摘要。
///
/// 返回:
/// - CrossCategoryMetrics 结构体。
pub fn compute_cross_category_metrics(
    events: &[MemoryEvent],
    categories: &[CategoryStats],
) -> CrossCategoryMetrics {
    let emotional_stability = compute_emotional_stability(events);
    let narrative_consistency = compute_narrative_consistency(categories);
    // 态度矛盾检测在 中基于分类对做标记，具体计数由 语义判断
    // 此处预留基础指标：分类数 >= 2 时标记可能存在矛盾
    let attitude_contradiction_count = if categories.len() >= 2 { 1 } else { 0 };
    let share_skewness = compute_share_skewness(events);
    let share_kurtosis = compute_share_kurtosis(events);

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

/// 执行完整的 统计管线。
///
/// 管线步骤:
/// 1. A1: 预过滤（排除 confidence < 0.6 的事件）
/// 2. A3: 按 keywords 主分类分组，计算 salience 加权统计量
/// 3. A6: 计算跨分类高阶指标
/// 4. A7: 选取代表性事件
///
/// 参数:
/// - `events`: 完整的 L2 事件列表（从 StorageBackend 读取）。
/// - `config`: 统计配置。
///
/// 返回:
/// - `StatsSummary`: 包含分类统计、跨分类指标和代表性事件的完整摘要。
///
/// 说明:
/// - 若预过滤后无可用事件，返回空的 StatsSummary（categories 为空）。
/// - 日志在调用方记录，本函数不执行 I/O。
pub fn run_phase_a_stats(events: &[MemoryEvent], config: &StatsConfig) -> StatsSummary {
    let total_events_in = events.len();

    // A1: 预过滤
    let (filtered, _excluded) = prefilter_events(events, config);
    let total_events_filtered = filtered.len();

    if filtered.is_empty() {
        return StatsSummary {
            total_events_in,
            total_events_filtered: 0,
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
        };
    }

    // A3: 按分类聚合
    let grouped = group_by_category(&filtered);
    let mut categories: Vec<CategoryStats> = grouped
        .iter()
        .map(|(cat, evts)| compute_category_stats(cat, evts))
        .collect();
    normalize_group_weights(&mut categories);
    let category_count = categories.len();

    // A6: 跨分类指标
    let cross_category = compute_cross_category_metrics(&filtered, &categories);

    // A7: 代表性事件
    let representative_events = select_representative_events(&filtered, &categories, config);

    StatsSummary {
        total_events_in,
        total_events_filtered,
        category_count,
        categories,
        cross_category,
        representative_events,
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

    // ---- 情境强度乘数 ----

    #[test]
    fn situation_multiplier_none_is_neutral() {
        assert!((situation_multiplier(None) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn situation_multiplier_3_is_neutral() {
        assert!((situation_multiplier(Some(3)) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn situation_multiplier_weak_amplifies() {
        assert!((situation_multiplier(Some(1)) - 1.5).abs() < 1e-10);
        assert!((situation_multiplier(Some(2)) - 1.5).abs() < 1e-10);
    }

    #[test]
    fn situation_multiplier_strong_dampens() {
        assert!((situation_multiplier(Some(4)) - 0.5).abs() < 1e-10);
        assert!((situation_multiplier(Some(5)) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn situation_multiplier_invalid_fallsback_to_neutral() {
        // 非法值（0 或 6+）应退回到中性
        assert!((situation_multiplier(Some(0)) - 1.0).abs() < 1e-10);
        assert!((situation_multiplier(Some(6)) - 1.0).abs() < 1e-10);
        assert!((situation_multiplier(Some(100)) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn category_stats_respects_situation_multiplier() {
        // 两条事件：相同 salience 但不同情境 → 权重应不同
        let weak_ev = make_event(
            "弱情境事件",
            "摘要",
            Some("工作"),
            0.9,
            0.8, // salience
            0.5,
            0.5,
            Presentation::Mixed,
            None,
        );
        let strong_ev = make_event(
            "强情境事件",
            "摘要",
            Some("工作"),
            0.9,
            0.8, // same salience
            0.5,
            0.5,
            Presentation::Mixed,
            None,
        );

        // Manually set situation_strength
        let events = vec![
            {
                let mut e = weak_ev;
                e.situation_strength = Some(2); // 弱情境 ×1.5
                e
            },
            {
                let mut e = strong_ev;
                e.situation_strength = Some(5); // 强情境 ×0.5
                e
            },
        ];

        let stats = compute_category_stats("工作", &events);
        // n_eff = 0.8*1.5 + 0.8*0.5 = 1.2 + 0.4 = 1.6
        assert!((stats.n_eff - 1.6).abs() < 1e-10);
    }

    #[test]
    fn category_stats_default_situation_neutral() {
        let events = vec![make_event(
            "默认情境",
            "摘要",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            None,
        )];
        // situation_strength = None → ×1.0
        let stats = compute_category_stats("工作", &events);
        assert!((stats.n_eff - 0.8).abs() < 1e-10);
    }

    // ---- A1 预过滤 ----

    #[test]
    fn prefilter_excludes_low_confidence() {
        let config = StatsConfig::default();
        let events = vec![
            make_event(
                "E1",
                "摘要1",
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
                "摘要2",
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
                "摘要3",
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
        // E1 和 E3 应保留
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
                "摘要1",
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
                "摘要2",
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

    // ---- 主分类提取 ----

    #[test]
    fn extract_primary_category_from_keywords() {
        let ev = make_event(
            "E1",
            "摘要",
            Some("工作, 会议, 紧张"),
            0.8,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        assert_eq!(extract_primary_category(&ev), "工作");
    }

    #[test]
    fn extract_primary_category_single_keyword() {
        let ev = make_event(
            "E1",
            "摘要",
            Some("家庭"),
            0.8,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        assert_eq!(extract_primary_category(&ev), "家庭");
    }

    #[test]
    fn extract_primary_category_none_keywords() {
        let ev = make_event(
            "E1",
            "摘要",
            None,
            0.8,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        assert_eq!(extract_primary_category(&ev), "未分类");
    }

    #[test]
    fn extract_primary_category_empty_keywords() {
        let ev = make_event(
            "E1",
            "摘要",
            Some(""),
            0.8,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        assert_eq!(extract_primary_category(&ev), "未分类");
    }

    // ---- 加权统计 ----

    #[test]
    fn weighted_mean_basic() {
        let values = vec![1.0, 2.0, 3.0];
        let weights = vec![1.0, 1.0, 1.0];
        let mean = weighted_mean(&values, &weights);
        assert!((mean - 2.0).abs() < 1e-10);
    }

    #[test]
    fn weighted_mean_with_weights() {
        // Σ(w*x) = 0.2*0.5 + 0.5*0.5 + 0.8*1.0 = 0.1 + 0.25 + 0.8 = 1.15
        // Σw = 0.2 + 0.5 + 0.8 = 1.5
        // mean = 1.15/1.5 ≈ 0.7667
        let values = vec![0.5, 0.5, 1.0];
        let weights = vec![0.2, 0.5, 0.8];
        let mean = weighted_mean(&values, &weights);
        assert!((mean - 0.7666).abs() < 0.001);
    }

    #[test]
    fn weighted_mean_zero_weights() {
        let values = vec![1.0, 2.0];
        let weights = vec![0.0, 0.0];
        let mean = weighted_mean(&values, &weights);
        assert!((mean - 0.0).abs() < 1e-10);
    }

    #[test]
    fn weighted_variance_basic() {
        // 值 [1,2,3]，权重[1,1,1]，均值=2
        // var = ((1-2)²+(2-2)²+(3-2)²)/3 = 2/3 ≈ 0.6667
        let values = vec![1.0, 2.0, 3.0];
        let weights = vec![1.0, 1.0, 1.0];
        let var = weighted_variance(&values, &weights, 2.0);
        assert!((var - 2.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn weighted_ratio_basic() {
        // 3 条事件，2 条正面
        let indicators = vec![1.0, 0.0, 1.0];
        let weights = vec![0.4, 0.6, 1.0];
        // Σ(ind*w) = 0.4 + 0 + 1.0 = 1.4, Σw = 2.0, ratio = 0.7
        let ratio = weighted_ratio(&indicators, &weights);
        assert!((ratio - 0.7).abs() < 1e-10);
    }

    // ---- 单分类统计 ----

    #[test]
    fn category_stats_computation() {
        let events = vec![
            make_event(
                "E1",
                "工作摘要1",
                Some("工作,会议"),
                0.9,
                0.8,
                0.5,
                0.7,
                Presentation::Objective,
                Some("对项目进展满意"),
            ),
            make_event(
                "E2",
                "工作摘要2",
                Some("工作,项目"),
                0.8,
                0.6,
                -0.3,
                0.4,
                Presentation::Subjective,
                Some("对截止日期感到焦虑"),
            ),
            make_event(
                "E3",
                "工作摘要3",
                Some("工作,汇报"),
                0.7,
                0.9,
                0.2,
                0.6,
                Presentation::Mixed,
                Some("汇报结果一般"),
            ),
        ];
        let stats = compute_category_stats("工作", &events);

        assert_eq!(stats.category, "工作");
        assert_eq!(stats.event_count, 3);
        // n_eff = 0.8 + 0.6 + 0.9 = 2.3
        assert!((stats.n_eff - 2.3).abs() < 1e-10);
        // valence_mean = (0.8*0.5 + 0.6*(-0.3) + 0.9*0.2) / 2.3 = (0.4 - 0.18 + 0.18) / 2.3 = 0.4/2.3 ≈ 0.1739
        assert!((stats.valence_mean - 0.4 / 2.3).abs() < 0.001);
    }

    #[test]
    fn category_stats_single_event() {
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
        let stats = compute_category_stats("家庭", &events);
        assert_eq!(stats.event_count, 1);
        assert!((stats.n_eff - 0.5).abs() < 1e-10);
        assert!((stats.valence_mean - 0.8).abs() < 1e-10);
        // 单事件方差为 0
        assert!((stats.valence_std - 0.0).abs() < 1e-10);
        assert!((stats.presentation_subjective_ratio - 1.0).abs() < 1e-10);
    }

    // ---- 分组 ----

    #[test]
    fn group_by_category_works() {
        let events = vec![
            make_event(
                "E1",
                "摘要1",
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
                "摘要2",
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
                "摘要3",
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
        let work_group = grouped.iter().find(|(k, _)| k == "社交").unwrap();
        assert_eq!(work_group.1.len(), 1);
        let work_group = grouped.iter().find(|(k, _)| k == "工作").unwrap();
        assert_eq!(work_group.1.len(), 2);
    }

    // ---- 组权重归一化 ----

    #[test]
    fn group_weights_normalize() {
        let events = vec![
            make_event(
                "E1",
                "摘要1",
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
                "摘要2",
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
            .map(|(cat, evts)| compute_category_stats(cat, evts))
            .collect();
        normalize_group_weights(&mut cats);
        let total: f64 = cats.iter().map(|c| c.group_weight).sum();
        assert!(
            (total - 1.0).abs() < 1e-10,
            "权重应归一化为和为1，实际为{}",
            total
        );
    }

    // ---- 跨分类指标 ----

    #[test]
    fn emotional_stability_computation() {
        let events = vec![
            make_event(
                "E1",
                "摘要1",
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
                "摘要2",
                Some("社交"),
                0.8,
                0.5,
                -0.3,
                0.5,
                Presentation::Mixed,
                None,
            ),
        ];
        let stability = compute_emotional_stability(&events);
        // 等权重均值 = (0.5 + (-0.3)) / 2 = 0.1
        // 方差 = ((0.5-0.1)² + (-0.3-0.1)²) / 2 = (0.16 + 0.16) / 2 = 0.16
        // std ≈ 0.4
        assert!(stability > 0.0, "方差应大于零");
        assert!((stability - 0.4).abs() < 0.01);
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
        assert!(
            (consistency - 1.0).abs() < 1e-10,
            "完全相同的分布应得一致性 1.0"
        );
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

    // ---- 代表性事件选取 ----

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
        let grouped = group_by_category(&events);
        let cats: Vec<CategoryStats> = grouped
            .iter()
            .map(|(cat, evts)| compute_category_stats(cat, evts))
            .collect();
        let representatives = select_representative_events(&events, &cats, &config);
        assert_eq!(representatives.len(), 2, "最多 2 条");
        // 应是 salience 最高的两条
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
        let grouped = group_by_category(&events);
        let cats: Vec<CategoryStats> = grouped
            .iter()
            .map(|(cat, evts)| compute_category_stats(cat, evts))
            .collect();
        let reps = select_representative_events(&events, &cats, &config);
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].attitude.as_deref(), Some("对项目进展感到满意"));
    }

    // ---- 完整管线 ----

    #[test]
    fn run_phase_a_stats_full_pipeline() {
        let config = StatsConfig::default();
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
        assert_eq!(summary.total_events_filtered, 3); // E2 被排除
        assert_eq!(summary.category_count, 3); // 工作、社交、家庭
        assert!(!summary.categories.is_empty());
        // 分类按 group_weight 降序
        assert!(
            summary.categories[0].group_weight >= summary.categories.last().unwrap().group_weight
        );
        // 跨分类指标
        assert!(summary.cross_category.emotional_stability >= 0.0);
        // 代表性事件
        assert!(!summary.representative_events.is_empty());
    }

    #[test]
    fn run_phase_a_stats_empty_after_filter() {
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
        assert_eq!(summary.category_count, 0);
        assert!(summary.categories.is_empty());
        assert!(summary.representative_events.is_empty());
    }

    #[test]
    fn run_phase_a_stats_empty_input() {
        let config = StatsConfig::default();
        let summary = run_phase_a_stats(&[], &config);
        assert_eq!(summary.total_events_in, 0);
        assert_eq!(summary.total_events_filtered, 0);
    }

    // ---- 偏度与峰度 ----

    #[test]
    fn share_skewness_symmetric() {
        // 对称分布：share = [0.3, 0.5, 0.7]，等权重
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
        let skew = compute_share_skewness(&events);
        // 对称分布偏度应接近 0
        assert!(skew.abs() < 0.1, "对称分布偏度应接近0，实际={}", skew);
    }

    #[test]
    fn share_kurtosis_uniform() {
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
        let kurt = compute_share_kurtosis(&events);
        // 两个相同值，方差为 0，应返回 0
        assert!((kurt - 0.0).abs() < 1e-10);
    }

    #[test]
    fn share_skewness_single_event() {
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
        let skew = compute_share_skewness(&events);
        // 单事件方差为 0，偏度为 0
        assert!((skew - 0.0).abs() < 1e-10);
    }
}
