//! crates/ramaria-cli/src/commands/rule/clusters/support.rs - 行为聚类统计辅助
//!
//! 设计特点:
//! - 反事实聚类汇总与质控闸门归类计数。
//! - 复刻真实管线口径的 θ_nb 重试聚类与参数校验。

use super::super::incremental::summarize_cluster_shapes;
use ramaria_core::config::BehaviorConfig;
use ramaria_memory::behavior::BehaviorSample;
use ramaria_memory::behavior::DensityClusterResult;
use ramaria_memory::behavior::QualityVerdict;
use ramaria_memory::behavior::RefinedCluster;
use ramaria_memory::behavior::RuleDegradeReason;
use ramaria_memory::behavior::RuleGenConfig;
use ramaria_memory::behavior::density_cluster;
use ramaria_memory::behavior::quality_gate;

// =========================================================
// clusters 辅助：反事实对照与管线口径估算
// =========================================================

/// min−1 反事实聚类汇总。
///
/// 职责:
/// - 量化 `min_cluster_size` 口径对成簇的影响：口径放宽 1 后回收的样本与小簇数量。
///
/// 字段约定:
/// - `min_cluster_size`: 反事实口径（本次覆盖值 − 1，下限 1）。
/// - `count` / `sizes`（降序）/ `outlier_count` / `outlier_ratio`: 反事实聚类结构。
/// - `recovered_samples`: 原始孤立数 − 反事实孤立数（> 0 = 口径放宽后回收的样本数）。
/// - `three_member_clusters`: 反事实中各簇规模恰为 3 的簇数量。本次 `min_cluster_size = 3`
///   （反事实 = 2）时即"被 min=3 口径整体孤置的 3 样本小簇"数量；其余 min 口径下该字段
///   固定按"规模恰为 3"计数，不再对应"被原口径孤置的增量小簇"（其规模为本次 min），
///   仅作规模参考。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CounterfactualSummary {
    pub(crate) min_cluster_size: usize,
    pub(crate) count: usize,
    pub(crate) sizes: Vec<usize>,
    pub(crate) outlier_count: usize,
    pub(crate) outlier_ratio: f64,
    pub(crate) recovered_samples: i64,
    pub(crate) three_member_clusters: usize,
}

/// 汇总 min−1 反事实聚类结果（纯函数）。
///
/// 参数:
/// - `min_cluster_size`: 反事实口径。
/// - `cluster_sizes`: 反事实各簇成员数（顺序无关，内部按降序排序）。
/// - `total`: 参与聚类的样本总数（含孤立点）。
/// - `original_outlier_count`: 原始口径的孤立点数（`recovered_samples` 的基准）。
///
/// 返回:
/// - 反事实簇结构统计；`total = 0` 时孤立点统计为 0。
pub(crate) fn summarize_counterfactual(
    min_cluster_size: usize,
    cluster_sizes: &[usize],
    total: usize,
    original_outlier_count: usize,
) -> CounterfactualSummary {
    let mut sizes = cluster_sizes.to_vec();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    let shape = summarize_cluster_shapes(&sizes, total);
    CounterfactualSummary {
        min_cluster_size,
        count: sizes.len(),
        three_member_clusters: sizes.iter().filter(|&&s| s == 3).count(),
        recovered_samples: original_outlier_count as i64 - shape.outlier_count as i64,
        sizes,
        outlier_count: shape.outlier_count,
        outlier_ratio: shape.outlier_ratio,
    }
}

/// 质控闸门归类计数。
///
/// 字段约定:
/// - `pass`: 通过闸门的簇数（可生成完整规则）。
/// - `low_evidence` / `low_neff` / `high_valence_variance`: 各降级原因的簇数；
///   三类互斥（`quality_gate` 按证据量 → n_eff → valence 方差的顺序返回首个不满足项）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GateTally {
    pub(crate) pass: usize,
    pub(crate) low_evidence: usize,
    pub(crate) low_neff: usize,
    pub(crate) high_valence_variance: usize,
}

/// 逐簇判定质控闸门并归类计数（纯函数）。
///
/// 参数:
/// - `clusters`: 提炼后的簇列表（`refine_cluster` 输出）。
/// - `config`: 闸门阈值（由 `RuleGenConfig::from(&BehaviorConfig)` 派生）。
///
/// 返回:
/// - 通过数与三类降级原因计数。
pub(crate) fn tally_quality_gate(clusters: &[RefinedCluster], config: &RuleGenConfig) -> GateTally {
    let mut tally = GateTally::default();
    for cluster in clusters {
        match quality_gate(cluster, config) {
            QualityVerdict::Pass => tally.pass += 1,
            QualityVerdict::Degrade(RuleDegradeReason::LowEvidence) => tally.low_evidence += 1,
            QualityVerdict::Degrade(RuleDegradeReason::LowNeff) => tally.low_neff += 1,
            QualityVerdict::Degrade(RuleDegradeReason::HighValenceVariance) => {
                tally.high_valence_variance += 1;
            }
            // `quality_gate` 只产生以上三类原因；其余变体来自 LLM 翻译阶段，
            // 不在闸门口径内，防御性忽略并记 warn。
            QualityVerdict::Degrade(other) => {
                tracing::warn!(reason = ?other, "质控闸门返回预期外的降级原因，未计入统计");
            }
        }
    }
    tally
}

/// 带 θ_nb 重试的密度聚类执行结果。
///
/// 字段约定:
/// - `result`: 最终一次聚类的完整结果（含簇与孤立点比例）。
/// - `retries_used`: 实际发生的 θ_nb 下调重试次数（0..=2）。
/// - `effective_theta_nb`: 最终实际使用的 θ_nb。
pub(crate) struct ClusterRunOutcome {
    pub(crate) result: DensityClusterResult,
    pub(crate) retries_used: usize,
    pub(crate) effective_theta_nb: f64,
}

/// 执行真实管线口径的密度聚类（含 θ_nb 重试；纯计算）。
///
/// 说明:
/// - 复刻 `BehaviorClusterer::cluster_samples` 的重试规则（本命令不复用其入口，
///   以保证统计过程可观测）：先按 θ_nb 聚类；孤立点比例 > `max_outlier_ratio`
///   时每次 θ_nb − 0.1（下限 0.05）重试，至多 2 次。
/// - `pipeline` 估算与 θ_join 模拟的前段/全量建簇共用本口径，保证对照一致。
///
/// 参数:
/// - `samples`: 已向量化的行为样本。
/// - `behavior`: 本次计算的行为配置（θ_nb/min_cluster_size/β 权重/孤立点比例上限）。
pub(crate) fn cluster_with_retry(
    samples: &[BehaviorSample],
    behavior: &BehaviorConfig,
) -> ClusterRunOutcome {
    let mut theta_nb = behavior.theta_nb;
    let mut result = density_cluster(
        samples,
        theta_nb,
        behavior.min_cluster_size,
        behavior.beta1,
        behavior.beta2,
    );
    let mut retries_used = 0usize;
    while result.outlier_ratio > behavior.max_outlier_ratio && retries_used < 2 {
        theta_nb = (theta_nb - 0.1).max(0.05);
        result = density_cluster(
            samples,
            theta_nb,
            behavior.min_cluster_size,
            behavior.beta1,
            behavior.beta2,
        );
        retries_used += 1;
    }
    ClusterRunOutcome {
        result,
        retries_used,
        effective_theta_nb: theta_nb,
    }
}

/// 校验本次计算覆盖的聚类参数。
///
/// 参数:
/// - `theta_nb`: 邻域相似度阈值，必须在 [0.0, 1.0]。
/// - `min_cluster_size`: 核心样本最小邻居数，必须 ≥ 1。
/// - `beta1` / `beta2`: 双通道权重，必须为非负有限值且 β1 + β2 ≤ 1.0。
///
/// 返回:
/// - `Ok(())`: 参数合法。
/// - `Err`: 首个非法参数（错误信息含参数名与当前取值）。
pub(crate) fn validate_cluster_params(
    theta_nb: f64,
    min_cluster_size: usize,
    beta1: f64,
    beta2: f64,
) -> anyhow::Result<()> {
    if !theta_nb.is_finite() || !(0.0..=1.0).contains(&theta_nb) {
        anyhow::bail!("--theta-nb 必须在 [0.0, 1.0] 内，当前值: {theta_nb}");
    }
    if min_cluster_size < 1 {
        anyhow::bail!("--min-cluster-size 必须 ≥ 1，当前值: {min_cluster_size}");
    }
    if !beta1.is_finite() || beta1 < 0.0 {
        anyhow::bail!("--beta1 必须为非负有限值，当前值: {beta1}");
    }
    if !beta2.is_finite() || beta2 < 0.0 {
        anyhow::bail!("--beta2 必须为非负有限值，当前值: {beta2}");
    }
    if beta1 + beta2 > 1.0 {
        anyhow::bail!(
            "--beta1 + --beta2 必须 ≤ 1.0（关键词通道权重 = 1 − β1 − β2 不可为负），当前和为: {}",
            beta1 + beta2
        );
    }
    Ok(())
}
