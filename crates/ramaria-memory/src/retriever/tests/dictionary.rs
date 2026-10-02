//! crates/ramaria-memory/src/retriever/tests/dictionary.rs - BM25 词典增强分词
//!
//! 设计特点:
//! - 由 父测试模块 以 mod dictionary; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// BM25 词典增强分词集成（index_l1/index_l2 走实例分词器）
// =========================================================

#[test]
fn set_bm25_dictionary_applies_to_indexed_l1() {
    let mut r = Retriever::new();
    r.config_mut().enable_vector = false;
    r.config_mut().enable_graph = false;
    r.set_bm25_dictionary(&["工作压力".to_string()]);

    r.index_l1(&L1DocView {
        id: uuid::Uuid::new_v4(),
        summary: "最近工作压力很大".to_string(),
        keywords: None,
        persona_uid: Some("user-0001".to_string()),
        created_at: 1000,
        salience: 0.8,
        last_accessed_at: None,
    });

    // 词典增强口径：跨词噪声 "作压" 不命中
    let noise = r.search(
        &SearchRequest {
            query: "作压".to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        },
        None,
    );
    assert!(noise.is_empty(), "词典口径下 '作压' 噪声不应命中");

    // 词典整词 "工作压力" 命中
    let hit = r.search(
        &SearchRequest {
            query: "工作压力".to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        },
        None,
    );
    assert!(
        hit.iter().any(|sr| sr.doc_summary.contains("工作压力")),
        "词典整词查询应命中 L1 文档"
    );
}
