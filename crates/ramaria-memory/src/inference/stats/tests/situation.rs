//! crates/ramaria-memory/src/inference/stats/tests/situation.rs - 情境强度乘数
//!
//! 设计特点:
//! - 由 父测试模块 以 mod situation; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 情境强度乘数
// =========================================================

/// situation_multiplier 全分支参数化验证：
/// - None / 3 / 非法值(0,6,100) → 中性 1.0
/// - 弱情境 (1,2) → 放大 1.5
/// - 强情境 (4,5) → 抑制 0.5
#[test]
fn situation_multiplier_cases() {
    let cases = [
        (None, 1.0),
        (Some(3), 1.0),
        (Some(1), 1.5),
        (Some(2), 1.5),
        (Some(4), 0.5),
        (Some(5), 0.5),
        (Some(0), 1.0),
        (Some(6), 1.0),
        (Some(100), 1.0),
    ];
    for (strength, expected) in cases {
        assert!(
            (situation_multiplier(strength) - expected).abs() < 1e-10,
            "strength={strength:?} 期望 {expected}",
        );
    }
}
