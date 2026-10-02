//! crates/ramaria-cli/src/commands/probe/report/render.rs - 探针 report 报告装配与建议
//!
//! 设计特点:
//! - 汇总各档位评分生成对比表，给出每维最佳档位与综合定稿建议
//! - 报告数据结构（ProbeReport / VariantReportRow / 建议与消融容器）集中在此
//! - run_report 负责读取输入、装配报告、按输出模式分发（stdout / 文件）
//! - 局限声明与建议构建为纯函数，便于单元测试

use super::super::evaluate::ProbeEvaluation;
use super::super::evaluate::read_experiment;
use super::super::now_iso8601;
use super::super::types::ProbeExperiment;
use super::super::types::VariantParams;
use super::ablation::build_ablation_report;
use super::aux_metrics::{AuxiliaryMetrics, VariantAuxMetrics, compute_auxiliary_metrics};
use super::calibration::{CalibrationResult, compute_calibration, read_manual_scores};
use super::knowledge_quality::{KnowledgeQualityReport, assess_knowledge_quality};
use super::markdown::{print_report_summary, write_report_json, write_report_markdown};
use super::style_metrics::{EMOTION_DESCRIPTIVE_NOTE, VariantStyleMetrics, compute_style_metrics};
use ramaria_core::error::RamariaError;
use ramaria_service::Engine;
use std::path::Path;
use std::sync::Arc;

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
    /// 数据特性与外部效度局限声明（仅单 persona 高信号数据，
    /// 不做跨 persona 推广；judge/embedding 可用性等评估限制）。
    pub limitations: Vec<String>,
    /// 描述性指标（不参与层价值判定）的口径声明（必出）。
    pub descriptive_metrics: Vec<String>,
    /// 辅助指标四件套（证据链可追溯率 / 行为规则命中率 /
    /// 情境路由误用率 / 画像回归）。基于 run/eval 产物可复算的近似口径，
    /// 语义与局限见 `AuxiliaryMetrics.annotation`。
    pub auxiliary: AuxiliaryMetrics,
    /// 客观风格形态指标（对照语气 judge；无回复样本时为空）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub style_metrics: Vec<VariantStyleMetrics>,
}

/// 档位报告行（评分对比表）。
///
/// 字段约定:
/// - `fact_score`: 事实维旧口径（2-gram 覆盖率）均分，冻结不变以支持历史口径对照。
/// - `fact_score_norm` / `fact_score_point`: 事实维两个重算口径（长度归一 / 事实点），
///   与 `fact_score` 并排展示，便于在新旧判据下核对；旧产物或未评分时为 None。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VariantReportRow {
    pub variant_id: String,
    pub description: String,
    pub params: VariantParams,
    pub fact_score: Option<f64>,
    /// 事实维长度归一均分（0.0~1.0；旧产物或未评分时 None）
    pub fact_score_norm: Option<f64>,
    /// 事实维事实点均分（0.0~1.0；旧产物或未评分时 None）
    pub fact_score_point: Option<f64>,
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

/// 消融对比报告（`probe report --ablation`）。
///
/// 结构:
/// - `baseline_variant`: 主基线档位 id（优先 F0；仅含 S/I 组时为 B1）。
/// - `rows`: 消融 vs 基线的逐"消融档位 × 维度"统计判定行，
///   每行自带 `comparison_type`（removal / substitution / increment）与 `base_variant`，
///   供报告把三类对照分栏表述。
/// - `aux`: 参与对比各档位的辅助指标（回复长度/耗时/空回复率）。
///
/// 对照语义:
/// - removal（F 组，基线 F0）: 全开中逐层关闭 → 回答"去掉某一层的边际损失"；
/// - substitution（S 组，基线 B1）: 去 RAG 摘要、仅单专属层 → 回答"单层能否替代 RAG"；
/// - increment（I 组，基线 B1）: B1 基座 + 单专属层 → 回答"在 RAG 之上叠加一层的净增量"。
///
/// 判定线: `p_fdr < 0.05 ∧ |cohens_d| ≥ 0.3 ∧ CI 不含 0` → 显著；
/// 贡献方向见 `AblationComparisonRow.direction`。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AblationReport {
    /// 主基线档位 id（F0 或 B1）
    pub baseline_variant: String,
    /// 逐消融档位 × 维度统计判定
    pub rows: Vec<AblationComparisonRow>,
    /// 参与对比档位的辅助指标（mean ± CI / 空回复率）
    pub aux: Vec<VariantAuxMetrics>,
    /// 参与层价值判定的维度（事实维三口径 fact / fact_norm / fact_point + 语气维 tone）。
    pub judgment_dimensions: Vec<String>,
    /// 展示但不参与判定的描述性维度（情感维，口径未校准）。
    pub descriptive_dimensions: Vec<String>,
    /// 维度范围说明（必出）。
    pub dimension_scope_note: String,
    /// 等效性检验口径说明（必出）：TOST + 等效边界语义。
    pub equivalence_note: String,
}

/// 单条消融对比（某消融档位 × 某维度，按题目配对）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AblationComparisonRow {
    /// 消融档位 id（如 F1 / S_behavior / I_behavior）
    pub ablation_variant: String,
    /// 消融档位描述
    pub description: String,
    /// 对照类型：removal（F 组逐层移除 vs F0）/ substitution（S 组替代，去 RAG 摘要 vs B1）/
    /// increment（I 组净增量，B1 基座 + 单专属层 vs B1）。
    pub comparison_type: String,
    /// 实际对照基线档位 id（F 组为 F0；S/I 组为 B1）。
    pub base_variant: String,
    /// 维度（fact / fact_norm / fact_point / tone；emotion 为描述性维度，仅在报告中展示）
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
    /// 合并标准差标准化的 Cohen's d（d_av = mean(diff) / sd_av；sd_av = 两档位配对样本合并 SD）
    pub cohens_d_pooled: f64,
    /// TOST 等效边界（原始差分量纲；= 0.3 × sd_av，对应 |d_av| = 0.3）
    pub equiv_bound: f64,
    /// TOST 等效性检验 p 值（双单侧，t 分布 df = n_pairs − 1）
    pub tost_p: f64,
    /// 是否可判定等效（tost_p < 0.05）
    pub equivalent: bool,
    /// 综合判定：significant_up / significant_down / equivalent / inconclusive
    pub verdict: String,
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
pub(crate) async fn run_report(
    _engine: &Arc<Engine>,
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
            fact_score_norm: ev.and_then(|v| v.fact_score_norm),
            fact_score_point: ev.and_then(|v| v.fact_score_point),
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

    // 数据特性与外部效度局限声明（报告必出字段）：
    // 消融结论基于单 persona 高信号数据，不做跨 persona 推广。
    let judge_used = evaluation.as_ref().map(|e| e.judge_used).unwrap_or(false);
    let embedding_used = evaluation
        .as_ref()
        .map(|e| e.embedding_used)
        .unwrap_or(false);
    let limitations = build_limitations(&experiment, judge_used, embedding_used);
    // 描述性指标口径声明（必出）：情感维未校准，仅作展示、不参与层价值判定。
    let descriptive_metrics = vec![EMOTION_DESCRIPTIVE_NOTE.to_string()];

    // 辅助指标四件套：有评分数值时可复算；缺失时给出空指标 + 说明。
    let auxiliary = match evaluation.as_ref() {
        Some(ev) => compute_auxiliary_metrics(ev),
        None => AuxiliaryMetrics {
            evidence_traceability_rate: None,
            behavior_rule_hit_rate: None,
            situation_route_misuse_rate: None,
            profile_regression_output_stability: None,
            annotation: "未提供评分数值文件（--evaluation），辅助指标不可计算".to_string(),
        },
    };

    // 客观风格形态指标：仅依赖 run 产物（回复文本）+ eval 产物（persona 参考长度），
    // 用于对照区分力不足的短回复语气 judge。
    let style_metrics = compute_style_metrics(&experiment, evaluation.as_ref());

    let report = ProbeReport {
        results_file: results_path.display().to_string(),
        evaluation_file: evaluation_path.map(|p| p.display().to_string()),
        persona_uid: experiment.persona_uid.clone(),
        dataset_seed: experiment.dataset_seed,
        judge_used,
        embedding_used,
        generated_at: now_iso8601(),
        variants: rows,
        recommendation,
        calibration,
        knowledge_quality,
        ablation: ablation_report,
        limitations,
        descriptive_metrics,
        style_metrics,
        auxiliary,
    };

    // Step 7: 输出
    if let Some(out) = output {
        // `-` + --json：stdout 只出一行信封，原始报告放 data.raw（避免两段 JSON）
        if out == "-" && json {
            let data = serde_json::json!({
                "file": "-",
                "persona_uid": report.persona_uid,
                "variants": report.variants.len(),
                "calibration": report.calibration.is_some(),
                "knowledge_quality": report.knowledge_quality.is_some(),
                "raw": &report,
            });
            return crate::json::emit_ok(&data);
        }
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

/// 构建数据特性与外部效度局限声明（报告必出字段）。
///
/// 内容:
/// - 仅单 persona 高信号数据 → 效度结论仅限本数据范围，不做跨 persona 推广；
/// - 语气维 judge / 事实维 embedding 可用性影响维度覆盖；
/// - 采样规模（repeat 次数）决定统计法置信度。
pub(crate) fn build_limitations(
    experiment: &ProbeExperiment,
    judge_used: bool,
    embedding_used: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    // 外部效度边界：仅一份单人对单人记录，显式声明不推广跨 persona。
    out.push(format!(
        "外部效度局限：评估基于单 persona（{}）高信号数据，仅作 D2 高信号效度，\
         不做 D3 跨 persona 普遍性推广；结论不得外推为产品级普遍主张",
        experiment.persona_uid
    ));
    if !judge_used {
        out.push(
            "语气维缺失：本地 judge 不可用或未提供（tone 分空缺），语气维结论需人工抽检补足"
                .to_string(),
        );
    }
    if !embedding_used {
        out.push("事实维降级：embedding 不可用，事实维退化为关键词命中（无语义余弦）".to_string());
    }
    if let Some(rep) = &experiment.repeat {
        out.push(format!(
            "统计法样本：repeat=N={}，逐轮评分聚合 n 以实际有效轮数为准",
            rep.count
        ));
    } else {
        out.push("统计法样本：本次为单次运行（无 --repeat），结论未做多次配对统计".to_string());
    }
    out
}

/// 构建定稿建议（每维最佳档位 + 综合建议）。
pub(crate) fn build_recommendation(rows: &[VariantReportRow]) -> Recommendation {
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

    // 情感维口径未校准（描述性指标）：不给最佳档位建议，仅声明口径。
    per_dimension.push(DimensionRecommendation {
        dimension: "emotion".to_string(),
        best_variant: None,
        best_score: None,
        reason: EMOTION_DESCRIPTIVE_NOTE.to_string(),
    });

    // 综合建议：以层价值判定维度（事实维 + 语气维）为准，两者最佳档位一致 → 取该档位；
    // 否则提示需人工权衡。情感维为描述性指标，不参与一致性判定。
    let all_same = |best: Option<&VariantReportRow>, id: &str| {
        best.map(|r| r.variant_id == id).unwrap_or(false)
    };
    let overall = match fact_best {
        Some(f) if all_same(tone_best, &f.variant_id) => {
            format!(
                "综合建议档位 {}（事实/语气均最优）；需人工抽检校准后定稿",
                f.variant_id
            )
        }
        _ => "各维最佳档位不一致，需结合人工抽检与消融实验权衡取舍".to_string(),
    };

    Recommendation {
        per_dimension,
        overall,
    }
}
