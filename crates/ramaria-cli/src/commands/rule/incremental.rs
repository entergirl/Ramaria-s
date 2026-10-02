//! crates/ramaria-cli/src/commands/rule/incremental.rs - 行为规则 θ_join 时序增量模拟
//!
//! 设计特点:
//! - 留一模拟：前段建簇 → 后段逐条喂入增量管线
//! - 模板规则构建、多数投票与一致性判定
//! - 相似度分布与簇形态统计（percentile / 孤立点）

use super::clusters::{GateTally, MAX_SIMILARITY_PAIRS, cluster_with_retry, tally_quality_gate};
use anyhow::Context;
use ramaria_core::behavior::BehaviorParams;
use ramaria_core::behavior::BehaviorRule;
use ramaria_core::behavior::RuleSource;
use ramaria_core::config::BehaviorConfig;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::MemoryEvent;
use ramaria_core::types::now_ms;
use ramaria_memory::behavior::BehaviorSample;
use ramaria_memory::behavior::PendingPool;
use ramaria_memory::behavior::RefinedCluster;
use ramaria_memory::behavior::RuleGenConfig;
use ramaria_memory::behavior::compute_incremental_update;
use ramaria_memory::behavior::fused_similarity;
use ramaria_memory::behavior::refine_cluster;
use std::collections::HashMap;
use std::collections::HashSet;

/// 真实管线口径（含 θ_nb 重试）的规则产出量估算。
///
/// 字段约定:
/// - `retries_used`: 实际发生的 θ_nb 下调重试次数（0..=2）。
/// - `effective_theta_nb`: 最终实际使用的 θ_nb。
/// - `cluster_count` / `sizes`（降序）/ `outlier_ratio`: 最终聚类结构。
/// - `gate`: 逐簇过质控闸门的归类计数。
pub(crate) struct PipelineEstimate {
    pub(crate) retries_used: usize,
    pub(crate) effective_theta_nb: f64,
    pub(crate) cluster_count: usize,
    pub(crate) sizes: Vec<usize>,
    pub(crate) outlier_ratio: f64,
    pub(crate) gate: GateTally,
}

/// 按真实行为管线口径估算规则产出量（纯计算，不写库、不调 LLM）。
///
/// 说明:
/// - 聚类口径（含 θ_nb 重试）由 `cluster_with_retry` 统一提供，与 θ_join 模拟一致。
/// - 对最终结果的每个簇执行 `refine_cluster`，再逐簇过 `quality_gate` 归类计数。
///
/// 参数:
/// - `samples`: 已向量化的行为样本。
/// - `behavior`: 应用本次覆盖后的行为配置（θ_nb/min_cluster_size/β 权重/孤立点比例上限）。
/// - `gate_config`: 质控闸门阈值（由 `RuleGenConfig::from` 从行为配置派生）。
pub(crate) fn estimate_pipeline(
    samples: &[BehaviorSample],
    behavior: &BehaviorConfig,
    gate_config: &RuleGenConfig,
) -> PipelineEstimate {
    let run = cluster_with_retry(samples, behavior);

    let mut sizes: Vec<usize> = run
        .result
        .clusters
        .iter()
        .map(|c| c.member_indices.len())
        .collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));

    let refined: Vec<RefinedCluster> = run
        .result
        .clusters
        .iter()
        .map(|c| refine_cluster(samples, &c.member_indices, behavior.beta1, behavior.beta2))
        .collect();

    PipelineEstimate {
        retries_used: run.retries_used,
        effective_theta_nb: run.effective_theta_nb,
        cluster_count: run.result.cluster_count,
        sizes,
        outlier_ratio: run.result.outlier_ratio,
        gate: tally_quality_gate(&refined, gate_config),
    }
}

/// 校验 θ_join 时序增量模拟参数。
///
/// 参数:
/// - `theta_join`: θ_join 档位列表（空 = 不启用模拟）；每个值必须在 [0.0, 1.0]。
/// - `split_ratio`: 前段事件占比，必须在 [0.1, 0.9]。
///
/// 返回:
/// - `Ok(())`: 参数合法（含空档位列表）。
/// - `Err`: 首个非法取值（错误信息含参数名与当前值）。
pub(crate) fn validate_incremental_params(
    theta_join: &[f64],
    split_ratio: f64,
) -> anyhow::Result<()> {
    for &value in theta_join {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            anyhow::bail!("--theta-join 必须在 [0.0, 1.0] 内，当前值: {value}");
        }
    }
    if !split_ratio.is_finite() || !(0.1..=0.9).contains(&split_ratio) {
        anyhow::bail!("--split-ratio 必须在 [0.1, 0.9] 内，当前值: {split_ratio}");
    }
    Ok(())
}

// =========================================================
// clusters 辅助：θ_join 时序增量模拟
// =========================================================

/// θ_join 时序增量模拟结果。
///
/// 职责:
/// - 汇总"留一"模拟：前段事件建簇为模板规则，后段事件按时间逐条喂入增量管线，
///   统计各 θ_join 档位的归簇/待定/新簇与全量参照一致性。
///
/// 状态:
/// - `Skipped`: 事件不足 2 条，无法切分前段/后段。
/// - `Done`: 模拟完成（外层 split/first/second + 逐档统计）。
pub(crate) enum IncrementalSimulation {
    /// 已跳过（附跳过原因）。
    Skipped { reason: String },
    /// 模拟完成。
    Done(IncrementalSimulationReport),
}

/// θ_join 时序增量模拟报告。
pub(crate) struct IncrementalSimulationReport {
    /// 前段事件占比（回显本次取值）。
    pub(crate) split_ratio: f64,
    /// 前段事件数（建簇）。
    pub(crate) first_count: usize,
    /// 后段事件数（逐条喂入增量管线）。
    pub(crate) second_count: usize,
    /// 各 θ_join 档位的统计（与请求档位一一对应、顺序一致）。
    pub(crate) by_theta_join: Vec<ThetaJoinSummary>,
}

/// 单个 θ_join 档位的模拟统计。
///
/// 字段约定:
/// - `assigned` / `assigned_rate`: 归入前段模板规则的后段事件数与其占后段总数的比例。
/// - `pending_remaining`: 模拟结束时待定池剩余事件数（含未成簇与已成簇未消费事件）。
/// - `new_clusters` / `new_cluster_sizes`: 待定池新成簇事件组数与规模（同一事件组按首见去重）。
/// - `low_confidence`: 新标记低置信事件数（模拟即时完成，通常为 0）。
/// - `decayed_rules`: 被证据衰减标记为应降级/失效的规则数（按规则 id 去重）。
/// - `drift_triggered`: 是否出现过漂移触发（逐条喂入批次 < 3，恒 false，保留字段对齐批次语义）。
/// - `agreement_checked` / `agreement_rate`: 与全量参照聚类的一致性对照
///   （分母仅含规则侧有全量映射的归簇事件）。
pub(crate) struct ThetaJoinSummary {
    pub(crate) theta_join: f64,
    pub(crate) assigned: usize,
    pub(crate) assigned_rate: f64,
    pub(crate) pending_remaining: usize,
    pub(crate) new_clusters: usize,
    pub(crate) new_cluster_sizes: Vec<usize>,
    pub(crate) low_confidence: usize,
    pub(crate) decayed_rules: usize,
    pub(crate) drift_triggered: bool,
    pub(crate) agreement_checked: usize,
    pub(crate) agreement_rate: Option<f64>,
}

/// 按比例计算前段事件数（纯函数）。
///
/// 参数:
/// - `total`: 事件总数。
/// - `ratio`: 前段占比（调用方已校验 ∈ [0.1, 0.9]）。
///
/// 返回:
/// - `Some(first_count)`: 前段条数，恒 ∈ [1, total−1]（两段各至少 1 条）。
/// - `None`: 事件总数 < 2，无法切分。
pub(crate) fn split_index_by_ratio(total: usize, ratio: f64) -> Option<usize> {
    if total < 2 {
        return None;
    }
    let scaled = (total as f64 * ratio).round() as usize;
    Some(scaled.clamp(1, total - 1))
}

/// 多数投票取全量簇标签（纯函数）。
///
/// 说明:
/// - 最高票并列时取较小标签，保证输出确定性。
///
/// 返回:
/// - `Some(label)`: 得票最多的标签。
/// - `None`: 空输入（无可投票标签）。
pub(crate) fn majority_vote_label(labels: &[usize]) -> Option<usize> {
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for &label in labels {
        *counts.entry(label).or_insert(0) += 1;
    }
    let mut ranked: Vec<(usize, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.first().map(|&(label, _)| label)
}

/// 判定一次增量归簇与全量参照标签是否一致（纯函数）。
///
/// 参数:
/// - `rule_full_cluster`: 归入规则对应的全量簇标签（前段簇成员在全量聚类中的多数投票）；
///   `None` = 规则侧无全量映射（不可判定）。
/// - `event_full_cluster`: 被归簇事件的全量簇标签；`None` = 事件在全量侧为孤立点。
///
/// 返回:
/// - `Some(true)`: 标签一致。
/// - `Some(false)`: 不一致（标签不同；或事件在全量侧孤立 = 误归）。
/// - `None`: 规则侧不可判定，不计入一致性分母。
pub(crate) fn judge_agreement(
    rule_full_cluster: Option<usize>,
    event_full_cluster: Option<usize>,
) -> Option<bool> {
    rule_full_cluster.map(|rule_label| event_full_cluster == Some(rule_label))
}

/// 由前段簇构造"无文本 Auto 规则"模板（纯函数）。
///
/// 说明:
/// - 每条规则对应前段一个簇，`id` 从 1 递增（规则顺序与簇顺序一致），供增量归簇使用。
/// - 规则无 reaction、无证据、置信度/稳定性为 0，仅携带簇情境中心；
///   不写库、不进入任何注入路径。
///
/// 参数:
/// - `persona_uid`: 规则所属 persona。
/// - `refined`: 前段提炼后的簇列表。
/// - `now`: 创建/更新时间（Unix 毫秒）。
pub(crate) fn build_template_rules(
    persona_uid: &str,
    refined: &[RefinedCluster],
    now: i64,
) -> Vec<BehaviorRule> {
    refined
        .iter()
        .enumerate()
        .map(|(idx, cluster)| BehaviorRule {
            id: idx as i64 + 1,
            persona_uid: persona_uid.to_string(),
            situation: cluster.situation.clone(),
            reaction: None,
            params: BehaviorParams::default(),
            avoid: Vec::new(),
            evidence: Vec::new(),
            confidence: 0.0,
            stability: 0.0,
            source: RuleSource::Auto,
            enabled: true,
            created_at: now,
            updated_at: now,
        })
        .collect()
}

/// 运行 θ_join 时序增量模拟（只读"留一"评估，不写库、不调 LLM）。
///
/// 流程:
/// 1. 事件按 `start` 升序稳定排序，按 `split_ratio` 切分前段（建簇）/后段（增量）。
/// 2. 前段用管线口径（含 θ_nb 重试）聚类并提炼，构造无文本 Auto 规则模板。
/// 3. 全量事件跑同一口径聚类，建立"事件 id → 全量簇 idx"参照标签，
///    并以模板规则成员的全量标签多数投票建立"规则 idx → 全量簇 idx"映射。
/// 4. 每个 θ_join 档位独立模拟：模板规则重新克隆、待定池独立，
///    后段事件逐条喂入 `compute_incremental_update`，累计归簇/新簇/低置信/衰减/漂移与一致性。
///
/// 说明:
/// - 本模拟的规则无 evidence，`compute_incremental_update` 的证据衰减会因总权重 0.0
///   低于阈值把全部模板规则标记为 decayed（该行为来自库函数，模拟不修改）；
///   `decayed_rules` 按规则 id 去重统计，不代表真实规则的衰减结论。
/// - 待定池 `advance` 不消费已成簇事件，后续轮次会重复返回同一批事件组；
///   `new_clusters` / `new_cluster_sizes` 按事件组首见去重，`pending_remaining` 保留池内全部事件。
/// - 逐条喂入时单批新事件 < 3，漂移检测分支不执行，`drift_triggered` 恒 false。
/// - 一致性对照仅在"规则侧有全量映射"时计入分母；事件在全量侧孤立（None）判为误归。
///
/// 参数:
/// - `persona_uid`: 目标 persona。
/// - `events`: 全量事件（与 `samples` 同索引、同顺序）。
/// - `samples`: 已向量化的全量样本。
/// - `behavior`: 应用本次覆盖后的行为配置（θ_join 按档位覆写）。
/// - `embedder`: 嵌入模型 provider（None → 纯关键词降级）。
/// - `theta_join_values`: θ_join 档位列表（每档独立模拟）。
/// - `split_ratio`: 前段事件占比。
///
/// 返回:
/// - `Skipped`: 事件不足 2 条（无法切分两段）。
/// - `Done`: 逐档统计报告。
pub(crate) async fn run_incremental_simulation(
    persona_uid: &str,
    events: &[MemoryEvent],
    samples: &[BehaviorSample],
    behavior: &BehaviorConfig,
    embedder: Option<&dyn EmbeddingProvider>,
    theta_join_values: &[f64],
    split_ratio: f64,
) -> anyhow::Result<IncrementalSimulation> {
    let Some(first_count) = split_index_by_ratio(events.len(), split_ratio) else {
        return Ok(IncrementalSimulation::Skipped {
            reason: format!(
                "事件数 {} 不足 2 条，前段/后段无法各保留至少 1 条，跳过模拟",
                events.len()
            ),
        });
    };

    // 事件按 start 升序稳定排序；samples 与 events 同索引，一并按同序取用
    let mut order: Vec<usize> = (0..events.len()).collect();
    order.sort_by_key(|&idx| events[idx].start);
    let first_samples: Vec<BehaviorSample> = order[..first_count]
        .iter()
        .map(|&idx| samples[idx].clone())
        .collect();
    let second_events: Vec<&MemoryEvent> = order[first_count..]
        .iter()
        .map(|&idx| &events[idx])
        .collect();
    let second_count = second_events.len();

    // 前段建簇（与 pipeline 同口径）并提炼为模板规则
    let first_run = cluster_with_retry(&first_samples, behavior);
    let refined: Vec<RefinedCluster> = first_run
        .result
        .clusters
        .iter()
        .map(|cluster| {
            refine_cluster(
                &first_samples,
                &cluster.member_indices,
                behavior.beta1,
                behavior.beta2,
            )
        })
        .collect();

    // 全量参照标签：事件 id → 全量簇 idx（不在任何簇 = 孤立）
    let full_run = cluster_with_retry(samples, behavior);
    let mut full_label_by_event: HashMap<i64, usize> = HashMap::new();
    for (cluster_idx, cluster) in full_run.result.clusters.iter().enumerate() {
        for &sample_idx in &cluster.member_indices {
            if let Some(sample) = samples.get(sample_idx) {
                full_label_by_event.insert(sample.event_id, cluster_idx);
            }
        }
    }

    // 规则 idx → 全量簇 idx：前段簇成员的全量标签多数投票
    let mut rule_full_label: HashMap<usize, usize> = HashMap::new();
    for (rule_idx, cluster) in refined.iter().enumerate() {
        let labels: Vec<usize> = cluster
            .member_event_ids
            .iter()
            .filter_map(|event_id| full_label_by_event.get(event_id).copied())
            .collect();
        if let Some(label) = majority_vote_label(&labels) {
            rule_full_label.insert(rule_idx, label);
        }
    }

    let now = now_ms();
    let template_rules = build_template_rules(persona_uid, &refined, now);
    let rule_id_to_idx: HashMap<i64, usize> = template_rules
        .iter()
        .enumerate()
        .map(|(idx, rule)| (rule.id, idx))
        .collect();

    let mut by_theta_join = Vec::with_capacity(theta_join_values.len());
    for &theta_join in theta_join_values {
        // 每档独立：模板规则重新克隆（证据衰减是原地修改），待定池独立
        let mut cfg = behavior.clone();
        cfg.theta_join = theta_join;
        let mut rules = template_rules.clone();
        let mut pending = PendingPool::new(&cfg);

        let mut assigned = 0usize;
        let mut agreement_checked = 0usize;
        let mut agreement = 0usize;
        let mut seen_groups: HashSet<Vec<i64>> = HashSet::new();
        let mut new_cluster_sizes: Vec<usize> = Vec::new();
        let mut low_confidence = 0usize;
        let mut decayed_seen: HashSet<i64> = HashSet::new();
        let mut drift_triggered = false;

        for event in &second_events {
            let outcome = compute_incremental_update(
                std::slice::from_ref(*event),
                &mut rules,
                &mut pending,
                &cfg,
                embedder,
                now,
            )
            .await
            .context("计算增量更新失败")?;

            for &(event_id, rule_id) in &outcome.assigned {
                assigned += 1;
                let Some(&rule_idx) = rule_id_to_idx.get(&rule_id) else {
                    continue;
                };
                let rule_label = rule_full_label.get(&rule_idx).copied();
                let event_label = full_label_by_event.get(&event_id).copied();
                if let Some(agreed) = judge_agreement(rule_label, event_label) {
                    agreement_checked += 1;
                    if agreed {
                        agreement += 1;
                    }
                }
            }

            for group in &outcome.new_cluster_event_ids {
                let mut key = group.clone();
                key.sort_unstable();
                if seen_groups.insert(key) {
                    new_cluster_sizes.push(group.len());
                }
            }
            low_confidence += outcome.low_confidence_event_ids.len();
            decayed_seen.extend(outcome.decayed_rule_ids.iter().copied());
            drift_triggered |= outcome.drift_triggered;
        }

        let assigned_rate = assigned as f64 / second_count as f64;
        let agreement_rate = if agreement_checked > 0 {
            Some(agreement as f64 / agreement_checked as f64)
        } else {
            None
        };
        by_theta_join.push(ThetaJoinSummary {
            theta_join,
            assigned,
            assigned_rate,
            pending_remaining: pending.events.len(),
            new_clusters: new_cluster_sizes.len(),
            new_cluster_sizes,
            low_confidence,
            decayed_rules: decayed_seen.len(),
            drift_triggered,
            agreement_checked,
            agreement_rate,
        });
    }

    Ok(IncrementalSimulation::Done(IncrementalSimulationReport {
        split_ratio,
        first_count,
        second_count,
        by_theta_join,
    }))
}

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

// =========================================================
// 单元测试（纯函数，不碰真实 DB / embedding）
// =========================================================
