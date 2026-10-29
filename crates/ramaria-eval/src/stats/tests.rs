//! crates/ramaria-eval/src/stats/tests.rs - 统计原语单元测试
//!
//! 设计特点:
//! - 数值函数对关键值断言（正态 CDF / 学生氏 t CDF）
//! - 检验方向性：单向强效应 p 小、符号混合 p 大
//! - 效应量边界：零方差非零均值标记为 ±10，零均值归零
//! - 等效性检验在近零效应上给出等效、在大效应上拒绝等效
//! - 多重比较校正单调且首尾正确

use super::tost::student_t_cdf;
use super::*;

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
