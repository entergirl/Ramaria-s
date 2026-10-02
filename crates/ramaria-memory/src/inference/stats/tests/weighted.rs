//! crates/ramaria-memory/src/inference/stats/tests/weighted.rs - 加权统计
//!
//! 设计特点:
//! - 由 父测试模块 以 mod weighted; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 加权统计
// =========================================================

/// weighted_mean 各分支参数化验证：等权重 / 非等权重 / 全零权重。
#[test]
fn weighted_mean_cases() {
    // 等权重 → 算术平均
    let values = vec![1.0, 2.0, 3.0];
    let weights = vec![1.0, 1.0, 1.0];
    let mean = weighted_mean(&values, &weights);
    assert!((mean - 2.0).abs() < 1e-10);
    // 非等权重 → 加权平均
    let values = vec![0.5, 0.5, 1.0];
    let weights = vec![0.2, 0.5, 0.8];
    let mean = weighted_mean(&values, &weights);
    assert!((mean - 0.7666).abs() < 0.001);
    // 全零权重 → 0.0
    let values = vec![1.0, 2.0];
    let weights = vec![0.0, 0.0];
    let mean = weighted_mean(&values, &weights);
    assert!((mean - 0.0).abs() < 1e-10);
}

#[test]
fn weighted_variance_basic() {
    let values = vec![1.0, 2.0, 3.0];
    let weights = vec![1.0, 1.0, 1.0];
    let var = weighted_variance(&values, &weights, 2.0);
    assert!((var - 2.0 / 3.0).abs() < 1e-10);
}

#[test]
fn weighted_ratio_basic() {
    let indicators = vec![1.0, 0.0, 1.0];
    let weights = vec![0.4, 0.6, 1.0];
    let ratio = weighted_ratio(&indicators, &weights);
    assert!((ratio - 0.7).abs() < 1e-10);
}
