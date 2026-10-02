//! tests/inference/promotion.rs - 三轨动态准入
//!
//! 设计特点:
//! - 由 tests/inference.rs 以 mod promotion; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 三轨动态准入测试
// =========================================================

#[test]
fn tentative_promotion_across_batches() {
    // 使用固定时间戳确保跨批次检测正确
    // 注意: MemoryEvent::new() 内部设置 created_at=now_ms(), 需要显式覆盖
    let base_time: i64 = 1700000000000; // 固定 Unix 毫秒时间戳

    let mut e1 = MemoryEvent::new(
        "persona-m4".into(),
        "T1".into(),
        "s1".into(),
        base_time - 1000,
        base_time,
    );
    e1.keywords = Some("工作,冲突".into());
    e1.confidence = 0.5;
    e1.salience = 0.4;
    e1.created_at = base_time; // 批次 1: base_time 时刻

    let mut e2 = MemoryEvent::new(
        "persona-m4".into(),
        "T2".into(),
        "s2".into(),
        base_time + 8 * 3_600_000 - 1000,
        base_time + 8 * 3_600_000,
    );
    e2.keywords = Some("工作,冲突".into());
    e2.confidence = 0.55;
    e2.salience = 0.35;
    e2.created_at = base_time + 8 * 3_600_000; // 批次 2: 8 小时后（> 6h 阈值）

    let tentative = vec![e1, e2];
    let confirmed: Vec<MemoryEvent> = vec![];
    let config = TentativePromotionConfig::default();

    let result = promote_tentative_events(&tentative, &confirmed, &config);

    // 两个事件关键词相同 Jaccard=1.0, 来自不同批次 (8h > 6h) → 应提升
    assert!(
        result.promoted_count > 0,
        "跨批次 tentative 应被提升: promoted_count={}, remaining_count={}",
        result.promoted_count,
        result.remaining_count
    );
    for ev in &result.promoted {
        assert!((ev.confidence - 0.6).abs() < 1e-10, "提升后置信度应为 0.6");
    }
}
