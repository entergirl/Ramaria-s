//! crates/ramaria-cli/src/commands/probe/tests/report_stats.rs - 探针 probe 报告统计与知识层质量 单元测试
//!
//! 设计特点:
//! - 配对非参统计（Wilcoxon / Cohen's d / BH-FDR）与 TOST 等效性
//! - 消融对比报告组装与局限声明
//! - 知识层双口径质量评估与风格形态指标
//! - run 有效性自检与诊断字段向后兼容

use super::super::evaluate::FactItemScore;
use super::super::evaluate::ItemEvaluation;
use super::super::evaluate::ProbeEvaluation;
use super::super::evaluate::VariantEvaluation;
use super::super::report::KnowledgeQualityScope;
use super::super::report::bh_fdr_adjust;
use super::super::report::build_ablation_report;
use super::super::report::cohens_d_paired;
use super::super::report::cohens_d_pooled;
use super::super::report::erf_approx;
use super::super::report::normal_cdf;
use super::super::report::read_manual_scores;
use super::super::report::student_t_cdf;
use super::super::report::tost_equivalence;
use super::super::report::wilcoxon_signed_rank_p;
use super::super::run::run_validity;
use super::super::*;
use ramaria_core::error::RamariaError;
use std::path::Path;

#[test]
fn read_manual_scores_missing_file_is_validation_error() {
    let err =
        read_manual_scores(Path::new("/nonexistent/calib.json")).expect_err("校准文件缺失必须报错");
    let ramaria_err = err.downcast_ref::<RamariaError>();
    assert!(
        matches!(ramaria_err, Some(RamariaError::Validation { .. })),
        "校准文件缺失应归类为业务校验失败（exit 4），实际: {ramaria_err:?}"
    );
}

/// erf / 正态 CDF 关键值：cdf(0)=0.5，cdf(1.96)≈0.975。
#[test]
fn normal_cdf_key_values() {
    assert!((normal_cdf(0.0) - 0.5).abs() < 1e-9);
    assert!((normal_cdf(1.96) - 0.975).abs() < 0.005);
    assert!((normal_cdf(-1.96) - 0.025).abs() < 0.005);
    assert!((erf_approx(0.0)).abs() < 1e-9);
}

/// Wilcoxon：单向强效应 → p 小；符号混合 → p 大（接近 1 侧）。
#[test]
fn wilcoxon_signed_rank_directionality() {
    // 8 个全正差分（不同绝对值避免全结）→ 秩和显著偏离零
    let diffs: Vec<f64> = (1..=8).map(|i| i as f64 * 0.1).collect();
    let p_strong = wilcoxon_signed_rank_p(&diffs).expect("n≥5 应可检验");
    assert!(p_strong < 0.05, "单向效应 p 应小，实际 {p_strong}");
    // 正负各半抵消 → p 大
    let mixed = vec![0.2, -0.3, 0.4, -0.5, 0.6, -0.7];
    let p_mixed = wilcoxon_signed_rank_p(&mixed).expect("n≥5 应可检验");
    assert!(p_mixed > 0.1, "符号混合 p 应大，实际 {p_mixed}");
    // 样本过小（n<5）→ None
    assert!(wilcoxon_signed_rank_p(&[0.1, 0.2, 0.3]).is_none());
}

/// Cohen's d：零方差非零均值 → ±10 标记；零均值 → 0。
#[test]
fn cohens_d_edge_cases() {
    assert_eq!(cohens_d_paired(&[1.0, 1.0, 1.0, 1.0]), 10.0);
    assert_eq!(cohens_d_paired(&[-0.5, -0.5]), -10.0);
    assert_eq!(cohens_d_paired(&[1.0, -1.0]), 0.0);
    assert!((cohens_d_paired(&[1.0, 2.0]) - 2.121).abs() < 0.01);
    assert_eq!(cohens_d_paired(&[]), 0.0);
}

/// 学生氏 t 分布 CDF 关键值（与 t 表比对）。
#[test]
fn student_t_cdf_key_values() {
    assert!((student_t_cdf(0.0, 29.0) - 0.5).abs() < 1e-6);
    // 单侧 0.025 分位：t(29, 0.975) = 2.045
    assert!((student_t_cdf(2.045, 29.0) - 0.975).abs() < 5e-4);
    assert!((student_t_cdf(-2.045, 29.0) - 0.025).abs() < 5e-4);
    // 大样本趋近正态
    assert!((student_t_cdf(1.96, 1_000_000.0) - 0.975).abs() < 5e-3);
    // 退化输入不 panic
    assert!((student_t_cdf(f64::NAN, 10.0) - 0.5).abs() < 1e-9);
    assert!((student_t_cdf(1.0, 0.0) - 0.5).abs() < 1e-9);
}

/// TOST：近零效应 → 可判定等效；大效应 → 拒绝等效。
///
/// 关键对照：同一份"近零效应"数据在显著性框架下只能得到"不显著"，
/// 只有 TOST 才能给出"等效（无实质净增量）"结论——这正是该检验的用途。
#[test]
fn tost_declares_equivalence_for_near_zero_effect() {
    // 两档位分数几乎一致：得分本身宽幅分布（合并 SD 大），差分仅为 ±0.01 级微扰，
    // 即"零净增量"的典型形态；等效边界（0.3×合并SD）远大于差分抽样误差。
    let base: Vec<f64> = (0..30).map(|i| 0.2 + (i % 10) as f64 * 0.08).collect();
    let ablated: Vec<f64> = base
        .iter()
        .enumerate()
        .map(|(i, b)| b + if i % 3 == 0 { 0.01 } else { -0.005 })
        .collect();
    let diffs: Vec<f64> = ablated.iter().zip(&base).map(|(a, b)| a - b).collect();
    let t = tost_equivalence(&diffs, &base, &ablated, 0.3).expect("n≥2 应可检验");
    assert!(t.p < 0.05, "近零效应应判定等效，实际 tost_p={}", t.p);
    assert!(t.equivalent);
    assert!(t.bound > 0.0);
    // 显著性框架下同一数据只能得到"不显著"（符号混合）
    assert!(
        wilcoxon_signed_rank_p(&diffs).expect("n≥5 应可检验") > 0.05,
        "该数据不应出现显著差异"
    );
    // 同集合（差分恒 0）→ 合并 SD 口径的 d 为 0
    assert_eq!(cohens_d_pooled(&base, &base), 0.0);

    // 大效应：ablated 系统性高于 base（差值 ≈0.5，远超等效边界）
    let ablated_big: Vec<f64> = base
        .iter()
        .enumerate()
        .map(|(i, b)| b + 0.5 + (i % 4) as f64 * 0.01)
        .collect();
    let diffs_big: Vec<f64> = ablated_big.iter().zip(&base).map(|(a, b)| a - b).collect();
    let t2 = tost_equivalence(&diffs_big, &base, &ablated_big, 0.3).expect("n≥2 应可检验");
    assert!(t2.p > 0.05, "大效应不应判定等效，实际 tost_p={}", t2.p);
    assert!(!t2.equivalent);

    // 样本不足 → None（调用方按不可判定处理）
    assert!(tost_equivalence(&[0.1], &[0.0], &[0.1], 0.3).is_none());
    // 差分为常数（sd=0）→ 无抽样波动，不做 t 检验
    assert!(tost_equivalence(&[0.5; 10], &[0.0; 10], &[0.5; 10], 0.3).is_none());
}

/// BH FDR：单调校正且首尾正确。
#[test]
fn bh_fdr_adjust_monotonic() {
    let p = vec![0.01, 0.04, 0.2];
    let q = bh_fdr_adjust(&p);
    // 预期: [0.03, 0.06, 0.2]
    assert!((q[0] - 0.03).abs() < 1e-12);
    assert!((q[1] - 0.06).abs() < 1e-12);
    assert!((q[2] - 0.2).abs() < 1e-12);
    // 空输入
    assert!(bh_fdr_adjust(&[]).is_empty());
}

/// 构造一个合成评分数值档位（纯 fact 维度，给定逐题分数）。
fn eval_variant_scores(id: &str, scores: &[f64]) -> VariantEvaluation {
    let items = scores
        .iter()
        .enumerate()
        .map(|(i, s)| ItemEvaluation {
            item_id: format!("fact-{:04}", i + 1),
            dimension: "fact".to_string(),
            question: String::new(),
            reference: None,
            reply_preview: String::new(),
            fact: Some(FactItemScore {
                cosine: Some(*s),
                keyword_hit: *s,
                score: *s,
                keyword_hit_norm: Some(*s),
                fact_point: Some(*s),
                score_norm: Some(*s),
                score_point: Some(*s),
            }),
            tone: None,
            emotion: None,
            error: None,
        })
        .collect();
    VariantEvaluation {
        variant_id: id.to_string(),
        description: format!("{id} 档位"),
        params: VariantParams {
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: Some(id.to_string()),
        },
        fact_score: None,
        fact_score_norm: None,
        fact_score_point: None,
        tone_score: None,
        emotion_score: None,
        dimension_scores: None,
        failed_count: 0,
        items,
    }
}

/// 集成：F0（高分）vs F1（同题低分）→ F1/fact 行显著且方向 down。
#[test]
fn build_ablation_report_marks_removal_effect() {
    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![
            eval_variant_scores("F0", &[0.9, 0.9, 0.9, 0.9, 0.9]),
            eval_variant_scores("F1", &[0.5, 0.5, 0.5, 0.5, 0.5]),
        ],
    };
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: None,
        generated_at: "t".into(),
    };
    let report = build_ablation_report(&exp, &eval);
    assert_eq!(report.baseline_variant, "F0");
    let row = report
        .rows
        .iter()
        .find(|r| r.ablation_variant == "F1" && r.dimension == "fact")
        .expect("应有 F1/fact 行");
    assert_eq!(row.n_pairs, 5);
    assert!(row.significant, "F1 移除行为层后应显著下降");
    assert_eq!(row.direction, "down");
    assert!(row.mean_diff < 0.0);
    assert!(row.p_fdr < 0.05);
    assert!(row.ci95_high < 0.0, "CI 不含 0");

    // 事实维两个重算口径同样纳入对照，供新旧判据分栏核对。
    let dims_of_f1: Vec<&str> = report
        .rows
        .iter()
        .filter(|r| r.ablation_variant == "F1")
        .map(|r| r.dimension.as_str())
        .collect();
    assert!(
        dims_of_f1.contains(&"fact_norm"),
        "F1 应含 fact_norm 行，实际 {dims_of_f1:?}"
    );
    assert!(
        dims_of_f1.contains(&"fact_point"),
        "F1 应含 fact_point 行，实际 {dims_of_f1:?}"
    );
}

/// S 组：B1（低分基座）vs S_behavior（高分单层）→ up 方向，类型=替代对照。
#[test]
fn build_ablation_report_s_group_positive() {
    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![
            eval_variant_scores("B1", &[0.4, 0.4, 0.4, 0.4, 0.4]),
            eval_variant_scores("S_behavior", &[0.8, 0.8, 0.8, 0.8, 0.8]),
        ],
    };
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: None,
        generated_at: "t".into(),
    };
    let report = build_ablation_report(&exp, &eval);
    assert_eq!(report.baseline_variant, "B1");
    let row = report
        .rows
        .iter()
        .find(|r| r.ablation_variant == "S_behavior" && r.dimension == "fact")
        .expect("应有 S_behavior/fact 行");
    assert!(row.significant, "S_behavior 单层注入应显著正向");
    assert_eq!(row.direction, "up");
    assert!(row.mean_diff > 0.0);
    assert_eq!(row.comparison_type, "substitution", "S 组应标注为替代对照");
    assert_eq!(row.base_variant, "B1", "S 组基线为 B1");
}

/// I 组（净增量对照）：B1（低分基座）vs I_behavior（B1 基座 + 行为层，高分）
/// → up 方向、comparison_type=increment（与 S 组替代对照可区分）。
#[test]
fn build_ablation_report_i_group_marks_increment() {
    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![
            eval_variant_scores("B1", &[0.4, 0.4, 0.4, 0.4, 0.4]),
            eval_variant_scores("I_behavior", &[0.75, 0.75, 0.75, 0.75, 0.75]),
            eval_variant_scores("I_narrative", &[0.3, 0.3, 0.3, 0.3, 0.3]),
        ],
    };
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: None,
        generated_at: "t".into(),
    };
    let report = build_ablation_report(&exp, &eval);

    let ib = report
        .rows
        .iter()
        .find(|r| r.ablation_variant == "I_behavior" && r.dimension == "fact")
        .expect("应有 I_behavior/fact 行");
    assert_eq!(ib.comparison_type, "increment", "I 组应标注为净增量对照");
    assert_eq!(ib.base_variant, "B1", "I 组基线为 B1");
    assert!(ib.significant, "I_behavior 叠加应显著正向");
    assert_eq!(ib.direction, "up");
    assert!(ib.mean_diff > 0.0, "B1 基座 + 行为层高于 B1 → 净增为正");

    let inn = report
        .rows
        .iter()
        .find(|r| r.ablation_variant == "I_narrative" && r.dimension == "fact")
        .expect("应有 I_narrative/fact 行");
    assert_eq!(inn.comparison_type, "increment");
    assert_eq!(inn.direction, "down", "叠加后低于 B1 → 负向净增");
    assert!(inn.significant);
}

/// 报告局限字段（必出）：单 persona 局限 + 可用性标注。
#[test]
fn report_limitations_always_contain_external_validity_note() {
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: None,
        generated_at: "t".into(),
    };
    let lim = super::super::report::build_limitations(&exp, false, true);
    assert!(
        lim.iter().any(|l| l.contains("单 persona")),
        "局限声明必须包含单 persona 外部效度说明"
    );
    assert!(
        lim.iter().any(|l| l.contains("语气维")),
        "judge 不可用时应标注语气维缺失"
    );
    // embedding 可用时不出现降级说明
    assert!(!lim.iter().any(|l| l.contains("embedding 不可用")));
}

// =========================================================
// 辅助指标四件套（产物可复算近似）
// =========================================================

/// 情感维口径未校准 → 报告明示为描述性指标，且消融判定维度不含情感维。
#[test]
fn ablation_judgment_excludes_uncalibrated_emotion() {
    use super::super::evaluate::{EmotionItemScore, ToneItemScore};
    use super::super::report::{
        AblationReport, AuxiliaryMetrics, KnowledgeQualityReport, ProbeReport, Recommendation,
        VariantAuxMetrics,
    };

    // 评测含 fact / tone / emotion 三维的 F0 与 F1
    fn variant_with_dims(id: &str) -> VariantEvaluation {
        let mk = |dim: &str, idx: usize, score: f64| ItemEvaluation {
            item_id: format!("{dim}-{idx:04}"),
            dimension: dim.to_string(),
            question: String::new(),
            reference: None,
            reply_preview: String::new(),
            fact: (dim == "fact").then_some(FactItemScore {
                cosine: Some(score),
                keyword_hit: score,
                score,
                keyword_hit_norm: Some(score),
                fact_point: Some(score),
                score_norm: Some(score),
                score_point: Some(score),
            }),
            tone: (dim == "tone").then_some(ToneItemScore {
                score: score as u32,
                reason: None,
            }),
            emotion: (dim == "emotion").then_some(EmotionItemScore {
                score,
                situation_negative: true,
                situation_positive: false,
                marker_hit: 0,
            }),
            error: None,
        };
        VariantEvaluation {
            variant_id: id.to_string(),
            description: format!("{id} 档位"),
            params: VariantParams {
                theta_gap_minutes: 10,
                max_msgs_per_block: 80,
                retrieve_top_k: 3,
                ablation: Some(id.to_string()),
            },
            fact_score: None,
            fact_score_norm: None,
            fact_score_point: None,
            tone_score: None,
            emotion_score: None,
            dimension_scores: None,
            failed_count: 0,
            items: vec![
                mk("fact", 1, 0.9),
                mk("fact", 2, 0.8),
                mk("tone", 1, 5.0),
                mk("tone", 2, 4.0),
                mk("emotion", 1, 1.0),
                mk("emotion", 2, 0.5),
            ],
        }
    }

    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![variant_with_dims("F0"), variant_with_dims("F1")],
    };
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: None,
        generated_at: "t".into(),
    };
    let ab = build_ablation_report(&exp, &eval);
    assert_eq!(
        ab.judgment_dimensions,
        vec![
            "fact".to_string(),
            "fact_norm".to_string(),
            "fact_point".to_string(),
            "tone".to_string()
        ],
        "判定维度为事实维三口径（fact / fact_norm / fact_point）+ 语气维"
    );
    assert_eq!(ab.descriptive_dimensions, vec!["emotion".to_string()]);
    assert!(
        ab.rows.iter().all(|r| r.dimension != "emotion"),
        "情感维不得出现在层价值判定行中"
    );
    assert!(ab.rows.iter().any(|r| r.dimension == "fact"));
    // 两个事实维重算口径也成行（与旧 fact 口径并列，供新旧判据核对）。
    assert!(ab.rows.iter().any(|r| r.dimension == "fact_norm"));
    assert!(ab.rows.iter().any(|r| r.dimension == "fact_point"));
    assert!(ab.dimension_scope_note.contains("未校准"));

    // 渲染：描述性小节必出（用最小报告）
    let report = ProbeReport {
        results_file: "r.json".into(),
        evaluation_file: None,
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
                description: "主口径".to_string(),
                variant_ids: vec![],
                sample_count: 0,
                fact_hit_count: 0,
                false_positive_rate: 0.0,
                false_negative_rate: 0.0,
                miss_target_met: false,
                judge_rates: vec![],
            },
            pooled: KnowledgeQualityScope {
                scope: "pooled_all".to_string(),
                description: "对照口径".to_string(),
                variant_ids: vec![],
                sample_count: 0,
                fact_hit_count: 0,
                false_positive_rate: 0.0,
                false_negative_rate: 0.0,
                miss_target_met: false,
                judge_rates: vec![],
            },
            annotation: "双口径".to_string(),
        }),
        ablation: Some(AblationReport {
            baseline_variant: "F0".into(),
            rows: vec![],
            aux: Vec::<VariantAuxMetrics>::new(),
            judgment_dimensions: ab.judgment_dimensions.clone(),
            descriptive_dimensions: ab.descriptive_dimensions.clone(),
            dimension_scope_note: ab.dimension_scope_note.clone(),
            equivalence_note: ab.equivalence_note.clone(),
        }),
        limitations: vec!["单 persona".into()],
        descriptive_metrics: vec![super::super::report::EMOTION_DESCRIPTIVE_NOTE.to_string()],
        auxiliary: AuxiliaryMetrics {
            evidence_traceability_rate: None,
            behavior_rule_hit_rate: None,
            situation_route_misuse_rate: None,
            profile_regression_output_stability: None,
            annotation: "无".into(),
        },
        style_metrics: vec![],
    };
    let md = super::super::report::render_report_markdown(&report);
    assert!(md.contains("描述性指标（不参与层价值判定）"));
    assert!(md.contains("口径未校准"));
    assert!(md.contains("判定维度：fact / fact_norm / fact_point / tone"));
}

/// 知识层质量双口径：主口径只统计含记忆注入档位（B1/F0/I_*），
/// 对照口径池化全部档位（含无记忆基线 B0/S_*）。
#[test]
fn knowledge_quality_splits_memory_and_pooled_scopes() {
    // 4 个档位、每档 2 条 fact 题：B0（低）/ B1（高）/ S_behavior（低）/ I_behavior（高）。
    // `eval_variant_scores` 已把 `params.ablation` 设为档位 id，口径判定按该名解析。
    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![
            eval_variant_scores("B0", &[0.1, 0.1]),
            eval_variant_scores("B1", &[0.9, 0.9]),
            eval_variant_scores("S_behavior", &[0.2, 0.2]),
            eval_variant_scores("I_behavior", &[0.8, 0.8]),
        ],
    };
    let kq = super::super::report::assess_knowledge_quality(&eval);
    // 主口径：仅 B1 + I_behavior（各 2 题，全部 ≥0.5 命中）
    assert_eq!(kq.primary.scope, "memory_injected");
    assert_eq!(kq.primary.sample_count, 4);
    assert_eq!(kq.primary.fact_hit_count, 4);
    assert!((kq.primary.false_negative_rate - 0.0).abs() < 1e-9);
    assert!(kq.primary.miss_target_met);
    let mut ids = kq.primary.variant_ids.clone();
    ids.sort();
    assert_eq!(ids, vec!["B1".to_string(), "I_behavior".to_string()]);
    // 对照口径：全部 4 档 8 题
    assert_eq!(kq.pooled.scope, "pooled_all");
    assert_eq!(kq.pooled.sample_count, 8);
    assert_eq!(kq.pooled.fact_hit_count, 4);
    assert!((kq.pooled.false_negative_rate - 0.5).abs() < 1e-9);
    assert_eq!(kq.pooled.variant_ids.len(), 4);
    // 口径说明同时提到两者
    assert!(kq.annotation.contains("含记忆注入"));
    assert!(kq.annotation.contains("全部档位池化"));
    // 判据分栏：每口径恒有 legacy/norm/point 三行，legacy 行与扁平字段一致
    let judges: Vec<&str> = kq
        .primary
        .judge_rates
        .iter()
        .map(|r| r.judge.as_str())
        .collect();
    assert_eq!(judges, vec!["legacy", "norm", "point"]);
    let legacy = &kq.primary.judge_rates[0];
    assert_eq!(legacy.sample_count, kq.primary.sample_count);
    assert!((legacy.false_negative_rate - kq.primary.false_negative_rate).abs() < 1e-9);
    assert!((legacy.hit_rate - 1.0).abs() < 1e-9);
    assert!(legacy.miss_target_met);
    // 合成题三口径同分 → 三行数值一致
    for r in &kq.primary.judge_rates {
        assert_eq!(r.sample_count, 4);
        assert!((r.hit_rate - 1.0).abs() < 1e-9, "{r:?}");
    }
    // 对照口径（含 0.2 低分档）legacy 行与扁平字段一致
    let pooled_legacy = &kq.pooled.judge_rates[0];
    assert_eq!(pooled_legacy.sample_count, 8);
    assert!((pooled_legacy.false_negative_rate - 0.5).abs() < 1e-9);
    assert!((pooled_legacy.hit_rate - 0.5).abs() < 1e-9);
    assert!(!pooled_legacy.miss_target_met);
    // 判据附注：主口径逐判据漏报（三口径同分 → 均标为达标）
    assert!(kq.annotation.contains("legacy 漏报"));
    assert!(kq.annotation.contains("norm 漏报"));
    assert!(kq.annotation.contains("point 漏报"));
}

/// 旧评分数值（缺 score_norm/score_point 字段）→ norm/point 口径样本为 0、不达标，
/// legacy 行仍从扁平字段回填，渲染不 panic。
#[test]
fn knowledge_judge_rates_backcompat_without_new_judges() {
    let mut variant = eval_variant_scores("B1", &[0.8, 0.2]);
    for item in &mut variant.items {
        if let Some(f) = item.fact.as_mut() {
            f.score_norm = None;
            f.score_point = None;
        }
    }
    let eval = ProbeEvaluation {
        results_file: "r".into(),
        persona_uid: "u".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: true,
        generated_at: "t".into(),
        variants: vec![variant],
    };
    let kq = super::super::report::assess_knowledge_quality(&eval);
    // legacy：2 题（0.8 命中、0.2 漏报）→ 命中率 50% / 漏报率 50%
    let legacy = &kq.primary.judge_rates[0];
    assert_eq!(legacy.sample_count, 2);
    assert!((legacy.hit_rate - 0.5).abs() < 1e-9);
    assert!((legacy.false_negative_rate - 0.5).abs() < 1e-9);
    assert!(!legacy.miss_target_met);
    // norm/point：字段缺失 → 样本 0、率 0、不达标
    for r in &kq.primary.judge_rates[1..] {
        assert_eq!(r.sample_count, 0, "{r:?}");
        assert_eq!(r.hit_rate, 0.0);
        assert_eq!(r.false_negative_rate, 0.0);
        assert!(!r.miss_target_met);
    }
    // 扁平字段仍取 legacy
    assert_eq!(kq.primary.sample_count, 2);
    assert_eq!(kq.primary.fact_hit_count, 1);
}

/// 检索器空载 → 本轮无效且告警；文档数 > 0 且通道有命中 → 有效无告警。
#[test]
fn run_validity_flags_empty_retriever() {
    let (valid, warnings) = run_validity(0, 0, true);
    assert!(!valid, "检索器文档数为 0 应判定无效");
    assert!(warnings.iter().any(|w| w.contains("检索器文档数为 0")));

    let (valid, warnings) = run_validity(129, 12, true);
    assert!(valid);
    assert!(warnings.is_empty(), "正常轮次不应有告警: {warnings:?}");

    // 文档数 > 0 但通道全空 → 有效但告警
    let (valid, warnings) = run_validity(129, 0, true);
    assert!(valid);
    assert!(warnings.iter().any(|w| w.contains("四通道命中均为 0")));

    // embedding 不可用 → 追加告警
    let (_valid, warnings) = run_validity(129, 12, false);
    assert!(warnings.iter().any(|w| w.contains("embedding 不可用")));
}

/// ProbeExperiment.diagnostics 向后兼容：旧产物无该字段 → None；新产物 roundtrip 保留。
#[test]
fn probe_experiment_diagnostics_serde_backcompat() {
    let old = r#"{"dataset_file":"d","dataset_seed":1,"persona_uid":"p","rebuild_utt":false,
        "variants":[],"generated_at":"t"}"#;
    let parsed: ProbeExperiment = serde_json::from_str(old).expect("旧产物应可反序列化");
    assert!(parsed.diagnostics.is_none());
    // None 时序列化省略该键
    let s = serde_json::to_string(&parsed).unwrap();
    assert!(!s.contains("diagnostics"), "None diagnostics 应省略: {s}");

    let with = ProbeExperiment {
        dataset_file: "d".into(),
        dataset_seed: 1,
        persona_uid: "p".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: Some(ProbeRunDiagnostics {
            retriever_doc_count: 129,
            utt_doc_count: 167,
            keyword_doc_count: 125,
            keyword_pool_len: 125,
            embeddings_available: true,
            probe_queries: 5,
            bm25_hits: 20,
            vector_hits: 15,
            graph_hits: 0,
            keyword_hits: 8,
            fused_hits: 25,
            valid: true,
            warnings: vec![],
        }),
        generated_at: "t".into(),
    };
    let roundtrip: ProbeExperiment =
        serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
    let d = roundtrip.diagnostics.expect("diagnostics 应保留");
    assert_eq!(d.retriever_doc_count, 129);
    assert!(d.valid);
}

// =========================================================
// 风格形态指标（客观口径，对照短回复下区分力不足的语气 judge）
// =========================================================

/// 客观风格形态指标：长度形态 / 参考长度分布重合 / 语气词 / 疑问感叹 / 复读 / 助手腔，
/// 且失败题与空回复不进样本；无评分数值（无 persona 参考）时重合度与参考均长为 None。
#[test]
fn style_metrics_compute_covers_length_and_marks() {
    use super::super::report::compute_style_metrics;

    // 1 档 5 题：4 条有效回复（含 1 条重复）+ 1 条失败（不计入）
    let experiment: ProbeExperiment = serde_json::from_str(
        r#"{
          "dataset_file": "ds.json",
          "dataset_seed": 1,
          "persona_uid": "char-0001",
          "rebuild_utt": false,
          "variants": [{
            "variant_id": "B1",
            "description": "基线",
            "params": {"theta_gap_minutes": 30, "max_msgs_per_block": 5, "retrieve_top_k": 5},
            "failed_count": 1,
            "runs": [
              {"item_id":"tone-0001","dimension":"tone","question":"q","reply":"哦哦",
               "metrics":{"reply_chars":2,"elapsed_ms":1},"error":null},
              {"item_id":"tone-0002","dimension":"tone","question":"q","reply":"哦哦",
               "metrics":{"reply_chars":2,"elapsed_ms":1},"error":null},
              {"item_id":"tone-0003","dimension":"tone","question":"q","reply":"我找一下，你先看看？",
               "metrics":{"reply_chars":11,"elapsed_ms":1},"error":null},
              {"item_id":"tone-0004","dimension":"tone","question":"q","reply":"总的来说，我建议你这样做。",
               "metrics":{"reply_chars":13,"elapsed_ms":1},"error":null},
              {"item_id":"tone-0005","dimension":"tone","question":"q","reply":"",
               "metrics":{"reply_chars":0,"elapsed_ms":1},"error":"失败"}
            ]
          }],
          "generated_at": "t"
        }"#,
    )
    .expect("实验产物反序列化");

    // persona 参考（tone 题 reference）长度 [2, 4] → 均长 3.0
    let evaluation: ProbeEvaluation = serde_json::from_str(
        r#"{
          "results_file": "r.json",
          "persona_uid": "char-0001",
          "dataset_seed": 1,
          "judge_used": false,
          "embedding_used": false,
          "generated_at": "t",
          "variants": [{
            "variant_id": "B1",
            "description": "基线",
            "params": {"theta_gap_minutes": 30, "max_msgs_per_block": 5, "retrieve_top_k": 5},
            "fact_score": null,
            "tone_score": null,
            "failed_count": 1,
            "items": [
              {"item_id":"tone-0001","dimension":"tone","question":"q","reference":"哦哦",
               "reply_preview":"哦哦","fact":null,"tone":null,"emotion":null,"error":null},
              {"item_id":"tone-0002","dimension":"tone","question":"q","reference":"我找一下",
               "reply_preview":"哦哦","fact":null,"tone":null,"emotion":null,"error":null}
            ]
          }]
        }"#,
    )
    .expect("评分产物反序列化");

    let metrics = compute_style_metrics(&experiment, Some(&evaluation));
    assert_eq!(metrics.len(), 1);
    let m = &metrics[0];
    assert_eq!(m.variant_id, "B1");
    assert_eq!(m.reply_count, 4, "失败题与空回复不应计入");
    // 长度 [2, 2, 10, 13] → 均长 6.75 / 中位 (2+10)/2 = 6.0
    assert!((m.len_mean - 6.75).abs() < 1e-9, "{m:?}");
    assert!((m.len_median - 6.0).abs() < 1e-9);
    assert!((m.len_le_30_rate - 1.0).abs() < 1e-9);
    // "哦哦" 出现 2 次 → 复读率 0.25
    assert!((m.repeat_rate - 0.25).abs() < 1e-9);
    // 语气词：仅 2 条 "哦哦" 命中（"我找一下，你先看看？" 不含语气词字符）
    assert!((m.tone_particle_rate - 0.5).abs() < 1e-9);
    assert!(
        (m.question_rate - 0.25).abs() < 1e-9,
        "仅 1 条以 ? / ？ 结尾"
    );
    assert_eq!(m.exclaim_rate, 0.0);
    assert!(
        (m.assistant_marker_rate - 0.25).abs() < 1e-9,
        "含「总的来说」1 条"
    );
    // 长度直方图：回复 {0..4: 0.5, 10..14: 0.5} vs 参考 {0..4: 1.0} → 重合 0.5
    assert!((m.ref_len_mean.expect("参考均长") - 3.0).abs() < 1e-9);
    assert!((m.len_ref_overlap.expect("长度重合度") - 0.5).abs() < 1e-9);

    // 无评分数值（无 persona 参考）→ 长度重合度与参考均长不可得，其余指标仍产出
    let no_ref = compute_style_metrics(&experiment, None);
    assert_eq!(no_ref[0].reply_count, 4);
    assert!(no_ref[0].len_ref_overlap.is_none());
    assert!(no_ref[0].ref_len_mean.is_none());
    assert!((no_ref[0].len_mean - 6.75).abs() < 1e-9);
}
