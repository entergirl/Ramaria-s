//! tests/inference/phase_a.rs - 校准权重链
//!
//! 设计特点:
//! - 由 tests/inference.rs 以 mod phase_a; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 校准权重链测试
// =========================================================

#[test]
fn phase_a_calibrated_weights_reduces_tentative_weight() {
    let events = make_diverse_events();
    let config = StatsConfig::default(); // use_calibrated_weights = true
    let summary = run_phase_a_stats(&events, &config);

    // discarded 事件应被排除
    assert_eq!(summary.total_events_in, 8);
    assert_eq!(summary.discarded_count, 1);
    assert_eq!(summary.total_events_filtered, 7); // 5 confirmed + 2 tentative

    // tentative 事件以半权重参与
    assert_eq!(summary.confirmed_count, 5);
    assert_eq!(summary.tentative_count, 2);
    // 三轨一致性: active events = confirmed + tentative
    assert_eq!(
        summary.confirmed_count + summary.tentative_count,
        summary.total_events_filtered,
        "active events = confirmed + tentative"
    );

    // 校准权重下 n_eff 应小于原始事件数（tentative 半权重 + situation_strength 影响）
    let total_n_eff: f64 = summary.categories.iter().map(|c| c.n_eff).sum();
    assert!(
        total_n_eff < 7.0,
        "校准权重下 n_eff({}) 应小于原始事件数 7",
        total_n_eff
    );
    assert!(
        total_n_eff > 1.0,
        "n_eff({}) 应 > 1.0（至少 confirmed 事件有效）",
        total_n_eff
    );
}

#[test]
fn phase_a_v12_compat_path_disables_calibrated_weights() {
    let events = make_diverse_events();
    let config = StatsConfig {
        use_calibrated_weights: false,
        ..Default::default()
    };
    let summary = run_phase_a_stats(&events, &config);

    assert_eq!(summary.total_events_in, 8);
    let total_n_eff: f64 = summary.categories.iter().map(|c| c.n_eff).sum();
    assert!(total_n_eff > 0.0, "路径应有有效 n_eff");
}
