//! crates/ramaria-memory/src/graph_retriever/tests.rs - //! crates/ramaria-memory/src/graph_retriever.rs — 知识图谱检索通道单元测试
//!
//! 设计特点:
//! - 位于 graph_retriever 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 graph_retriever.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

fn make_test_retriever() -> GraphRetriever {
    let mut retriever = GraphRetriever::new();

    let nodes = vec![
        (1i64, "用户".to_string(), "person".to_string()),
        (2, "机器学习".to_string(), "project".to_string()),
        (3, "Python".to_string(), "module".to_string()),
        (4, "数据清洗".to_string(), "concept".to_string()),
        (5, "TensorFlow".to_string(), "module".to_string()),
    ];

    let edges = vec![
        (1i64, 1i64, 2i64, "TASK_STATUS".to_string()),
        (2, 2, 3, "USES_DEPENDS".to_string()),
        (3, 2, 4, "BELONGS_TO".to_string()),
        (4, 2, 5, "USES_DEPENDS".to_string()),
        (5, 3, 4, "USES_DEPENDS".to_string()),
    ];

    retriever.load(&nodes, &edges);
    retriever
}

#[test]
fn load_and_count() {
    let r = make_test_retriever();
    assert_eq!(r.node_count(), 5);
    assert_eq!(r.edge_count(), 5);
}

#[test]
fn extract_entities_exact_match() {
    let r = make_test_retriever();
    let entities = r.extract_entities("机器学习");
    assert!(!entities.is_empty());
    assert_eq!(entities[0].0, "机器学习");
    assert!((entities[0].1 - 1.0).abs() < 0.01);
}

#[test]
fn extract_entities_partial_match() {
    let r = make_test_retriever();
    let entities = r.extract_entities("我在做机器学习项目");
    // 应匹配到 "机器学习"
    assert!(entities.iter().any(|(name, _)| *name == "机器学习"));
}

#[test]
fn extract_entities_query_shorter_than_entity() {
    let r = make_test_retriever();
    let entities = r.extract_entities("Python");
    assert!(!entities.is_empty());
    assert_eq!(entities[0].0, "Python");
}

#[test]
fn extract_entities_no_match() {
    let r = make_test_retriever();
    let entities = r.extract_entities("今天吃火锅");
    assert!(entities.is_empty());
}

#[test]
fn extract_entities_empty_query() {
    let r = make_test_retriever();
    let entities = r.extract_entities("");
    assert!(entities.is_empty());
}

#[test]
fn search_returns_hits_with_neighbors() {
    let r = make_test_retriever();
    let config = GraphRetrieverConfig::default();
    let hits = r.search("机器学习", &config);

    assert!(!hits.is_empty());
    let ml_hit = hits.iter().find(|h| h.entity_name == "机器学习").unwrap();
    assert!(!ml_hit.related_entities.is_empty());
    // "机器学习" 应有邻居：Python, 数据清洗, TensorFlow
    assert!(ml_hit.related_entities.contains(&"Python".to_string()));
    assert!(ml_hit.related_entities.contains(&"数据清洗".to_string()));
}

#[test]
fn search_no_match_returns_empty() {
    let r = make_test_retriever();
    let config = GraphRetrieverConfig::default();
    let hits = r.search("吃火锅", &config);
    assert!(hits.is_empty());
}

#[test]
fn search_scores_in_range() {
    let r = make_test_retriever();
    let config = GraphRetrieverConfig::default();
    let hits = r.search("Python 数据清洗", &config);

    for hit in &hits {
        assert!(
            hit.score >= 0.0 && hit.score <= 1.0,
            "score {} out of range for {}",
            hit.score,
            hit.entity_name
        );
    }
}

#[test]
fn search_max_entities_limit() {
    let config = GraphRetrieverConfig {
        max_entities: 1,
        ..Default::default()
    };

    let r = make_test_retriever();
    let hits = r.search("Python 机器学习 数据清洗", &config);
    assert!(hits.len() <= 1);
}

// ---- 评分公式对齐（决策 D-V17-014-16）----
// 文档公式与实现对齐：relation_boost = 1.0 + Σ(关系权重 × 0.1)，出边/入边对称。
// 与搜索分数耦合的既有测试（search_scores_in_range）只断言范围，不受公式修正影响。

/// 构造仅有入边的实体：入边必须贡献与出边一致的关系权重加成。
#[test]
fn search_in_edge_contributes_boost() {
    let mut r = GraphRetriever::new();
    let nodes = vec![
        (1i64, "Alpha".to_string(), "concept".to_string()),
        (2, "Beta".to_string(), "concept".to_string()),
    ];
    // Beta → Alpha（TASK_STATUS，权重 1.0）：Alpha 只有入边
    let edges = vec![(1i64, 2i64, 1i64, "TASK_STATUS".to_string())];
    r.load(&nodes, &edges);

    let config = GraphRetrieverConfig::default();

    // 查询 "Al"（部分子序列匹配）：Alpha match_score = 2/5 = 0.4
    let hits = r.search("Al", &config);
    let alpha = hits
        .iter()
        .find(|h| h.entity_name == "Alpha")
        .expect("Alpha 应命中");
    // boost = 1.0 + TASK_STATUS(1.0)×0.1 = 1.10（仅入边贡献）
    // score = 0.4 × 1.10 = 0.44
    assert!(
        (alpha.score - 0.44).abs() < 0.001,
        "入边应贡献权重加成，got {}",
        alpha.score
    );
    assert!(
        alpha.related_entities.contains(&"Beta".to_string()),
        "入边邻居应被收集"
    );
    assert!(
        alpha
            .relation_types
            .iter()
            .any(|t| t.contains("←TASK_STATUS")),
        "入边关系类型应带 ← 前缀"
    );
}

/// 出边与入边的 boost 行为对称：同样权重的边贡献相同加成。
///
/// 构造: Gamma → Alpha（TASK_STATUS 权重 1.0）。
/// - Alpha 仅有入边（被引用）：boost = 1.0 + 1.0×0.1 = 1.10
/// - Gamma 仅有出边（引用）：boost = 1.0 + 1.0×0.1 = 1.10（对称）
#[test]
fn search_out_in_edge_boost_symmetric() {
    let mut r = GraphRetriever::new();
    let nodes = vec![
        (1i64, "Alpha".to_string(), "concept".to_string()),
        (3, "Gamma".to_string(), "concept".to_string()),
    ];
    let edges = vec![(1i64, 3i64, 1i64, "TASK_STATUS".to_string())];
    r.load(&nodes, &edges);

    let config = GraphRetrieverConfig::default();

    // Alpha：仅有入边。查询 "Al" match_score=2/5=0.4
    // boost = 1.0 + 1.0×0.1 = 1.10；score = 0.4 × 1.10 = 0.44
    let hits = r.search("Al", &config);
    let alpha = hits
        .iter()
        .find(|h| h.entity_name == "Alpha")
        .expect("Alpha 应命中");
    assert!(
        (alpha.score - 0.44).abs() < 0.001,
        "仅入边的 boost 应为 1.10，got {}",
        alpha.score
    );

    // Gamma：仅有出边。查询 "Ga" match_score=2/5=0.4
    // boost = 1.0 + 1.0×0.1 = 1.10；score = 0.4 × 1.10 = 0.44（与 Alpha 对称）
    let hits = r.search("Ga", &config);
    let gamma = hits
        .iter()
        .find(|h| h.entity_name == "Gamma")
        .expect("Gamma 应命中");
    assert!(
        (gamma.score - 0.44).abs() < 0.001,
        "入边/出边 boost 应对称，got {}",
        gamma.score
    );
}

#[test]
fn test_graph_hits_to_rrf_pairs() {
    let hits = vec![GraphHit {
        entity_name: "Python".to_string(),
        entity_type: "module".to_string(),
        score: 0.9,
        related_entities: vec![],
        relation_types: vec![],
    }];
    let pairs = graph_hits_to_rrf_pairs(&hits);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].0, "graph:Python");
    assert!((pairs[0].1 - 0.9).abs() < 0.01);
}

/// contains_subsequence 各输入参数化验证。
#[test]
fn contains_subsequence_cases() {
    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }
    let cases = [
        (chars("机器学习项目"), chars("学习"), true),
        (chars("机器学习"), chars("深度"), false),
        (chars("测试"), Vec::new(), true),     // 空 needle
        (chars("短"), chars("太长了"), false), // needle 更长
    ];
    for (haystack, needle, expected) in cases {
        assert_eq!(
            contains_subsequence(&haystack, &needle),
            expected,
            "haystack={haystack:?} needle={needle:?}"
        );
    }
}

#[test]
fn clear_and_reuse() {
    let mut r = make_test_retriever();
    assert!(r.node_count() > 0);

    r.clear();
    assert_eq!(r.node_count(), 0);
    assert_eq!(r.edge_count(), 0);

    let config = GraphRetrieverConfig::default();
    assert!(r.search("机器学习", &config).is_empty());
}

// ---- 倒排候选与全量扫描的等价性 ----
// 倒排索引只用于生成候选，最终仍以原匹配公式校验；以下用例锁定两者结果完全一致。

/// 复刻旧全量扫描算法，作为等价性基准。
fn full_scan_entities(r: &GraphRetriever, query: &str) -> Vec<(String, f64)> {
    if query.is_empty() || r.nodes.is_empty() {
        return Vec::new();
    }

    let q_chars: Vec<char> = query.chars().collect();
    let q_len = q_chars.len();
    let mut matches: Vec<(String, f64)> = Vec::new();

    for entity_name in r.nodes.keys() {
        let e_chars: Vec<char> = entity_name.chars().collect();
        let e_len = e_chars.len();

        if e_len == 0 {
            continue;
        }

        let match_len = if q_len >= e_len {
            contains_subsequence(&q_chars, &e_chars) as usize * e_len
        } else {
            contains_subsequence(&e_chars, &q_chars) as usize * q_len
        };

        if match_len > 0 {
            let ratio = match_len as f64 / e_len.max(q_len) as f64;
            matches.push((entity_name.clone(), ratio));
        }
    }

    matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    matches
}

/// 断言新实现与全量扫描结果完全一致（实体名与 ratio 均一致）。
fn assert_entities_match_full_scan(r: &GraphRetriever, query: &str) {
    let mut actual: Vec<(String, f64)> = r
        .extract_entities(query)
        .into_iter()
        .map(|(name, ratio)| (name.to_string(), ratio))
        .collect();
    let mut expected = full_scan_entities(r, query);

    actual.sort_by(|a, b| a.0.cmp(&b.0));
    expected.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(
        actual.len(),
        expected.len(),
        "query={query:?} 结果数量不一致: {actual:?} vs {expected:?}"
    );
    for (new_item, old_item) in actual.iter().zip(expected.iter()) {
        assert_eq!(new_item.0, old_item.0, "query={query:?} 实体名不一致");
        assert!(
            (new_item.1 - old_item.1).abs() < 1e-9,
            "query={query:?} 实体 {} 的 ratio 不一致: {} vs {}",
            new_item.0,
            new_item.1,
            old_item.1
        );
    }
}

#[test]
fn entity_extraction_matches_full_scan() {
    let r = make_test_retriever();
    assert!(
        !r.entity_bigrams.is_empty(),
        "load 后应建立 bigram 倒排索引"
    );

    let queries = [
        "机器学习",
        "我在做机器学习项目",
        "Python",
        "数据清洗与TensorFlow",
        "Al",
        "今",
        "",
        "吃火锅",
    ];
    for query in queries {
        assert_entities_match_full_scan(&r, query);
    }
}

#[test]
fn extract_entities_single_char_query_matches_full_scan() {
    let r = make_test_retriever();
    for query in ["学", "P", "x", ""] {
        assert_entities_match_full_scan(&r, query);
    }
}

/// 单字符实体名必须由单字符索引召回，且多字符查询下与全量扫描等价。
#[test]
fn single_char_entity_indexed_and_matched() {
    let mut r = GraphRetriever::new();
    let nodes = vec![
        (1i64, "A".to_string(), "concept".to_string()),
        (2, "AB".to_string(), "concept".to_string()),
        (3, "机器学习".to_string(), "project".to_string()),
    ];
    r.load(&nodes, &[]);

    assert_eq!(
        r.single_char_entities.get(&'A').map(|v| v.len()),
        Some(1),
        "单字符实体应进入单字符索引"
    );
    for query in ["AB", "A机器", "吃A", "机器学习"] {
        assert_entities_match_full_scan(&r, query);
    }
}

/// 同名节点覆盖后索引项不得重复累积，clear 后索引须一并清空。
#[test]
fn add_node_refreshes_index_without_duplicates() {
    let mut r = GraphRetriever::new();
    r.add_node(GraphNode {
        id: 1,
        entity_name: "机器学习".to_string(),
        entity_type: "project".to_string(),
    });
    r.add_node(GraphNode {
        id: 2,
        entity_name: "机器学习".to_string(),
        entity_type: "project".to_string(),
    });
    assert_eq!(
        r.entity_bigrams.get(&('机', '器')).map(|v| v.len()),
        Some(1)
    );

    r.clear();
    assert!(r.entity_bigrams.is_empty(), "clear 后 bigram 倒排应清空");
    assert!(
        r.single_char_entities.is_empty(),
        "clear 后单字符索引应清空"
    );
}
