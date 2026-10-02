//! crates/ramaria-memory/src/event/batcher/graph/tests.rs - //! crates/ramaria-memory/src/event/batcher/graph.rs - TopicBatcher 关键词图单元测试
//!
//! 设计特点:
//! - 位于 event::batcher::graph 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 graph.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::keyword::KeywordToken;
use uuid::Uuid;

/// 辅助函数：创建带关键词的 L1Item
fn make_l1(keywords: Vec<&str>, salience: f64) -> L1Item {
    L1Item {
        id: Uuid::new_v4(),
        summary: format!("summary_{}", keywords.join("_")),
        keywords: keywords.into_iter().filter_map(KeywordToken::new).collect(),
        evidence_notes: vec![],
        embedding: None,
        salience,
        created_at: 1_000_000,
    }
}

// ---- KeywordGraph::new ----

#[test]
fn graph_new_is_empty() {
    let g = KeywordGraph::new();
    assert_eq!(g.node_count(), 0);
    assert_eq!(g.edge_count(), 0);
}

// ---- build_jaccard_graph ----

#[test]
fn build_graph_empty_input() {
    let items: Vec<L1Item> = vec![];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    assert_eq!(g.node_count(), 0);
    assert_eq!(g.edge_count(), 0);
    assert!(g.find_connected_components().is_empty());
}

#[test]
fn build_graph_single_node() {
    let items = vec![make_l1(vec!["工作", "压力"], 0.5)];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    assert_eq!(g.node_count(), 1);
    assert_eq!(g.edge_count(), 0);
    let comps = g.find_connected_components();
    assert_eq!(comps.len(), 1);
    assert_eq!(comps[0], vec![0]);
}

#[test]
fn build_graph_fully_connected() {
    // 三个节点共享大量关键词，应全连通
    let items = vec![
        make_l1(vec!["工作", "压力", "加班"], 0.5),
        make_l1(vec!["工作", "压力", "倦怠"], 0.6),
        make_l1(vec!["工作", "加班", "倦怠"], 0.7),
    ];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    assert_eq!(g.node_count(), 3);
    // 三节点全连通：应有 C(3,2)=3 条边
    assert_eq!(g.edge_count(), 3);
    let comps = g.find_connected_components();
    assert_eq!(comps.len(), 1);
    assert_eq!(comps[0].len(), 3);
}

#[test]
fn build_graph_all_isolated() {
    // 三组关键词完全不相交 → 三个孤立节点
    let items = vec![
        make_l1(vec!["工作"], 0.5),
        make_l1(vec!["休闲"], 0.5),
        make_l1(vec!["学习"], 0.5),
    ];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    assert_eq!(g.node_count(), 3);
    assert_eq!(g.edge_count(), 0);
    let comps = g.find_connected_components();
    assert_eq!(comps.len(), 3);
    // 每个分量含 1 个节点
    for comp in &comps {
        assert_eq!(comp.len(), 1);
    }
}

#[test]
fn build_graph_mixed_components() {
    // A（工作压力）↔ B（工作倦怠）  Jaccard = 1/3 ≈ 0.33 > 0.2，有边
    // C（休闲娱乐）↔ D（娱乐放松）Jaccard = 1/3 ≈ 0.33 > 0.2，有边
    // A↔C 无交集，无边
    let items = vec![
        make_l1(vec!["工作", "压力"], 0.5),
        make_l1(vec!["工作", "倦怠"], 0.5),
        make_l1(vec!["休闲", "娱乐"], 0.5),
        make_l1(vec!["娱乐", "放松"], 0.5),
    ];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    assert_eq!(g.node_count(), 4);
    assert_eq!(g.edge_count(), 2); // A↔B, C↔D
    let comps = g.find_connected_components();
    assert_eq!(comps.len(), 2);
    // 每个分量应有 2 个节点
    let sizes: Vec<usize> = comps.iter().map(|c| c.len()).collect();
    assert!(sizes.contains(&2));
}

#[test]
fn build_graph_high_threshold_reduces_edges() {
    // 低阈值（0.1）时三个节点可能全连通
    // 高阈值（0.9）时应全部孤立
    let items = vec![
        make_l1(vec!["工作", "压力", "加班"], 0.5),
        make_l1(vec!["工作", "压力", "倦怠"], 0.6),
        make_l1(vec!["工作", "加班", "倦怠"], 0.7),
    ];
    // 阈值为 0.1: 全部连通
    let g_low = KeywordGraph::build_jaccard_graph(&items, 0.1);
    let comps_low = g_low.find_connected_components();
    assert_eq!(comps_low.len(), 1, "低阈值应全部连通");

    // 阈值为 0.9: 全部孤立（因为所有对 Jaccard < 0.9）
    let g_high = KeywordGraph::build_jaccard_graph(&items, 0.9);
    let comps_high = g_high.find_connected_components();
    assert_eq!(comps_high.len(), 3, "高阈值应全部孤立");
}

#[test]
fn build_graph_repeated_keywords_dont_affect_jaccard() {
    // 重复关键词不影响 Jaccard 计算（集合去重）
    let items = vec![
        make_l1(vec!["工作", "工作", "压力"], 0.5),
        make_l1(vec!["工作", "倦怠"], 0.5),
    ];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    // Jaccard( {工作,压力}, {工作,倦怠} ) = 1/3 ≈ 0.33 > 0.2，有边
    assert!(g.edge_count() > 0, "重复关键词不应影响 Jaccard 计算");
}

// ---- find_connected_components ----

#[test]
fn connected_components_empty_graph() {
    let g = KeywordGraph::new();
    let comps = g.find_connected_components();
    assert!(comps.is_empty());
}

#[test]
fn connected_components_chain() {
    // A↔B B↔C (链式结构)
    let items = vec![
        make_l1(vec!["工作"], 0.5),
        make_l1(vec!["工作", "压力"], 0.5),
        make_l1(vec!["压力"], 0.5),
    ];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();
    assert_eq!(comps.len(), 1, "链式连接应属同一分量");
    assert_eq!(comps[0].len(), 3);
}

// ---- component_l1_indices ----

#[test]
fn component_l1_indices_mapping() {
    let items = vec![
        make_l1(vec!["A"], 0.5),
        make_l1(vec!["B"], 0.5),
        make_l1(vec!["A", "B"], 0.5), // 连接前两者
    ];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();
    assert_eq!(comps.len(), 1);

    let indices = g.component_l1_indices(&comps[0]);
    assert_eq!(indices.len(), 3);
    // l1_index 应映射回原始 item 索引
    for (node_idx, &l1_idx) in indices.iter().enumerate() {
        assert_eq!(g.nodes[comps[0][node_idx]].l1_index, l1_idx);
    }
}

// =========================================================
// 模块度 Q 二分拆分测试
// =========================================================

/// 分量大小 ≤ max_cluster_size（含恰好相等）时保持不变。
#[test]
fn split_small_or_exact_component_unchanged() {
    // 2 节点，max=25 → 不拆分
    let items = vec![
        make_l1(vec!["工作", "压力"], 0.5),
        make_l1(vec!["工作", "倦怠"], 0.5),
    ];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();
    let result = split_large_components(&g, comps, 25, 0.3);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len(), 2);
    // 5 节点恰好等于 max=5 → 不拆分
    let items: Vec<L1Item> = (0..5)
        .map(|i| make_l1(vec!["工作", "压力"], 0.5 + i as f64 * 0.1))
        .collect();
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();
    assert_eq!(comps.len(), 1);
    let result = split_large_components(&g, comps, 5, 0.3);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len(), 5);
}

/// 无边的连通分量（孤立节点组）不可拆分
#[test]
fn bisect_no_edges_returns_none() {
    // 单节点分量
    let items = vec![make_l1(vec!["工作"], 0.5)];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();
    assert_eq!(comps.len(), 1);
    let result = try_bisect_component(&g, &comps[0], 0.3);
    assert!(result.is_none(), "单个节点不可二分");
}

/// 高 Q_min 阻止拆分
#[test]
fn high_q_min_prevents_split() {
    // 双社区结构：组内 Jaccard≈0.67，组间经 b1/b2 桥接（Jaccard=0.25）。
    // 该结构在 q_min=0.0 下必然可拆分（模块度 Q≈0.14 ≥ 0.0），
    // 但 Q 达不到 q_min=0.99 → 拆分被阻止。
    let items = vec![
        // Group A: work-related + 2 bridge keywords
        make_l1(vec!["工作", "加班", "会议", "b1", "b2"], 0.5),
        make_l1(vec!["工作", "加班", "报告", "b1", "b2"], 0.5),
        make_l1(vec!["工作", "会议", "报告", "b1", "b2"], 0.5),
        // Group B: leisure-related + 2 bridge keywords
        make_l1(vec!["休闲", "旅游", "摄影", "b1", "b2"], 0.5),
        make_l1(vec!["休闲", "旅游", "美食", "b1", "b2"], 0.5),
        make_l1(vec!["休闲", "摄影", "美食", "b1", "b2"], 0.5),
    ];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();
    assert_eq!(comps.len(), 1, "bridge 关键词应连接两组为一个分量");

    // Q_min=0.0 应接受拆分，且两组均非空
    let result_low = try_bisect_component(&g, &comps[0], 0.0);
    let (a, b) = result_low.expect("双社区结构在 q_min=0.0 时应成功拆分");
    assert!(!a.is_empty() && !b.is_empty(), "拆分两组均不应为空");

    // Q_min=0.99 应拒绝拆分（真实图的 Q 几乎不可能达到 0.99）
    let result_high = try_bisect_component(&g, &comps[0], 0.99);
    assert!(
        result_high.is_none(),
        "Q_min=0.99 应阻止拆分，实际: {:?}",
        result_high
    );
}

/// 二分结果两组均非空
#[test]
fn bisect_produces_two_nonempty_groups() {
    // 双社区结构（组内 Jaccard≈0.67，组间 0.25）：6 节点单连通分量，
    // 贪心二分在 q_min=0.0 下必然成功，且两组均非空。
    let items = vec![
        // Group A: work-related + 2 bridge keywords
        make_l1(vec!["工作", "加班", "会议", "b1", "b2"], 0.5),
        make_l1(vec!["工作", "加班", "报告", "b1", "b2"], 0.5),
        make_l1(vec!["工作", "会议", "报告", "b1", "b2"], 0.5),
        // Group B: leisure-related + 2 bridge keywords
        make_l1(vec!["休闲", "旅游", "摄影", "b1", "b2"], 0.5),
        make_l1(vec!["休闲", "旅游", "美食", "b1", "b2"], 0.5),
        make_l1(vec!["休闲", "摄影", "美食", "b1", "b2"], 0.5),
    ];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();
    assert_eq!(comps.len(), 1, "bridge 关键词应连接两组为一个分量");
    assert_eq!(comps[0].len(), 6);

    let result = try_bisect_component(&g, &comps[0], 0.0);
    let (a, b) = result.expect("双社区结构应可二分");
    assert!(!a.is_empty(), "组 A 不应为空");
    assert!(!b.is_empty(), "组 B 不应为空");
    assert_eq!(a.len() + b.len(), comps[0].len(), "节点总数应不变");
}

/// 递归拆分：有社区结构的图应被拆分
///
/// 两组各 3 节点，组内关键词高度重叠（Jaccard ≥ 0.5），
/// 组间通过 2 个 bridge 关键词保持连通（Jaccard ≈ 0.2-0.33）。
/// max_cluster_size=2 下应能触发拆分。
#[test]
fn recursive_split_community_structure() {
    let items = vec![
        // Group A: work-related + 2 bridge keywords
        make_l1(vec!["工作", "加班", "会议", "b1", "b2"], 0.5),
        make_l1(vec!["工作", "加班", "报告", "b1", "b2"], 0.5),
        make_l1(vec!["工作", "会议", "报告", "b1", "b2"], 0.5),
        // Group B: leisure-related + 2 bridge keywords
        make_l1(vec!["休闲", "旅游", "摄影", "b1", "b2"], 0.5),
        make_l1(vec!["休闲", "旅游", "美食", "b1", "b2"], 0.5),
        make_l1(vec!["休闲", "摄影", "美食", "b1", "b2"], 0.5),
    ];
    let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();

    // 两组通过 b1/b2 共享 → 应为单个连通分量
    assert_eq!(comps.len(), 1, "bridge 关键词应连接两组为一个分量");
    assert_eq!(comps[0].len(), 6);

    // max=2，社区结构应触发拆分
    let result = split_large_components(&g, comps, 2, 0.0);

    let total: usize = result.iter().map(|c| c.len()).sum();
    assert_eq!(total, 6, "拆分后节点总数应不变");

    // 应产生多于原始 1 个的分量（社区结构被识别）
    assert!(
        result.len() > 1,
        "社区结构图应触发至少一次拆分（实际分量数: {}）",
        result.len()
    );
}
