//! crates/ramaria-memory/src/retriever/tests/index.rs - L1 镜像索引
//!
//! 设计特点:
//! - 由 父测试模块 以 mod index; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;
use ramaria_testkit::fixtures::l1_with;

// =========================================================
// index_l1_record 测试
// =========================================================

#[test]
fn index_l1_record_adds_to_bm25() {
    let mut r = Retriever::new();
    let l1 = l1_with(
        uuid::Uuid::new_v4(),
        Some("user-0001"),
        "用户讨论Rust异步编程",
        |l1| {
            l1.keywords = Some("Rust,异步,编程".to_string());
            l1.time_period = None;
            l1.atmosphere = None;
            l1.salience = 0.8;
            l1.created_at = 1718000000000;
            l1.occurred_at = None;
            l1.situation_strength = None;
        },
    );

    let result = r.index_l1_record(&l1);
    assert!(result.is_ok());
    // 验证文档数增加了
    assert_eq!(r.doc_count(), 1);
}

#[test]
fn index_l1_record_searchable_immediately() {
    let mut r = Retriever::new();
    let l1 = l1_with(
        uuid::Uuid::new_v4(),
        Some("user-0001"),
        "用户今天学习了Rust编程语言的基础语法",
        |l1| {
            l1.keywords = Some("学习,Rust,编程".to_string());
            l1.time_period = None;
            l1.atmosphere = None;
            l1.valence = 0.8;
            l1.salience = 0.9;
            l1.created_at = 1718000000000;
            l1.occurred_at = None;
            l1.situation_strength = None;
        },
    );

    r.index_l1_record(&l1).unwrap();

    // 立即检索，应能命中
    let req = SearchRequest {
        query: "Rust".to_string(),
        persona_uid: None,
        top_k: 5,
        filter_share: false,
    };
    let results = r.search(&req, None);
    assert!(!results.is_empty());
    assert!(results.iter().any(|sr| sr.doc_summary.contains("Rust")));
}

#[test]
fn index_l1_record_respects_persona_uid() {
    let mut r = Retriever::new();
    let l1_user_a = l1_with(
        uuid::Uuid::new_v4(),
        Some("user-a"),
        "用户A的私密对话",
        |l1| {
            l1.keywords = Some("私密".to_string());
            l1.time_period = None;
            l1.atmosphere = None;
            l1.valence = 0.0;
            l1.created_at = 1718000000000;
            l1.occurred_at = None;
            l1.situation_strength = None;
        },
    );

    r.index_l1_record(&l1_user_a).unwrap();

    // 以 user-b 检索，不应命中 user-a 的文档
    let req = SearchRequest {
        query: "私密".to_string(),
        persona_uid: Some("user-b".to_string()),
        top_k: 5,
        filter_share: false,
    };
    let results = r.search(&req, None);
    assert!(results.is_empty());
}

#[test]
fn index_l1_record_preserves_fields() {
    let mut r = Retriever::new();
    let id = uuid::Uuid::new_v4();
    let sid = uuid::Uuid::new_v4();
    let mut l1 = l1_with(sid, Some("test-persona"), "测试摘要", |l1| {
        l1.keywords = Some("测试,标签".to_string());
        l1.time_period = Some("下午".to_string());
        l1.atmosphere = Some("轻松".to_string());
        l1.valence = 0.7;
        l1.salience = 0.9;
        l1.created_at = 1718000000000;
        l1.occurred_at = None;
        l1.situation_strength = None;
    });
    l1.id = id;

    r.index_l1_record(&l1).unwrap();

    // 验证 L1 文档被正确存储
    let req = SearchRequest {
        query: "测试".to_string(),
        persona_uid: None,
        top_k: 5,
        filter_share: false,
    };
    let results = r.search(&req, None);
    assert!(!results.is_empty());

    let found = results
        .iter()
        .find(|sr| matches!(&sr.doc_id, DocId::L1(uid) if *uid == id));
    assert!(found.is_some(), "应能通过 ID 找到刚索引的文档");
    let found = found.unwrap();
    assert_eq!(found.persona_uid.as_deref(), Some("test-persona"));
    assert_eq!(found.doc_summary, "测试摘要");
}
