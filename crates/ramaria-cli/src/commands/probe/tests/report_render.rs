//! crates/ramaria-cli/src/commands/probe/tests/report_render.rs - 探针 probe 报告辅助指标与渲染 单元测试
//!
//! 设计特点:
//! - 辅助指标四件套的产物可复算近似
//! - markdown 渲染分节（I/S 列、局限声明、辅助指标）覆盖

use super::super::evaluate::FactItemScore;
use super::super::evaluate::ItemEvaluation;
use super::super::evaluate::ProbeEvaluation;
use super::super::evaluate::VariantEvaluation;
use super::super::report::KnowledgeJudgeRates;
use super::super::report::KnowledgeQualityScope;
use super::super::report::compute_auxiliary_metrics;
use super::super::*;

/// 构造带 fact/emotion 明细与跨轮聚合的评分数值档位（辅助指标测试用）。
fn eval_variant_mixed() -> VariantEvaluation {
    use super::super::evaluate::{DimensionScoreAgg, EmotionItemScore};
    let item =
        |id: &str, dim: &str, fact: Option<f64>, emo: Option<EmotionItemScore>| -> ItemEvaluation {
            ItemEvaluation {
                item_id: id.to_string(),
                dimension: dim.to_string(),
                question: "q".to_string(),
                reference: None,
                reply_preview: String::new(),
                fact: fact.map(|score| FactItemScore {
                    cosine: Some(score),
                    keyword_hit: score,
                    score,
                    keyword_hit_norm: Some(score),
                    fact_point: Some(score),
                    score_norm: Some(score),
                    score_point: Some(score),
                }),
                tone: None,
                emotion: emo,
                error: None,
            }
        };
    let items = vec![
        // fact 两条：0.9 可追溯 / 0.2 不可追溯 → 可追溯率 0.5
        item("fact-0001", "fact", Some(0.9), None),
        item("fact-0002", "fact", Some(0.2), None),
        // emotion 两条：恰当 1.0（负面情境） / 不当 0.0（正面情境）→ 命中 0.5、误用 0.5
        item(
            "emotion-0001",
            "emotion",
            None,
            Some(EmotionItemScore {
                score: 1.0,
                situation_negative: true,
                situation_positive: false,
                marker_hit: 3,
            }),
        ),
        item(
            "emotion-0002",
            "emotion",
            None,
            Some(EmotionItemScore {
                score: 0.0,
                situation_negative: false,
                situation_positive: true,
                marker_hit: 0,
            }),
        ),
    ];
    VariantEvaluation {
        variant_id: "v1".to_string(),
        description: "d".to_string(),
        params: VariantParams {
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
        },
        fact_score: None,
        fact_score_norm: None,
        fact_score_point: None,
        tone_score: None,
        emotion_score: None,
        dimension_scores: Some(vec![
            DimensionScoreAgg {
                dimension: "fact".to_string(),
                mean: 0.5,
                std: 0.1,
                ci95_low: 0.4,
                ci95_high: 0.6,
                n: 3,
            },
            DimensionScoreAgg {
                dimension: "emotion".to_string(),
                mean: 0.5,
                std: 0.2,
                ci95_low: 0.3,
                ci95_high: 0.7,
                n: 3,
            },
        ]),
        failed_count: 0,
        items,
    }
}

/// 四件套计算：可追溯率 / 规则命中 / 路由误用 / 画像回归均值可从构造产物复算。
#[test]
fn auxiliary_metrics_recomputable_from_product() {
    let evaluation = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: true,
        generated_at: "t".into(),
        variants: vec![eval_variant_mixed()],
    };
    let m = compute_auxiliary_metrics(&evaluation);

    // 证据链可追溯率：2 条 fact 中 1 条 score≥0.5 → 0.5
    let t = m.evidence_traceability_rate.expect("有 fact 题应可算");
    assert!((t - 0.5).abs() < 1e-9, "可追溯率应 0.5，实际 {t}");
    // 行为规则命中率：2 条 emotion 中 1 条恰当 → 0.5
    let h = m.behavior_rule_hit_rate.expect("有 emotion 题应可算");
    assert!((h - 0.5).abs() < 1e-9, "规则命中率应 0.5，实际 {h}");
    // 情境路由误用率：2 条有极性中 1 条 0 分 → 0.5
    let u = m.situation_route_misuse_rate.expect("有极性样本应可算");
    assert!((u - 0.5).abs() < 1e-9, "路由误用率应 0.5，实际 {u}");
    // 画像回归：档位跨轮 std 均值 = (0.1+0.2)/2 = 0.15
    let p = m
        .profile_regression_output_stability
        .expect("有 dimension_scores 应可算");
    assert!(
        (p - 0.15).abs() < 1e-9,
        "画像回归 std 均值应 0.15，实际 {p}"
    );
    assert!(m.annotation.contains("可复算"), "annotation 应说明口径");
}

/// 画像回归口径固定为 fact/tone/emotion 三维：新增事实维重算维度（fact_norm /
/// fact_point）不计入，故 `dimension_scores` 额外含这两维时数值不变。
#[test]
fn profile_regression_ignores_fact_recalc_dimensions() {
    use super::super::evaluate::DimensionScoreAgg;

    let agg = |dim: &str, std: f64| DimensionScoreAgg {
        dimension: dim.to_string(),
        mean: 0.5,
        std,
        ci95_low: 0.4,
        ci95_high: 0.6,
        n: 3,
    };
    let variant = |dims: Vec<DimensionScoreAgg>| VariantEvaluation {
        variant_id: "v1".to_string(),
        description: "d".to_string(),
        params: VariantParams {
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
        },
        fact_score: None,
        fact_score_norm: None,
        fact_score_point: None,
        tone_score: None,
        emotion_score: None,
        dimension_scores: Some(dims),
        failed_count: 0,
        items: Vec::new(),
    };
    let eval_with = |dims: Vec<DimensionScoreAgg>| ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![variant(dims)],
    };

    let base = eval_with(vec![
        agg("fact", 0.1),
        agg("tone", 0.2),
        agg("emotion", 0.3),
    ]);
    // 额外插入两个重算维度（std 明显不同），验证不参与既有口径的均值。
    let with_recalc = eval_with(vec![
        agg("fact", 0.1),
        agg("fact_norm", 9.0),
        agg("fact_point", 9.0),
        agg("tone", 0.2),
        agg("emotion", 0.3),
    ]);

    let base_std = compute_auxiliary_metrics(&base)
        .profile_regression_output_stability
        .expect("三维应有画像回归值");
    let recalc_std = compute_auxiliary_metrics(&with_recalc)
        .profile_regression_output_stability
        .expect("三维应有画像回归值");
    assert!(
        (base_std - 0.2).abs() < 1e-12,
        "三维 std 均值应为 (0.1+0.2+0.3)/3=0.2，实际 {base_std}"
    );
    assert!(
        (recalc_std - base_std).abs() < 1e-12,
        "新增事实维重算维度不应改变画像回归口径：{recalc_std} vs {base_std}"
    );
}

/// 空评分数值（无 fact/emotion/聚合）→ 各指标 None（标注缺项而非报错）。
#[test]
fn auxiliary_metrics_empty_variants_all_none() {
    let evaluation = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![],
    };
    let m = compute_auxiliary_metrics(&evaluation);
    assert!(m.evidence_traceability_rate.is_none());
    assert!(m.behavior_rule_hit_rate.is_none());
    assert!(m.situation_route_misuse_rate.is_none());
    assert!(m.profile_regression_output_stability.is_none());
    assert!(!m.annotation.is_empty());
}

/// markdown 渲染快照断言（I/S 分栏 + 局限字段必出 + 辅助指标节）。
///
/// 构造一个带消融报告（含 I/S 行）与局限/辅助指标的 `ProbeReport`，
/// 断言渲染文本包含三类对照小节、净增量/替代标注与局限声明节。
#[test]
fn render_report_markdown_sections_cover_i_s_columns_and_limitations() {
    use super::super::report::{
        AblationComparisonRow, AblationReport, AuxiliaryMetrics, KnowledgeQualityReport,
        ProbeReport, Recommendation, VariantAuxMetrics, VariantStyleMetrics,
    };
    // 手工构造最小报告（重点校验渲染分段，不依赖完整评分明细）。
    let report = ProbeReport {
        results_file: "r.json".into(),
        evaluation_file: Some("e.json".into()),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![],
        recommendation: Recommendation {
            per_dimension: vec![],
            overall: "无".into(),
        },
        calibration: None,
        knowledge_quality: Some(KnowledgeQualityReport {
            primary: KnowledgeQualityScope {
                scope: "memory_injected".to_string(),
                description: "主口径：含记忆注入档位".to_string(),
                variant_ids: vec!["B1".to_string()],
                sample_count: 1,
                fact_hit_count: 1,
                false_positive_rate: 0.0,
                false_negative_rate: 0.0,
                miss_target_met: true,
                judge_rates: vec![
                    KnowledgeJudgeRates {
                        judge: "legacy".to_string(),
                        sample_count: 1,
                        hit_rate: 1.0,
                        false_positive_rate: 0.0,
                        false_negative_rate: 0.0,
                        miss_target_met: true,
                    },
                    KnowledgeJudgeRates {
                        judge: "norm".to_string(),
                        sample_count: 1,
                        hit_rate: 1.0,
                        false_positive_rate: 0.0,
                        false_negative_rate: 0.0,
                        miss_target_met: true,
                    },
                    KnowledgeJudgeRates {
                        judge: "point".to_string(),
                        sample_count: 1,
                        hit_rate: 1.0,
                        false_positive_rate: 0.0,
                        false_negative_rate: 0.0,
                        miss_target_met: true,
                    },
                ],
            },
            pooled: KnowledgeQualityScope {
                scope: "pooled_all".to_string(),
                description: "对照口径：全部档位池化".to_string(),
                variant_ids: vec!["B1".to_string(), "B0".to_string()],
                sample_count: 2,
                fact_hit_count: 1,
                false_positive_rate: 0.0,
                false_negative_rate: 0.5,
                miss_target_met: false,
                judge_rates: vec![
                    KnowledgeJudgeRates {
                        judge: "legacy".to_string(),
                        sample_count: 2,
                        hit_rate: 0.5,
                        false_positive_rate: 0.0,
                        false_negative_rate: 0.5,
                        miss_target_met: false,
                    },
                    KnowledgeJudgeRates {
                        judge: "norm".to_string(),
                        sample_count: 0,
                        hit_rate: 0.0,
                        false_positive_rate: 0.0,
                        false_negative_rate: 0.0,
                        miss_target_met: false,
                    },
                ],
            },
            annotation: "双口径说明".to_string(),
        }),
        ablation: Some(AblationReport {
            baseline_variant: "B1".into(),
            rows: vec![
                AblationComparisonRow {
                    ablation_variant: "F1".into(),
                    description: "移除".into(),
                    comparison_type: "removal".into(),
                    base_variant: "F0".into(),
                    dimension: "fact".into(),
                    n_pairs: 5,
                    base_mean: 0.8,
                    ablated_mean: 0.4,
                    mean_diff: -0.4,
                    wilcoxon_p: 0.01,
                    p_fdr: 0.02,
                    cohens_d: 0.9,
                    cohens_d_pooled: 0.88,
                    equiv_bound: 0.09,
                    tost_p: 0.41,
                    equivalent: false,
                    verdict: "significant_down".into(),
                    ci95_low: -0.7,
                    ci95_high: -0.1,
                    significant: true,
                    direction: "down".into(),
                    annotation: "移除显著".into(),
                },
                AblationComparisonRow {
                    ablation_variant: "S_behavior".into(),
                    description: "替代".into(),
                    comparison_type: "substitution".into(),
                    base_variant: "B1".into(),
                    dimension: "fact".into(),
                    n_pairs: 5,
                    base_mean: 0.4,
                    ablated_mean: 0.8,
                    mean_diff: 0.4,
                    wilcoxon_p: 0.01,
                    p_fdr: 0.02,
                    cohens_d: 0.9,
                    cohens_d_pooled: 0.88,
                    equiv_bound: 0.09,
                    tost_p: 0.44,
                    equivalent: false,
                    verdict: "significant_up".into(),
                    ci95_low: 0.1,
                    ci95_high: 0.7,
                    significant: true,
                    direction: "up".into(),
                    annotation: "替代对照显著".into(),
                },
                AblationComparisonRow {
                    ablation_variant: "I_behavior".into(),
                    description: "净增量".into(),
                    comparison_type: "increment".into(),
                    base_variant: "B1".into(),
                    dimension: "fact".into(),
                    n_pairs: 5,
                    base_mean: 0.4,
                    ablated_mean: 0.75,
                    mean_diff: 0.35,
                    wilcoxon_p: 0.01,
                    p_fdr: 0.02,
                    cohens_d: 0.9,
                    cohens_d_pooled: 0.86,
                    equiv_bound: 0.08,
                    tost_p: 0.46,
                    equivalent: false,
                    verdict: "significant_up".into(),
                    ci95_low: 0.1,
                    ci95_high: 0.6,
                    significant: true,
                    direction: "up".into(),
                    annotation: "净增量显著".into(),
                },
            ],
            aux: vec![VariantAuxMetrics {
                variant_id: "B1".into(),
                description: "基线".into(),
                reply_chars_mean: 80.0,
                elapsed_ms_mean: 1000.0,
                empty_reply_rate: 0.0,
                success_count: 30,
                total_count: 30,
            }],
            judgment_dimensions: vec!["fact".to_string(), "tone".to_string()],
            descriptive_dimensions: vec!["emotion".to_string()],
            dimension_scope_note: "情感维未校准".to_string(),
            equivalence_note: "等效性检验：TOST（双单侧 t 检验），等效边界取 |d_av|=0.3；\
                tost_p<0.05 判定「等效（无实质净增量）」。"
                .to_string(),
        }),
        limitations: vec![
            "外部效度局限：基于单 persona".into(),
            "统计法样本：单次运行".into(),
        ],
        descriptive_metrics: vec!["情感维口径未校准，为描述性指标".to_string()],
        auxiliary: AuxiliaryMetrics {
            evidence_traceability_rate: Some(0.5),
            behavior_rule_hit_rate: Some(0.5),
            situation_route_misuse_rate: Some(0.5),
            profile_regression_output_stability: Some(0.15),
            annotation: "产物可复算近似".into(),
        },
        style_metrics: vec![VariantStyleMetrics {
            variant_id: "B1".into(),
            description: "基线".into(),
            reply_count: 30,
            len_mean: 23.1,
            len_median: 22.0,
            len_le_30_rate: 0.8,
            len_ref_overlap: Some(0.567),
            tone_particle_rate: 0.878,
            question_rate: 0.122,
            exclaim_rate: 0.0,
            repeat_rate: 0.0,
            assistant_marker_rate: 0.011,
            ref_len_mean: Some(15.9),
        }],
    };
    let md = super::super::report::render_report_markdown(&report);

    // 三类对照小节标题分栏
    assert!(md.contains("移除对照（F 组 vs F0）"), "应渲染移除对照小节");
    assert!(md.contains("替代对照（S 组 vs B1）"), "应渲染替代对照小节");
    assert!(
        md.contains("净增量对照（I 组 vs B1）"),
        "应渲染净增量对照小节"
    );
    assert!(md.contains("F1"), "移除行应出现");
    assert!(md.contains("S_behavior"), "替代行应出现");
    assert!(md.contains("I_behavior"), "净增量行应出现");
    assert!(md.contains("等效性检验"), "消融小节应渲染等效性口径说明");
    // 局限声明节必出（含两条局限文本）
    assert!(md.contains("数据特性与外部效度局限"), "局限节必出");
    assert!(md.contains("基于单 persona"), "单 persona 局限文本应出现");
    assert!(md.contains("统计法样本"), "repeat 局限文本应出现");
    // 辅助指标四件套节（渲染文本统一标注代理口径）
    assert!(md.contains("辅助指标（产物可复算）"), "辅助指标节必出");
    for label in [
        "证据链可追溯率（代理口径）",
        "行为规则命中率（代理口径）",
        "情境路由误用率（代理口径）",
        "画像回归（代理口径",
    ] {
        assert!(md.contains(label), "辅助指标渲染缺少代理口径标注: {label}");
    }
    assert!(
        md.contains("描述性指标（不参与层价值判定）"),
        "描述性指标小节必出"
    );
    assert!(
        md.contains("知识层抽取质量评估（双口径）"),
        "知识层双口径小节必出"
    );
    assert!(md.contains("| legacy |"), "知识层判据分栏应含 legacy 行");
    assert!(md.contains("| norm |"), "知识层判据分栏应含 norm 行");
    assert!(md.contains("| point |"), "知识层判据分栏应含 point 行");
    // 客观风格形态指标节
    assert!(
        md.contains("风格形态指标（客观口径，对照语气 judge）"),
        "风格形态指标节必出"
    );
    assert!(md.contains("参考重合"), "长度重合度列应出现");
    assert!(md.contains("助手腔"), "助手腔列应出现");
}

// =========================================================
// 测量口径收口：情感维描述性降级 / 知识层双口径 / run 有效性自检
// =========================================================
