//! crates/ramaria-memory/src/inference/stats/tests/category.rs - 主分类提取
//!
//! 设计特点:
//! - 由 父测试模块 以 mod category; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 主分类提取
// =========================================================

/// extract_primary_category 各分支参数化验证：多关键词取首个 / 单关键词 / None / 空串。
#[test]
fn extract_primary_category_cases() {
    let cases = [
        (Some("工作, 会议, 紧张"), "工作"),
        (Some("家庭"), "家庭"),
        (None, "未分类"),
        (Some(""), "未分类"),
    ];
    for (keywords, expected) in cases {
        let ev = make_event(
            "E1",
            "摘要",
            keywords,
            0.8,
            0.5,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
        );
        assert_eq!(extract_primary_category(&ev), expected);
    }
}
