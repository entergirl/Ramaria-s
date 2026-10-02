//! crates/ramaria-memory/src/rrf/tests.rs - //! crates/ramaria-memory/src/rrf.rs - Ramaria RRF 多通道融合模块单元测试
//!
//! 设计特点:
//! - 位于 rrf 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 rrf.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

/// 创建测试用通道结果: [(id, score), ...]，按 score 降序。
fn make_channel<I: Clone + std::hash::Hash + Eq>(results: Vec<(I, f64)>) -> ChannelResult<I> {
    ChannelResult { results }
}

// --- penalty_rank ---

#[test]
fn penalty_rank_formula() {
    assert!((penalty_rank(5) - 11.0).abs() < 0.001);
    assert!((penalty_rank(10) - 21.0).abs() < 0.001);
}

// --- rrf_single_channel ---

#[test]
fn single_channel_basic() {
    let config = RrfConfig {
        top_k: 3,
        ..Default::default()
    };
    let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7), ("d", 0.6)]);

    let fused = rrf_single_channel(&vec, &config);
    assert_eq!(fused.len(), 3);
    assert_eq!(fused[0].doc_id, "a");
    assert_eq!(fused[1].doc_id, "b");
    assert_eq!(fused[2].doc_id, "c");

    // rank=1 → 1/(60+1) ≈ 0.01639
    assert!((fused[0].rrf_score - 0.01639).abs() < 0.0001);
}

#[test]
fn single_channel_empty() {
    let config = RrfConfig::default();
    let vec = make_channel::<&str>(vec![]);
    let fused = rrf_single_channel(&vec, &config);
    assert!(fused.is_empty());
}

// --- rrf_two_channels ---

#[test]
fn two_channels_basic_fusion() {
    let config = RrfConfig {
        top_k: 3,
        ..Default::default()
    };
    // 向量: a > b > c
    let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7)]);
    // BM25: b > d > a
    let bm25 = make_channel(vec![("b", 0.9), ("d", 0.8), ("a", 0.7)]);

    let fused = rrf_two_channels(&vec, &bm25, &config);
    assert_eq!(fused.len(), 3);

    // b 在两个通道都排高位，应排第一
    assert_eq!(fused[0].doc_id, "b");
}

#[test]
fn two_channels_doc_in_both_ranks_higher() {
    let config = RrfConfig {
        top_k: 2,
        ..Default::default()
    };
    let vec = make_channel(vec![("x", 0.9)]);
    let bm25 = make_channel(vec![("x", 0.8), ("y", 0.7)]);

    let fused = rrf_two_channels(&vec, &bm25, &config);
    assert_eq!(fused.len(), 2);
    // x 在两个通道都出现，应排第一
    assert_eq!(fused[0].doc_id, "x");
}

#[test]
fn two_channels_penalty_applied() {
    let config = RrfConfig {
        top_k: 2,
        ..Default::default()
    };
    let vec = make_channel(vec![("only_vec", 0.9)]);
    let bm25 = make_channel(vec![("only_bm25", 0.8)]);

    let fused = rrf_two_channels(&vec, &bm25, &config);
    assert_eq!(fused.len(), 2);

    // both should have penalty on the other channel
    for result in &fused {
        if result.doc_id == "only_vec" {
            assert!(
                result.bm25_raw_score.is_none(),
                "only_vec should not have BM25 score"
            );
        }
        if result.doc_id == "only_bm25" {
            assert!(
                result.vector_raw_score.is_none(),
                "only_bm25 should not have vector score"
            );
        }
    }
}

#[test]
fn two_channels_empty_inputs() {
    let config = RrfConfig::default();
    let vec = make_channel::<&str>(vec![]);
    let bm25 = make_channel::<&str>(vec![]);

    let fused = rrf_two_channels(&vec, &bm25, &config);
    assert!(fused.is_empty());
}

#[test]
fn two_channels_one_empty() {
    let config = RrfConfig {
        top_k: 2,
        ..Default::default()
    };
    let vec = make_channel(vec![("a", 0.9), ("b", 0.8)]);
    let bm25 = make_channel::<&str>(vec![]);

    let fused = rrf_two_channels(&vec, &bm25, &config);
    assert_eq!(fused.len(), 2);
    // 仅向量通道有结果，BM25 用惩罚排名
    assert_eq!(fused[0].doc_id, "a");
}

// --- rrf_fuse (三通道) ---

#[test]
fn three_channels_basic_fusion() {
    let config = RrfConfig {
        top_k: 3,
        ..Default::default()
    };
    // 向量: a > b > c
    let vec = make_channel(vec![("a", 0.95), ("b", 0.85), ("c", 0.75)]);
    // BM25: b > d > a
    let bm25 = make_channel(vec![("b", 0.9), ("d", 0.8), ("a", 0.7)]);
    // 图谱: c > b > e
    let graph = make_channel(vec![("c", 0.9), ("b", 0.8), ("e", 0.7)]);

    let fused = rrf_fuse(&vec, &bm25, &graph, &config);
    assert_eq!(fused.len(), 3);
    // b 在三通道都排在高位，应排第一
    assert_eq!(fused[0].doc_id, "b");
}

#[test]
fn three_channels_doc_in_all_three_wins() {
    let config = RrfConfig {
        top_k: 1,
        ..Default::default()
    };
    let vec = make_channel(vec![("common", 0.9)]);
    let bm25 = make_channel(vec![("common", 0.8)]);
    let graph = make_channel(vec![("common", 0.7)]);

    let fused = rrf_fuse(&vec, &bm25, &graph, &config);
    assert_eq!(fused.len(), 1);
    assert_eq!(fused[0].doc_id, "common");
    assert!(fused[0].vector_raw_score.is_some());
    assert!(fused[0].bm25_raw_score.is_some());
    assert!(fused[0].graph_raw_score.is_some());
}

#[test]
fn three_channels_empty() {
    let config = RrfConfig::default();
    let vec = make_channel::<&str>(vec![]);
    let bm25 = make_channel::<&str>(vec![]);
    let graph = make_channel::<&str>(vec![]);

    let fused = rrf_fuse(&vec, &bm25, &graph, &config);
    assert!(fused.is_empty());
}

// --- rank 公式验证 ---

#[test]
fn rrf_score_formula_verification() {
    // rank 1 在向量通道，不在其他通道
    // RRF = 1/(60+1) + 1.0/(60+penalty) + 0.8/(60+penalty)
    // penalty = 5*2+1 = 11
    // RRF = 1/61 + 1.0/71 + 0.8/71 ≈ 0.01639 + 0.01408 + 0.01127 ≈ 0.04175
    let config = RrfConfig {
        top_k: 5,
        ..Default::default()
    };
    let vec = make_channel(vec![("x", 0.9)]);
    let bm25 = make_channel::<&str>(vec![]);
    let graph = make_channel::<&str>(vec![]);

    let fused = rrf_fuse(&vec, &bm25, &graph, &config);
    assert_eq!(fused.len(), 1);
    let expected = 1.0 / 61.0 + 1.0 / 71.0 + 0.8 / 71.0;
    assert!(
        (fused[0].rrf_score - expected).abs() < 0.0001,
        "expected {:.6}, got {:.6}",
        expected,
        fused[0].rrf_score
    );
}

// --- top_k 截断 ---

#[test]
fn top_k_truncation() {
    let config = RrfConfig {
        top_k: 2,
        ..Default::default()
    };
    let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7), ("d", 0.6)]);
    let bm25 = make_channel::<&str>(vec![]);
    let graph = make_channel::<&str>(vec![]);

    let fused = rrf_fuse(&vec, &bm25, &graph, &config);
    assert_eq!(fused.len(), 2, "should be truncated to top_k=2");
}

// --- 分数排序验证 ---

#[test]
fn results_sorted_descending() {
    let config = RrfConfig {
        top_k: 10,
        ..Default::default()
    };
    let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7)]);
    let bm25 = make_channel(vec![("d", 0.6), ("e", 0.5)]);
    let graph = make_channel(vec![("f", 0.4)]);

    let fused = rrf_fuse(&vec, &bm25, &graph, &config);
    for window in fused.windows(2) {
        assert!(
            window[0].rrf_score >= window[1].rrf_score,
            "results should be sorted descending"
        );
    }
}

// --- rrf_fuse_with_keyword (四通道) ---

/// 关键词通道独有命中可把文档提升进 top（向量前 k 之外的文档进入结果）。
#[test]
fn keyword_channel_lifts_doc_into_top() {
    // 向量返回恰好 top_k=4 条；关键词通道独有第 5 篇文档 E（rank 1）
    let config = RrfConfig {
        top_k: 4,
        ..Default::default()
    };
    let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7), ("d", 0.6)]);
    let bm25 = make_channel::<&str>(vec![]);
    let graph = make_channel::<&str>(vec![]);
    let keyword = make_channel(vec![("e", 0.9)]);

    let fused = rrf_fuse_with_keyword(Some(&vec), Some(&bm25), Some(&graph), &keyword, &config);
    let ids: Vec<&str> = fused.iter().map(|f| f.doc_id).collect();
    assert!(
        ids.contains(&"e"),
        "关键词通道独有命中应进入 top，实际 {ids:?}"
    );
    // 关键词通道无 raw 字段暴露（e 非向量/BM25/图谱命中，其通道分数仅计入 rrf）
    let e = fused.iter().find(|f| f.doc_id == "e").unwrap();
    assert!(e.vector_raw_score.is_none());
    assert!(e.bm25_raw_score.is_none());
    assert!(e.graph_raw_score.is_none());
}

/// keyword_weight 参与融合：权重拉低后关键词独有命中退出 top（权重改变排序）。
#[test]
fn keyword_weight_changes_membership() {
    let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7), ("d", 0.6)]);
    let keyword = make_channel(vec![("e", 0.9)]);

    let high = RrfConfig {
        top_k: 4,
        keyword_weight: 1.0,
        ..Default::default()
    };
    let fused_high = rrf_fuse_with_keyword(Some(&vec), None, None, &keyword, &high);
    assert!(
        fused_high.iter().any(|f| f.doc_id == "e"),
        "keyword_weight=1.0 时关键词独有命中应进入 top"
    );

    let low = RrfConfig {
        top_k: 4,
        keyword_weight: 0.05,
        ..Default::default()
    };
    let fused_low = rrf_fuse_with_keyword(Some(&vec), None, None, &keyword, &low);
    assert!(
        !fused_low.iter().any(|f| f.doc_id == "e"),
        "keyword_weight 过低时关键词独有命中应退出 top（权重参与融合）"
    );
}

/// 仅关键词通道有数据 → 单通道语义仍可返回该文档。
#[test]
fn keyword_only_channel_returns_doc() {
    let config = RrfConfig {
        top_k: 5,
        ..Default::default()
    };
    let keyword = make_channel(vec![("kw_only", 0.8), ("kw2", 0.6)]);

    let fused = rrf_fuse_with_keyword(None, None, None, &keyword, &config);
    assert_eq!(fused.len(), 2);
    assert_eq!(fused[0].doc_id, "kw_only");
    assert!(fused[0].rrf_score > 0.0);
}

/// 关键词通道权重默认值 1.0（与向量同权重）。
#[test]
fn keyword_weight_default_is_one() {
    assert!((RrfConfig::default().keyword_weight - 1.0).abs() < f64::EPSILON);
}

// --- rrf_fuse_optional（按通道身份取权重） ---

/// 二通道融合按通道身份取权重：向量缺席、BM25 与图谱同时命中时，
/// 图谱项必须使用 graph_weight（修复前会被当成 bm25_weight 计权）。
#[test]
fn fusion_uses_channel_identity_weights() {
    let config = RrfConfig {
        top_k: 5,
        bm25_weight: 0.4,
        graph_weight: 0.9,
        ..Default::default()
    };
    let bm25 = make_channel(vec![("x", 0.9)]);
    let graph = make_channel(vec![("y", 0.8)]);
    let fused = rrf_fuse_optional(
        &OptionalChannels {
            vector: None,
            bm25: Some(&bm25),
            graph: Some(&graph),
            keyword: None,
        },
        &config,
    );
    // penalty = 5*2+1 = 11 → k+penalty = 71；命中项 k+rank = 61
    let x = fused.iter().find(|f| f.doc_id == "x").unwrap();
    let y = fused.iter().find(|f| f.doc_id == "y").unwrap();
    assert!((x.rrf_score - (0.4 / 61.0 + 0.9 / 71.0)).abs() < 1e-9);
    assert!((y.rrf_score - (0.4 / 71.0 + 0.9 / 61.0)).abs() < 1e-9);
    assert_eq!(
        fused[0].doc_id, "y",
        "graph_weight 更高时图谱独有命中应排前"
    );
    // 原始分数槽位按通道身份写入（不因调用位置错位）
    assert!(x.bm25_raw_score.is_some() && x.graph_raw_score.is_none());
    assert!(y.graph_raw_score.is_some() && y.bm25_raw_score.is_none());
}

/// 通道缺席不产生惩罚项：仅向量通道存在时分数为 1/(k+rank)。
#[test]
fn absent_channels_contribute_nothing() {
    let config = RrfConfig {
        top_k: 3,
        ..Default::default()
    };
    let vec = make_channel(vec![("a", 0.9), ("b", 0.8)]);
    let fused = rrf_fuse_optional(
        &OptionalChannels {
            vector: Some(&vec),
            bm25: None,
            graph: None,
            keyword: None,
        },
        &config,
    );
    assert_eq!(fused.len(), 2);
    assert!((fused[0].rrf_score - 1.0 / 61.0).abs() < 1e-9);
}
