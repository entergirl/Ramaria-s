//! crates/ramaria-memory/src/retriever/tests.rs - 三通道组合检索编排器单元测试
//!
//! 设计特点:
//! - 覆盖 BM25/向量/图谱三通道索引、RRF 融合、persona 过滤与 doc_id 解析。
//! - 使用内存 Retriever（构造 L1/L2/L0 文档）做确定性断言，不依赖真实 LLM/embedding。
use super::*;

fn make_test_retriever() -> Retriever {
    let mut r = Retriever::new();

    // 添加 L1 文档
    r.index_l1(&L1DocView {
        id: uuid::Uuid::new_v4(),
        summary: "用户今天学习了Rust编程语言的基础语法".to_string(),
        keywords: Some("学习,Rust,编程".to_string()),
        persona_uid: Some("user-0001".to_string()),
        created_at: 1000,
        salience: 0.8,
        last_accessed_at: None,
    });

    r.index_l1(&L1DocView {
        id: uuid::Uuid::new_v4(),
        summary: "用户和朋友去吃了火锅，很开心".to_string(),
        keywords: Some("社交,火锅,开心".to_string()),
        persona_uid: Some("user-0001".to_string()),
        created_at: 2000,
        salience: 0.6,
        last_accessed_at: None,
    });

    // 添加 L2 事件
    r.index_l2(&L2DocView {
        id: 1,
        title: "完成Rust项目".to_string(),
        summary: "用户完成了第一个Rust项目，发布了crate".to_string(),
        keywords: Some("Rust,项目,发布".to_string()),
        attitude: Some("感到很有成就感".to_string()),
        paraphrase: Some("对完成重要工作感到满意".to_string()),
        persona_uid: "user-0001".to_string(),
        share: 0.8,
        confidence: 0.9,
        created_at: 1500,
        salience: 0.9,
    });

    r
}

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
    // query_vec = None → 向量通道跳过；BM25 无关键词命中 → 空结果（不报错）
    let results = r.search(&req, None);
    // 不 panic、返回空或 BM25 结果均可（此用例仅验证不阻塞）
    let _ = results;
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

// =========================================================
// index_l1_record 测试
// =========================================================

#[test]
fn index_l1_record_adds_to_bm25() {
    let mut r = Retriever::new();
    let l1 = MemoryL1 {
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::new_v4(),
        summary: "用户讨论Rust异步编程".to_string(),
        keywords: Some("Rust,异步,编程".to_string()),
        time_period: None,
        atmosphere: None,
        valence: 0.5,
        salience: 0.8,
        absorbed: false,
        created_at: 1718000000000,
        last_accessed_at: None,
        persona_uid: Some("user-0001".to_string()),
        context_json: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    };

    let result = r.index_l1_record(&l1);
    assert!(result.is_ok());
    // 验证文档数增加了
    assert_eq!(r.doc_count(), 1);
}

#[test]
fn index_l1_record_searchable_immediately() {
    let mut r = Retriever::new();
    let l1 = MemoryL1 {
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::new_v4(),
        summary: "用户今天学习了Rust编程语言的基础语法".to_string(),
        keywords: Some("学习,Rust,编程".to_string()),
        time_period: None,
        atmosphere: None,
        valence: 0.8,
        salience: 0.9,
        absorbed: false,
        created_at: 1718000000000,
        last_accessed_at: None,
        persona_uid: Some("user-0001".to_string()),
        context_json: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    };

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
    let l1_user_a = MemoryL1 {
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::new_v4(),
        summary: "用户A的私密对话".to_string(),
        keywords: Some("私密".to_string()),
        time_period: None,
        atmosphere: None,
        valence: 0.0,
        salience: 0.5,
        absorbed: false,
        created_at: 1718000000000,
        last_accessed_at: None,
        persona_uid: Some("user-a".to_string()),
        context_json: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    };

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
    let l1 = MemoryL1 {
        id,
        session_id: sid,
        summary: "测试摘要".to_string(),
        keywords: Some("测试,标签".to_string()),
        time_period: Some("下午".to_string()),
        atmosphere: Some("轻松".to_string()),
        valence: 0.7,
        salience: 0.9,
        absorbed: false,
        created_at: 1718000000000,
        last_accessed_at: None,
        persona_uid: Some("test-persona".to_string()),
        context_json: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    };

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

// =========================================================
// utt 原文通道测试（v1.4）
// =========================================================

fn make_utt_doc(id: i64, persona_uid: &str, text: &str, created_at: i64) -> UttDocView {
    UttDocView {
        id,
        persona_uid: persona_uid.to_string(),
        session_id: uuid::Uuid::new_v4(),
        block_text: text.to_string(),
        msg_count: 2,
        created_at,
    }
}

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

fn l1_view_doc(id: uuid::Uuid, summary: &str, created_at: i64) -> L1DocView {
    L1DocView {
        id,
        summary: summary.to_string(),
        keywords: None,
        persona_uid: Some("user-0001".to_string()),
        created_at,
        salience: 0.5,
        last_accessed_at: None,
    }
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
