//! crates/ramaria-cli/src/commands/rule/clusters.rs - 行为规则 聚类统计主流程
//!
//! 设计特点:
//! - 只读统计：读事件 → 双通道向量化 → 密度聚类 → 输出簇结构与相似度分布
//! - min−1 反事实对照与含 θ_nb 重试的管线口径 / 质控闸门估算
//! - 聚类参数校验与簇运行结果容器

use super::DEFAULT_RULE_PERSONA;
use super::incremental::{
    IncrementalSimulation, SimilarityDistribution, compute_similarity_stats, estimate_pipeline,
    has_text, run_incremental_simulation, summarize_cluster_shapes, validate_incremental_params,
};
use crate::json;
use anyhow::Context;
use ramaria_core::config::BehaviorConfig;
use ramaria_memory::behavior::BehaviorSample;
use ramaria_memory::behavior::DensityClusterResult;
use ramaria_memory::behavior::QualityVerdict;
use ramaria_memory::behavior::RefinedCluster;
use ramaria_memory::behavior::RuleDegradeReason;
use ramaria_memory::behavior::RuleGenConfig;
use ramaria_memory::behavior::density_cluster;
use ramaria_memory::behavior::quality_gate;
use ramaria_memory::behavior::sample_from_event;
use ramaria_memory::behavior::vectorize;
use ramaria_service::Engine;
use std::sync::Arc;

/// 相似度全对计算的对数上限（超过则跳过分布统计，避免长耗时）。
pub(crate) const MAX_SIMILARITY_PAIRS: usize = 500_000;

/// 只读统计 persona 的行为聚类结构。
///
/// 用法:
/// - `ramaria rule clusters [--persona <uid>] [--theta-nb <f64>] [--min-cluster-size <n>] [--beta1 <f64>] [--beta2 <f64>] [--theta-join <f64>...] [--split-ratio <f64>]`
///
/// 参数:
/// - `engine`: 服务层引擎引用。
/// - `persona`: 目标 persona（None = 默认 persona）。
/// - `theta_nb` / `min_cluster_size` / `beta1` / `beta2`: 本次计算的参数覆盖（None = 取行为配置值）。
/// - `theta_join`: θ_join 档位列表（空 = 不启用时序增量模拟）。
/// - `split_ratio`: 增量模拟的前段事件占比。
/// - `json`: JSON 信封输出。
///
/// 说明:
/// - 只读：不写库、不调用 LLM；embedding 不可用时降级纯关键词通道并在输出中标注。
/// - 参数覆盖仅作用于本次计算，不回写配置。
/// - 聚类走原始口径（不经孤立点比例超限的 θ_nb 重试），`retry_would_fire` 标记重试是否会触发。
/// - 追加 `counterfactual`：min_cluster_size − 1 的反事实对照，量化核心口径对成簇的影响。
/// - 追加 `pipeline`：复刻真实管线的 θ_nb 重试口径并逐簇过质控闸门，估算规则产出量。
/// - 追加 `incremental`（可选）：θ_join 时序增量模拟（前段建簇为模板规则，后段逐条喂入增量管线，
///   输出各档位归簇率/待定/新簇/一致性对照）；未传 `--theta-join` 时为 null。
// 参数为只读统计命令的完整输入集合（含输出模式与模拟档位透传），逐一显式传递保持可读性；
// 与 probe run 的 `run_experiment` 采用同一 allow 约定。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_clusters(
    engine: &Arc<Engine>,
    persona: Option<String>,
    theta_nb: Option<f64>,
    min_cluster_size: Option<usize>,
    beta1: Option<f64>,
    beta2: Option<f64>,
    theta_join: Vec<f64>,
    split_ratio: f64,
    json: bool,
) -> anyhow::Result<()> {
    let persona_uid = persona.unwrap_or_else(|| DEFAULT_RULE_PERSONA.to_string());

    // 本次计算参数：行为配置克隆 + CLI 覆盖（仅本次计算，不回写配置）；
    // 克隆体同时用于管线口径复刻与质控闸门阈值派生（RuleGenConfig::from）
    let mut behavior = engine.config().behavior.clone();
    if let Some(v) = theta_nb {
        behavior.theta_nb = v;
    }
    if let Some(v) = min_cluster_size {
        behavior.min_cluster_size = v;
    }
    if let Some(v) = beta1 {
        behavior.beta1 = v;
    }
    if let Some(v) = beta2 {
        behavior.beta2 = v;
    }
    let theta_nb = behavior.theta_nb;
    let min_cluster_size = behavior.min_cluster_size;
    let beta1 = behavior.beta1;
    let beta2 = behavior.beta2;
    validate_cluster_params(theta_nb, min_cluster_size, beta1, beta2)?;
    validate_incremental_params(&theta_join, split_ratio)?;
    let beta3 = (1.0 - beta1 - beta2).max(0.0);

    // 事件（只读查询）
    let events = engine
        .storage()
        .list_events_by_persona(&persona_uid, 0, i64::MAX)
        .await
        .context("查询行为事件失败")?;
    if events.is_empty() && !json {
        crate::ui::info(&format!(
            "人格 {persona_uid} 暂无行为事件，跳过行为聚类统计"
        ));
        return Ok(());
    }

    // 样本与双通道向量化（embedding 不可用 → 纯关键词降级，不阻塞）
    let mut samples: Vec<BehaviorSample> = events.iter().map(sample_from_event).collect();
    let embedder = engine.embedding();
    let embedding_available = embedder.is_some();
    vectorize(&mut samples, &events, embedder.as_deref())
        .await
        .context("行为样本向量化失败")?;

    // 原始口径密度聚类（不走 BehaviorClusterer 的 θ_nb 重试路径）
    let cluster_result = density_cluster(&samples, theta_nb, min_cluster_size, beta1, beta2);
    let mut sizes: Vec<usize> = cluster_result
        .clusters
        .iter()
        .map(|c| c.member_indices.len())
        .collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    let shape = summarize_cluster_shapes(&sizes, samples.len());
    let retry_would_fire = shape.outlier_ratio > behavior.max_outlier_ratio;

    // 相似度分布（全对融合相似度；对数超限 → 跳过并记 warn）
    let similarity = compute_similarity_stats(&samples, beta1, beta2);

    // 事件级覆盖统计（向量覆盖取自向量化后的样本）
    let paraphrase_non_empty = events
        .iter()
        .filter(|e| has_text(e.paraphrase.as_deref()))
        .count();
    let attitude_non_empty = events
        .iter()
        .filter(|e| has_text(e.attitude.as_deref()))
        .count();
    let keywords_non_empty = events
        .iter()
        .filter(|e| has_text(e.keywords.as_deref()))
        .count();
    let reaction_vector_non_empty = samples
        .iter()
        .filter(|s| s.reaction_vector.is_some())
        .count();
    let situation_vector_non_empty = samples
        .iter()
        .filter(|s| s.situation_vector.is_some())
        .count();

    // ---- min−1 反事实对照（评估 min_cluster_size 口径影响；θ/β 与本次计算一致） ----
    let counterfactual: Option<CounterfactualSummary> = if min_cluster_size <= 1 {
        // 不存在更小有效口径（下限 1）：跳过反事实
        None
    } else {
        let cf_min = min_cluster_size - 1;
        let cf_result = density_cluster(&samples, theta_nb, cf_min, beta1, beta2);
        let cf_sizes: Vec<usize> = cf_result
            .clusters
            .iter()
            .map(|c| c.member_indices.len())
            .collect();
        Some(summarize_counterfactual(
            cf_min,
            &cf_sizes,
            samples.len(),
            shape.outlier_count,
        ))
    };
    let counterfactual_skip_reason: Option<String> = if counterfactual.is_none() {
        Some(format!(
            "min_cluster_size ≤ 1（当前 {min_cluster_size}），不存在更小有效口径（下限 1），跳过反事实对照"
        ))
    } else {
        None
    };

    // ---- 真实管线口径（复刻 θ_nb 重试）+ 质控闸门产出量估算 ----
    let gate_config = RuleGenConfig::from(&behavior);
    let pipeline = estimate_pipeline(&samples, &behavior, &gate_config);

    // ---- θ_join 时序增量模拟（仅 --theta-join 时；只读、不调 LLM） ----
    let incremental: Option<IncrementalSimulation> = if theta_join.is_empty() {
        None
    } else {
        Some(
            run_incremental_simulation(
                &persona_uid,
                &events,
                &samples,
                &behavior,
                embedder.as_deref(),
                &theta_join,
                split_ratio,
            )
            .await
            .context("θ_join 时序增量模拟失败")?,
        )
    };
    let incremental_json = match &incremental {
        None => serde_json::Value::Null,
        Some(IncrementalSimulation::Skipped { reason }) => serde_json::json!({
            "skipped": true,
            "skip_reason": reason,
        }),
        Some(IncrementalSimulation::Done(report)) => serde_json::json!({
            "split_ratio": report.split_ratio,
            "first_count": report.first_count,
            "second_count": report.second_count,
            "by_theta_join": report
                .by_theta_join
                .iter()
                .map(|row| serde_json::json!({
                    "theta_join": row.theta_join,
                    "assigned": row.assigned,
                    "assigned_rate": row.assigned_rate,
                    "pending_remaining": row.pending_remaining,
                    "new_clusters": row.new_clusters,
                    "new_cluster_sizes": row.new_cluster_sizes,
                    "low_confidence": row.low_confidence,
                    "decayed_rules": row.decayed_rules,
                    "drift_triggered": row.drift_triggered,
                    "agreement_checked": row.agreement_checked,
                    "agreement_rate": row.agreement_rate,
                }))
                .collect::<Vec<_>>(),
        }),
    };

    if json {
        let counterfactual_json = match &counterfactual {
            Some(cf) => serde_json::json!({
                "min_cluster_size": cf.min_cluster_size,
                "count": cf.count,
                "sizes": cf.sizes,
                "outlier_count": cf.outlier_count,
                "outlier_ratio": cf.outlier_ratio,
                "recovered_samples": cf.recovered_samples,
                "three_member_clusters": cf.three_member_clusters,
            }),
            None => serde_json::Value::Null,
        };
        let similarity_json = match &similarity {
            SimilarityDistribution::Computed(stats) => serde_json::json!({
                "pairs": stats.pairs,
                "min": stats.min,
                "p25": stats.p25,
                "p50": stats.p50,
                "p75": stats.p75,
                "p90": stats.p90,
                "max": stats.max,
                "mean": stats.mean,
                "skipped": false,
                "skip_reason": null,
            }),
            SimilarityDistribution::Skipped(reason) => serde_json::json!({
                "pairs": null,
                "min": null,
                "p25": null,
                "p50": null,
                "p75": null,
                "p90": null,
                "max": null,
                "mean": null,
                "skipped": true,
                "skip_reason": reason,
            }),
        };
        let data = serde_json::json!({
            "persona_uid": persona_uid,
            "embedding_available": embedding_available,
            "embedding_note": if embedding_available {
                serde_json::Value::Null
            } else {
                serde_json::json!("embedding 不可用，纯关键词降级")
            },
            "params": {
                "theta_nb": theta_nb,
                "min_cluster_size": min_cluster_size,
                "beta1": beta1,
                "beta2": beta2,
                "beta3": beta3,
                "max_outlier_ratio": behavior.max_outlier_ratio,
                "retry_would_fire": retry_would_fire,
            },
            "events": {
                "total": events.len(),
                "paraphrase_non_empty": paraphrase_non_empty,
                "attitude_non_empty": attitude_non_empty,
                "keywords_non_empty": keywords_non_empty,
                "reaction_vector_non_empty": reaction_vector_non_empty,
                "situation_vector_non_empty": situation_vector_non_empty,
            },
            "similarity": similarity_json,
            "clusters": {
                "count": cluster_result.cluster_count,
                "sizes": sizes,
                "max_share": shape.max_share,
                "outlier_count": shape.outlier_count,
                "outlier_ratio": shape.outlier_ratio,
                "coverage": 1.0 - shape.outlier_ratio,
            },
            "counterfactual": counterfactual_json,
            "counterfactual_skip_reason": counterfactual_skip_reason,
            "pipeline": {
                "retries_used": pipeline.retries_used,
                "effective_theta_nb": pipeline.effective_theta_nb,
                "count": pipeline.cluster_count,
                "sizes": pipeline.sizes,
                "outlier_ratio": pipeline.outlier_ratio,
                "gate": {
                    "pass": pipeline.gate.pass,
                    "low_evidence": pipeline.gate.low_evidence,
                    "low_neff": pipeline.gate.low_neff,
                    "high_valence_variance": pipeline.gate.high_valence_variance,
                },
            },
            "incremental": incremental_json,
        });
        return json::emit_ok(&data);
    }

    // 人读输出：参数 → 事件覆盖 → 相似度分布 → 簇结构
    crate::ui::separator();
    crate::ui::labeled("Persona", &persona_uid);
    crate::ui::labeled(
        "向量通道",
        if embedding_available {
            "双通道（embedding 可用）"
        } else {
            "纯关键词降级（embedding 不可用）"
        },
    );
    crate::ui::labeled("事件数", &events.len().to_string());
    crate::ui::labeled("θ_nb", &format!("{theta_nb:.3}"));
    crate::ui::labeled("min_cluster_size", &min_cluster_size.to_string());
    crate::ui::labeled(
        "β1 / β2 / β3",
        &format!("{beta1:.3} / {beta2:.3} / {beta3:.3}"),
    );
    crate::ui::labeled(
        "max_outlier_ratio",
        &format!("{:.3}", behavior.max_outlier_ratio),
    );
    crate::ui::labeled("重试会触发", if retry_would_fire { "是" } else { "否" });
    crate::ui::labeled(
        "字段覆盖",
        &format!(
            "paraphrase {paraphrase_non_empty} · attitude {attitude_non_empty} · keywords {keywords_non_empty}"
        ),
    );
    crate::ui::labeled(
        "向量覆盖",
        &format!("反应通道 {reaction_vector_non_empty} · 情境通道 {situation_vector_non_empty}"),
    );
    match &similarity {
        SimilarityDistribution::Computed(stats) => crate::ui::labeled(
            "相似度分布",
            &format!(
                "pairs {} · min {:.3} · P25 {:.3} · P50 {:.3} · P75 {:.3} · P90 {:.3} · max {:.3} · mean {:.3}",
                stats.pairs,
                stats.min,
                stats.p25,
                stats.p50,
                stats.p75,
                stats.p90,
                stats.max,
                stats.mean
            ),
        ),
        SimilarityDistribution::Skipped(reason) => {
            crate::ui::labeled("相似度分布", &format!("（跳过）{reason}"))
        }
    }
    crate::ui::labeled("簇数", &cluster_result.cluster_count.to_string());
    crate::ui::labeled("簇规模（降序）", &format!("{sizes:?}"));
    crate::ui::labeled("最大簇占比", &format!("{:.3}", shape.max_share));
    crate::ui::labeled(
        "孤立点",
        &format!(
            "{}（{:.1}%）",
            shape.outlier_count,
            shape.outlier_ratio * 100.0
        ),
    );
    crate::ui::labeled("覆盖率", &format!("{:.3}", 1.0 - shape.outlier_ratio));

    // 反事实对照（两行）
    match &counterfactual {
        Some(cf) => {
            crate::ui::labeled(
                "反事实对照",
                &format!(
                    "min_cluster_size {} · 簇数 {} · 孤立点 {}（{:.1}%）",
                    cf.min_cluster_size,
                    cf.count,
                    cf.outlier_count,
                    cf.outlier_ratio * 100.0
                ),
            );
            crate::ui::labeled(
                "反事实细节",
                &format!(
                    "3 成员簇数 {} · 回收样本数 {}",
                    cf.three_member_clusters, cf.recovered_samples
                ),
            );
        }
        None => {
            let reason = counterfactual_skip_reason
                .as_deref()
                .unwrap_or("不存在更小有效口径");
            crate::ui::labeled("反事实对照", &format!("（跳过）{reason}"));
        }
    }

    // 真实管线口径小节（含 θ_nb 重试）与质控闸门
    crate::ui::separator();
    crate::ui::labeled("管线口径", "含 θ_nb 重试（真实管线复刻）");
    crate::ui::labeled("重试次数", &pipeline.retries_used.to_string());
    crate::ui::labeled("实际 θ_nb", &format!("{:.3}", pipeline.effective_theta_nb));
    crate::ui::labeled("管线簇数", &pipeline.cluster_count.to_string());
    crate::ui::labeled("管线簇规模（降序）", &format!("{:?}", pipeline.sizes));
    crate::ui::labeled("管线孤立点比例", &format!("{:.3}", pipeline.outlier_ratio));
    crate::ui::labeled(
        "质控闸门",
        &format!(
            "通过 {} · 证据不足 {} · n_eff 不足 {} · valence 方差超限 {}",
            pipeline.gate.pass,
            pipeline.gate.low_evidence,
            pipeline.gate.low_neff,
            pipeline.gate.high_valence_variance
        ),
    );
    crate::ui::labeled(
        "闸门阈值",
        &format!(
            "证据 ≥ {} · n_eff ≥ {} · valence σ ≤ {:.3}",
            gate_config.min_evidence, gate_config.min_n_eff, gate_config.valence_std_limit
        ),
    );

    // θ_join 时序增量模拟小节（仅 --theta-join 时输出）
    match &incremental {
        None => {}
        Some(IncrementalSimulation::Skipped { reason }) => {
            crate::ui::labeled("θ_join 增量模拟", &format!("（跳过）{reason}"));
        }
        Some(IncrementalSimulation::Done(report)) => {
            crate::ui::separator();
            crate::ui::labeled(
                "θ_join 增量模拟",
                &format!(
                    "split_ratio {:.2} · 前段 {} 条 / 后段 {} 条（留一：前段建簇 → 后段逐条喂入）",
                    report.split_ratio, report.first_count, report.second_count
                ),
            );
            println!(
                "  {:<8} {:<10} {:<10} {:<8} {:<10} {:<6}",
                "θ_join", "归簇率", "待定剩余", "新簇", "一致率", "漂移"
            );
            for row in &report.by_theta_join {
                let agreement = match row.agreement_rate {
                    Some(rate) => format!("{rate:.3}"),
                    None => "—".to_string(),
                };
                println!(
                    "  {:<8.3} {:<10.3} {:<10} {:<8} {:<10} {:<6}",
                    row.theta_join,
                    row.assigned_rate,
                    row.pending_remaining,
                    row.new_clusters,
                    agreement,
                    if row.drift_triggered { "是" } else { "否" },
                );
            }
        }
    }
    crate::ui::separator();
    Ok(())
}

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
