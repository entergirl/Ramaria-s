//! crates/ramaria-cli/src/commands/rule/incremental/stats.rs - 行为增量模拟统计辅助
//!
//! 设计特点:
//! - 文本字段判定、百分位与簇形态汇总。
//! - 全对融合相似度分布统计（含样本对数上限保护）。

use super::super::clusters::MAX_SIMILARITY_PAIRS;
use ramaria_memory::behavior::BehaviorSample;
use ramaria_memory::behavior::fused_similarity;

/// 判断可选文本字段是否存在有效内容（None / 纯空白视为空）。
pub(crate) fn has_text(value: Option<&str>) -> bool {
    value.is_some_and(|s| !s.trim().is_empty())
}

/// 计算升序序列的百分位（线性插值）。
///
/// 参数:
/// - `sorted`: 升序排列的数值序列。
/// - `p`: 百分位（0.0..=1.0，越界自动 clamp）。
///
/// 返回:
/// - 位置 `p·(n−1)` 处的线性插值；空输入无定义，返回 0.0。
pub(crate) fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let p = p.clamp(0.0, 1.0);
    let pos = p * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = pos - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// 簇形状汇总（最大簇占比与孤立点统计）。
///
/// 职责:
/// - 由各簇规模与样本总数推导 max_share / outlier_count / outlier_ratio，供输出与单测复用。
///
/// 字段约定:
/// - `max_share`: 最大簇成员数 / 样本总数（无样本 → 0.0）。
/// - `outlier_count`: 未入簇样本数（样本总数 − 各簇规模之和）。
/// - `outlier_ratio`: outlier_count / 样本总数（无样本 → 0.0）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClusterShapeSummary {
    pub(crate) max_share: f64,
    pub(crate) outlier_count: usize,
    pub(crate) outlier_ratio: f64,
}

/// 汇总簇形状统计（纯函数）。
///
/// 参数:
/// - `sizes`: 各簇成员数（顺序无关，内部取最大值）。
/// - `total`: 参与聚类的样本总数（含孤立点）。
///
/// 返回:
/// - 最大簇占比与孤立点统计；`total = 0` 时全部为 0。
pub(crate) fn summarize_cluster_shapes(sizes: &[usize], total: usize) -> ClusterShapeSummary {
    if total == 0 {
        return ClusterShapeSummary {
            max_share: 0.0,
            outlier_count: 0,
            outlier_ratio: 0.0,
        };
    }
    let max_size = sizes.iter().copied().max().unwrap_or(0);
    let assigned: usize = sizes.iter().sum();
    let outlier_count = total.saturating_sub(assigned);
    ClusterShapeSummary {
        max_share: max_size as f64 / total as f64,
        outlier_count,
        outlier_ratio: outlier_count as f64 / total as f64,
    }
}

/// 全对融合相似度的分布统计。
///
/// 字段约定:
/// - `pairs`: 参与统计的样本对数 n·(n−1)/2。
/// - `min` / `p25` / `p50` / `p75` / `p90` / `max`: 升序分布的百分位（线性插值）。
/// - `mean`: 算术平均。
#[derive(Debug, Clone)]
pub(crate) struct SimilarityStats {
    pub(crate) pairs: usize,
    pub(crate) min: f64,
    pub(crate) p25: f64,
    pub(crate) p50: f64,
    pub(crate) p75: f64,
    pub(crate) p90: f64,
    pub(crate) max: f64,
    pub(crate) mean: f64,
}

/// 相似度分布的计算结果。
///
/// 职责:
/// - 区分「已算出分布」与「跳过（样本不足 / 对数超限）」两种状态，供输出层统一处理。
pub(crate) enum SimilarityDistribution {
    /// 完整的全对分布统计。
    Computed(SimilarityStats),
    /// 跳过计算（附跳过原因）。
    Skipped(String),
}

/// 计算全部样本对的融合相似度分布（纯计算，不写库）。
///
/// 参数:
/// - `samples`: 已向量化的行为样本。
/// - `beta1` / `beta2`: 三路融合权重（仅本次计算）。
///
/// 返回:
/// - `Computed`: 对数在 `MAX_SIMILARITY_PAIRS` 内且 ≥ 1 时的分布统计。
/// - `Skipped`: 样本不足 2 条（无可比较对），或对数超限（记 warn 后跳过）。
pub(crate) fn compute_similarity_stats(
    samples: &[BehaviorSample],
    beta1: f64,
    beta2: f64,
) -> SimilarityDistribution {
    let pair_count = samples
        .len()
        .saturating_mul(samples.len().saturating_sub(1))
        / 2;
    if pair_count > MAX_SIMILARITY_PAIRS {
        tracing::warn!(
            pairs = pair_count,
            max_pairs = MAX_SIMILARITY_PAIRS,
            "行为聚类样本对数超过上限，跳过相似度分布计算"
        );
        return SimilarityDistribution::Skipped(format!(
            "样本对数 {pair_count} 超过上限 {MAX_SIMILARITY_PAIRS}，已跳过相似度分布计算"
        ));
    }
    if pair_count == 0 {
        return SimilarityDistribution::Skipped("样本不足 2 条，无可比较样本对".to_string());
    }

    let mut sims: Vec<f64> = Vec::with_capacity(pair_count);
    for i in 0..samples.len() {
        for j in (i + 1)..samples.len() {
            sims.push(fused_similarity(&samples[i], &samples[j], beta1, beta2));
        }
    }
    sims.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean = sims.iter().sum::<f64>() / sims.len() as f64;
    SimilarityDistribution::Computed(SimilarityStats {
        pairs: pair_count,
        min: percentile(&sims, 0.0),
        p25: percentile(&sims, 0.25),
        p50: percentile(&sims, 0.5),
        p75: percentile(&sims, 0.75),
        p90: percentile(&sims, 0.9),
        max: percentile(&sims, 1.0),
        mean,
    })
}
