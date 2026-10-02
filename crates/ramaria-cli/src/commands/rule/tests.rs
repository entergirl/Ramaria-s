//! crates/ramaria-cli/src/commands/rule/tests.rs - 行为规则纯函数单元测试
//!
//! 设计特点:
//! - 覆盖 percentile / 簇形态统计 / 质控闸门归类 / 反事实汇总等纯函数
//! - 覆盖 θ_join 增量模拟的分段、投票与一致性判定辅助函数
//! - 经 `use super::*` 复用规则模块的私有项与装配函数

use super::*;
use ramaria_core::behavior::BehaviorSituation;
use ramaria_memory::behavior::{RefinedCluster, RuleGenConfig};

/// `percentile`: 空输入返回 0.0，单元素恒为该值。
#[test]
fn percentile_handles_empty_and_single() {
    assert_eq!(percentile(&[], 0.5), 0.0);
    assert_eq!(percentile(&[0.42], 0.0), 0.42);
    assert_eq!(percentile(&[0.42], 1.0), 0.42);
}

/// `percentile`: 两元素线性插值（中点 = 均值），p=0/1 取端点。
#[test]
fn percentile_interpolates_linearly() {
    let data = [10.0, 20.0];
    assert_eq!(percentile(&data, 0.0), 10.0);
    assert_eq!(percentile(&data, 1.0), 20.0);
    assert_eq!(percentile(&data, 0.5), 15.0);
    assert_eq!(percentile(&data, 0.25), 12.5);
}

/// `summarize_cluster_shapes`: 最大簇占比与孤立点统计。
#[test]
fn summarize_cluster_shapes_counts_outliers_and_share() {
    // 3 + 1 簇、共 10 个样本：最大占比 0.3，孤立点 6 个（60%）
    let shape = summarize_cluster_shapes(&[3, 1], 10);
    assert!((shape.max_share - 0.3).abs() < 1e-12);
    assert_eq!(shape.outlier_count, 6);
    assert!((shape.outlier_ratio - 0.6).abs() < 1e-12);

    // 全部入簇：孤立点为 0
    let full = summarize_cluster_shapes(&[4, 2], 6);
    assert!((full.max_share - 4.0 / 6.0).abs() < 1e-12);
    assert_eq!(full.outlier_count, 0);
    assert_eq!(full.outlier_ratio, 0.0);

    // 无样本：全部为 0
    let empty = summarize_cluster_shapes(&[], 0);
    assert_eq!(empty.max_share, 0.0);
    assert_eq!(empty.outlier_count, 0);
    assert_eq!(empty.outlier_ratio, 0.0);
}

/// `tally_quality_gate`: 逐簇判定按 Pass / 证据不足 / n_eff 不足 / valence 方差超限归类。
#[test]
fn tally_quality_gate_classifies_verdicts() {
    let config = RuleGenConfig {
        min_evidence: 5,
        min_n_eff: 5,
        valence_std_limit: 0.5,
        ..RuleGenConfig::default()
    };
    let clusters = vec![
        // 三项全达标 → Pass
        test_refined_cluster(10, 10.0, 0.1),
        // 证据量不足（按闸门顺序先于 n_eff / 方差判定）
        test_refined_cluster(4, 1.0, 0.9),
        // n_eff 不足
        test_refined_cluster(10, 2.0, 0.1),
        // valence 标准差超限
        test_refined_cluster(10, 10.0, 0.9),
    ];
    let tally = tally_quality_gate(&clusters, &config);
    assert_eq!(
        tally,
        GateTally {
            pass: 1,
            low_evidence: 1,
            low_neff: 1,
            high_valence_variance: 1,
        }
    );
}

/// `summarize_counterfactual`: 规模降序、3 成员簇计数与回收样本数。
#[test]
fn summarize_counterfactual_counts_small_clusters_and_recoveries() {
    // 反事实 4 簇（3/3/2/1 共 9 样本）+ 7 孤立点；原始孤立 14 → 回收 7
    let summary = summarize_counterfactual(2, &[1, 3, 2, 3], 16, 14);
    assert_eq!(summary.min_cluster_size, 2);
    assert_eq!(summary.count, 4);
    assert_eq!(summary.sizes, vec![3, 3, 2, 1]);
    assert_eq!(summary.three_member_clusters, 2);
    assert_eq!(summary.outlier_count, 7);
    assert!((summary.outlier_ratio - 7.0 / 16.0).abs() < 1e-12);
    assert_eq!(summary.recovered_samples, 7);

    // 无样本：孤立点统计为 0，不 panic
    let empty = summarize_counterfactual(1, &[], 0, 0);
    assert_eq!(empty.count, 0);
    assert_eq!(empty.outlier_count, 0);
    assert_eq!(empty.outlier_ratio, 0.0);
    assert_eq!(empty.recovered_samples, 0);
}

/// `split_index_by_ratio`: 前段条数按比例取整后夹在 [1, total−1]（两段非空）。
#[test]
fn split_index_by_ratio_keeps_both_segments_non_empty() {
    assert_eq!(split_index_by_ratio(10, 0.8), Some(8));
    assert_eq!(split_index_by_ratio(5, 0.8), Some(4));
    assert_eq!(split_index_by_ratio(10, 0.1), Some(1), "下界夹取");
    assert_eq!(split_index_by_ratio(2, 0.9), Some(1), "上界夹取");
    assert_eq!(split_index_by_ratio(1, 0.8), None, "单条无法切分");
    assert_eq!(split_index_by_ratio(0, 0.8), None, "空输入无法切分");
}

/// `judge_agreement`: 标签一致 / 不一致 / 全量孤立误归 / 规则侧不可判定。
#[test]
fn judge_agreement_classifies_matches_and_mismatches() {
    assert_eq!(judge_agreement(Some(2), Some(2)), Some(true));
    assert_eq!(judge_agreement(Some(2), Some(3)), Some(false));
    assert_eq!(
        judge_agreement(Some(2), None),
        Some(false),
        "全量侧孤立 = 误归"
    );
    assert_eq!(
        judge_agreement(None, Some(1)),
        None,
        "规则侧无映射 → 不计分母"
    );
    assert_eq!(judge_agreement(None, None), None);
}

/// `majority_vote_label`: 取最高票；并列时取较小标签；空输入无标签。
#[test]
fn majority_vote_label_prefers_count_then_smallest_label() {
    assert_eq!(majority_vote_label(&[1, 1, 2]), Some(1));
    assert_eq!(majority_vote_label(&[2, 2, 1]), Some(2));
    assert_eq!(majority_vote_label(&[3, 1]), Some(1), "并列取较小标签");
    assert_eq!(majority_vote_label(&[]), None);
}

/// 构造提炼簇（测试辅助）：仅设置闸门判定所需字段。
fn test_refined_cluster(sample_count: usize, n_eff: f64, valence_std: f64) -> RefinedCluster {
    let mut situation = BehaviorSituation::empty();
    situation.sample_count = sample_count;
    situation.valence_std = valence_std;
    RefinedCluster {
        situation,
        n_eff,
        cohesion: 1.0,
        quality: 1.0,
        member_event_ids: Vec::new(),
        member_events: Vec::new(),
    }
}
