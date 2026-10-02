//! crates/ramaria-memory/src/inference/causal/tests.rs - A8 因果链特征提取单元测试
//!
//! 设计特点:
//! - 覆盖基础四特征、时延/情绪扩展段与文本格式化渲染
//! - 使用固定基准时间与最小字段集构造事件，结果不依赖真实时钟
//! - 锁定旧路径文本逐字节快照，保证扩展段不改变未启用时的输出

use super::extract::MS_PER_DAY;
use super::graph::{detect_cycle_patterns, dfs_all_paths};
use super::*;
use ramaria_core::types::EventRelation;
use ramaria_core::types::EventRelationKind;
use ramaria_core::types::MemoryEvent;
use ramaria_core::types::Presentation;
use std::collections::HashMap;

/// 固定测试基准时间（Unix 毫秒），保证用例不依赖真实时钟、连续运行结果一致。
const TEST_NOW_MS: i64 = 1_760_000_000_000;

/// 创建测试用 MemoryEvent（最小字段集）。
fn make_event(id: i64, keywords: &str) -> MemoryEvent {
    let now = TEST_NOW_MS;
    MemoryEvent {
        id,
        persona_uid: "test-persona".into(),
        title: format!("Event {}", id),
        summary: format!("Summary of event {}", id),
        keywords: if keywords.is_empty() {
            None
        } else {
            Some(keywords.to_string())
        },
        participants: None,
        start: now,
        end: now,
        confidence: 0.8,
        salience: 0.7,
        valence: -0.3,
        presentation: Presentation::Mixed,
        share: 0.5,
        attitude: None,
        paraphrase: None,
        absorbed: 0,
        situation_strength: Some(3),
        motives: None,
        created_at: now,
        last_accessed_at: None,
        indexed_at: None,
        index_version: None,
    }
}

/// 创建测试用 MemoryEvent，支持自定义发生时间与 valence。
fn make_event_ts(id: i64, start_ms: i64, valence: f64) -> MemoryEvent {
    let mut ev = make_event(id, "");
    ev.start = start_ms;
    ev.end = start_ms + 60_000;
    ev.valence = valence;
    ev
}

/// 创建 CausedBy 关系。
fn make_causal(from_id: i64, to_id: i64) -> EventRelation {
    EventRelation {
        id: 0,
        from_id,
        to_id,
        kind: EventRelationKind::CausedBy,
        weight: 0.7,
        created_at: 1000,
    }
}

/// 创建非 CausedBy 关系（应被过滤）。
fn make_related(from_id: i64, to_id: i64) -> EventRelation {
    EventRelation {
        id: 0,
        from_id,
        to_id,
        kind: EventRelationKind::RelatedTo,
        weight: 0.5,
        created_at: 1000,
    }
}

// =========================================================
// extract_causal_features 测试
// =========================================================

/// extract_causal_features 无 CausedBy 关系时的默认结果验证。
#[test]
fn no_causal_relations_returns_default() {
    // 空关系列表
    let events = vec![make_event(1, "工作")];
    let relations: Vec<EventRelation> = vec![];
    let features = extract_causal_features(&events, &relations);
    assert_eq!(features.chain_length, 0);
    assert!(features.cyclic_patterns.is_empty());
    assert_eq!(features.total_causal_events, 0);
    // 仅 RelatedTo 关系（非因果）
    let events = vec![make_event(1, "工作"), make_event(2, "生活")];
    let relations = vec![make_related(1, 2)];
    let features = extract_causal_features(&events, &relations);
    assert_eq!(features.chain_length, 0);
    assert!(features.cyclic_patterns.is_empty());
}

#[test]
fn single_causal_link_chain_length_1() {
    // 压力 → 拖延
    let events = vec![make_event(1, "工作压力"), make_event(2, "拖延")];
    let relations = vec![make_causal(1, 2)];
    let features = extract_causal_features(&events, &relations);
    assert_eq!(features.chain_length, 1);
    assert_eq!(features.total_causal_events, 2);
    assert_eq!(features.total_causal_edges, 1);
}

#[test]
fn chain_of_three_length_2() {
    // 压力 → 拖延 → 自责
    let events = vec![
        make_event(1, "工作压力"),
        make_event(2, "拖延"),
        make_event(3, "自责"),
    ];
    let relations = vec![make_causal(1, 2), make_causal(2, 3)];
    let features = extract_causal_features(&events, &relations);
    assert_eq!(features.chain_length, 2);
}

#[test]
fn branching_chain_takes_longest() {
    //     1→2→3 (length 2)
    //     1→4     (length 1)
    let events = vec![
        make_event(1, "压力"),
        make_event(2, "拖延"),
        make_event(3, "自责"),
        make_event(4, "爆发"),
    ];
    let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(1, 4)];
    let features = extract_causal_features(&events, &relations);
    assert_eq!(features.chain_length, 2);
}

#[test]
fn cycle_detected() {
    // 压力 → 拖延 → 自责
    // 压力 → 拖延 → 自责 (第二次重复)
    let events = vec![
        make_event(1, "工作压力"),
        make_event(2, "拖延"),
        make_event(3, "自责"),
        make_event(4, "工作压力"),
        make_event(5, "拖延"),
        make_event(6, "自责"),
    ];
    let relations = vec![
        make_causal(1, 2),
        make_causal(2, 3),
        make_causal(4, 5),
        make_causal(5, 6),
    ];
    let features = extract_causal_features(&events, &relations);
    // 应该有循环模式被检测到
    assert!(
        !features.cyclic_patterns.is_empty(),
        "应该检测到重复的因果链模式"
    );
    assert!(features.cyclic_patterns.iter().any(|p| p.occurrences >= 2));
}

#[test]
fn non_causal_relations_filtered() {
    // CausedBy + RelatedTo 混合，只计 CausedBy
    let events = vec![
        make_event(1, "压力"),
        make_event(2, "拖延"),
        make_event(3, "发泄"),
    ];
    let relations = vec![
        make_causal(1, 2),
        make_related(2, 3), // 非因果，应被过滤
    ];
    let features = extract_causal_features(&events, &relations);
    assert_eq!(features.chain_length, 1);
    assert_eq!(features.total_causal_edges, 1);
}

#[test]
fn no_source_nodes_all_nodes_as_start() {
    // 环形: 1→2→1 (CausedBy 双向)
    let events = vec![make_event(1, "压力"), make_event(2, "拖延")];
    let relations = vec![make_causal(1, 2), make_causal(2, 1)];
    let features = extract_causal_features(&events, &relations);
    // 应该能找到路径
    assert!(features.chain_length >= 1);
}

#[test]
fn event_without_keywords_uses_fallback() {
    let event = make_event(1, ""); // empty → keywords=None via make_event
    let events = vec![event, make_event(2, "拖延")];
    let relations = vec![make_causal(1, 2)];
    let features = extract_causal_features(&events, &relations);
    assert_eq!(features.chain_length, 1);
}

// =========================================================
// format_causal_features_text 测试
// =========================================================

#[test]
fn format_empty_features_returns_empty() {
    let features = CausalChainFeatures::default();
    let text = format_causal_features_text(&features);
    assert!(text.is_empty());
}

#[test]
fn format_with_chain_length() {
    let features = CausalChainFeatures {
        chain_length: 3,
        total_causal_events: 5,
        total_causal_edges: 4,
        cyclic_patterns: vec![],
        latency_stats: CausalLatencyStats::default(),
        emotion_trend: CausalEmotionTrend::default(),
    };
    let text = format_causal_features_text(&features);
    assert!(text.contains("因果链分析"));
    assert!(text.contains("3 跳"));
    assert!(text.contains("主动驱动者"));
}

#[test]
fn format_with_cycle_patterns() {
    let features = CausalChainFeatures {
        chain_length: 2,
        total_causal_events: 6,
        total_causal_edges: 4,
        cyclic_patterns: vec![CyclePattern {
            description: "工作压力 → 拖延 → 自责".into(),
            occurrences: 2,
            event_categories: vec!["工作压力".into(), "拖延".into(), "自责".into()],
            relation_types: vec!["CausedBy".into(), "CausedBy".into()],
        }],
        latency_stats: CausalLatencyStats::default(),
        emotion_trend: CausalEmotionTrend::default(),
    };
    let text = format_causal_features_text(&features);
    assert!(text.contains("循环模式"));
    assert!(text.contains("工作压力 → 拖延 → 自责"));
    assert!(text.contains("出现 2 次"));
}

#[test]
fn format_short_chain_no_driver_hint() {
    let features = CausalChainFeatures {
        chain_length: 1,
        total_causal_events: 2,
        total_causal_edges: 1,
        cyclic_patterns: vec![],
        latency_stats: CausalLatencyStats::default(),
        emotion_trend: CausalEmotionTrend::default(),
    };
    let text = format_causal_features_text(&features);
    assert!(text.contains("1 跳"));
    assert!(text.contains("较为局部"));
    assert!(!text.contains("主动驱动者"));
}

// =========================================================
// extract_causal_features_extended 测试
// =========================================================

#[test]
fn extended_empty_relations_returns_default() {
    let events = vec![make_event_ts(1, 1000, 0.2)];
    let relations: Vec<EventRelation> = vec![];
    let features = extract_causal_features_extended(&events, &relations);
    assert_eq!(features.chain_length, 0);
    assert!(features.latency_stats.is_empty());
    assert!(features.emotion_trend.is_empty());
    // 与旧 extract 路径完全一致
    let legacy = extract_causal_features(&events, &relations);
    assert_eq!(legacy.chain_length, features.chain_length);
    assert!(features.latency_stats.sampled_edge_count == 0);
    assert_eq!(
        format_causal_features_text(&legacy),
        format_causal_features_text(&features)
    );
}

#[test]
fn extended_missing_time_no_panic() {
    // start=0 视为时间缺失 + valence 中性场景：不 panic，扩展字段合理缺省
    let events = vec![
        make_event_ts(1, 0, 0.0),
        make_event_ts(2, 0, 0.0),
        make_event_ts(3, 0, 0.0),
    ];
    let relations = vec![make_causal(1, 2)];
    let features = extract_causal_features_extended(&events, &relations);
    assert_eq!(features.chain_length, 1);
    // start=0 被过滤为时间缺失 → 时延无有效采样
    assert!(features.latency_stats.is_empty());
    assert!(features.latency_stats.excluded_edge_count >= 1);
    // 路径上 valence 全为 0，仍构成 2 节点采样
    assert_eq!(features.emotion_trend.sampled_node_count, 2);
}

#[test]
fn extended_latency_stats_correct() {
    // 链 1→2→3→4，时延分别为 1 天 / 2 天 / 7 天。
    // 时间起点取 MS_PER_DAY 起（>0 保证不被当作时间缺失剔除）。
    let events = vec![
        make_event_ts(1, MS_PER_DAY, 0.1),
        make_event_ts(2, 2 * MS_PER_DAY, 0.2),
        make_event_ts(3, 4 * MS_PER_DAY, 0.3),
        make_event_ts(4, 11 * MS_PER_DAY, 0.4),
    ];
    let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(3, 4)];
    let features = extract_causal_features_extended(&events, &relations);
    assert_eq!(features.total_causal_edges, 3);
    let s = &features.latency_stats;
    assert_eq!(s.sampled_edge_count, 3);
    assert_eq!(s.excluded_edge_count, 0);
    assert_eq!(s.min_ms, Some(MS_PER_DAY as f64));
    assert_eq!(s.max_ms, Some((7 * MS_PER_DAY) as f64));
    assert_eq!(s.median_ms, Some((2 * MS_PER_DAY) as f64));
    let expected_mean = (MS_PER_DAY + 2 * MS_PER_DAY + 7 * MS_PER_DAY) as f64 / 3.0;
    assert!((s.mean_ms.unwrap() - expected_mean).abs() < 1.0);
    assert_eq!(s.within_1d_count, 1);
    assert_eq!(s.within_7d_count, 2);
    assert_eq!(s.over_7d_count, 0);
}

#[test]
fn extended_latency_drops_negative_and_missing() {
    // 1→2 时延接近 1 天（有效）；2→1 负时延剔除；2→4 事件缺失剔除。
    let events = vec![
        make_event_ts(1, 5_000, 0.1),
        make_event_ts(2, MS_PER_DAY, 0.2),
        make_event_ts(3, 0, 0.3), // start=0 → 时间缺失（但未参与出边）
    ];
    let relations = vec![
        make_causal(1, 2),
        make_causal(2, 1), // 负时延（2 晚于 1，反向则负）
        make_causal(2, 4), // to_id=4 不在 events → 缺失
    ];
    let features = extract_causal_features_extended(&events, &relations);
    let s = &features.latency_stats;
    assert_eq!(s.sampled_edge_count, 1);
    assert_eq!(s.excluded_edge_count, 2);
    assert_eq!(s.min_ms, Some((MS_PER_DAY - 5_000) as f64));
    assert_eq!(s.max_ms, Some((MS_PER_DAY - 5_000) as f64));
}

#[test]
fn extended_emotion_trend_increasing() {
    // 链 valence: -0.6 → -0.2 → 0.3 → 0.7（逐级增强）
    let events = vec![
        make_event_ts(1, 0, -0.6),
        make_event_ts(2, MS_PER_DAY, -0.2),
        make_event_ts(3, 2 * MS_PER_DAY, 0.3),
        make_event_ts(4, 3 * MS_PER_DAY, 0.7),
    ];
    let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(3, 4)];
    let features = extract_causal_features_extended(&events, &relations);
    let t = &features.emotion_trend;
    assert_eq!(t.sampled_node_count, 4);
    assert!(t.mean_valence.unwrap() > 0.0);
    assert!(t.head_tail_delta.unwrap() > 1.2);
    assert!(t.linear_slope.unwrap() > 0.3);
    assert_eq!(t.direction, "逐级增强");
    assert_eq!(t.polarity_flips, 1);
}

#[test]
fn extended_emotion_trend_decreasing() {
    // 链 valence: 0.8 → 0.4 → -0.2 → -0.6（逐级衰减）
    let events = vec![
        make_event_ts(1, 0, 0.8),
        make_event_ts(2, MS_PER_DAY, 0.4),
        make_event_ts(3, 2 * MS_PER_DAY, -0.2),
        make_event_ts(4, 3 * MS_PER_DAY, -0.6),
    ];
    let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(3, 4)];
    let features = extract_causal_features_extended(&events, &relations);
    let t = &features.emotion_trend;
    assert_eq!(t.direction, "逐级衰减");
    assert!(t.head_tail_delta.unwrap() < -1.2);
}

#[test]
fn extended_emotion_trend_flapping() {
    // 链 valence: 0.5 → -0.6 → 0.7 → -0.5 → 0.4（净变化小、多次翻转 → 波动）
    let events = vec![
        make_event_ts(1, 0, 0.5),
        make_event_ts(2, MS_PER_DAY, -0.6),
        make_event_ts(3, 2 * MS_PER_DAY, 0.7),
        make_event_ts(4, 3 * MS_PER_DAY, -0.5),
        make_event_ts(5, 4 * MS_PER_DAY, 0.4),
    ];
    let relations = vec![
        make_causal(1, 2),
        make_causal(2, 3),
        make_causal(3, 4),
        make_causal(4, 5),
    ];
    let features = extract_causal_features_extended(&events, &relations);
    let t = &features.emotion_trend;
    assert_eq!(t.polarity_flips, 4);
    assert_eq!(t.direction, "波动");
}

#[test]
fn extended_emotion_path_too_short_is_empty() {
    // 只有 1 条边 = 2 节点，已满足最小采样；但若事件缺失使有效节点不足则空
    let events = vec![make_event_ts(1, 0, 0.5)];
    let relations = vec![make_causal(1, 2)]; // id2 无事件 → 有效节点 1 个
    let features = extract_causal_features_extended(&events, &relations);
    assert!(features.emotion_trend.is_empty());
    // 时延同样因 to 端缺失而为空
    assert!(features.latency_stats.is_empty());
}

#[test]
fn extended_shortest_path_pick_longest_deterministic() {
    // 分叉图: 1→2→3 与 1→4。情绪应沿最长路径 1→2→3 采样。
    let events = vec![
        make_event_ts(1, 0, -0.5),
        make_event_ts(2, MS_PER_DAY, -0.2),
        make_event_ts(3, 2 * MS_PER_DAY, 0.3),
        make_event_ts(4, MS_PER_DAY, 0.8),
    ];
    let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(1, 4)];
    let features = extract_causal_features_extended(&events, &relations);
    assert_eq!(features.chain_length, 2);
    let t = &features.emotion_trend;
    assert_eq!(t.sampled_node_count, 3);
    assert!(t.head_tail_delta.unwrap() > 0.7);
    assert_eq!(t.direction, "逐级增强");
}

// =========================================================
// format 扩展段渲染测试
// =========================================================

#[test]
fn format_with_empty_extended_omits_sections() {
    let features = CausalChainFeatures {
        chain_length: 2,
        total_causal_events: 3,
        total_causal_edges: 2,
        cyclic_patterns: vec![],
        latency_stats: CausalLatencyStats::default(),
        emotion_trend: CausalEmotionTrend::default(),
    };
    let text = format_causal_features_text(&features);
    assert!(text.contains("因果链分析"));
    assert!(!text.contains("因果边时延分布"));
    assert!(!text.contains("情绪沿链走势"));
}

#[test]
fn format_with_latency_renders_section() {
    let features = CausalChainFeatures {
        chain_length: 1,
        total_causal_events: 2,
        total_causal_edges: 2,
        cyclic_patterns: vec![],
        latency_stats: CausalLatencyStats {
            sampled_edge_count: 1,
            excluded_edge_count: 1,
            mean_ms: Some(2.0 * MS_PER_DAY as f64),
            median_ms: Some(2.0 * MS_PER_DAY as f64),
            min_ms: Some(MS_PER_DAY as f64),
            max_ms: Some(3.0 * MS_PER_DAY as f64),
            within_1d_count: 1,
            within_7d_count: 0,
            over_7d_count: 0,
        },
        emotion_trend: CausalEmotionTrend::default(),
    };
    let text = format_causal_features_text(&features);
    assert!(text.contains("因果边时延分布"));
    assert!(text.contains("剔除 1 条"));
    assert!(!text.contains("情绪沿链走势"));
}

#[test]
fn format_with_emotion_renders_section() {
    let features = CausalChainFeatures {
        chain_length: 1,
        total_causal_events: 2,
        total_causal_edges: 1,
        cyclic_patterns: vec![],
        latency_stats: CausalLatencyStats::default(),
        emotion_trend: CausalEmotionTrend {
            sampled_node_count: 2,
            mean_valence: Some(0.1),
            head_tail_delta: Some(0.6),
            linear_slope: Some(0.6),
            polarity_flips: 0,
            direction: "逐级增强".to_string(),
        },
    };
    let text = format_causal_features_text(&features);
    assert!(!text.contains("因果边时延分布"));
    assert!(text.contains("情绪沿链走势"));
    assert!(text.contains("逐级增强"));
}

#[test]
fn format_extended_full_features_from_chain() {
    let events = vec![
        make_event_ts(1, 0, -0.6),
        make_event_ts(2, MS_PER_DAY, -0.2),
        make_event_ts(3, 3 * MS_PER_DAY, 0.3),
        make_event_ts(4, 10 * MS_PER_DAY, 0.7),
    ];
    let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(3, 4)];
    let features = extract_causal_features_extended(&events, &relations);
    let text = format_causal_features_text(&features);
    assert!(text.contains("因果边时延分布"));
    assert!(text.contains("情绪沿链走势"));
    assert!(text.contains("逐级增强"));
}

/// v1.7 等价锁定：旧路径（extract_causal_features）产出文本不含扩展段，
/// 且与 v1.7 旧格式逐字节一致（未因扩展段渲染引入任何前缀/后缀改动）。
#[test]
fn legacy_format_text_snapshot_unchanged() {
    // 旧路径仅计算基础特征，扩展字段恒为空 → format 输出不含扩展段
    let events = vec![
        make_event(1, "工作压力"),
        make_event(2, "拖延"),
        make_event(3, "自责"),
    ];
    let relations = vec![make_causal(1, 2), make_causal(2, 3)];
    let legacy = extract_causal_features(&events, &relations);
    let text = format_causal_features_text(&legacy);
    assert_eq!(legacy.chain_length, 2);
    assert!(legacy.latency_stats.is_empty());
    assert!(legacy.emotion_trend.is_empty());
    assert!(!text.contains("因果边时延分布"));
    assert!(!text.contains("情绪沿链走势"));

    // 逐字节快照：2 跳、2 条边、无循环模式的旧格式文本
    let expected = "## 因果链分析 (A8)\n\n因果网络概况: 3 个事件通过 2 条因果关系连接，最长因果链为 2 跳。\n解读提示: 中等因果链提示用户行为有一定连锁效应。\n\n";
    assert_eq!(text, expected);
}

// =========================================================
// dfs_all_paths 测试
// =========================================================

#[test]
fn dfs_single_node_no_edges() {
    let adjacency: HashMap<i64, Vec<(i64, f64)>> = HashMap::new();
    let cat: HashMap<i64, String> = [(1, "work".into())].into();
    let paths = dfs_all_paths(1, &adjacency, &cat);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], vec![1]);
}

#[test]
fn dfs_linear_chain() {
    let adjacency: HashMap<i64, Vec<(i64, f64)>> =
        HashMap::from([(1, vec![(2, 0.7)]), (2, vec![(3, 0.8)])]);
    let cat: HashMap<i64, String> = HashMap::new();
    let paths = dfs_all_paths(1, &adjacency, &cat);
    assert!(paths.iter().any(|p| p == &vec![1, 2, 3]));
}

#[test]
fn dfs_branching() {
    let adjacency: HashMap<i64, Vec<(i64, f64)>> = HashMap::from([(1, vec![(2, 0.7), (3, 0.8)])]);
    let cat: HashMap<i64, String> = HashMap::new();
    let paths = dfs_all_paths(1, &adjacency, &cat);
    assert!(paths.iter().any(|p| p == &vec![1, 2]));
    assert!(paths.iter().any(|p| p == &vec![1, 3]));
}

// =========================================================
// detect_cycle_patterns 测试
// =========================================================

#[test]
fn no_cycle_with_single_path() {
    let paths = vec![vec![1, 2, 3]];
    let cat: HashMap<i64, String> = [(1, "A".into()), (2, "B".into()), (3, "C".into())].into();
    let patterns = detect_cycle_patterns(&paths, &cat);
    assert!(patterns.is_empty());
}

#[test]
fn detects_repeated_pattern() {
    // 两个完全相同的路径
    let paths = vec![vec![1, 2, 3], vec![4, 5, 6]];
    let cat: HashMap<i64, String> = [
        (1, "压力".into()),
        (2, "拖延".into()),
        (3, "自责".into()),
        (4, "压力".into()),
        (5, "拖延".into()),
        (6, "自责".into()),
    ]
    .into();
    let patterns = detect_cycle_patterns(&paths, &cat);
    assert!(!patterns.is_empty());
    assert!(patterns.iter().any(|p| p.occurrences >= 2));
}

#[test]
fn empty_paths_returns_empty() {
    let paths: Vec<Vec<i64>> = vec![];
    let cat: HashMap<i64, String> = HashMap::new();
    let patterns = detect_cycle_patterns(&paths, &cat);
    assert!(patterns.is_empty());
}
