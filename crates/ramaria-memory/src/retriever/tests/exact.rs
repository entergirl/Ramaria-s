//! crates/ramaria-memory/src/retriever/tests/exact.rs - 精确关键词检索
//!
//! 设计特点:
//! - 由 父测试模块 以 mod exact; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// search_exact 测试
// =========================================================

use ramaria_core::keyword::KeywordToken;

#[test]
fn search_exact_finds_matching_docs() {
    let r = make_test_retriever();
    let kw = vec![
        KeywordToken::new("Rust").unwrap(),
        KeywordToken::new("编程").unwrap(),
    ];
    let results = r.search_exact(&kw, "user-0001", 10);
    // 应命中至少 2 条：L1 "Rust,编程" 和 L2 "Rust,项目,发布"
    assert!(!results.is_empty());
    // L2 事件命中 "Rust"，L1 命中 "Rust"+"编程"
    assert!(results.iter().any(|sr| sr.layer == "l1"));
    assert!(results.iter().any(|sr| sr.layer == "l2"));
}

#[test]
fn search_exact_empty_keywords() {
    let r = make_test_retriever();
    let results = r.search_exact(&[], "user-0001", 10);
    assert!(results.is_empty());
}

#[test]
fn search_exact_no_match() {
    let r = make_test_retriever();
    let kw = vec![KeywordToken::new("不存在的关键词xyz").unwrap()];
    let results = r.search_exact(&kw, "user-0001", 10);
    assert!(results.is_empty());
}

#[test]
fn search_exact_filters_by_persona() {
    let r = make_test_retriever();
    let kw = vec![KeywordToken::new("Rust").unwrap()];
    // user-0002 不应有任何文档
    let results = r.search_exact(&kw, "user-0002", 10);
    assert!(results.is_empty());
}

#[test]
fn search_exact_top_k_truncation() {
    let mut r = make_test_retriever();
    // 添加更多含相同关键词的文档
    for i in 0..5 {
        r.index_l1(&L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: format!("文档{} 关于Rust", i),
            keywords: Some("Rust,测试".to_string()),
            persona_uid: Some("user-0001".to_string()),
            created_at: 3000 + i as i64,
            occurred_at: 3000 + i as i64,
            salience: 0.5,
            last_accessed_at: None,
        });
    }
    let kw = vec![KeywordToken::new("Rust").unwrap()];
    let results = r.search_exact(&kw, "user-0001", 3);
    assert_eq!(results.len(), 3);
}

#[test]
fn search_exact_sorts_by_match_count() {
    let mut r = Retriever::new();
    // 文档 A: 命中 1 个关键词
    r.index_l1(&L1DocView {
        id: uuid::Uuid::new_v4(),
        summary: "A".to_string(),
        keywords: Some("Rust".to_string()),
        persona_uid: Some("u1".to_string()),
        created_at: 1000,
        occurred_at: 1000,
        salience: 0.5,
        last_accessed_at: None,
    });
    // 文档 B: 命中 2 个关键词
    r.index_l1(&L1DocView {
        id: uuid::Uuid::new_v4(),
        summary: "B".to_string(),
        keywords: Some("Rust,编程,异步".to_string()),
        persona_uid: Some("u1".to_string()),
        created_at: 2000,
        occurred_at: 2000,
        salience: 0.5,
        last_accessed_at: None,
    });

    let kw = vec![
        KeywordToken::new("Rust").unwrap(),
        KeywordToken::new("编程").unwrap(),
    ];
    let results = r.search_exact(&kw, "u1", 10);
    assert_eq!(results.len(), 2);
    // 文档 B（命中 2 个）应排在前面
    assert!(
        results[0].doc_summary.contains("B"),
        "命中更多关键词的文档应排在前面"
    );
}

/// L2 投影：search_exact 命中的时间基准取事件开始时间（start），而非登记时间。
#[test]
fn search_exact_l2_time_base_is_event_start() {
    let mut r = Retriever::new();
    r.index_l2(&L2DocView {
        id: 1,
        title: "事件标题".to_string(),
        summary: "事件摘要".to_string(),
        keywords: Some("工作".to_string()),
        attitude: None,
        paraphrase: None,
        persona_uid: "user-0001".to_string(),
        share: 0.5,
        confidence: 0.9,
        created_at: 9000, // 登记时刻
        start: 1000,      // 发生时刻
        salience: 0.5,
    });

    let kw = vec![KeywordToken::new("工作").unwrap()];
    let results = r.search_exact(&kw, "user-0001", 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].layer, "l2");
    assert_eq!(results[0].created_at, 1000, "L2 时间基准应取事件 start");
}

/// L2 投影：BM25 路径（resolve_bm25_doc）命中的时间基准同样取事件 start。
#[test]
fn search_substring_l2_time_base_is_event_start() {
    let mut r = Retriever::new();
    r.index_l2(&L2DocView {
        id: 2,
        title: "篮球比赛".to_string(),
        summary: "参加了周末篮球比赛".to_string(),
        keywords: Some("篮球".to_string()),
        attitude: None,
        paraphrase: None,
        persona_uid: "user-0001".to_string(),
        share: 0.5,
        confidence: 0.9,
        created_at: 8000, // 登记时刻
        start: 2000,      // 发生时刻
        salience: 0.5,
    });

    let results = r.search_substring("篮球", "user-0001", 10);
    assert!(!results.is_empty(), "BM25 应命中篮球事件");
    assert_eq!(
        results[0].created_at, 2000,
        "BM25 路径 L2 时间基准应取事件 start"
    );
    // 同分排序次键（search_exact）也随 start：再放一条 start 更早的 L2 文档
    let results_exact = {
        let kw = vec![KeywordToken::new("篮球").unwrap()];
        r.search_exact(&kw, "user-0001", 10)
    };
    assert!(!results_exact.is_empty());
    assert_eq!(results_exact[0].created_at, 2000);
}
