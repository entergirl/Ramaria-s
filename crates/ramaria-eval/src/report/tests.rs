//! crates/ramaria-eval/src/report/tests.rs - Ramaria 评测报告测试
//!
//! 设计特点:
//! - 两个合成运行（主 + oracle）覆盖条件行、消融与 oracle 比较、H1~H6 与 markdown 章节
//! - 幂等用例逐字节比较两份产物，锁定无时间戳与稳定排序
//! - 缺项三态各自构造：oracle 未执行、判分产物缺失、judge 关闭
//! - 预算轴用 800 / 1600 / 3200 三段验证 H4 的 supported / partial 两条分支
//! - 配对统计直接对手算序列断言均值差、效应量、p 值与 BH 校正

mod fixtures;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;
use fixtures::{ConditionSpec, RunSpec, alternating, constant};

// =========================================================
// 夹具与辅助
// =========================================================

/// 报告选项（主运行给路径，oracle 走自动发现）。
fn options(root: &Path) -> ReportOptions {
    ReportOptions {
        run_id: root.to_string_lossy().to_string(),
        oracle: None,
        compare: Vec::new(),
        output: None,
    }
}

/// 主运行 + oracle 的完整夹具。
fn full_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let dir = fixtures::temp_dir(tag);
    let primary = dir.join("main");
    let spec = RunSpec {
        run_id: "fixture-main",
        judge: true,
        question_sample: Some(30),
        conditions: vec![
            ConditionSpec {
                tier: "u1-y1",
                pipeline: "layered",
                budget: 800,
                ratios: alternating(0.5, 0.7, 30),
            },
            ConditionSpec {
                tier: "u1-y1",
                pipeline: "l1",
                budget: 800,
                ratios: constant(0.3, 30),
            },
        ],
        with_score: true,
        usage: true,
    };
    fixtures::write_run(&primary, &spec);
    fixtures::write_run(
        &primary.join("oracle"),
        &RunSpec {
            run_id: "fixture-oracle",
            judge: true,
            question_sample: Some(30),
            conditions: vec![ConditionSpec {
                tier: "u1-y1",
                pipeline: "oracle-l1",
                budget: 800,
                ratios: alternating(0.9, 1.0, 30),
            }],
            with_score: true,
            usage: false,
        },
    );
    (dir, primary)
}

/// 结论状态。
fn status(artifact: &ReportArtifact, id: &str) -> HypothesisStatus {
    artifact
        .hypotheses
        .iter()
        .find(|hypothesis| hypothesis.id == id)
        .map(|hypothesis| hypothesis.status)
        .unwrap_or_else(|| panic!("缺少结论 {id}"))
}

// =========================================================
// 产物结构
// =========================================================

/// 一条主运行 + oracle 生成两份产物，字段与 markdown 章节齐全。
#[test]
fn report_writes_json_and_markdown() {
    let (dir, primary) = full_fixture("full");
    let outcome = run(&dir, &options(&primary)).expect("报告应生成");
    let artifact = &outcome.artifact;

    assert_eq!(artifact.artifact_kind, REPORT_ARTIFACT_KIND);
    assert_eq!(artifact.schema_version, REPORT_SCHEMA_VERSION);
    assert_eq!(artifact.run_id, "fixture-main");
    assert_eq!(
        artifact.source.items_sha256,
        fixtures::items_digest("fixture-main")
    );
    assert_eq!(artifact.source.score_schema_version, 3);
    assert!(
        matches!(
            &artifact.source.judge,
            crate::score::JudgeStatus::Configured { model, .. } if model == "judge-model"
        ),
        "判分模型应取 score.json"
    );
    assert_eq!(artifact.runs.len(), 2);
    assert_eq!(artifact.runs[0].role, RunRole::Primary);
    assert_eq!(artifact.runs[1].role, RunRole::Oracle);
    assert_eq!(artifact.runs[0].tiers, vec!["u1-y1".to_string()]);
    assert_eq!(artifact.runs[0].question_sample, Some(30));

    assert_eq!(artifact.conditions.len(), 2);
    let layered = artifact
        .conditions
        .iter()
        .find(|condition| condition.pipeline == "layered")
        .expect("应有 layered 条件");
    assert_eq!(layered.tier, "u1-y1");
    assert_eq!(layered.budget_chars, 800);
    assert_eq!(layered.answer_model, "answer-model");
    assert_eq!(layered.questions, 30);
    assert_eq!(layered.scored, 30);
    assert_eq!(layered.judge_failed, 0);
    assert!((layered.rubric.as_ref().unwrap().micro_mean_ratio.unwrap() - 0.6).abs() < 1e-9);
    assert!(layered.retrieval.artifact.is_some());
    assert!(layered.persona.is_some());
    assert!(layered.usage.is_some(), "usage.json 应被读入");

    let ablation = artifact
        .comparisons
        .iter()
        .find(|row| row.id == "u1-y1/layered-vs-l1")
        .expect("应有消融比较");
    assert_eq!(ablation.n_pairs, 30);
    assert!((ablation.mean_diff.unwrap() - 0.3).abs() < 1e-9);
    assert!(ablation.ci_low.unwrap() > 0.0);
    assert!(ablation.significant, "消融差应显著");

    let oracle = artifact
        .comparisons
        .iter()
        .find(|row| row.id == "u1-y1/oracle-l1-vs-layered")
        .expect("应有 oracle 比较");
    assert_eq!(oracle.left, "u1-y1/oracle-l1/800/0");
    assert!((oracle.mean_diff.unwrap() - 0.35).abs() < 1e-9);
    assert!(oracle.significant, "oracle 差应显著");

    assert_eq!(artifact.hypotheses.len(), 6);
    assert_eq!(status(artifact, "H1"), HypothesisStatus::NotEvidenced);
    assert_eq!(status(artifact, "H2"), HypothesisStatus::Supported);
    assert_eq!(status(artifact, "H3"), HypothesisStatus::Partial);
    assert_eq!(status(artifact, "H4"), HypothesisStatus::NotEvidenced);
    assert_eq!(status(artifact, "H5"), HypothesisStatus::Supported);
    assert_eq!(status(artifact, "H6"), HypothesisStatus::Partial);
    assert!(artifact.missing.is_empty(), "缺项: {:?}", artifact.missing);

    let raw = std::fs::read_to_string(&outcome.json).expect("报告 JSON 应可读");
    assert!(raw.ends_with('\n'), "report.json 应以换行结尾");
    let parsed: ReportArtifact = serde_json::from_str(&raw).expect("报告 JSON 应可解析");
    assert_eq!(parsed, *artifact);

    let markdown = std::fs::read_to_string(&outcome.markdown).expect("报告 markdown 应可读");
    for heading in [
        "## 1. 运行摘要",
        "## 2. 条件行",
        "## 3. 主表",
        "## 4. 分层指标",
        "## 5. 配对比较",
        "## 6. 预算轴与 oracle",
        "## 7. 结论",
        "## 8. 缺项与负面结果",
        "## 9. 口径说明",
    ] {
        assert!(markdown.contains(heading), "缺少章节 {heading}");
    }
    assert!(markdown.contains("不可直接比较"));
    assert!(markdown.contains("u1-y1/layered-vs-l1"));
    assert!(markdown.contains("fixture-main"));
    assert!(markdown.contains("fixture-oracle"));
}

/// 同一输入两次生成 report.json 与 report.md 逐字节一致。
#[test]
fn report_is_byte_identical_on_repeat() {
    let (dir, primary) = full_fixture("idempotent");
    let options = options(&primary);
    let first = run(&dir, &options).expect("首次报告应生成");
    let json = std::fs::read(&first.json).expect("报告 JSON 应可读");
    let markdown = std::fs::read(&first.markdown).expect("报告 markdown 应可读");

    let second = run(&dir, &options).expect("再次报告应生成");
    assert_eq!(json, std::fs::read(&second.json).expect("报告 JSON 应可读"));
    assert_eq!(
        markdown,
        std::fs::read(&second.markdown).expect("报告 markdown 应可读")
    );
}

// =========================================================
// 统计
// =========================================================

/// 已知差值序列的均值差、效应量、p 值与 BH 校正。
#[test]
fn paired_statistics_match_known_sequence() {
    let left: BTreeMap<String, f64> = (0..30)
        .map(|index| (format!("q{index}"), if index % 2 == 0 { 0.7 } else { 0.5 }))
        .collect();
    let right: BTreeMap<String, f64> = (0..30).map(|index| (format!("q{index}"), 0.3)).collect();
    let stats = super::stats::paired(&left, &right);
    assert_eq!(stats.n, 30);
    assert!((stats.mean.expect("应有均值") - 0.3).abs() < 1e-9);
    let dz = stats.dz.expect("应有效应量");
    assert!(dz > 2.9, "d_z 实际 {dz}");
    assert!(stats.p.expect("应有 p 值") < 0.001);
    let (low, high) = stats.ci.expect("30 对应有区间");
    assert!(low > 0.0 && high > 0.0, "区间应不含 0：{low}..{high}");

    let adjusted = crate::stats::bh_fdr_adjust(&[0.001, 0.04]);
    assert!((adjusted[0] - 0.002).abs() < 1e-12);
    assert!((adjusted[1] - 0.04).abs() < 1e-12);
}

/// 效应量不参与显著判定：d_z 低于 0.3 但 p_adj 与区间达标仍判显著。
#[test]
fn significance_ignores_effect_size_threshold() {
    let dir = fixtures::temp_dir("effect-size");
    let primary = dir.join("main");
    let layered: Vec<Option<f64>> = (0..300)
        .map(|index| Some(if index < 20 { 0.35 } else { 0.52 }))
        .collect();
    fixtures::write_run(
        &primary,
        &RunSpec {
            run_id: "effect-main",
            judge: true,
            question_sample: Some(300),
            conditions: vec![
                ConditionSpec {
                    tier: "u1-y1",
                    pipeline: "layered",
                    budget: 800,
                    ratios: layered,
                },
                ConditionSpec {
                    tier: "u1-y1",
                    pipeline: "l1",
                    budget: 800,
                    ratios: constant(0.5, 300),
                },
            ],
            with_score: true,
            usage: false,
        },
    );
    let outcome = run(&dir, &options(&primary)).expect("报告应生成");
    let row = outcome
        .artifact
        .comparisons
        .iter()
        .find(|row| row.id == "u1-y1/layered-vs-l1")
        .expect("应有消融比较");
    let dz = row.effect_dz.expect("应有效应量");
    assert!(dz.abs() < 0.3, "d_z 应低于 0.3：{dz}");
    assert!(row.significant, "效应量不应参与判定：{row:?}");
}

/// 配对样本少于 10 时置信区间留空，并记不支持缺项。
#[test]
fn small_sample_leaves_interval_null() {
    let dir = fixtures::temp_dir("small");
    let primary = dir.join("main");
    fixtures::write_run(
        &primary,
        &RunSpec {
            run_id: "small-main",
            judge: true,
            question_sample: Some(8),
            conditions: vec![
                ConditionSpec {
                    tier: "u1-y1",
                    pipeline: "layered",
                    budget: 800,
                    ratios: alternating(0.5, 0.7, 8),
                },
                ConditionSpec {
                    tier: "u1-y1",
                    pipeline: "l1",
                    budget: 800,
                    ratios: constant(0.3, 8),
                },
            ],
            with_score: true,
            usage: false,
        },
    );
    let outcome = run(&dir, &options(&primary)).expect("报告应生成");
    let row = outcome
        .artifact
        .comparisons
        .iter()
        .find(|row| row.id == "u1-y1/layered-vs-l1")
        .expect("应有消融比较");
    assert_eq!(row.n_pairs, 8);
    assert!(row.ci_low.is_none());
    assert!(row.ci_high.is_none());
    assert!(!row.significant, "样本不足按不显著处理");
    assert!(
        outcome.artifact.missing.iter().any(|entry| {
            entry.kind == MissingKind::Unsupported && entry.detail.contains("样本不足（n<10）")
        }),
        "缺项: {:?}",
        outcome.artifact.missing
    );
}

// =========================================================
// H1 跨档
// =========================================================

/// H1 只比较各档均值：最小档与最大档 CI 不重叠 → supported；重叠 → partial。
#[test]
fn h1_compares_tier_means_without_pairing() {
    let (dir, primary) = h1_fixture(
        "h1-supported",
        [
            ("u1-y1", 0.8, 1.0),
            ("u5-y1", 0.4, 0.6),
            ("u10-y1", 0.0, 0.2),
        ],
    );
    let outcome = run(&dir, &options(&primary)).expect("报告应生成");
    let h1 = hypothesis_of(&outcome.artifact, "H1");
    assert_eq!(h1.status, HypothesisStatus::Supported, "H1: {h1:?}");
    assert!(h1.summary.contains("非配对"), "{}", h1.summary);
    assert!(h1.summary.contains("跨档题集重叠 n=30"), "{}", h1.summary);
    assert!(
        h1.evidence
            .iter()
            .any(|item| item.contains("u5-y1") && item.contains("95% CI")),
        "evidence: {:?}",
        h1.evidence
    );

    let (dir, primary) = h1_fixture(
        "h1-partial",
        [
            ("u1-y1", 0.2, 0.8),
            ("u5-y1", 0.15, 0.75),
            ("u10-y1", 0.1, 0.7),
        ],
    );
    let outcome = run(&dir, &options(&primary)).expect("报告应生成");
    let h1 = hypothesis_of(&outcome.artifact, "H1");
    assert_eq!(h1.status, HypothesisStatus::Partial, "H1: {h1:?}");
    assert!(h1.summary.contains("95% CI 重叠"), "{}", h1.summary);
}

/// 三档 layered 夹具（每档同题号、交替取值）。
fn h1_fixture(tag: &str, tiers: [(&'static str, f64, f64); 3]) -> (PathBuf, PathBuf) {
    let dir = fixtures::temp_dir(tag);
    let primary = dir.join("main");
    fixtures::write_run(
        &primary,
        &RunSpec {
            run_id: "h1-main",
            judge: true,
            question_sample: Some(30),
            conditions: tiers
                .into_iter()
                .map(|(tier, low, high)| ConditionSpec {
                    tier,
                    pipeline: "layered",
                    budget: 800,
                    ratios: alternating(low, high, 30),
                })
                .collect(),
            with_score: true,
            usage: false,
        },
    );
    (dir, primary)
}

/// 按编号取结论。
fn hypothesis_of<'a>(artifact: &'a ReportArtifact, id: &str) -> &'a Hypothesis {
    artifact
        .hypotheses
        .iter()
        .find(|hypothesis| hypothesis.id == id)
        .unwrap_or_else(|| panic!("缺少结论 {id}"))
}

// =========================================================
// H3 消融
// =========================================================

/// H3 的分母只计已出比较的消融：2 条比较 1 条显著 → partial 且摘要为 1/2。
#[test]
fn h3_counts_only_compared_ablations() {
    let dir = fixtures::temp_dir("h3-compared");
    let primary = dir.join("main");
    fixtures::write_run(
        &primary,
        &RunSpec {
            run_id: "h3-main",
            judge: true,
            question_sample: Some(30),
            conditions: vec![
                ConditionSpec {
                    tier: "u1-y1",
                    pipeline: "layered",
                    budget: 800,
                    ratios: alternating(0.5, 0.7, 30),
                },
                ConditionSpec {
                    tier: "u1-y1",
                    pipeline: "l1",
                    budget: 800,
                    ratios: constant(0.3, 30),
                },
                ConditionSpec {
                    tier: "u1-y1",
                    pipeline: "l1-l2",
                    budget: 800,
                    ratios: alternating(0.5, 0.7, 30),
                },
            ],
            with_score: true,
            usage: false,
        },
    );
    let outcome = run(&dir, &options(&primary)).expect("报告应生成");
    let h3 = hypothesis_of(&outcome.artifact, "H3");
    assert_eq!(h3.status, HypothesisStatus::Partial, "H3: {h3:?}");
    assert_eq!(
        h3.summary, "在 u1-y1 上显著优于 1/2 条已比较消融（2 条未比较）",
        "H3: {h3:?}"
    );
    assert!(
        h3.evidence
            .iter()
            .any(|item| item == "u1-y1 vs l1-l2-l3: 未出比较"),
        "evidence: {:?}",
        h3.evidence
    );
}

// =========================================================
// 缺项三态
// =========================================================

/// oracle 未执行 → not_run；判分产物缺失 → failed；judge 关闭 → unsupported。
#[test]
fn missing_states_are_explicit() {
    // 未执行：没有 oracle 子目录
    let dir = fixtures::temp_dir("not-run");
    let primary = dir.join("main");
    fixtures::write_run(
        &primary,
        &RunSpec::layered("no-oracle", alternating(0.5, 0.7, 30)),
    );
    let outcome = run(&dir, &options(&primary)).expect("报告应生成");
    assert!(
        outcome
            .artifact
            .missing
            .iter()
            .any(|entry| entry.kind == MissingKind::NotRun && entry.detail.contains("oracle")),
        "缺项: {:?}",
        outcome.artifact.missing
    );

    // 失败：判分产物不存在
    let dir = fixtures::temp_dir("failed");
    let primary = dir.join("main");
    let mut spec = RunSpec::layered("no-score", alternating(0.5, 0.7, 30));
    spec.with_score = false;
    fixtures::write_run(&primary, &spec);
    let outcome = run(&dir, &options(&primary)).expect("报告仍应生成");
    assert!(outcome.artifact.conditions.is_empty());
    assert!(
        outcome.artifact.missing.iter().any(|entry| {
            entry.kind == MissingKind::Failed && entry.detail.contains("判分产物不存在")
        }),
        "缺项: {:?}",
        outcome.artifact.missing
    );
    let markdown = std::fs::read_to_string(&outcome.markdown).expect("报告 markdown 应可读");
    assert!(markdown.contains("没有判分产物"));

    // 不支持：judge 关闭
    let dir = fixtures::temp_dir("unsupported");
    let primary = dir.join("main");
    let mut spec = RunSpec::layered("no-judge", alternating(0.5, 0.7, 30));
    spec.judge = false;
    fixtures::write_run(&primary, &spec);
    let outcome = run(&dir, &options(&primary)).expect("报告应生成");
    assert!(
        outcome
            .artifact
            .conditions
            .iter()
            .all(|condition| condition.rubric.is_none())
    );
    assert!(
        outcome.artifact.missing.iter().any(|entry| {
            entry.kind == MissingKind::Unsupported && entry.detail.contains("本次未判分")
        }),
        "缺项: {:?}",
        outcome.artifact.missing
    );
    let markdown = std::fs::read_to_string(&outcome.markdown).expect("报告 markdown 应可读");
    assert!(markdown.contains("本次未判分"));
}

// =========================================================
// 预算轴
// =========================================================

/// 800 / 1600 / 3200 三段：3200 无增量 → supported；仍有增量 → partial。
#[test]
fn budget_axis_judges_h4() {
    let (dir, primary) = budget_fixture("budget-supported", false);
    let mut compare = options(&primary);
    for run_id in ["compare-1600", "compare-3200"] {
        compare
            .compare
            .push(dir.join(run_id).to_string_lossy().to_string());
    }
    let outcome = run(&dir, &compare).expect("报告应生成");
    assert_eq!(
        status(&outcome.artifact, "H4"),
        HypothesisStatus::Supported,
        "结论: {:?}",
        outcome.artifact.hypotheses
    );
    assert!(
        outcome
            .artifact
            .comparisons
            .iter()
            .any(|row| row.id == "u10-y1/layered/b1600-vs-b800")
    );
    assert!(
        outcome
            .artifact
            .comparisons
            .iter()
            .any(|row| row.id == "u10-y1/layered/b3200-vs-b1600")
    );

    let (dir, primary) = budget_fixture("budget-partial", true);
    let mut compare = options(&primary);
    for run_id in ["compare-1600", "compare-3200"] {
        compare
            .compare
            .push(dir.join(run_id).to_string_lossy().to_string());
    }
    let outcome = run(&dir, &compare).expect("报告应生成");
    assert_eq!(
        status(&outcome.artifact, "H4"),
        HypothesisStatus::Partial,
        "结论: {:?}",
        outcome.artifact.hypotheses
    );
}

/// 三段预算夹具：1600 相对 800 显著提升，3200 是否继续提升由 `improve` 决定。
fn budget_fixture(tag: &str, improve: bool) -> (PathBuf, PathBuf) {
    let dir = fixtures::temp_dir(tag);
    let primary = dir.join("main");
    let base = alternating(0.1, 0.3, 30);
    fixtures::write_run(
        &primary,
        &RunSpec {
            run_id: "budget-main",
            judge: true,
            question_sample: Some(30),
            conditions: vec![ConditionSpec {
                tier: "u10-y1",
                pipeline: "layered",
                budget: 800,
                ratios: base,
            }],
            with_score: true,
            usage: false,
        },
    );
    let mid = alternating(0.5, 0.9, 30);
    let high = if improve {
        alternating(0.7, 1.0, 30)
    } else {
        alternating(0.48, 0.92, 30)
    };
    for (run_id, budget, ratios) in [
        ("compare-1600", 1_600u32, mid),
        ("compare-3200", 3_200u32, high),
    ] {
        fixtures::write_run(
            &dir.join(run_id),
            &RunSpec {
                run_id,
                judge: true,
                question_sample: Some(30),
                conditions: vec![ConditionSpec {
                    tier: "u10-y1",
                    pipeline: "layered",
                    budget,
                    ratios,
                }],
                with_score: true,
                usage: false,
            },
        );
    }
    (dir, primary)
}
