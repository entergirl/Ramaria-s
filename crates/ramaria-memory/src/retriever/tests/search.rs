//! crates/ramaria-memory/src/retriever/tests/search.rs - 三通道检索基础
//!
//! 设计特点:
//! - 由 父测试模块 以 mod search; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

#[test]
fn bm25_search_finds_results() {
    let r = make_test_retriever();
    let req = SearchRequest {
        query: "Rust".to_string(),
        persona_uid: None,
        top_k: 10,
        filter_share: false,
    };
    let results = r.search(&req, None);
    assert!(!results.is_empty());
    // 应找到至少一条包含 "Rust" 的结果
    assert!(results.iter().any(|sr| sr.doc_summary.contains("Rust")));
}

/// 向量通道接线：经 `index_l1_with_vector`（L1 增量路径）与 app 全量 rebuild
/// 路径（`index_l2` + 向量索引实例直接 `add`）写入的 L1/L2 文档，
/// 在带 query 向量的检索中被真实命中。
#[test]
fn vector_channel_finds_indexed_l1_l2() {
    use crate::vector::{VectorIndex, make_vector_label};
    let mut r = Retriever::new();
    let l1_id = uuid::Uuid::new_v4();
    r.index_l1_with_vector(
        &L1DocView {
            id: l1_id,
            summary: "用户喜欢打篮球，每周三晚上去球场".to_string(),
            keywords: None,
            persona_uid: Some("user-0001".to_string()),
            created_at: 1000,
            salience: 0.8,
            last_accessed_at: None,
        },
        Some(vec![1.0, 0.0, 0.0]),
    );
    let l2_id: i64 = 7;
    // L2 向量写入按 app 全量路径：`index_l2` 入 BM25/内存 + 向量索引实例直接 add。
    r.index_l2(&L2DocView {
        id: l2_id,
        title: "篮球比赛".to_string(),
        summary: "参加了周末篮球比赛".to_string(),
        keywords: None,
        attitude: None,
        paraphrase: None,
        persona_uid: "user-0001".to_string(),
        share: 0.9,
        confidence: 0.9,
        created_at: 2000,
        salience: 0.7,
    });
    let label = make_vector_label("l2", &l2_id.to_string());
    r.vector_mut().add(&label, vec![0.9, 0.1, 0.0], 2000);

    let req = SearchRequest {
        query: "篮球".to_string(),
        persona_uid: None,
        top_k: 10,
        filter_share: false,
    };
    // 查询向量与 L1 文档向量高度相似（cos≈1.0），向量通道必须命中
    let results = r.search(&req, Some(&[1.0, 0.0, 0.0]));
    assert!(
        results
            .iter()
            .any(|sr| sr.layer == "l1" && sr.doc_summary.contains("篮球")),
        "L1 文档应通过向量通道被检索到（此前零产出缺陷）"
    );
    // L2 文档（cos≈0.994 > min_similarity=0.0）也应被检索到
    assert!(
        results
            .iter()
            .any(|sr| sr.layer == "l2" && sr.doc_summary.contains("篮球")),
        "L2 文档应通过向量通道被检索到"
    );
}

/// 向量通道降级：无 query 向量（embedding 不可用）→ 向量通道跳过，
/// BM25 仍可命中（回归红线 2：embedding 不可用不阻塞检索）。
#[test]
fn vector_channel_skipped_without_query_vector() {
    let mut r = Retriever::new();
    r.index_l1_with_vector(
        &L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: "用户喜欢打篮球".to_string(),
            keywords: None,
            persona_uid: Some("user-0001".to_string()),
            created_at: 1000,
            salience: 0.8,
            last_accessed_at: None,
        },
        Some(vec![1.0, 0.0, 0.0]),
    );
    let req = SearchRequest {
        query: "篮球".to_string(),
        persona_uid: None,
        top_k: 10,
        filter_share: false,
    };
    // query_vec = None → 向量通道跳过（embedding 不可用），BM25 通道仍应命中
    let results = r.search(&req, None);
    // 回归红线：embedding 不可用不阻塞检索（BM25 命中仍返回）
    assert!(
        !results.is_empty(),
        "无 query 向量时 BM25 应命中（embedding 不可用不阻塞检索）"
    );
    assert!(
        results.iter().any(|sr| sr.doc_summary.contains("篮球")),
        "BM25 应命中篮球相关文档"
    );
    // 向量通道被跳过：结果不应携带向量分数
    assert!(
        results.iter().all(|sr| sr.vector_score.is_none()),
        "无 query 向量时结果不应携带向量分数"
    );
}

/// enable_vector=false 时向量通道关闭：文档向量与查询向量高度相似（向量是唯一命中通道）
/// 时，关闭后不再返回该命中；开启时返回（向量命中不回退）。
#[test]
fn enable_vector_false_disables_vector_channel() {
    let mut r = Retriever::new();
    // 文档摘要不含查询 "篮球" 的 bigram（BM25 无法命中），仅靠向量通道召回
    r.index_l1_with_vector(
        &L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: "用户喜欢户外徒步与摄影".to_string(),
            keywords: None,
            persona_uid: Some("user-0001".to_string()),
            created_at: 1000,
            salience: 0.8,
            last_accessed_at: None,
        },
        Some(vec![1.0, 0.0, 0.0]),
    );

    let req = SearchRequest {
        query: "篮球".to_string(),
        persona_uid: None,
        top_k: 10,
        filter_share: false,
    };

    // 开启向量通道（默认）→ 向量命中返回
    let on = r.search(&req, Some(&[1.0, 0.0, 0.0]));
    assert!(!on.is_empty(), "enable_vector=true 时向量命中应返回");
    assert!(
        on.iter().any(|sr| sr.vector_score.is_some()),
        "向量通道开启时结果应携带向量分数"
    );

    // 关闭向量通道 → 无 BM25/图谱可命中 → 空结果（向量命中不再产出）
    r.config_mut().enable_vector = false;
    let off = r.search(&req, Some(&[1.0, 0.0, 0.0]));
    assert!(off.is_empty(), "enable_vector=false 时不得返回向量通道命中");
}

/// enable_vector=true（默认）下带向量命中的检索结果不回退（向量命中仍产出）。
#[test]
fn enable_vector_default_keeps_vector_hits() {
    let mut r = Retriever::new();
    // 仅向量可命中的文档（BM25 无法命中）
    r.index_l1_with_vector(
        &L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: "用户喜欢户外徒步与摄影".to_string(),
            keywords: None,
            persona_uid: Some("user-0001".to_string()),
            created_at: 1000,
            salience: 0.8,
            last_accessed_at: None,
        },
        Some(vec![1.0, 0.0, 0.0]),
    );
    let results = r.search(
        &SearchRequest {
            query: "篮球".to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        },
        Some(&[1.0, 0.0, 0.0]),
    );
    assert!(
        !results.is_empty() && results.iter().any(|sr| sr.vector_score.is_some()),
        "默认配置下向量命中不得回退"
    );
}

/// enable_graph=false 时图谱通道关闭：仅图谱可命中的查询不再产出；
/// 开启时同一查询命中图谱实体（开关短路生效的对照断言）。
#[test]
fn enable_graph_false_disables_graph_channel() {
    let mut r = Retriever::new();
    // 仅图谱有数据：文档集合为空（BM25 无索引文档），无查询向量、无关键词注入
    r.graph_mut()
        .load(&[(1, "陶艺展".to_string(), "event".to_string())], &[]);

    let req = SearchRequest {
        query: "陶艺展".to_string(),
        persona_uid: None,
        top_k: 10,
        filter_share: false,
    };

    // 开启图谱通道（默认）→ 图谱实体命中返回
    let on = r.search(&req, None);
    assert!(
        on.iter()
            .any(|sr| sr.layer == "graph" && sr.doc_summary.contains("陶艺展")),
        "enable_graph=true 时图谱实体应被检索到: {on:?}"
    );
    assert!(
        on.iter().any(|sr| sr.graph_score.is_some()),
        "图谱通道开启时结果应携带图谱分数"
    );

    // 关闭图谱通道 → 其余通道均无可命中数据 → 空结果（图谱命中不再产出）
    r.config_mut().enable_graph = false;
    let off = r.search(&req, None);
    assert!(
        off.iter().all(|sr| sr.layer != "graph"),
        "enable_graph=false 时不得返回图谱通道命中: {off:?}"
    );
    assert!(off.is_empty(), "其余通道无数据时关闭图谱应为空结果");
}

#[test]
fn search_filters_by_persona_uid() {
    let r = make_test_retriever();
    let req = SearchRequest {
        query: "Rust".to_string(),
        persona_uid: Some("user-0002".to_string()),
        top_k: 10,
        filter_share: false,
    };
    let results = r.search(&req, None);
    // user-0002 没有任何文档
    assert!(results.is_empty());
}

#[test]
fn search_top_k_truncation() {
    let mut r = make_test_retriever();
    // 添加更多文档
    for i in 0..10 {
        r.index_l1(&L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: format!("文档{} 测试内容", i),
            keywords: Some("测试".to_string()),
            persona_uid: Some("user-0001".to_string()),
            created_at: 3000 + i as i64,
            salience: 0.5,
            last_accessed_at: None,
        });
    }

    let req = SearchRequest {
        query: "测试".to_string(),
        persona_uid: None,
        top_k: 3,
        filter_share: false,
    };
    let results = r.search(&req, None);
    assert!(results.len() <= 3);
}

#[test]
fn search_empty_query_bm25_returns_empty() {
    let r = make_test_retriever();
    let req = SearchRequest {
        query: "".to_string(),
        persona_uid: None,
        top_k: 10,
        filter_share: false,
    };
    let results = r.search(&req, None);
    // BM25 空查询返回空，向量无 query_vec，图谱无实体
    // 三个通道均为空 → 结果为空
    assert!(results.is_empty());
}

#[test]
fn search_bm25_only_disables_other_channels() {
    let mut r = make_test_retriever();
    r.config_mut().enable_vector = false;
    r.config_mut().enable_graph = false;

    let req = SearchRequest {
        query: "火锅".to_string(),
        persona_uid: None,
        top_k: 10,
        filter_share: false,
    };
    let results = r.search(&req, None);
    assert!(!results.is_empty());
    assert!(results.iter().any(|sr| sr.doc_summary.contains("火锅")));
}

#[test]
fn rebuild_bm25_preserves_data() {
    let mut r = make_test_retriever();
    // 先搜索确认有结果
    let req = SearchRequest {
        query: "火锅".to_string(),
        persona_uid: None,
        top_k: 10,
        filter_share: false,
    };
    let before = r.search(&req, None);
    assert!(!before.is_empty());

    // 重建 BM25 索引（清空后从 l1_docs/l2_docs 重新构建）→ 检索结果应保持不变
    r.rebuild_bm25();
    let after = r.search(&req, None);
    assert!(!after.is_empty(), "重建后仍应能检索到火锅文档");
    assert!(
        after.iter().any(|sr| sr.doc_summary.contains("火锅")),
        "重建后结果应仍包含火锅文档"
    );
    // 文档总数不变（重建只重建索引，不丢失文档）
    assert_eq!(r.doc_count(), 3);
}

#[test]
fn clear_removes_all() {
    let mut r = make_test_retriever();
    assert!(r.doc_count() > 0);

    r.clear();
    assert_eq!(r.doc_count(), 0);
    assert_eq!(r.bm25_index.doc_count(), 0);
}

#[test]
fn doc_count_reflects_indexed_docs() {
    let r = make_test_retriever();
    // 2 L1 + 1 L2
    assert_eq!(r.doc_count(), 3);
}

#[test]
fn search_result_contains_required_fields() {
    let r = make_test_retriever();
    let req = SearchRequest {
        query: "Rust".to_string(),
        persona_uid: None,
        top_k: 5,
        filter_share: false,
    };
    let results = r.search(&req, None);
    for sr in &results {
        assert!(!sr.layer.is_empty());
        assert!(!sr.doc_summary.is_empty());
        assert!(sr.rrf_score > 0.0);
        assert!(sr.created_at > 0);
    }
}
