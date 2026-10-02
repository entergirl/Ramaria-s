//! crates/ramaria-cli/src/commands/probe/report/ablation.rs - 探针 report 消融对比报告
//!
//! 设计特点:
//! - F 组（移除）/ S 组（替代）/ I 组（净增量）三类对照分别配对判定
//! - 输出 Wilcoxon + Cohen's d + 95% CI + FDR 校正的结构化对比行
//! - I_* 保留 B1 基座测净增量、S_* 去 RAG 摘要测替代

use super::super::evaluate::ProbeEvaluation;
use super::super::evaluate::VariantEvaluation;
use super::super::run::metric_stat;
use super::super::types::ProbeExperiment;
use super::aux_metrics::variant_aux_metrics;
use super::render::{AblationComparisonRow, AblationReport};
use super::stats::{
    bh_fdr_adjust, cohens_d_paired, cohens_d_pooled, collect_variant_dim_scores,
    pair_dimension_diffs, wilcoxon_signed_rank_p,
};
use super::style_metrics::EMOTION_DESCRIPTIVE_NOTE;
use super::tost::{equivalence_annotation, tost_equivalence};

/// 构建消融对比报告。
///
/// 基线识别:
/// - F 组: F0（完整体系）为基线，F1~F4 为逐层消融；
/// - S 组: B1（压缩摘要基座）为基线，S_behavior/S_knowledge/S_expression/
///   S_narrative 为单层注入。
///
/// 统计（按题目配对）:
/// - 配对 Wilcoxon 符号秩检验（双尾，正态近似）；
/// - Cohen's d（配对 d_z）；
/// - 均值差 95% CI（t 分布，复用 `metric_stat`）；
/// - 全部行 p 值经 Benjamini–Hochberg FDR 校正。
///
/// 判定线: `p_fdr < 0.05 ∧ |d| ≥ 0.3 ∧ CI 不含 0` → 显著。
///
/// 等效性检验: 显著性框架只能证明"存在差异"，无法证明"零净增量"（相关对照长期
/// 只得到不显著）。故并行做 TOST（双单侧 t 检验），等效边界取 |d_av|=0.3
/// （d_av 为合并 SD 口径），`tost_p < 0.05` 即判定"等效（无实质净增量）"。
///
/// 判定维度: 情感维口径未校准，不参与判定（仅报告展示数值）；事实维按三套判据分别
/// 成行——`fact`（旧 2-gram 覆盖口径，计算逻辑未变）、`fact_norm`（长度归一口径）、
/// `fact_point`（子句级事实点口径），另加语气维 `tone`。三套事实口径共用同一 FDR
/// 校正池，故 `fact` 的 `p_fdr` 与只跑两维时的原报告可能略有差异，属预期。
pub(crate) fn build_ablation_report(
    experiment: &ProbeExperiment,
    evaluation: &ProbeEvaluation,
) -> AblationReport {
    // 索引评分数值档位（id → evaluation）
    let by_id: std::collections::HashMap<&str, &VariantEvaluation> = evaluation
        .variants
        .iter()
        .map(|v| (v.variant_id.as_str(), v))
        .collect();

    // 基线识别（F0 / B1）
    let find_baseline = |names: &[&str]| -> Option<&VariantEvaluation> {
        names
            .iter()
            .find_map(|n| by_id.get(*n).copied())
            .or_else(|| {
                // 兼容：id 非 F0/B1 但 params.ablation 标注了基线名的档位
                evaluation.variants.iter().find(|v| {
                    v.params
                        .ablation
                        .as_deref()
                        .map(|a| names.contains(&a))
                        .unwrap_or(false)
                })
            })
    };
    let f0 = find_baseline(&["F0"]);
    let b1 = find_baseline(&["B1"]);

    // 待比较组：F 组（F1~F4 vs F0）与 S 组（S_* vs B1），按数据集实际出现的档位驱动。
    // 判定维度取事实维三口径 + 语气维：情感维口径未校准，已移出层价值判定，仅在报告中展示数值。
    //
    // 事实维同时跑三套判据口径，用于在新旧判据下分栏核对：
    // - `fact` = 旧 2-gram 覆盖口径，计算逻辑未变；只是把它与两个重算口径一并纳入
    //   FDR 校正池，故其 `p_fdr` 与原（两维）报告可能略有差异，属预期；
    // - `fact_norm` = 长度归一口径（分母不随回复长度单调衰减）；
    // - `fact_point` = 子句级事实点口径（回复覆盖参考事实点的比例）。
    let dims = ["fact", "fact_norm", "fact_point", "tone"];

    // 先收集全部"候选行"（含未校正 p 值），再统一 FDR 校正后补判定字段。
    struct RawRow<'a> {
        ablation: &'a VariantEvaluation,
        dimension: &'a str,
        comparison_type: &'a str,
        base_variant: &'a str,
        diffs: Vec<f64>,
        base_scores: Vec<f64>,
        ablated_scores: Vec<f64>,
        base_mean: f64,
        ablated_mean: f64,
        wilcoxon_p: f64,
        cohens_d: f64,
        ci_low: f64,
        ci_high: f64,
    }

    let mut raw_rows: Vec<RawRow> = Vec::new();
    let mut compared_ids: Vec<String> = Vec::new();

    // F 组（removal）：F1~F4 逐层关闭 vs F0——回答"去掉某一层的边际损失"。
    if let Some(base) = f0 {
        for name in ["F1", "F2", "F3", "F4"] {
            if let Some(ablated) = by_id.get(name) {
                compared_ids.push(ablated.variant_id.clone());
                for dim in dims {
                    let (diffs, base_mean, ablated_mean, base_scores, ablated_scores) =
                        pair_dimension_diffs(
                            &collect_variant_dim_scores(ablated, dim),
                            &collect_variant_dim_scores(base, dim),
                        );
                    if diffs.len() < 2 {
                        tracing::debug!(
                            ablation = name,
                            dimension = dim,
                            pairs = diffs.len(),
                            "消融对比配对样本不足，跳过该行"
                        );
                        continue;
                    }
                    raw_rows.push(RawRow {
                        ablation: ablated,
                        dimension: dim,
                        comparison_type: "removal",
                        base_variant: base.variant_id.as_str(),
                        base_scores,
                        ablated_scores,
                        base_mean,
                        ablated_mean,
                        wilcoxon_p: wilcoxon_signed_rank_p(&diffs).unwrap_or(1.0),
                        cohens_d: cohens_d_paired(&diffs),
                        ci_low: metric_stat(&diffs).ci_low,
                        ci_high: metric_stat(&diffs).ci_high,
                        diffs,
                    });
                }
            }
        }
    } else {
        tracing::warn!("消融对比报告：未找到 F0 基线档位，F 组（F1~F4）无法对比");
    }

    // S 组（substitution）与 I 组（increment）均对照 B1，但对照口径不同：
    // - S_*（替代）＝去 RAG 摘要、仅单专属层——回答"单层能否替代 RAG"；
    // - I_*（净增量）＝B1 基座 + 单专属层——回答"在 RAG 之上叠加一层的净增量"。
    if let Some(base) = b1 {
        for (name, comparison_type) in [
            ("S_behavior", "substitution"),
            ("S_knowledge", "substitution"),
            ("S_expression", "substitution"),
            ("S_narrative", "substitution"),
            ("I_behavior", "increment"),
            ("I_knowledge", "increment"),
            ("I_expression", "increment"),
            ("I_narrative", "increment"),
        ] {
            if let Some(ablated) = by_id.get(name) {
                compared_ids.push(ablated.variant_id.clone());
                for dim in dims {
                    let (diffs, base_mean, ablated_mean, base_scores, ablated_scores) =
                        pair_dimension_diffs(
                            &collect_variant_dim_scores(ablated, dim),
                            &collect_variant_dim_scores(base, dim),
                        );
                    if diffs.len() < 2 {
                        tracing::debug!(
                            ablation = name,
                            dimension = dim,
                            pairs = diffs.len(),
                            "消融对比配对样本不足，跳过该行"
                        );
                        continue;
                    }
                    raw_rows.push(RawRow {
                        ablation: ablated,
                        dimension: dim,
                        comparison_type,
                        base_variant: base.variant_id.as_str(),
                        base_scores,
                        ablated_scores,
                        base_mean,
                        ablated_mean,
                        wilcoxon_p: wilcoxon_signed_rank_p(&diffs).unwrap_or(1.0),
                        cohens_d: cohens_d_paired(&diffs),
                        ci_low: metric_stat(&diffs).ci_low,
                        ci_high: metric_stat(&diffs).ci_high,
                        diffs,
                    });
                }
            }
        }
    } else {
        tracing::warn!("消融对比报告：未找到 B1 基线档位，S 组（替代）与 I 组（净增量）无法对比");
    }

    // 多比较 FDR 校正（Benjamini–Hochberg，作用于全部候选行）。
    let p_raw: Vec<f64> = raw_rows.iter().map(|r| r.wilcoxon_p).collect();
    let p_fdr = bh_fdr_adjust(&p_raw);

    let mut rows = Vec::with_capacity(raw_rows.len());
    for (raw, p_fdr) in raw_rows.into_iter().zip(p_fdr) {
        let ablation_name = raw.ablation.variant_id.as_str();
        // 显著性判定线：p_fdr<0.05 ∧ |d_z|≥0.3 ∧ CI 不含 0
        let ci_excludes_zero = raw.ci_low > 0.0 || raw.ci_high < 0.0;
        let significant = p_fdr < 0.05 && raw.cohens_d.abs() >= 0.3 && ci_excludes_zero;
        // 等效性判定：TOST 只能证伪"存在实质净增量"，用于补上显著性框架的盲区
        // （长期只得到"不显著"时，无法区分"真的没增量"与"样本不足"）。
        let tost = tost_equivalence(&raw.diffs, &raw.base_scores, &raw.ablated_scores, 0.3);
        let (equiv_bound, tost_p, equivalent) = match tost {
            Some(t) => (t.bound, t.p, t.equivalent),
            None => (0.0, 1.0, false),
        };
        let cohens_d_pooled = cohens_d_pooled(&raw.base_scores, &raw.ablated_scores);
        // 均值差（消融档 − 基线档）。
        let mean_diff = raw.ablated_mean - raw.base_mean;
        // 对照类型名（人类可读），三态结论文案共用。
        let type_label = match raw.comparison_type {
            "removal" => "移除对照",
            "substitution" => "替代对照",
            _ => "净增量对照",
        };
        // 未达显著时的结论文案（等效 / 不可判定），三类对照共用；
        // 显著分支不使用，故只在需要时 clone。
        let equivalence_text = equivalence_annotation(
            type_label,
            p_fdr,
            tost_p,
            equiv_bound,
            cohens_d_pooled,
            raw.cohens_d,
            equivalent,
        );
        // 方向语义按对照类型区分：
        // - removal（F 组 vs F0）：关注"移除后是否下降"；
        // - substitution（S 组 vs B1）：去 RAG 摘要只留单层，关注"能否替代 RAG 基座"；
        // - increment（I 组 vs B1）：B1 基座 + 单层，关注"叠加后是否净增"。
        let (direction, annotation) = match raw.comparison_type {
            "removal" => {
                if significant && mean_diff < 0.0 {
                    (
                        "down".to_string(),
                        format!(
                            "移除该层后质量显著下降（{:.3}），该层对「{}」有贡献",
                            mean_diff, raw.dimension
                        ),
                    )
                } else if significant {
                    (
                        "up".to_string(),
                        format!(
                            "移除该层后质量反升（{:.3}），该层在本维度疑似冗余/负作用",
                            mean_diff
                        ),
                    )
                } else {
                    ("none".to_string(), equivalence_text.clone())
                }
            }
            "substitution" => {
                // S 组：目标层在无 RAG 摘要时单独注入，与 B1（仅 RAG 摘要）比较。
                if significant && mean_diff < 0.0 {
                    (
                        "down".to_string(),
                        format!(
                            "替代对照：去 RAG 摘要仅该层显著低于 B1（{:.3}），该层无法独立替代 RAG 摘要基座",
                            mean_diff
                        ),
                    )
                } else if significant {
                    (
                        "up".to_string(),
                        format!(
                            "替代对照：去 RAG 摘要仅该层显著高于 B1（{:.3}），该层可独立替代 RAG 摘要基座",
                            mean_diff
                        ),
                    )
                } else {
                    ("none".to_string(), equivalence_text.clone())
                }
            }
            _ => {
                // increment（I 组）：B1 基座 + 该层，与 B1 比较净增量。
                if significant && mean_diff < 0.0 {
                    (
                        "down".to_string(),
                        format!(
                            "净增量对照：在 B1 基座上叠加该层显著下降（{:.3}），层叠加为负向（压缩/干扰）",
                            mean_diff
                        ),
                    )
                } else if significant {
                    (
                        "up".to_string(),
                        format!(
                            "净增量对照：在 B1 基座上叠加该层显著提升（{:.3}），该层有正向净增量",
                            mean_diff
                        ),
                    )
                } else {
                    ("none".to_string(), equivalence_text)
                }
            }
        };
        // 综合判定：显著优先（显著与等效互斥），否则区分"证得等效"与"证据不足"。
        let verdict = if significant {
            format!("significant_{direction}")
        } else if equivalent {
            "equivalent".to_string()
        } else {
            "inconclusive".to_string()
        };

        rows.push(AblationComparisonRow {
            ablation_variant: ablation_name.to_string(),
            description: raw.ablation.description.clone(),
            comparison_type: raw.comparison_type.to_string(),
            base_variant: raw.base_variant.to_string(),
            dimension: raw.dimension.to_string(),
            n_pairs: raw.diffs.len(),
            base_mean: raw.base_mean,
            ablated_mean: raw.ablated_mean,
            mean_diff: raw.ablated_mean - raw.base_mean,
            wilcoxon_p: raw.wilcoxon_p,
            p_fdr,
            cohens_d: raw.cohens_d,
            cohens_d_pooled,
            equiv_bound,
            tost_p,
            equivalent,
            verdict,
            ci95_low: raw.ci_low,
            ci95_high: raw.ci_high,
            significant,
            direction,
            annotation,
        });
    }

    // 辅助指标：覆盖所有参与对比档位 + 基线档位（从 run 实验明细取回复指标）。
    let mut compared: Vec<String> = compared_ids;
    if let Some(b) = f0 {
        compared.push(b.variant_id.clone());
    }
    if let Some(b) = b1 {
        compared.push(b.variant_id.clone());
    }
    let mut aux = Vec::new();
    for vr in &experiment.variants {
        if !compared.contains(&vr.variant_id) {
            continue;
        }
        aux.push(variant_aux_metrics(vr));
    }

    AblationReport {
        baseline_variant: f0
            .map(|v| v.variant_id.clone())
            .or_else(|| b1.map(|v| v.variant_id.clone()))
            .unwrap_or_default(),
        rows,
        aux,
        judgment_dimensions: dims.iter().map(|d| d.to_string()).collect(),
        descriptive_dimensions: vec!["emotion".to_string()],
        dimension_scope_note: EMOTION_DESCRIPTIVE_NOTE.to_string(),
        equivalence_note: "等效性检验：TOST（双单侧 t 检验，df=配对数−1），等效边界取 |d_av|=0.3，\
            即原始差分 ±0.3×合并SD；tost_p<0.05 判定「等效（无实质净增量）」。\
            显著性仍按配对 Wilcoxon + d_z + 95%CI + BH-FDR。"
            .to_string(),
    }
}
