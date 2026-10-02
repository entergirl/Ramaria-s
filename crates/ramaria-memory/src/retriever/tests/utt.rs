//! crates/ramaria-memory/src/retriever/tests/utt.rs - utt 原文通道
//!
//! 设计特点:
//! - 由 父测试模块 以 mod utt; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// utt 原文通道测试（v1.4）
// =========================================================

#[test]
fn index_utt_and_search_vector() {
    let mut r = Retriever::new();
    // 过滤零相似度命中（相似度恰为 0 的块不应作为结果返回）
    r.config_mut().vector.min_similarity = 0.01;
    r.index_utt(
        &make_utt_doc(1, "char-0001", "今天天气很好我们去公园吧", 1000),
        Some(vec![1.0, 0.0]),
    );
    r.index_utt(
        &make_utt_doc(2, "char-0001", "晚饭想吃火锅", 2000),
        Some(vec![0.0, 1.0]),
    );

    let hits = r.search_utt("天气", Some(&[1.0, 0.0]), 5, Some("char-0001"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].doc.id, 1);
    assert_eq!(hits[0].channel, "vector");
    assert!(hits[0].score > 0.0);
}

#[test]
fn search_utt_persona_isolation() {
    // 跨 persona 严格隔离：char-0002 检索不到 char-0001 的块
    let mut r = Retriever::new();
    r.index_utt(
        &make_utt_doc(1, "char-0001", "这是我的秘密原文内容", 1000),
        Some(vec![1.0, 0.0]),
    );

    let hits = r.search_utt("秘密", Some(&[1.0, 0.0]), 5, Some("char-0002"));
    assert!(hits.is_empty(), "跨 persona 不可见");
    assert_eq!(r.utt_doc_count(), 1);
}

#[test]
fn search_utt_without_persona_returns_empty() {
    // 未指定目标 persona → 不检索原文（隔离红线）
    let mut r = Retriever::new();
    r.index_utt(&make_utt_doc(1, "char-0001", "原文", 1000), Some(vec![1.0]));
    assert!(r.search_utt("原文", Some(&[1.0]), 5, None).is_empty());
}

#[test]
fn search_utt_vector_empty_index_falls_back_to_substring() {
    // 向量索引为空（块无 embedding）→ 子串降级
    let mut r = Retriever::new();
    r.index_utt(
        &make_utt_doc(1, "char-0001", "今天天气很好我们去公园吧", 1000),
        None,
    );
    r.index_utt(&make_utt_doc(2, "char-0001", "晚饭想吃火锅", 2000), None);

    let hits = r.search_utt("火锅", Some(&[1.0, 0.0]), 5, Some("char-0001"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].doc.id, 2);
    assert_eq!(hits[0].channel, "substring");
}

#[test]
fn search_utt_substring_scores_by_token_hits() {
    let mut r = Retriever::new();
    // 块1 命中 1 个 token（"天气"），块2 命中 2 个 token（"天气""公园"）
    r.index_utt(&make_utt_doc(1, "char-0001", "天气不错", 1000), None);
    r.index_utt(
        &make_utt_doc(2, "char-0001", "天气好去公园散步", 2000),
        None,
    );

    let hits = r.search_utt("天气 公园", None, 5, Some("char-0001"));
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].doc.id, 2, "命中更多 token 的块排前");
    assert!(hits[0].score > hits[1].score);
}

#[test]
fn search_utt_substring_no_match_returns_empty() {
    let mut r = Retriever::new();
    r.index_utt(&make_utt_doc(1, "char-0001", "今天天气很好", 1000), None);
    let hits = r.search_utt("完全无关的话题词汇", None, 5, Some("char-0001"));
    assert!(hits.is_empty());
}

#[test]
fn search_utt_top_k_limits_results() {
    let mut r = Retriever::new();
    for i in 0..5 {
        r.index_utt(
            &make_utt_doc(i, "char-0001", &format!("天气讨论第{i}轮内容"), i * 1000),
            None,
        );
    }
    let hits = r.search_utt("天气", None, 2, Some("char-0001"));
    assert_eq!(hits.len(), 2);
}

#[test]
fn remove_utt_removes_doc_and_vector() {
    let mut r = Retriever::new();
    r.index_utt(
        &make_utt_doc(1, "char-0001", "原文内容", 1000),
        Some(vec![1.0, 0.0]),
    );
    r.remove_utt(1);
    assert_eq!(r.utt_doc_count(), 0);
    assert!(
        r.search_utt("原文", Some(&[1.0, 0.0]), 5, Some("char-0001"))
            .is_empty()
    );
}

#[test]
fn clear_removes_utt_docs() {
    let mut r = Retriever::new();
    r.index_utt(&make_utt_doc(1, "char-0001", "原文", 1000), Some(vec![1.0]));
    r.clear();
    assert_eq!(r.utt_doc_count(), 0);
}

#[test]
fn index_utt_block_decodes_embedding_blob() {
    use ramaria_core::types::UttBlock;
    let mut r = Retriever::new();
    let mut block = UttBlock::new(
        "char-0001".to_string(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "块原文文本".to_string(),
        3,
        1000,
    );
    block.embedding = Some(crate::utt::encode_embedding(&[0.5, -0.25]));
    r.index_utt_block(&block);

    let hits = r.search_utt("块原文", Some(&[0.5, -0.25]), 5, Some("char-0001"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].channel, "vector");
}

#[test]
fn index_utt_block_corrupted_blob_degrades_to_substring() {
    use ramaria_core::types::UttBlock;
    let mut r = Retriever::new();
    let mut block = UttBlock::new(
        "char-0001".to_string(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "损坏向量但文本可检索".to_string(),
        3,
        1000,
    );
    block.embedding = Some(vec![1, 2, 3]); // 长度非 4 倍数 → 解码失败
    r.index_utt_block(&block);

    let hits = r.search_utt("文本可检索", Some(&[1.0, 2.0, 3.0]), 5, Some("char-0001"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].channel, "substring", "损坏 BLOB 降级子串");
}

#[test]
fn l0_labels_do_not_leak_into_regular_search() {
    // 回归红线：utt 块（L0: label）不得混入三通道 RAG 检索结果
    let mut r = make_test_retriever();
    r.index_utt(
        &make_utt_doc(99, "user-0001", "用户原文内容", 5000),
        Some(vec![1.0, 0.0]),
    );
    let results = r.search(
        &SearchRequest {
            query: "用户原文内容".to_string(),
            persona_uid: Some("user-0001".to_string()),
            top_k: 5,
            filter_share: true,
        },
        Some(&[1.0, 0.0]),
    );
    // 既有 L1 文档（user-0001）可命中，但 L0: 块不会作为结果出现
    for sr in &results {
        assert_ne!(sr.layer, "l0", "L0 块不应混入常规 RAG 结果");
    }
}
