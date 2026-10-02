//! crates/ramaria-memory/src/inference/stats/tests/config.rs - CalibratedWeightConfig 默认值
//!
//! 设计特点:
//! - 由 父测试模块 以 mod config; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// CalibratedWeightConfig::default() 验证
// =========================================================

#[test]
fn calibrated_weight_config_defaults() {
    let config = CalibratedWeightConfig::default();
    assert!((config.salience_exponent - 1.0).abs() < 1e-10);
    assert!((config.recurrence_boost_max - 0.30).abs() < 1e-10);
    assert!((config.intensity_boost_max - 0.20).abs() < 1e-10);
    assert!((config.mention_boost_max - 0.15).abs() < 1e-10);
    assert_eq!(config.min_sources_for_full_support, 3);
    assert!((config.tentative_weight_factor - 0.5).abs() < 1e-10);
}
