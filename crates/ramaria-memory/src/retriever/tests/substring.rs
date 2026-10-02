//! crates/ramaria-memory/src/retriever/tests/substring.rs - 子串与叙事检索
//!
//! 设计特点:
//! - 由 父测试模块 以 mod substring; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// search_substring 测试
// =========================================================

#[test]
fn search_substring_finds_partial_match() {
    let r = make_test_retriever();
    // "Rust编程" 应能匹配到 BM25 bigram 命中的文档
    let results = r.search_substring("Rust编程", "user-0001", 10);
    assert!(!results.is_empty());
    assert!(results.iter().any(|sr| sr.doc_summary.contains("Rust")));
}

#[test]
fn search_substring_empty_query() {
    let r = make_test_retriever();
    let results = r.search_substring("", "user-0001", 10);
    assert!(results.is_empty());
}

#[test]
fn search_substring_filters_by_persona() {
    let r = make_test_retriever();
    let results = r.search_substring("火锅", "user-0002", 10);
    assert!(results.is_empty());
}

#[test]
fn search_substring_top_k() {
    let mut r = make_test_retriever();
    for i in 0..5 {
        r.index_l1(&L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: format!("文档{} Rust相关", i),
            keywords: Some("Rust".to_string()),
            persona_uid: Some("user-0001".to_string()),
            created_at: 3000 + i as i64,
            salience: 0.5,
            last_accessed_at: None,
        });
    }
    let results = r.search_substring("Rust", "user-0001", 2);
    assert_eq!(results.len(), 2);
}

#[test]
fn search_narrative_ranks_relevant_over_recent() {
    // 脉络加权（决策 D-V17-006）：话题相关（BM25 命中）优先于更新的无关记忆。
    // 使用真实时间戳（now - 天数），避免 1970 年小时间戳导致衰减下溢。
    let mut r = Retriever::new();
    let now = 1_700_000_000_000i64;
    // 相关但更旧（3 天前）
    r.index_l1(&L1DocView {
        id: uuid::Uuid::new_v4(),
        summary: "用户讨论了Rust异步编程".to_string(),
        keywords: Some("Rust,编程".to_string()),
        persona_uid: Some("user-0001".to_string()),
        created_at: now - 3 * 86_400_000,
        salience: 0.5,
        last_accessed_at: None,
    });
    // 无关但更新（1 天前）
    r.index_l1(&L1DocView {
        id: uuid::Uuid::new_v4(),
        summary: "用户和朋友去吃了火锅".to_string(),
        keywords: Some("社交,火锅".to_string()),
        persona_uid: Some("user-0001".to_string()),
        created_at: now - 86_400_000,
        salience: 0.5,
        last_accessed_at: None,
    });

    let decay = DecayConfig::l1();
    let results = r.search_narrative("Rust 编程", "user-0001", 3, now, &decay);

    assert!(!results.is_empty(), "应命中 Rust 相关 L1");
    assert!(
        results[0].doc_summary.contains("Rust"),
        "话题相关应排前（即使更旧），got: {}",
        results[0].doc_summary
    );
    assert!(
        results[0].bm25_score.unwrap_or(0.0) > 0.0,
        "相关性命中应携带 BM25 分数"
    );
}

#[test]
fn search_narrative_no_relevance_falls_back_to_recent() {
    // 无相关性命中 → 按创建时间降序回退最近 N 条（v1.6 语义等价）。
    let r = make_test_retriever();
    let now = 1_000_000_000_000i64;
    let decay = DecayConfig::l1();
    let results = r.search_narrative("完全不相关", "user-0001", 3, now, &decay);

    assert!(!results.is_empty(), "兜底应返回最近 L1");
    // 火锅文档 created_at=2000 最新 → 应排第一
    assert!(
        results[0].doc_summary.contains("火锅"),
        "无相关性命中应按时间兜底（最近优先），got: {}",
        results[0].doc_summary
    );
    assert!(
        results.iter().all(|sr| sr.bm25_score.is_none()),
        "无相关性命中不应携带 BM25 分数"
    );
}

#[test]
fn search_substring_returns_bm25_score() {
    let r = make_test_retriever();
    let results = r.search_substring("Rust", "user-0001", 5);
    // BM25 分数应 > 0
    for sr in &results {
        assert!(sr.bm25_score.unwrap_or(0.0) > 0.0, "BM25 分数应大于 0");
        assert!(sr.rrf_score > 0.0, "rrf_score 应为 BM25 分数");
    }
}
