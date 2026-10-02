//! crates/ramaria-memory/src/inference/stats/tests/admission.rs - Tentative 跨批次自动提升
//!
//! 设计特点:
//! - 由 父测试模块 以 mod admission; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// Tentative 跨批次复现自动提升
// =========================================================

/// 创建一个带有自定义 created_at 的事件（用于批次检测）。
fn make_event_with_time(
    title: &str,
    keywords: Option<&str>,
    confidence: f64,
    salience: f64,
    created_at: i64,
) -> MemoryEvent {
    let mut ev = make_event(
        title,
        "摘要",
        keywords,
        confidence,
        salience,
        0.0,
        0.5,
        Presentation::Mixed,
        None,
    );
    ev.created_at = created_at;
    ev
}

/// are_different_batches 各分支参数化验证：跨批次 / 间隔内 / created_at 为 0 保守同批次。
#[test]
fn are_different_batches_cases() {
    let config = TentativePromotionConfig::default(); // min_batch_interval_hours = 6.0
    let base_time = 1700000000000i64; // 某个 Unix 毫秒时间戳
    let a = make_event_with_time("E1", Some("工作"), 0.5, 0.6, base_time);
    // 8 小时后 → 不同批次
    let b = make_event_with_time("E2", Some("工作"), 0.5, 0.6, base_time + 8 * 3600 * 1000);
    assert!(are_different_batches(&a, &b, &config));
    // 3 小时后 → 同批次
    let b = make_event_with_time("E2", Some("工作"), 0.5, 0.6, base_time + 3 * 3600 * 1000);
    assert!(!are_different_batches(&a, &b, &config));
    // created_at == 0 → 保守视为同批次
    let c = make_event_with_time("E3", Some("工作"), 0.5, 0.6, 0);
    let d = make_event_with_time("E4", Some("工作"), 0.5, 0.6, base_time);
    assert!(!are_different_batches(&c, &d, &config));
}

#[test]
fn promote_tentative_cross_batch_promotes() {
    let config = TentativePromotionConfig::default();
    let base_time = 1700000000000i64;
    // 两条同簇（工作）tentative 事件，来自不同批次，关键词相似度高 → 应提升
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议, 压力"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("工作, 会议, 项目"),
            0.55,
            0.5,
            base_time + 8 * 3600 * 1000,
        ),
    ];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 2);
    assert_eq!(result.remaining_count, 0);
    // 提升后的置信度应为 0.6
    for event in &result.promoted {
        assert!(
            (event.confidence - 0.6).abs() < 1e-10,
            "提升后 confidence 应为 0.6，实际为 {}",
            event.confidence
        );
    }
}

#[test]
fn promote_tentative_single_event_not_promoted() {
    let config = TentativePromotionConfig::default();
    // 单条 tentative 事件 → 簇大小不足，不提升
    let tentative = vec![make_event_with_time(
        "E1",
        Some("工作, 会议"),
        0.5,
        0.6,
        1700000000000i64,
    )];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 0);
    assert_eq!(result.remaining_count, 1);
}

#[test]
fn promote_tentative_same_batch_not_promoted() {
    let config = TentativePromotionConfig::default();
    let base_time = 1700000000000i64;
    // 两条同簇事件，但来自同一批次（时间间隔不足）→ 不提升
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("工作, 项目"),
            0.55,
            0.5,
            base_time + 3600 * 1000, // 仅 1 小时后
        ),
    ];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 0);
    assert_eq!(result.remaining_count, 2);
}

#[test]
fn promote_tentative_low_keyword_similarity_not_promoted() {
    let config = TentativePromotionConfig::default();
    let base_time = 1700000000000i64;
    // 两条事件来自不同批次，但关键词无交集 → Jaccard=0，不提升
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("社交, 聚会"),
            0.55,
            0.5,
            base_time + 8 * 3600 * 1000,
        ),
    ];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 0);
    assert_eq!(result.remaining_count, 2);
}

#[test]
fn promote_tentative_mixed_clusters() {
    let config = TentativePromotionConfig::default();
    let base_time = 1700000000000i64;
    // 工作簇：2 条，跨批次，关键词相似 → 应提升
    // 社交簇：1 条 → 不提升
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议, 压力"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("工作, 会议, 项目"),
            0.55,
            0.5,
            base_time + 8 * 3600 * 1000,
        ),
        make_event_with_time("E3", Some("社交, 聚会"), 0.5, 0.4, base_time),
    ];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 2, "工作簇应提升");
    assert_eq!(result.remaining_count, 1, "社交簇不提升");
    // 验证提升的是工作簇
    let promoted_titles: Vec<&str> = result.promoted.iter().map(|e| e.title.as_str()).collect();
    assert!(promoted_titles.contains(&"E1"));
    assert!(promoted_titles.contains(&"E2"));
    assert_eq!(result.remaining_tentative[0].title, "E3");
}

#[test]
fn promote_tentative_empty_input() {
    let config = TentativePromotionConfig::default();
    let tentative: Vec<MemoryEvent> = vec![];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 0);
    assert_eq!(result.remaining_count, 0);
    assert!(result.promoted.is_empty());
    assert!(result.remaining_tentative.is_empty());
}

#[test]
fn promote_tentative_custom_min_cluster_size() {
    let config = TentativePromotionConfig {
        min_cluster_size: 3,
        ..Default::default()
    };
    let base_time = 1700000000000i64;
    // 3 条同簇事件，跨批次 → 满足 min_cluster_size=3
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议, 压力"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("工作, 会议, 项目"),
            0.55,
            0.5,
            base_time + 8 * 3600 * 1000,
        ),
        make_event_with_time(
            "E3",
            Some("工作, 会议, 汇报"),
            0.5,
            0.4,
            base_time + 16 * 3600 * 1000,
        ),
    ];
    let confirmed: Vec<MemoryEvent> = vec![];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    assert_eq!(result.promoted_count, 3);
    assert_eq!(result.remaining_count, 0);
}

#[test]
fn promote_tentative_respects_confirmed_list() {
    // confirmed 列表存在但不应影响提升逻辑（当前为签名保留参数）
    let config = TentativePromotionConfig::default();
    let base_time = 1700000000000i64;
    let tentative = vec![
        make_event_with_time("E1", Some("工作, 会议"), 0.5, 0.6, base_time),
        make_event_with_time(
            "E2",
            Some("工作, 会议, 项目"),
            0.55,
            0.5,
            base_time + 8 * 3600 * 1000,
        ),
    ];
    let confirmed = vec![make_event(
        "E0_confirmed",
        "已有确认事件",
        Some("工作"),
        0.9,
        0.8,
        0.5,
        0.5,
        Presentation::Mixed,
        None,
    )];
    let result = promote_tentative_events(&tentative, &confirmed, &config);

    // confirmed 列表存在时提升逻辑不受影响
    assert_eq!(result.promoted_count, 2);
}
