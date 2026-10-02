//! crates/ramaria-memory/src/retriever/tests/keyword.rs - 关键词镜像通道融合
//!
//! 设计特点:
//! - 由 父测试模块 以 mod keyword; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 关键词镜像通道（第四通道）融合测试
// =========================================================

/// 构造仅含指定 L1 文档的最小检索器（不启用 BM25/图谱，便于隔离通道贡献）。
///
/// 文档均索引进 `l1_docs`（label 解析必需）；向量索引是否写入由调用方控制。
fn keyword_test_retriever() -> Retriever {
    let mut r = Retriever::new();
    r.config_mut().enable_bm25 = false;
    r.config_mut().enable_graph = false;
    r
}

/// 关键词通道独有命中把不在向量前 k 的文档提升进结果（通道贡献真实生效）。
#[test]
fn keyword_channel_lifts_doc_into_top_k() {
    use crate::vector::{VectorIndex, make_vector_label};
    let mut r = keyword_test_retriever();
    // D1..D4 带向量（向量通道 top 4）；D5 无向量、关键词通道独有命中
    let d1 = uuid::Uuid::new_v4();
    let d2 = uuid::Uuid::new_v4();
    let d3 = uuid::Uuid::new_v4();
    let d4 = uuid::Uuid::new_v4();
    let d5 = uuid::Uuid::new_v4();
    for (id, created) in [(d1, 1000), (d2, 2000), (d3, 3000), (d4, 4000), (d5, 5000)] {
        r.index_l1(&l1_view_doc(id, "仅用于 label 解析的占位摘要", created));
    }
    for id in [d1, d2, d3, d4] {
        let label = make_vector_label("l1", &id.to_string());
        r.vector_mut().add(&label, vec![1.0, 0.0], 0);
    }

    let req = SearchRequest {
        query: "占位".to_string(),
        persona_uid: Some("user-0001".to_string()),
        top_k: 4,
        filter_share: false,
    };
    let keyword_hits = vec![(format!("L1:{d5}"), 0.9)];

    let with_keyword =
        r.search_with_keyword_hits(&req, Some(&[1.0, 0.0]), Some(keyword_hits.clone()));
    assert!(
        with_keyword
            .iter()
            .any(|sr| matches!(sr.doc_id, DocId::L1(id) if id == d5)),
        "关键词通道独有命中应把 D5 提升进 top_k"
    );

    // 关闭关键词通道 → 与不带关键词通道的检索完全一致（D5 不出现）
    r.config_mut().enable_keyword_channel = false;
    let disabled = r.search_with_keyword_hits(&req, Some(&[1.0, 0.0]), Some(keyword_hits));
    let base = r.search(&req, Some(&[1.0, 0.0]));
    let ids_disabled: Vec<uuid::Uuid> = disabled
        .iter()
        .filter_map(|sr| match sr.doc_id {
            DocId::L1(id) => Some(id),
            _ => None,
        })
        .collect();
    let ids_base: Vec<uuid::Uuid> = base
        .iter()
        .filter_map(|sr| match sr.doc_id {
            DocId::L1(id) => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(
        ids_disabled, ids_base,
        "enable_keyword_channel=false 结果应与仅三通道完全一致"
    );
    assert!(!ids_disabled.contains(&d5), "关闭关键词通道后 D5 不应出现");
}

/// 仅关键词通道有数据（向量/BM25/图谱全关闭）仍能返回该文档（单通道可用）。
#[test]
fn keyword_only_channel_returns_doc() {
    let mut r = keyword_test_retriever();
    r.config_mut().enable_vector = false;
    let d = uuid::Uuid::new_v4();
    r.index_l1(&l1_view_doc(d, "关键词命中的记忆", 1000));

    let req = SearchRequest {
        query: "无关查询".to_string(),
        persona_uid: None,
        top_k: 5,
        filter_share: false,
    };
    let results = r.search_with_keyword_hits(
        &req,
        Some(&[1.0, 0.0]),
        Some(vec![(format!("L1:{d}"), 0.8)]),
    );
    assert!(
        results
            .iter()
            .any(|sr| matches!(sr.doc_id, DocId::L1(id) if id == d)),
        "仅关键词通道有数据也应返回该文档"
    );
    assert!(
        results.iter().all(|sr| sr.vector_score.is_none()),
        "向量通道关闭时结果不应带向量分数"
    );
}

/// 空关键词命中列表 → 与不注入关键词通道完全一致（静默降级）。
#[test]
fn empty_keyword_hits_keeps_three_channel_behavior() {
    use crate::vector::VectorIndex;
    let r = keyword_test_retriever();
    let d = uuid::Uuid::new_v4();
    let mut rr = r;
    rr.index_l1(&l1_view_doc(d, "向量命中记忆", 1000));
    {
        let label = crate::vector::make_vector_label("l1", &d.to_string());
        rr.vector_mut().add(&label, vec![1.0, 0.0], 0);
    }
    let req = SearchRequest {
        query: "查询".to_string(),
        persona_uid: None,
        top_k: 5,
        filter_share: false,
    };
    let base = rr.search(&req, Some(&[1.0, 0.0]));
    let empty_kw = rr.search_with_keyword_hits(&req, Some(&[1.0, 0.0]), Some(Vec::new()));
    let none_kw = rr.search_with_keyword_hits(&req, Some(&[1.0, 0.0]), None);
    let summarize = |v: Vec<SearchResult>| -> Vec<(String, f64)> {
        v.into_iter()
            .map(|sr| (sr.doc_id.to_string(), sr.rrf_score))
            .collect()
    };
    assert_eq!(summarize(empty_kw), summarize(base.clone()));
    assert_eq!(summarize(none_kw), summarize(base));
}
