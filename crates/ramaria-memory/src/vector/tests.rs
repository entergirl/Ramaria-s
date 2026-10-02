//! crates/ramaria-memory/src/vector/tests.rs - //! crates/ramaria-memory/src/vector.rs — 向量检索引擎封装单元测试
//!
//! 设计特点:
//! - 位于 vector 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 vector.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

// ---- BruteForceIndex ----

#[test]
fn index_add_and_search() {
    let mut idx = BruteForceIndex::new();
    let v1 = vec![1.0, 0.0, 0.0];
    let v2 = vec![0.0, 1.0, 0.0];
    let v3 = vec![0.0, 0.0, 1.0];

    idx.add("doc1", v1.clone(), 1000);
    idx.add("doc2", v2.clone(), 1000);
    idx.add("doc3", v3.clone(), 1000);

    assert_eq!(idx.len(), 3);
    assert_eq!(idx.dimension(), Some(3));

    // 查询 [1.0, 0.0, 0.0]，doc1 应排第一
    let query = vec![1.0, 0.0, 0.0];
    let config = VectorIndexConfig::default();
    let hits = idx.search(&query, &config).unwrap();

    assert!(!hits.is_empty());
    assert_eq!(hits[0].doc_label, "doc1");
    assert!((hits[0].similarity - 1.0).abs() < 0.01);
}

#[test]
fn index_search_empty_returns_error() {
    let idx = BruteForceIndex::new();
    let config = VectorIndexConfig::default();
    let result = idx.search(&[1.0, 0.0], &config);
    assert_eq!(result, Err(VectorIndexError::Empty));
}

#[test]
fn index_dimension_mismatch() {
    let mut idx = BruteForceIndex::new();
    idx.add("doc1", vec![1.0, 0.0, 0.0], 1000);

    let config = VectorIndexConfig::default();
    let result = idx.search(&[1.0, 0.0], &config);
    assert!(matches!(
        result,
        Err(VectorIndexError::DimensionMismatch { .. })
    ));
}

/// 0 维向量（空 embedding）不得被记为首个期望维度：
/// 一旦记录，后续真实维度会被全部拒绝，向量通道整体静默失效（只剩 BM25/关键词）。
#[test]
fn add_rejects_zero_dim_and_keeps_channel_usable() {
    let mut idx = BruteForceIndex::new();

    idx.add("empty", Vec::new(), 1000);
    assert_eq!(idx.len(), 0, "0 维向量不入索引");
    assert_eq!(idx.dimension(), None, "0 维不得被记为首个期望维度");

    idx.add("doc1", vec![1.0, 0.0, 0.0], 1000);
    assert_eq!(idx.len(), 1);
    assert_eq!(idx.dimension(), Some(3));

    let config = VectorIndexConfig::default();
    let hits = idx.search(&[1.0, 0.0, 0.0], &config).unwrap();
    assert_eq!(hits[0].doc_label, "doc1", "真实维度写入后向量通道仍可用");
}

#[test]
fn index_remove_and_clear() {
    let mut idx = BruteForceIndex::new();
    idx.add("doc1", vec![1.0, 0.0], 1000);
    idx.add("doc2", vec![0.0, 1.0], 1000);
    assert_eq!(idx.len(), 2);

    idx.remove("doc1");
    assert_eq!(idx.len(), 1);
    assert_eq!(idx.dimension(), Some(2));

    idx.clear();
    assert_eq!(idx.len(), 0);
    assert_eq!(idx.dimension(), None);
}

#[test]
fn index_add_overwrite() {
    let mut idx = BruteForceIndex::new();
    idx.add("doc1", vec![1.0, 0.0], 1000);
    idx.add("doc1", vec![0.0, 1.0], 2000);

    let config = VectorIndexConfig::default();
    let hits = idx.search(&[0.0, 1.0], &config).unwrap();
    assert_eq!(hits[0].doc_label, "doc1");
    assert!((hits[0].similarity - 1.0).abs() < 0.01);
}

#[test]
fn index_top_k_truncation() {
    let mut idx = BruteForceIndex::new();
    for i in 0..10 {
        let mut v = vec![0.0_f32; 10];
        v[i] = 1.0;
        idx.add(&format!("doc{}", i), v, 1000);
    }

    let config = VectorIndexConfig {
        top_k: 3,
        ..Default::default()
    };
    let mut query = vec![0.0_f32; 10];
    query[0] = 1.0;

    let hits = idx.search(&query, &config).unwrap();
    assert_eq!(hits.len(), 3);
}

#[test]
fn index_min_similarity_filter() {
    let mut idx = BruteForceIndex::new();
    idx.add("a", vec![1.0, 0.0], 1000);
    idx.add("b", vec![0.0, 1.0], 1000);

    let config = VectorIndexConfig {
        min_similarity: 0.9,
        ..Default::default()
    };

    // 查询与 "a" 非常相似
    let hits = idx.search(&[0.99, 0.14], &config).unwrap();
    assert!(hits.iter().any(|h| h.doc_label == "a"));
    // "b" 相似度低，应被过滤
    assert!(hits.iter().all(|h| h.doc_label == "a"));
}

// ---- label utilities ----

/// make_vector_label / parse_vector_label 往返与非法输入验证。
#[test]
fn vector_label_cases() {
    let label = make_vector_label("l1", "550e8400-e29b-41d4-a716-446655440000");
    assert_eq!(label, "L1:550e8400-e29b-41d4-a716-446655440000");
    let parsed = parse_vector_label(&label).unwrap();
    assert_eq!(parsed.0, "L1");
    assert_eq!(parsed.1, "550e8400-e29b-41d4-a716-446655440000");
    // 非法格式 → None
    assert!(parse_vector_label("invalid").is_none());
}

// ---- VectorIndex trait object ----

#[test]
fn vector_index_trait_object() {
    fn _accept(v: &dyn VectorIndex) {
        let _ = v.len();
    }

    let idx = BruteForceIndex::new();
    _accept(&idx);
}

// ---- CachedVectorIndex（LRU 容量策略）----

/// 缓存容量满时淘汰"最久未使用"（LRU），而非最早插入（FIFO）。
///
/// 场景: max_entries=2，依次查询 A/B/A/C：
/// - A、B 入缓存 [A, B]；再查 A → LRU 提升 [B, A]；
/// - 查 C（未命中）→ 容量满 → 淘汰队头 B（最久未使用）→ 缓存 [A, C]；
/// - 若为 FIFO 则淘汰 A，B 仍在——本断言锁定 LRU 语义。
#[test]
fn cached_index_evicts_least_recently_used() {
    let mut inner = BruteForceIndex::new();
    inner.add("a", vec![1.0, 0.0, 0.0], 1);
    inner.add("b", vec![0.0, 1.0, 0.0], 2);
    inner.add("c", vec![0.0, 0.0, 1.0], 3);
    let cfg = VectorCacheConfig {
        max_entries: 2,
        enabled: true,
    };
    let idx = CachedVectorIndex::new(inner, Some(cfg));
    let conf = VectorIndexConfig::default();

    let qa = [1.0, 0.0, 0.0];
    let qb = [0.0, 1.0, 0.0];
    let qc = [0.0, 0.0, 1.0];

    idx.search(&qa, &conf).unwrap(); // 缓存 [A]
    idx.search(&qb, &conf).unwrap(); // 缓存 [A, B]
    assert_eq!(idx.cache_len(), 2);
    idx.search(&qa, &conf).unwrap(); // 命中 A → LRU 提升 [B, A]
    idx.search(&qc, &conf).unwrap(); // C 未命中 → 驱逐队头 B → [A, C]

    assert_eq!(idx.cache_len(), 2);
    let cache = idx.cache.lock().unwrap();
    let labels: Vec<&str> = cache
        .iter()
        .map(|(_, _, _, hits)| hits[0].doc_label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["a", "c"],
        "应淘汰最久未使用的 B（LRU 而非 FIFO）"
    );
}

/// 缓存命中提升：重复查询同一向量走缓存，不重复全量扫描（cache_len 不增长）。
#[test]
fn cached_index_hit_does_not_grow_cache() {
    let mut inner = BruteForceIndex::new();
    inner.add("a", vec![1.0, 0.0], 1);
    let cfg = VectorCacheConfig {
        max_entries: 8,
        enabled: true,
    };
    let idx = CachedVectorIndex::new(inner, Some(cfg));
    let conf = VectorIndexConfig::default();

    let q = [1.0, 0.0];
    for _ in 0..5 {
        let hits = idx.search(&q, &conf).unwrap();
        assert_eq!(hits[0].doc_label, "a");
    }
    assert_eq!(idx.cache_len(), 1, "同查询命中缓存，不新增条目");
}

/// 负分量不得被折叠为同一缓存 key：
/// 量化若把负分量截断为 0，仅负分量不同的查询会误命中同一缓存并返回错误结果。
#[test]
fn cached_index_negative_components_do_not_collide() {
    let mut inner = BruteForceIndex::new();
    inner.add("a", vec![1.0, -0.9, 0.2], 1);
    inner.add("b", vec![1.0, -0.1, 0.2], 2);
    let cfg = VectorCacheConfig {
        max_entries: 8,
        enabled: true,
    };
    let idx = CachedVectorIndex::new(inner, Some(cfg));
    let conf = VectorIndexConfig::default();

    // q1 与 a 同向，q2 与 b 同向；两者仅负分量不同
    let q1 = [1.0, -0.9, 0.2];
    let hits1 = idx.search(&q1, &conf).unwrap();
    assert_eq!(hits1[0].doc_label, "a");

    let q2 = [1.0, -0.1, 0.2];
    let hits2 = idx.search(&q2, &conf).unwrap();
    assert_eq!(
        hits2[0].doc_label, "b",
        "负分量不同不得命中同一缓存 key（否则返回上一次的错误结果）"
    );
}

/// `max_entries=0` 不得触发空表 `remove(0)` 越界 panic：
/// 容量下限保护为 1，退化为"仅保留最近一条"，两次不同查询结果各自正确。
#[test]
fn cached_index_zero_capacity_does_not_panic() {
    let mut inner = BruteForceIndex::new();
    inner.add("a", vec![1.0, 0.0, 0.0], 1);
    inner.add("b", vec![0.0, 1.0, 0.0], 2);
    let cfg = VectorCacheConfig {
        max_entries: 0,
        enabled: true,
    };
    let idx = CachedVectorIndex::new(inner, Some(cfg));
    let conf = VectorIndexConfig::default();

    let hits_a = idx.search(&[1.0, 0.0, 0.0], &conf).unwrap();
    assert_eq!(hits_a[0].doc_label, "a");

    let hits_b = idx.search(&[0.0, 1.0, 0.0], &conf).unwrap();
    assert_eq!(hits_b[0].doc_label, "b");

    assert_eq!(idx.cache_len(), 1, "容量 0 退化为仅保留最近一条");
}
