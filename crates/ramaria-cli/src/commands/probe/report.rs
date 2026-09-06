//! crates/ramaria-cli/src/commands/probe/report.rs - 探针 report：档位对比报告 + 定稿建议 + 校准 + 消融
//!
//! 设计特点:
//! - 档位对比报告（`probe report`）：汇总各档位评分生成对比表，给出每维最佳档位与综合定稿建议。
//! - 消融对比（--ablation）：F 组 / S 组按题目配对做 Wilcoxon 符号秩 + Cohen's d + 95% CI + BH-FDR 判定。
//! - 人工抽检校准：比对 judge 与人工分数的一致性 / 偏差 / 校准系数（由校准文件驱动，可选）。
//! - 知识层质量：基于评分数值中的事实维题目评估误报 / 漏报率（目标 <10%）。
//! - 输出 markdown / JSON 双形态；配对非参检验等纯函数逻辑独立，便于单元测试。

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use ramaria_core::error::RamariaError;

use super::evaluate::{ItemEvaluation, ProbeEvaluation, VariantEvaluation, read_experiment};
use super::run::metric_stat;
use super::types::{ProbeExperiment, ProbeVariantResult, VariantParams};

// =========================================================
// probe report：档位对比报告 + 定稿建议 + 校准 + 知识层质量评估
// =========================================================

/// 档位对比报告（`probe report` 的输出，markdown/JSON 双形态）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeReport {
    pub results_file: String,
    pub evaluation_file: Option<String>,
    pub persona_uid: String,
    pub dataset_seed: u64,
    pub judge_used: bool,
    pub embedding_used: bool,
    pub generated_at: String,
    /// 各档位评分汇总表
    pub variants: Vec<VariantReportRow>,
    /// 定稿建议（每维度的推荐档位 + 理由）
    pub recommendation: Recommendation,
    /// 人工抽检校准结果（未提供校准文件时为 None）
    pub calibration: Option<CalibrationResult>,
    /// 知识层抽取质量评估（基于 fact 题误报/漏报；可选）
    pub knowledge_quality: Option<KnowledgeQualityReport>,
    /// 消融对比报告（`probe report --ablation`；普通模式为 None）
    pub ablation: Option<AblationReport>,
}

/// 档位报告行（评分对比表）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VariantReportRow {
    pub variant_id: String,
    pub description: String,
    pub params: VariantParams,
    pub fact_score: Option<f64>,
    pub tone_score: Option<f64>,
    /// 情感表达维均分（0.0~1.0；无 emotion 题时为 None）
    pub emotion_score: Option<f64>,
    pub success_count: usize,
    pub total_count: usize,
    pub failed_count: usize,
}

/// 定稿建议。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Recommendation {
    /// 每维度的最佳档位 id 与理由
    pub per_dimension: Vec<DimensionRecommendation>,
    /// 综合建议（兼顾各维的平衡档位）
    pub overall: String,
}

/// 单维度定稿建议。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DimensionRecommendation {
    pub dimension: String,
    pub best_variant: Option<String>,
    pub best_score: Option<f64>,
    pub reason: String,
}

/// 人工抽检校准结果。
///
/// 说明:
/// - `consistency`: judge 与人工分数的一致性（同分占比 / 平均绝对差）。
/// - `bias`: judge 相对人工的系统性偏差（judge 均分 − 人工均分；>0 偏高、<0 偏低）。
/// - `calibrated_coefficient`: 校准系数（人工均分 / judge 均分，用于把 judge 分缩放到人工尺度）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CalibrationResult {
    pub sample_count: usize,
    pub total_count: usize,
    pub sample_rate: f64,
    pub consistency_exact: f64,
    pub mean_abs_diff: f64,
    pub bias: f64,
    pub calibrated_coefficient: Option<f64>,
    /// 是否不一致（一致性低或偏差大，报告标注）
    pub inconsistent: bool,
    /// 标注说明
    pub annotation: String,
}

/// 知识层抽取质量评估报告。
///
/// 说明:
/// - 基于事实维探针题评估知识层抽取质量：以「回复是否包含事件事实」判定命中/漏报。
/// - `false_positive_rate`（误报）：判定器注入但回复未涵盖事实（答非所问）。
/// - `false_negative_rate`（漏报）：回复未包含应有的事实信息（目标 <10%）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct KnowledgeQualityReport {
    pub sample_count: usize,
    pub fact_hit_count: usize,
    pub false_positive_rate: f64,
    pub false_negative_rate: f64,
    /// 是否达到漏报 <10% 目标
    pub miss_target_met: bool,
    pub annotation: String,
}

// =========================================================
// 消融对比报告（M5a T-004：配对 Wilcoxon + Cohen's d + CI + FDR）
// =========================================================

/// 消融对比报告（`probe report --ablation`）。
///
/// 结构:
/// - `baseline_variant`: 消融基线档位 id（F 组为 F0，S 组为 B1）。
/// - `rows`: 消融 vs 基线的逐"消融档位 × 维度"统计判定行。
/// - `aux`: 参与对比各档位的辅助指标（回复长度/耗时/空回复率）。
///
/// 判定线（D-V17-009）: `p_fdr < 0.05 ∧ |cohens_d| ≥ 0.3 ∧ CI 不含 0`
/// → 显著；F 组 diff<0（移除后下降）或 S 组 diff>0（加入后上升）→ 该层有贡献。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AblationReport {
    /// 消融基线档位 id（F0 或 B1）
    pub baseline_variant: String,
    /// 逐消融档位 × 维度统计判定
    pub rows: Vec<AblationComparisonRow>,
    /// 参与对比档位的辅助指标（mean ± CI / 空回复率）
    pub aux: Vec<VariantAuxMetrics>,
}

/// 单条消融对比（某消融档位 × 某维度，按题目配对）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AblationComparisonRow {
    /// 消融档位 id（如 F1 / S_behavior）
    pub ablation_variant: String,
    /// 消融档位描述
    pub description: String,
    /// 维度（fact / tone / emotion）
    pub dimension: String,
    /// 配对题数
    pub n_pairs: usize,
    /// 基线均值（F0 或 B1）
    pub base_mean: f64,
    /// 消融后均值
    pub ablated_mean: f64,
    /// 均值差（消融 − 基线）
    pub mean_diff: f64,
    /// 配对 Wilcoxon 符号秩检验 p 值（双尾，正态近似）
    pub wilcoxon_p: f64,
    /// FDR 校正后 p 值（Benjamini–Hochberg）
    pub p_fdr: f64,
    /// Cohen's d（配对 d_z = mean(diff)/sd(diff)；sd=0 时 ±10 标记远超阈值）
    pub cohens_d: f64,
    /// 均值差 95% 置信区间（t 分布）
    pub ci95_low: f64,
    /// 均值差 95% 置信区间上界
    pub ci95_high: f64,
    /// 是否显著（p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0）
    pub significant: bool,
    /// 该层贡献方向结论（up = 消融后提升 / down = 消融后下降 / none）
    pub direction: String,
    /// 人类可读结论
    pub annotation: String,
}

/// 档位辅助指标（消融报告交叉验证用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VariantAuxMetrics {
    pub variant_id: String,
    pub description: String,
    /// 平均回复字符数
    pub reply_chars_mean: f64,
    /// 平均耗时（毫秒）
    pub elapsed_ms_mean: f64,
    /// 空回复率（0.0~1.0）
    pub empty_reply_rate: f64,
    /// 成功题数 / 总题数
    pub success_count: usize,
    pub total_count: usize,
}

// =========================================================
// 执行 `probe report`
// =========================================================

/// 执行 `probe report`。
///
/// 流程:
/// 1. 读取实验结果（probe run 产物）。
/// 2. 读取评分数值（probe evaluate 产物；缺失则仅汇总 run 指标；
///    `--ablation` 模式必须提供评分数值，否则业务校验失败）。
/// 3. 生成档位对比表 + 定稿建议（每维最佳档位）。
/// 4. 若提供校准文件 → 计算 judge/人工一致性、偏差、校准系数。
/// 5. 若提供评分数值 → 基于 fact 题评估知识层误报/漏报。
/// 6. `--ablation` 模式 → 自动识别 F0/B1 基线生成消融对比统计。
/// 7. 输出 markdown / JSON 双形态。
pub(super) async fn run_report(
    _app: &Arc<ramaria_app::App>,
    results_path: &Path,
    evaluation_path: Option<&Path>,
    calibration_path: Option<&Path>,
    output: Option<&str>,
    ablation: bool,
    json: bool,
) -> anyhow::Result<()> {
    // Step 1: 读取实验结果
    let experiment = read_experiment(results_path)?;

    // --ablation 模式前置校验：需要评分数值文件（含逐题明细）。
    if ablation && evaluation_path.is_none() {
        return Err(anyhow::anyhow!(RamariaError::validation(
            "消融对比报告（--ablation）需要评分数值文件：请先运行 `ramaria probe evaluate --results <run> --dataset <ds> --output <eval>`"
        )));
    }

    // Step 2: 读取评分数值（可选）
    let evaluation: Option<ProbeEvaluation> = match evaluation_path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| {
                anyhow::anyhow!(RamariaError::validation(format!(
                    "读取评分数值失败: {}（请先运行 `ramaria probe evaluate` 生成）: {e}",
                    p.display()
                )))
            })?;
            match serde_json::from_str(&text) {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!(error = %e, "评分数值解析失败，报告仅含运行指标");
                    None
                }
            }
        }
        None => None,
    };

    // Step 3: 档位对比表
    let mut rows = Vec::with_capacity(experiment.variants.len());
    for vr in &experiment.variants {
        let ev = evaluation
            .as_ref()
            .and_then(|e| e.variants.iter().find(|v| v.variant_id == vr.variant_id));
        let success = vr.runs.len().saturating_sub(vr.failed_count);
        rows.push(VariantReportRow {
            variant_id: vr.variant_id.clone(),
            description: vr.description.clone(),
            params: vr.params.clone(),
            fact_score: ev.and_then(|v| v.fact_score),
            tone_score: ev.and_then(|v| v.tone_score),
            emotion_score: ev.and_then(|v| v.emotion_score),
            success_count: success,
            total_count: vr.runs.len(),
            failed_count: vr.failed_count,
        });
    }

    // Step 4: 定稿建议（基于评分，无评分时基于运行指标）
    let recommendation = build_recommendation(&rows);

    // Step 5: 人工抽检校准（可选）
    let calibration = match calibration_path {
        Some(p) => {
            let manual = read_manual_scores(p)?;
            Some(compute_calibration(&manual, evaluation.as_ref()))
        }
        None => None,
    };

    // Step 6: 知识层质量评估（基于评分数值 fact 题）
    let knowledge_quality = evaluation.as_ref().map(assess_knowledge_quality);

    // Step 6.5: 消融对比报告（--ablation 模式）
    // 评分数值解析失败时评估为 None → 消融段缺省（记 warn 已在上游输出）。
    let ablation_report = if ablation {
        evaluation
            .as_ref()
            .map(|eval| build_ablation_report(&experiment, eval))
    } else {
        None
    };

    let report = ProbeReport {
        results_file: results_path.display().to_string(),
        evaluation_file: evaluation_path.map(|p| p.display().to_string()),
        persona_uid: experiment.persona_uid.clone(),
        dataset_seed: experiment.dataset_seed,
        judge_used: evaluation.as_ref().map(|e| e.judge_used).unwrap_or(false),
        embedding_used: evaluation
            .as_ref()
            .map(|e| e.embedding_used)
            .unwrap_or(false),
        generated_at: super::now_iso8601(),
        variants: rows,
        recommendation,
        calibration,
        knowledge_quality,
        ablation: ablation_report,
    };

    // Step 7: 输出
    if let Some(out) = output {
        // 按扩展名判断输出形态：.json → JSON；.md → markdown；其他按 --json 决定
        let is_json_file = out.ends_with(".json");
        if is_json_file || (json && !out.ends_with(".md")) {
            write_report_json(out, &report)?;
        } else {
            write_report_markdown(out, &report)?;
        }
        if json {
            let data = serde_json::json!({
                "file": out,
                "persona_uid": report.persona_uid,
                "variants": report.variants.len(),
                "calibration": report.calibration.is_some(),
                "knowledge_quality": report.knowledge_quality.is_some(),
            });
            return crate::json::emit_ok(&data);
        }
        crate::ui::success(&format!(
            "探针报告已写入 {}（{} 档位对比，{}）",
            out,
            report.variants.len(),
            if report.ablation.is_some() {
                "含消融对比统计"
            } else if report.calibration.is_some() {
                "含人工抽检校准"
            } else {
                "未校准"
            }
        ));
        return Ok(());
    }

    if json {
        return crate::json::emit_ok(&report);
    }

    print_report_summary(&report);
    Ok(())
}

/// 构建定稿建议（每维最佳档位 + 综合建议）。
fn build_recommendation(rows: &[VariantReportRow]) -> Recommendation {
    let mut per_dimension = Vec::new();

    // 事实维：取 fact_score 最高档位
    let fact_best = rows
        .iter()
        .filter(|r| r.fact_score.is_some())
        .max_by(|a, b| {
            a.fact_score
                .partial_cmp(&b.fact_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    per_dimension.push(DimensionRecommendation {
        dimension: "fact".to_string(),
        best_variant: fact_best.map(|r| r.variant_id.clone()),
        best_score: fact_best.and_then(|r| r.fact_score),
        reason: match fact_best {
            Some(r) => format!(
                "事实维最高分 {:.2}（档位 {}）；综合 embedding 余弦与关键词命中",
                r.fact_score.unwrap_or(0.0),
                r.variant_id
            ),
            None => {
                "无有效事实维评分（embedding 不可用或全部失败），无法给出事实维建议".to_string()
            }
        },
    });

    // 语气维：取 tone_score 最高档位
    let tone_best = rows
        .iter()
        .filter(|r| r.tone_score.is_some())
        .max_by(|a, b| {
            a.tone_score
                .partial_cmp(&b.tone_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    per_dimension.push(DimensionRecommendation {
        dimension: "tone".to_string(),
        best_variant: tone_best.map(|r| r.variant_id.clone()),
        best_score: tone_best.and_then(|r| r.tone_score),
        reason: match tone_best {
            Some(r) => format!(
                "语气维最高分 {:.2}（档位 {}）；judge rubric 1~5 评分",
                r.tone_score.unwrap_or(0.0),
                r.variant_id
            ),
            None => "语气维 judge 不可用或已跳过，无法给出语气维建议".to_string(),
        },
    });

    // 情感表达维：取 emotion_score 最高档位（rubric 0/0.5/1 回应恰当性）
    let emotion_best = rows
        .iter()
        .filter(|r| r.emotion_score.is_some())
        .max_by(|a, b| {
            a.emotion_score
                .partial_cmp(&b.emotion_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    per_dimension.push(DimensionRecommendation {
        dimension: "emotion".to_string(),
        best_variant: emotion_best.map(|r| r.variant_id.clone()),
        best_score: emotion_best.and_then(|r| r.emotion_score),
        reason: match emotion_best {
            Some(r) => format!(
                "情感表达维最高分 {:.2}（档位 {}）；rubric 0/0.5/1 回应恰当性",
                r.emotion_score.unwrap_or(0.0),
                r.variant_id
            ),
            None => "无情感表达维评分（无 emotion 题或全部失败），无法给出情感维建议".to_string(),
        },
    });

    // 综合建议：若事实/语气/情感最佳档位一致 → 取该档位；否则提示需人工权衡
    let all_same = |best: Option<&VariantReportRow>, id: &str| {
        best.map(|r| r.variant_id == id).unwrap_or(false)
    };
    let overall = match fact_best {
        Some(f) if all_same(tone_best, &f.variant_id) && all_same(emotion_best, &f.variant_id) => {
            format!(
                "综合建议档位 {}（事实/语气/情感均最优）；需人工抽检校准后定稿",
                f.variant_id
            )
        }
        _ => "各维最佳档位不一致，需结合人工抽检与消融实验（M5）权衡取舍".to_string(),
    };

    Recommendation {
        per_dimension,
        overall,
    }
}

// =========================================================
// 消融对比报告实现（M5a T-004）
// =========================================================

/// 供消融配对的逐维度"item_id → 分数"索引。
type VariantDimScores = std::collections::HashMap<String, f64>;

/// 从评分数值档位提取某维度的逐题分数（仅成功题）。
///
/// 说明: tone 维 judge 分 1~5 直接作连续分使用；fact/emotion 维 0~1。
fn collect_variant_dim_scores(ev: &VariantEvaluation, dim: &str) -> VariantDimScores {
    let mut map = VariantDimScores::new();
    for item in &ev.items {
        if item.error.is_some() {
            continue;
        }
        let score = match dim {
            "fact" => item.fact.as_ref().map(|s| s.score),
            "tone" => item.tone.as_ref().map(|s| s.score as f64),
            "emotion" => item.emotion.as_ref().map(|s| s.score),
            _ => None,
        };
        if let Some(s) = score {
            map.insert(item.item_id.clone(), s);
        }
    }
    map
}

/// 按题目配对两个档位在某维度的差分样本。
///
/// 配对规则: 仅取两端都成功评分的 item_id（同一题目），
/// `diffs = ablated − base`；两端任一缺失的题不参与配对。
fn pair_dimension_diffs(
    ablated: &VariantDimScores,
    base: &VariantDimScores,
) -> (Vec<f64>, f64, f64) {
    let mut diffs = Vec::new();
    let mut base_sum = 0.0;
    let mut ablated_sum = 0.0;
    for (item_id, base_score) in base {
        if let Some(ablated_score) = ablated.get(item_id) {
            diffs.push(ablated_score - base_score);
            base_sum += base_score;
            ablated_sum += ablated_score;
        }
    }
    let n = diffs.len() as f64;
    if n == 0.0 {
        return (diffs, 0.0, 0.0);
    }
    (diffs, base_sum / n, ablated_sum / n)
}

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
/// 判定线（D-V17-009）: `p_fdr < 0.05 ∧ |d| ≥ 0.3 ∧ CI 不含 0` → 显著。
pub(super) fn build_ablation_report(
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
    let dims = ["fact", "tone", "emotion"];

    // 先收集全部"候选行"（含未校正 p 值），再统一 FDR 校正后补判定字段。
    struct RawRow<'a> {
        ablation: &'a VariantEvaluation,
        dimension: &'a str,
        diffs: Vec<f64>,
        base_mean: f64,
        ablated_mean: f64,
        wilcoxon_p: f64,
        cohens_d: f64,
        ci_low: f64,
        ci_high: f64,
    }

    let mut raw_rows: Vec<RawRow> = Vec::new();
    let mut compared_ids: Vec<String> = Vec::new();

    // F 组：F1~F4 逐层关闭 vs F0
    if let Some(base) = f0 {
        for name in ["F1", "F2", "F3", "F4"] {
            if let Some(ablated) = by_id.get(name) {
                compared_ids.push(ablated.variant_id.clone());
                for dim in dims {
                    let (diffs, base_mean, ablated_mean) = pair_dimension_diffs(
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

    // S 组：单层注入 vs B1
    if let Some(base) = b1 {
        for name in ["S_behavior", "S_knowledge", "S_expression", "S_narrative"] {
            if let Some(ablated) = by_id.get(name) {
                compared_ids.push(ablated.variant_id.clone());
                for dim in dims {
                    let (diffs, base_mean, ablated_mean) = pair_dimension_diffs(
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
        tracing::warn!("消融对比报告：未找到 B1 基线档位，S 组（单层注入）无法对比");
    }

    // 多比较 FDR 校正（Benjamini–Hochberg，作用于全部候选行）。
    let p_raw: Vec<f64> = raw_rows.iter().map(|r| r.wilcoxon_p).collect();
    let p_fdr = bh_fdr_adjust(&p_raw);

    let is_f_group = |ablation_name: &str| matches!(ablation_name, "F1" | "F2" | "F3" | "F4");

    let mut rows = Vec::with_capacity(raw_rows.len());
    for (raw, p_fdr) in raw_rows.into_iter().zip(p_fdr) {
        let ablation_name = raw.ablation.variant_id.as_str();
        // 判定线：p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0
        let ci_excludes_zero = raw.ci_low > 0.0 || raw.ci_high < 0.0;
        let significant = p_fdr < 0.05 && raw.cohens_d.abs() >= 0.3 && ci_excludes_zero;
        // 方向语义：F 组关注"移除后下降"；S 组关注"加入后上升"。
        let mean_diff = raw.ablated_mean - raw.base_mean;
        let (direction, annotation) = if significant {
            if mean_diff < 0.0 {
                (
                    "down".to_string(),
                    if is_f_group(ablation_name) {
                        format!(
                            "移除该层后质量显著下降（{:.3}），该层对「{}」有贡献",
                            mean_diff, raw.dimension
                        )
                    } else {
                        format!(
                            "加入该层后质量显著下降（{:.3}），该层单独注入为负向",
                            mean_diff
                        )
                    },
                )
            } else {
                (
                    "up".to_string(),
                    if is_f_group(ablation_name) {
                        format!(
                            "移除该层后质量反升（{:.3}），该层在本维度疑似冗余/负作用",
                            mean_diff
                        )
                    } else {
                        format!("加入该层后质量显著提升（{:.3}），该层有正向贡献", mean_diff)
                    },
                )
            }
        } else {
            (
                "none".to_string(),
                format!("无显著差异（p_fdr={:.3}, |d|={:.2}）", p_fdr, raw.cohens_d),
            )
        };

        rows.push(AblationComparisonRow {
            ablation_variant: ablation_name.to_string(),
            description: raw.ablation.description.clone(),
            dimension: raw.dimension.to_string(),
            n_pairs: raw.diffs.len(),
            base_mean: raw.base_mean,
            ablated_mean: raw.ablated_mean,
            mean_diff: raw.ablated_mean - raw.base_mean,
            wilcoxon_p: raw.wilcoxon_p,
            p_fdr,
            cohens_d: raw.cohens_d,
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
    }
}

/// 计算单档位辅助指标（平均回复长度 / 平均耗时 / 空回复率）。
fn variant_aux_metrics(vr: &ProbeVariantResult) -> VariantAuxMetrics {
    let total = vr.runs.len();
    let success = total.saturating_sub(vr.failed_count);
    let mut chars = 0usize;
    let mut ms: u128 = 0;
    let mut empty = 0usize;
    for run in &vr.runs {
        chars += run.metrics.reply_chars;
        ms += run.metrics.elapsed_ms;
        if run.reply.trim().is_empty() {
            empty += 1;
        }
    }
    VariantAuxMetrics {
        variant_id: vr.variant_id.clone(),
        description: vr.description.clone(),
        reply_chars_mean: if total > 0 {
            chars as f64 / total as f64
        } else {
            0.0
        },
        elapsed_ms_mean: if total > 0 {
            ms as f64 / total as f64
        } else {
            0.0
        },
        empty_reply_rate: if total > 0 {
            empty as f64 / total as f64
        } else {
            0.0
        },
        success_count: success,
        total_count: total,
    }
}

// =========================================================
// 配对非参检验与效应量（纯函数，可单测）
// =========================================================

/// 配对 Wilcoxon 符号秩检验双尾 p 值（正态近似，无零差分）。
///
/// 算法:
/// - 剔除零差分后取绝对值排序，相同绝对值取平均秩；
/// - W+ = 正差分秩和；W 均值/方差（不含结校正的近似）→ z → 双尾 p。
/// - 样本量过小（n<5）时近似偏保守/不可靠，返回 `None`（调用方按 p=1.0 处理）。
pub(super) fn wilcoxon_signed_rank_p(diffs: &[f64]) -> Option<f64> {
    // 剔除零差分
    let mut abs_pairs: Vec<(f64, bool)> = diffs
        .iter()
        .filter(|d| d.abs() > 1e-12)
        .map(|d| (d.abs(), *d > 0.0))
        .collect();
    let n = abs_pairs.len();
    if n < 5 {
        return None; // 样本过小，正态近似不可靠
    }
    abs_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // 平均秩（处理相同绝对值）
    let mut w_plus = 0.0f64;
    let mut i = 0usize;
    while i < n {
        let mut j = i;
        while j + 1 < n && (abs_pairs[j + 1].0 - abs_pairs[i].0).abs() < 1e-12 {
            j += 1;
        }
        let rank_avg = (i + j + 2) as f64 / 2.0; // 1-based 位置平均
        for pair in &abs_pairs[i..=j] {
            if pair.1 {
                w_plus += rank_avg;
            }
        }
        i = j + 1;
    }

    // 无结近似：mean = n(n+1)/4，var = n(n+1)(2n+1)/24
    let n_f = n as f64;
    let mean = n_f * (n_f + 1.0) / 4.0;
    let variance = n_f * (n_f + 1.0) * (2.0 * n_f + 1.0) / 24.0;
    if variance <= 0.0 {
        return None;
    }
    let z = (w_plus - mean) / variance.sqrt();
    Some(2.0 * (1.0 - normal_cdf(z.abs())))
}

/// 标准正态分布 CDF（erf 近似）。
pub(super) fn normal_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf_approx(z / std::f64::consts::SQRT_2))
}

/// erf 近似（Abramowitz–Stegun 7.1.26，最大误差 ~1.5e-7）。
pub(super) fn erf_approx(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    if x > 6.0 {
        return sign;
    }
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    sign * (1.0 - poly * (-x * x).exp())
}

/// 配对 Cohen's d（d_z = mean(diff) / sd(diff)）。
///
/// 说明: 差分为零（sd≈0）且均值非零时以 ±10 标记"远超效应量阈值"
/// （避免 inf 破坏判定与序列化）；均值亦为零 → 0.0。
pub(super) fn cohens_d_paired(diffs: &[f64]) -> f64 {
    let n = diffs.len();
    if n == 0 {
        return 0.0;
    }
    let n_f = n as f64;
    let mean = diffs.iter().sum::<f64>() / n_f;
    if n == 1 {
        return if mean.abs() < 1e-12 {
            0.0
        } else {
            mean.signum() * 10.0
        };
    }
    let variance = diffs.iter().map(|d| (d - mean) * (d - mean)).sum::<f64>() / (n_f - 1.0);
    let sd = variance.sqrt();
    if sd < 1e-12 {
        if mean.abs() < 1e-12 {
            0.0
        } else {
            mean.signum() * 10.0
        }
    } else {
        mean / sd
    }
}

/// Benjamini–Hochberg FDR 校正。
///
/// 返回与输入等长的校正后 q 值；空输入返回空。
pub(super) fn bh_fdr_adjust(p_values: &[f64]) -> Vec<f64> {
    let m = p_values.len();
    if m == 0 {
        return Vec::new();
    }
    // 索引排序（小 → 大）
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|a, b| {
        p_values[*a]
            .partial_cmp(&p_values[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut q = vec![1.0f64; m];
    // 从最大 p 反向累计取最小
    let mut running_min = f64::INFINITY;
    for (rank_idx, &orig_idx) in order.iter().enumerate().rev() {
        let raw = p_values[orig_idx];
        let adjusted = (raw * m as f64 / (rank_idx + 1) as f64).min(1.0);
        running_min = running_min.min(adjusted);
        q[orig_idx] = running_min;
    }
    q
}

// =========================================================
// 人工抽检校准（T-V16-4-004）
// =========================================================

/// 读取人工抽检校准文件。
///
/// 格式（JSON）:
/// ```json
/// { "scores": [ {"item_id": "tone-0001", "score": 4}, ... ] }
/// ```
/// 或简单数组 `[{"item_id": "...", "score": 4}]`。
pub(super) fn read_manual_scores(path: &Path) -> anyhow::Result<Vec<ManualScore>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "读取校准文件失败: {}: {e}",
            path.display()
        )))
    })?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| RamariaError::validation(format!("校准文件解析失败: {e}")))?;

    let scores = if let Some(arr) = value.as_array() {
        arr.clone()
    } else if let Some(obj) = value.get("scores").and_then(|s| s.as_array()) {
        obj.clone()
    } else {
        return Err(anyhow::anyhow!(RamariaError::validation(
            "校准文件格式无效（应为 JSON 数组或 {scores:[...]}）"
        )));
    };

    let mut out = Vec::with_capacity(scores.len());
    for s in scores {
        let item_id = s
            .get("item_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!(RamariaError::validation("校准条目缺少 item_id 字段")))?
            .to_string();
        let score = s
            .get("score")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!(RamariaError::validation("校准条目缺少 score 字段")))?;
        out.push(ManualScore { item_id, score });
    }
    Ok(out)
}

/// 人工抽检单条分数。
#[derive(Debug, Clone)]
pub struct ManualScore {
    pub item_id: String,
    pub score: u64,
}

/// 计算人工抽检校准结果（一致性 / 偏差 / 校准系数）。
///
/// 说明:
/// - 只统计 judge 有分的条目（tone 题 judge 分）。
/// - `consistency_exact`: judge 与人工同分占比。
/// - `mean_abs_diff`: 平均绝对差。
/// - `bias`: judge 均分 − 人工均分。
/// - `calibrated_coefficient`: 人工均分 / judge 均分（judge 均分为 0 时为 None）。
/// - `inconsistent`: 同分占比 < 0.5 或 |bias| > 1.0（判定校准不一致）。
fn compute_calibration(
    manual: &[ManualScore],
    evaluation: Option<&ProbeEvaluation>,
) -> CalibrationResult {
    // 收集 judge 分（从 evaluation 的 tone 题逐题明细）
    let mut judge_by_item: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    if let Some(eval) = evaluation {
        for v in &eval.variants {
            for item in &v.items {
                if let Some(tone) = &item.tone {
                    judge_by_item.insert(item.item_id.clone(), tone.score);
                }
            }
        }
    }

    // 配对：manual 中能在 judge 中找到分且维度匹配 tone 的条目
    let mut pairs: Vec<(u32, u64)> = Vec::new();
    for m in manual {
        if let Some(j) = judge_by_item.get(&m.item_id) {
            pairs.push((*j, m.score));
        }
    }

    let sample_count = pairs.len();
    if sample_count == 0 {
        return CalibrationResult {
            sample_count: 0,
            total_count: manual.len(),
            sample_rate: 0.0,
            consistency_exact: 0.0,
            mean_abs_diff: 0.0,
            bias: 0.0,
            calibrated_coefficient: None,
            inconsistent: true,
            annotation: "未找到任何与 judge 分匹配的人工抽检条目，无法校准".to_string(),
        };
    }

    let total_count = manual.len();
    let sample_rate = sample_count as f64 / total_count.max(1) as f64;

    let exact = pairs.iter().filter(|(j, m)| *j as u64 == *m).count();
    let consistency_exact = exact as f64 / sample_count as f64;

    let mean_abs_diff = pairs
        .iter()
        .map(|(j, m)| (*j as f64 - *m as f64).abs())
        .sum::<f64>()
        / sample_count as f64;

    let judge_mean = pairs.iter().map(|(j, _)| *j as f64).sum::<f64>() / sample_count as f64;
    let manual_mean = pairs.iter().map(|(_, m)| *m as f64).sum::<f64>() / sample_count as f64;
    let bias = judge_mean - manual_mean;

    let calibrated_coefficient = if judge_mean.abs() < 1e-9 {
        None
    } else {
        Some(manual_mean / judge_mean)
    };

    let inconsistent = consistency_exact < 0.5 || bias.abs() > 1.0;
    let annotation = if inconsistent {
        format!(
            "一致性偏低（同分 {:.0}% / 均差 {:.2} / 偏差 {:.2}），judge 需人工复核或调参",
            consistency_exact * 100.0,
            mean_abs_diff,
            bias
        )
    } else {
        format!(
            "一致性可接受（同分 {:.0}% / 均差 {:.2} / 偏差 {:.2}）",
            consistency_exact * 100.0,
            mean_abs_diff,
            bias
        )
    };

    CalibrationResult {
        sample_count,
        total_count,
        sample_rate,
        consistency_exact,
        mean_abs_diff,
        bias,
        calibrated_coefficient,
        inconsistent,
        annotation,
    }
}

// =========================================================
// 知识层抽取质量评估（T-V16-4-005）
// =========================================================

/// 评估知识层抽取质量（误报 / 漏报率）。
///
/// 说明:
/// - 基于评分数值中的 fact 题：以「回复是否充分回应事实性问题」判定命中。
/// - 判定规则（无 reference 时的近似）:
///   - `fact_hit`: 事实维 score ≥ 0.5（回复具体、信息充分）。
///   - `false_positive`（误报）: score 低但判定器/知识注入本应提供事实（此处以 score < 0.3 计）。
///   - `false_negative`（漏报）: score 居中但信息不足（score < 0.4 视为未充分回答事实）。
/// - 漏报率目标 < 10%（D-V16-004）。
fn assess_knowledge_quality(evaluation: &ProbeEvaluation) -> KnowledgeQualityReport {
    let mut fact_items: Vec<&ItemEvaluation> = Vec::new();
    for v in &evaluation.variants {
        for item in &v.items {
            if item.dimension == "fact" && item.fact.is_some() {
                fact_items.push(item);
            }
        }
    }

    let sample_count = fact_items.len();
    if sample_count == 0 {
        return KnowledgeQualityReport {
            sample_count: 0,
            fact_hit_count: 0,
            false_positive_rate: 0.0,
            false_negative_rate: 0.0,
            miss_target_met: false,
            annotation: "无事实维样本，无法评估知识层质量".to_string(),
        };
    }

    let fact_hit_count = fact_items
        .iter()
        .filter(|i| i.fact.as_ref().map(|f| f.score >= 0.5).unwrap_or(false))
        .count();

    // 误报：判定注入但回复未覆盖事实（score 低）——近似为 score < 0.3
    let false_positive = fact_items
        .iter()
        .filter(|i| i.fact.as_ref().map(|f| f.score < 0.3).unwrap_or(false))
        .count();

    // 漏报：回复未充分回答事实问题（score < 0.4 视为信息不足）
    let false_negative = fact_items
        .iter()
        .filter(|i| i.fact.as_ref().map(|f| f.score < 0.4).unwrap_or(false))
        .count();

    let false_positive_rate = false_positive as f64 / sample_count as f64;
    let false_negative_rate = false_negative as f64 / sample_count as f64;
    let miss_target_met = false_negative_rate < 0.10;

    let annotation = if miss_target_met {
        format!(
            "漏报率 {:.1}% 达标（<10%）；误报率 {:.1}%",
            false_negative_rate * 100.0,
            false_positive_rate * 100.0
        )
    } else {
        format!(
            "漏报率 {:.1}% 未达标（目标 <10%）；误报率 {:.1}%；需优化知识抽取或检索召回",
            false_negative_rate * 100.0,
            false_positive_rate * 100.0
        )
    };

    KnowledgeQualityReport {
        sample_count,
        fact_hit_count,
        false_positive_rate,
        false_negative_rate,
        miss_target_met,
        annotation,
    }
}

// =========================================================
// 报告输出辅助
// =========================================================

/// 写 JSON 报告到文件。
fn write_report_json(out: &str, report: &ProbeReport) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(report).context("报告 JSON 序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n")).with_context(|| format!("写入报告失败: {out}"))?;
    }
    Ok(())
}

/// 写 markdown 报告到文件。
fn write_report_markdown(out: &str, report: &ProbeReport) -> anyhow::Result<()> {
    let md = render_report_markdown(report);
    if out == "-" {
        print!("{md}");
    } else {
        std::fs::write(out, md).with_context(|| format!("写入报告失败: {out}"))?;
    }
    Ok(())
}

/// 渲染 markdown 报告（档位对比表 + 定稿建议 + 校准 + 知识层质量）。
fn render_report_markdown(report: &ProbeReport) -> String {
    let mut md = String::new();
    md.push_str("# Ramaria 探针档位对比报告\n\n");
    md.push_str(&format!("- persona: `{}`\n", report.persona_uid));
    md.push_str(&format!("- 数据集 seed: {}\n", report.dataset_seed));
    md.push_str(&format!(
        "- 语气 judge: {} / 事实 embedding: {}\n",
        report.judge_used, report.embedding_used
    ));
    md.push_str(&format!("- 生成时间: {}\n\n", report.generated_at));

    // 档位对比表
    md.push_str("## 档位评分对比\n\n");
    md.push_str("| 档位 | 事实维 | 语气维 | 情感维 | 成功/总 | 失败 | 说明 |\n");
    md.push_str("|------|:---:|:---:|:---:|:---:|:---:|------|\n");
    for r in &report.variants {
        let fact = r
            .fact_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let tone = r
            .tone_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let emotion = r
            .emotion_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        md.push_str(&format!(
            "| {} | {} | {} | {} | {}/{} | {} | {} |\n",
            r.variant_id,
            fact,
            tone,
            emotion,
            r.success_count,
            r.total_count,
            r.failed_count,
            r.description.replace('|', "\\|")
        ));
    }
    md.push('\n');

    // 定稿建议
    md.push_str("## 定稿建议\n\n");
    for d in &report.recommendation.per_dimension {
        md.push_str(&format!(
            "**{}**：{}（最佳档位 {}）\n\n",
            d.dimension,
            d.reason,
            d.best_variant.as_deref().unwrap_or("—")
        ));
    }
    md.push_str(&format!("**综合**：{}\n\n", report.recommendation.overall));

    // 人工抽检校准
    if let Some(c) = &report.calibration {
        md.push_str("## 人工抽检校准\n\n");
        md.push_str(&format!(
            "- 抽检样本：{}/{}（{:.0}%）\n",
            c.sample_count,
            c.total_count,
            c.sample_rate * 100.0
        ));
        md.push_str(&format!(
            "- 同分一致性：{:.0}%\n",
            c.consistency_exact * 100.0
        ));
        md.push_str(&format!("- 平均绝对差：{:.2}\n", c.mean_abs_diff));
        md.push_str(&format!("- 偏差（judge−人工）：{:.2}\n", c.bias));
        if let Some(coef) = c.calibrated_coefficient {
            md.push_str(&format!("- 校准系数：{:.3}\n", coef));
        }
        md.push_str(&format!("- 标注：{}\n\n", c.annotation));
    }

    // 知识层质量
    if let Some(kq) = &report.knowledge_quality {
        md.push_str("## 知识层抽取质量评估\n\n");
        md.push_str(&format!("- 样本数：{}（事实维）\n", kq.sample_count));
        md.push_str(&format!("- 事实命中：{}\n", kq.fact_hit_count));
        md.push_str(&format!(
            "- 误报率：{:.1}%\n",
            kq.false_positive_rate * 100.0
        ));
        md.push_str(&format!(
            "- 漏报率：{:.1}%（目标 <10% → {})\n",
            kq.false_negative_rate * 100.0,
            if kq.miss_target_met {
                "达标"
            } else {
                "未达标"
            }
        ));
        md.push_str(&format!("- 结论：{}\n\n", kq.annotation));
    }

    // 消融对比统计
    if let Some(ab) = &report.ablation {
        md.push_str("## 消融对比统计\n\n");
        md.push_str(&format!("- 基线档位：`{}`\n", ab.baseline_variant));
        md.push_str(
            "- 方法：按题目配对 Wilcoxon 符号秩检验 + Cohen's d + 95% CI；\
             多比较经 Benjamini–Hochberg FDR 校正\n",
        );
        md.push_str("- 判定线：p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0\n\n");
        md.push_str("| 消融档位 | 维度 | 基线 | 消融后 | Δ | p_fdr | d | 95%CI | 判定 |\n");
        md.push_str("|------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|------|\n");
        for r in &ab.rows {
            md.push_str(&format!(
                "| {} | {} | {:.3} | {:.3} | {:.3} | {:.4} | {:.2} | [{:.3}, {:.3}] | {}(n={}) |\n",
                r.ablation_variant,
                r.dimension,
                r.base_mean,
                r.ablated_mean,
                r.mean_diff,
                r.p_fdr,
                r.cohens_d,
                r.ci95_low,
                r.ci95_high,
                match (r.significant, r.direction.as_str()) {
                    (true, "down") => "↓ 移除后下降（层有贡献）",
                    (true, "up") => "↑ 差异显著",
                    _ => "→ 无差异",
                },
                r.n_pairs
            ));
        }
        md.push('\n');
        md.push_str("### 辅助指标\n\n");
        md.push_str("| 档位 | 平均回复(字符) | 平均耗时(ms) | 空回复率 | 成功/总 |\n");
        md.push_str("|------|:---:|:---:|:---:|:---:|\n");
        for a in &ab.aux {
            md.push_str(&format!(
                "| {} | {:.1} | {:.1} | {:.1}% | {}/{} |\n",
                a.variant_id,
                a.reply_chars_mean,
                a.elapsed_ms_mean,
                a.empty_reply_rate * 100.0,
                a.success_count,
                a.total_count
            ));
        }
        md.push('\n');
    }

    md.push_str("---\n*由 `ramaria probe report` 自动生成，供 M5 定稿实验参考。*\n");
    md
}

/// 文本模式打印报告摘要（stdout 只输出数据）。
fn print_report_summary(report: &ProbeReport) {
    println!(
        "探针报告: persona={} | {} 档位对比 | judge={} | embedding={}",
        report.persona_uid,
        report.variants.len(),
        report.judge_used,
        report.embedding_used
    );
    for r in &report.variants {
        let fact = r
            .fact_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let tone = r
            .tone_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let emotion = r
            .emotion_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  档位 {:<14} 事实={:<6} 语气={:<6} 情感={:<6} 成功={}/{} — {}",
            r.variant_id, fact, tone, emotion, r.success_count, r.total_count, r.description
        );
    }
    println!("定稿建议: {}", report.recommendation.overall);
    if let Some(c) = &report.calibration {
        println!(
            "校准: 样本 {}/{} 同分 {:.0}% 偏差 {:.2} {}",
            c.sample_count,
            c.total_count,
            c.consistency_exact * 100.0,
            c.bias,
            if c.inconsistent {
                "⚠ 不一致"
            } else {
                "✓ 一致"
            }
        );
    }
    if let Some(kq) = &report.knowledge_quality {
        println!(
            "知识层: 误报 {:.1}% 漏报 {:.1}% {}",
            kq.false_positive_rate * 100.0,
            kq.false_negative_rate * 100.0,
            if kq.miss_target_met {
                "（达标）"
            } else {
                "（未达标）"
            }
        );
    }
    if let Some(ab) = &report.ablation {
        let sig = ab.rows.iter().filter(|r| r.significant).count();
        println!(
            "消融对比: {} 行对比（基线 {}），显著 {} 行",
            ab.rows.len(),
            ab.baseline_variant,
            sig
        );
    }
    crate::ui::info("用 --output 生成 markdown/JSON 报告文件");
}
