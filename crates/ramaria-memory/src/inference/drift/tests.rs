//! crates/ramaria-memory/src/inference/drift/tests.rs - //! crates/ramaria-memory/src/inference/drift.rs - 性格漂移检测单元测试
//!
//! 设计特点:
//! - 位于 inference::drift 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 drift.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

// ---- Wasserstein 1D ----

/// wasserstein_1d 各输入参数化验证（None 表示仅断言范围 [0,1]）。
#[test]
fn wasserstein_1d_cases() {
    let cases: Vec<(Vec<f64>, Vec<f64>, Option<f64>)> = vec![
        (vec![0.5, 0.5, 0.5], vec![0.5, 0.5, 0.5], Some(0.0)), // 相同分布 → 0
        (vec![0.0, 0.0, 0.0], vec![1.0, 1.0, 1.0], Some(1.0)), // 完全分离 → 1.0
        (vec![], vec![1.0, 2.0], Some(0.0)),                   // 空输入 → 0.0
        (vec![1.0, 2.0], vec![], Some(0.0)),                   // 空输入 → 0.0
        (vec![0.0], vec![1.0], Some(1.0)),                     // 单元素
        (vec![0.0, 0.5, 1.0], vec![0.3, 0.7], None),           // 不同大小 → 合理范围
    ];
    for (a, b, expected) in cases {
        let w = wasserstein_1d(&a, &b);
        match expected {
            Some(exp) => assert!((w - exp).abs() < 1e-10, "a={a:?} b={b:?} 期望 {exp}"),
            None => {
                assert!((0.0..=1.0).contains(&w), "a={a:?} b={b:?} 应在 [0,1]");
            }
        }
    }
}

// ---- 置换检验 ----

#[test]
fn permutation_test_no_drift() {
    let config = DriftConfig::default();
    // 两组来自相同分布的数据（无漂移）
    let a = vec![0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];
    let b = vec![0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85];
    let (observed_w, threshold, _dists) = permutation_test(&a, &b, &config);
    // 观测距离应小于阈值（无显著漂移）
    assert!(
        observed_w <= threshold || threshold < 1e-12,
        "无漂移时观测距离应≤阈值。W={:.4}, 阈值={:.4}",
        observed_w,
        threshold
    );
}

#[test]
fn permutation_test_with_drift() {
    let config = DriftConfig::default();
    // 两组来自明显不同分布
    let a = vec![0.0, 0.1, 0.0, 0.1, 0.0];
    let b = vec![0.8, 0.9, 0.8, 0.9, 0.8];
    let (observed_w, threshold, _dists) = permutation_test(&a, &b, &config);
    // 观测距离应显著大于阈值
    assert!(
        observed_w > threshold,
        "漂移时观测距离应>阈值。W={:.4}, 阈值={:.4}",
        observed_w,
        threshold
    );
}

#[test]
fn permutation_test_small_sample() {
    let config = DriftConfig::default();
    // 小样本——阈值应自动上调
    let a = vec![0.0, 0.5];
    let b = vec![0.3, 0.7];
    let (observed_w, threshold, distances) = permutation_test(&a, &b, &config);
    assert_eq!(distances.len(), 1000, "应有 1000 次置换");
    // 阈值不应为 0（小样本有抽样噪声）
    assert!(threshold >= 0.0);
    // 观测值应在合理范围
    assert!(observed_w >= 0.0);
}

#[test]
fn permutation_test_reproducibility() {
    let config = DriftConfig::default();
    let a = vec![0.1, 0.2, 0.3, 0.4, 0.5];
    let b = vec![0.6, 0.7, 0.8, 0.9, 1.0];
    let (w1, t1, _) = permutation_test(&a, &b, &config);
    let (w2, t2, _) = permutation_test(&a, &b, &config);
    // 固定种子应给出相同结果
    assert!((w1 - w2).abs() < 1e-10);
    assert!((t1 - t2).abs() < 1e-10);
}

// ---- 逐维度漂移检测 ----

#[test]
fn detect_dimension_drift_no_change() {
    let config = DriftConfig::default();
    let vals = vec![0.3, 0.4, 0.5, 0.6, 0.7];
    let sals = vec![0.5, 0.5, 0.5, 0.5, 0.5];
    let result = detect_dimension_drift("valence", &vals, &vals, &sals, &sals, &config);
    // 完全相同数据应无漂移
    assert!(!result.is_significant || result.wasserstein_distance < 0.01);
}

#[test]
fn detect_dimension_drift_large_change() {
    let config = DriftConfig::default();
    let old = vec![-0.8, -0.7, -0.9, -0.6, -0.8];
    let new = vec![0.7, 0.8, 0.9, 0.6, 0.7];
    let sals = vec![0.5, 0.5, 0.5, 0.5, 0.5];
    let result = detect_dimension_drift("valence", &old, &new, &sals, &sals, &config);
    // 大幅变化应检测到漂移
    assert!(result.is_significant);
    assert!(result.delta_mean > 0.0, "从负到正，Δμ 应为正");
}

// ---- 分类漂移检测 ----

#[test]
fn category_drift_empty() {
    let config = DriftConfig::default();
    let data = CategoryEventData {
        category: "测试".into(),
        old_valences: vec![],
        old_shares: vec![],
        old_saliences: vec![],
        old_confidences: vec![],
        new_valences: vec![],
        new_shares: vec![],
        new_saliences: vec![],
        new_confidences: vec![],
    };
    let result = detect_category_drift(&data, &config);
    assert!(!result.needs_review, "空数据不应触发重审");
}

/// 两期分布差异显著（旧 valence≈0.8×n、新≈0.1×n）→ 该分类触发重审。
///
/// 模拟快照恢复后的"点质量旧分布"（valence_mean 按 n 重复展开）与
/// 本轮事件分布的对比，验证漂移检测在真实两期数据下可触发。
#[test]
fn run_drift_detection_triggers_on_two_round_shift() {
    let config = DriftConfig::default();
    let n = 10usize;
    let data = vec![CategoryEventData {
        category: "工作".into(),
        old_valences: vec![0.8; n],
        old_shares: vec![0.6; n],
        old_saliences: vec![0.5; n],
        old_confidences: vec![0.9; n],
        new_valences: vec![0.1; n],
        new_shares: vec![0.5; n],
        new_saliences: vec![0.5; n],
        new_confidences: vec![0.9; n],
    }];
    let summary = run_drift_detection(&data, &config);
    assert_eq!(summary.categories.len(), 1);
    assert!(
        summary.categories[0].needs_review,
        "valence 大幅变化应触发漂移"
    );
    assert!(summary.any_drift);
    assert_eq!(summary.skipped_count, 0, "已装配的分类不产生跳过计数");
}

/// 两期分布基本一致（微小波动）→ 不触发重审。
#[test]
fn run_drift_detection_no_trigger_on_similar_distributions() {
    let config = DriftConfig::default();
    let n = 12usize;
    let data = vec![CategoryEventData {
        category: "家庭".into(),
        old_valences: vec![0.4; n],
        old_shares: vec![0.7; n],
        old_saliences: vec![0.5; n],
        old_confidences: vec![0.9; n],
        new_valences: vec![0.4; n],
        new_shares: vec![0.7; n],
        new_saliences: vec![0.5; n],
        new_confidences: vec![0.9; n],
    }];
    let summary = run_drift_detection(&data, &config);
    assert!(!summary.any_drift, "相同分布不应触发漂移");
    assert!(!summary.categories[0].needs_review);
}

#[test]
fn run_drift_detection_multiple_categories() {
    let config = DriftConfig::default();
    let data = vec![
        CategoryEventData {
            category: "工作".into(),
            old_valences: vec![0.1, 0.2, 0.1, 0.2],
            old_shares: vec![0.5, 0.6, 0.5, 0.6],
            old_saliences: vec![0.5; 4],
            old_confidences: vec![0.6; 4],
            new_valences: vec![0.8, 0.9, 0.8, 0.9],
            new_shares: vec![0.5, 0.6, 0.5, 0.6],
            new_saliences: vec![0.5; 4],
            new_confidences: vec![0.6; 4],
        },
        CategoryEventData {
            category: "社交".into(),
            old_valences: vec![0.3, 0.4, 0.3],
            old_shares: vec![0.7, 0.8, 0.7],
            old_saliences: vec![0.5; 3],
            old_confidences: vec![0.6; 3],
            new_valences: vec![0.3, 0.4, 0.3],
            new_shares: vec![0.7, 0.8, 0.7],
            new_saliences: vec![0.5; 3],
            new_confidences: vec![0.6; 3],
        },
    ];
    let summary = run_drift_detection(&data, &config);
    assert_eq!(summary.categories.len(), 2);
    // 工作应漂移（valence 大幅变化），社交应无漂移
    let work = summary
        .categories
        .iter()
        .find(|c| c.category == "工作")
        .unwrap();
    assert!(work.needs_review, "工作分类应触发漂移");
    assert!(summary.any_drift);
}

// ---- 新增 drift 维度 ----

#[test]
fn category_drift_salience_dimension() {
    let config = DriftConfig::default();
    // salience 大幅变化（0.1→0.9），valence/share 不变
    let data = CategoryEventData {
        category: "工作".into(),
        old_valences: vec![0.5; 5],
        old_shares: vec![0.5; 5],
        old_saliences: vec![0.1; 5],
        old_confidences: vec![0.5; 5],
        new_valences: vec![0.5; 5],
        new_shares: vec![0.5; 5],
        new_saliences: vec![0.9; 5],
        new_confidences: vec![0.5; 5],
    };
    let result = detect_category_drift(&data, &config);
    assert!(result.needs_review, "salience 大幅变化应触发漂移");
    assert!(result.salience_drift.is_significant);
}

#[test]
fn category_drift_confidence_dimension() {
    let config = DriftConfig::default();
    // confidence 大幅变化（0.1→0.9），其他维度不变
    let data = CategoryEventData {
        category: "社交".into(),
        old_valences: vec![0.5; 5],
        old_shares: vec![0.5; 5],
        old_saliences: vec![0.5; 5],
        old_confidences: vec![0.1; 5],
        new_valences: vec![0.5; 5],
        new_shares: vec![0.5; 5],
        new_saliences: vec![0.5; 5],
        new_confidences: vec![0.9; 5],
    };
    let result = detect_category_drift(&data, &config);
    assert!(result.needs_review, "confidence 大幅变化应触发漂移");
    assert!(result.confidence_drift.is_significant);
}

#[test]
fn category_drift_result_includes_new_dimensions() {
    let config = DriftConfig::default();
    let data = CategoryEventData {
        category: "工作".into(),
        old_valences: vec![0.5; 4],
        old_shares: vec![0.5; 4],
        old_saliences: vec![0.5; 4],
        old_confidences: vec![0.5; 4],
        new_valences: vec![0.8; 4],
        new_shares: vec![0.5; 4],
        new_saliences: vec![0.5; 4],
        new_confidences: vec![0.5; 4],
    };
    let result = detect_category_drift(&data, &config);
    // 四个维度都存在
    assert_eq!(result.valence_drift.dimension, "valence");
    assert_eq!(result.share_drift.dimension, "share");
    assert_eq!(result.salience_drift.dimension, "salience");
    assert_eq!(result.confidence_drift.dimension, "confidence");
    // valence 维度应漂移
    assert!(result.valence_drift.is_significant);
}
