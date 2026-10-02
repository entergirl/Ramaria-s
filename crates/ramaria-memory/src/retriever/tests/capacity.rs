//! crates/ramaria-memory/src/retriever/tests/capacity.rs - 检索通道回归用例
//!
//! 设计特点:
//! - 由 父测试模块 以 mod capacity; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 检索通道修复回归用例
// =========================================================

/// 请求 top_k 可超过 RRF 配置默认 top_k（融合截断按请求值而非配置默认值）。
///
/// 修复前: `search_with_keyword_hits` 融合直接使用 `self.config.rrf.top_k`
/// （默认 5），即使请求 `top_k = 12` 也会被截到 ≤ 5 条——core 配置的较大
/// 检索条数静默失效。
/// 修复后: 融合前把 `rrf_config.top_k` 对齐为 `request.top_k`，请求多少就
/// 能拿到多少（受实际命中数限制）。
#[test]
fn request_top_k_beyond_rrf_default_is_reachable() {
    let mut r = Retriever::new();
    r.config_mut().enable_vector = false;
    r.config_mut().enable_graph = false;

    // 15 篇均含 "Rust" 的 L1 文档，全部可被 BM25 命中
    for i in 0..15 {
        r.index_l1(&l1_view_doc(
            uuid::Uuid::new_v4(),
            &format!("Rust 学习记录 {i}"),
            1000 + i as i64,
        ));
    }

    let req = SearchRequest {
        query: "Rust".to_string(),
        persona_uid: None,
        top_k: 12,
        filter_share: false,
    };
    let results = r.search(&req, None);
    // 修复前 RRF 融合先按 RrfConfig.top_k 默认 5 截断，结果永远 ≤ 5
    assert_eq!(
        results.len(),
        12,
        "请求 top_k=12 时不应被 RRF 配置默认 top_k=5 截断"
    );
}

/// 空 embedding（0 字节 BLOB）→ 解码失败按"无向量"降级为子串检索，
/// 且不污染向量索引的期望维度（后续真实向量仍可用）。
///
/// 修复前: 空 BLOB 被解码为 0 维向量写入索引，索引首个期望维度被记为 0，
/// 后续真实维度向量全部被拒绝，向量通道整体静默失效（下方向量断言会失败）。
/// 修复后: 空 BLOB 视为非法输入，块仅入内存文档（子串降级可命中），
/// 后续真实向量的写入与检索不受影响。
#[test]
fn index_utt_block_empty_blob_degrades_and_keeps_vector_channel_usable() {
    use ramaria_core::types::UttBlock;
    let mut r = Retriever::new();
    let mut block = UttBlock::new(
        "char-0001".to_string(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "空向量块仍可子串命中".to_string(),
        3,
        1000,
    );
    block.embedding = Some(Vec::new()); // 空 BLOB → 解码失败 → 按无向量降级
    r.index_utt_block(&block);

    // 向量索引为空 → 子串降级仍可命中该块
    let hits = r.search_utt("子串命中", Some(&[1.0, 0.0]), 5, Some("char-0001"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].channel, "substring", "空 BLOB 应降级为子串检索");

    // 后续真实向量写入不被 0 维记录污染 → 向量通道仍可用
    r.index_utt(
        &make_utt_doc(2, "char-0001", "块2文本内容", 2000),
        Some(vec![1.0, 0.0]),
    );
    let hits = r.search_utt("块2文本", Some(&[1.0, 0.0]), 5, Some("char-0001"));
    assert_eq!(hits.len(), 1, "块2应经向量通道命中");
    assert_eq!(hits[0].channel, "vector", "后续真实维度向量应正常入索引");
}

/// L1/L2 LRU 驱逐：超过容量上限的 110% 才触发，一次驱逐"超出上限"的全部条目。
///
/// 修复前: 文档数一超过上限就立即触发驱逐，上限 10 时插入第 11 篇即被清理
/// 回 10 篇（本用例首个断言失败），驱逐频率随每次写入放大。
/// 修复后: 上限 10 时 11 篇（=110%）不触发；第 12 篇触发一次批量驱逐，
/// 按 created_at 最旧优先清理 2 篇回到上限，BM25 索引同步清理。
#[test]
fn lru_eviction_triggers_at_110_percent_and_evicts_oldest() {
    let mut r = Retriever::new();
    r.set_lru_max_entries(10);

    // 11 篇 = 上限的 110%，未超水位 → 不驱逐
    for i in 0..11 {
        r.index_l1(&l1_view_doc(
            uuid::Uuid::new_v4(),
            &format!("LRU 驱逐用例文档 {i}"),
            1000 + i as i64,
        ));
    }
    assert_eq!(r.doc_count(), 11, "未超 110% 水位不应驱逐");

    // 第 12 篇超过水位 → 一次驱逐超出上限的 2 篇（最旧）
    r.index_l1(&l1_view_doc(uuid::Uuid::new_v4(), "触发驱逐的新文档", 2000));
    assert_eq!(r.doc_count(), 10, "触发后应一次驱逐回容量上限");

    // 最旧两篇（created_at=1000/1001）已被清理，BM25 同步
    assert!(
        r.l1_docs
            .values()
            .all(|d| d.created_at != 1000 && d.created_at != 1001),
        "最旧两篇应按 created_at 被驱逐"
    );
    assert_eq!(
        r.bm25_index.doc_count(),
        10,
        "BM25 索引应与内存文档同步清理"
    );
}

/// utt 块容量治理：超过 `utt_max_entries` 的 110% 时驱逐最旧块，
/// 并同步清理其 `L0:{id}` 向量；未驱逐块与子串检索不受影响。
///
/// 修复前: utt 块不参与容量治理（不设上限），长期导入后内存随块数无限增长。
/// 修复后: 上限 2 时插入第 3 块（超过 110%）触发一次驱逐；被驱逐块的内存
/// 视图与向量一并清理，未驱逐块的向量保留、子串降级仍可命中。
#[test]
fn utt_capacity_evicts_oldest_block_and_its_vector() {
    use crate::vector::VectorIndex;
    let mut r = Retriever::new();
    r.set_utt_max_entries(2);
    // 过滤零相似度命中，验证被驱逐块的向量确已清理
    r.config_mut().vector.min_similarity = 0.01;

    r.index_utt(
        &make_utt_doc(1, "char-0001", "第一块内容", 1000),
        Some(vec![1.0, 0.0]),
    );
    r.index_utt(
        &make_utt_doc(2, "char-0001", "第二块内容", 2000),
        Some(vec![0.0, 1.0]),
    );
    assert_eq!(r.utt_doc_count(), 2, "未超 110% 水位不应驱逐");

    // 第 3 块无向量；插入后超过上限 110%（2 → 3）→ 驱逐最旧块（块1）
    r.index_utt(&make_utt_doc(3, "char-0001", "第三块内容", 3000), None);
    assert_eq!(r.utt_doc_count(), 2, "超限后应驱逐回容量上限");

    // 块1 的文本与向量都已清理：查询不再命中（剩余块向量与之正交且被相似度阈值过滤）
    assert!(
        r.search_utt("第一块", Some(&[1.0, 0.0]), 5, Some("char-0001"))
            .is_empty(),
        "被驱逐块不得再被检索到"
    );

    // 向量清理与保留：L0:1 已清，L0:2（块2）保留，L0:3（块3 无向量）不存在
    let labels = r.vector_mut().labels();
    assert!(
        !labels.contains(&"L0:1".to_string()),
        "被驱逐块的向量应一并清理"
    );
    assert!(labels.contains(&"L0:2".to_string()), "未驱逐块的向量应保留");
    assert!(
        !labels.contains(&"L0:3".to_string()),
        "无向量块不应写入向量索引"
    );

    // 块3 仍可子串降级命中
    let hits = r.search_utt("第三块", None, 5, Some("char-0001"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].doc.id, 3);
    assert_eq!(hits[0].channel, "substring");
}

/// 图谱通道按通道身份取权重：向量缺席时图谱独有命中应能因高 graph_weight 排首位。
///
/// 修复前: 融合把图谱通道结果按 `bm25_weight` 计权（通道身份丢失），
/// `graph_weight=1.0` 且 `bm25_weight=0.1` 时图谱实体与 BM25 文档同分并列，
/// 图谱独有命中无法凭权重排首位。
/// 修复后: 图谱项按 `graph_weight` 计权，图谱实体（rank 1）排首位，
/// BM25 独有文档因低 bm25_weight 居后。
#[test]
fn graph_channel_uses_graph_weight_when_vector_absent() {
    let mut r = Retriever::new();
    r.config_mut().enable_vector = false;
    r.config_mut().rrf.bm25_weight = 0.1;
    r.config_mut().rrf.graph_weight = 1.0;

    // 仅 BM25 通道可命中该文档（向量通道关闭、图谱此时无数据）
    r.index_l1(&l1_view_doc(
        uuid::Uuid::new_v4(),
        "用户在学习机器学习",
        1000,
    ));

    // 图谱通道命中实体 "机器学习"（无边孤立节点，match_score=1.0）
    r.graph_mut()
        .load(&[(1, "机器学习".to_string(), "concept".to_string())], &[]);

    let req = SearchRequest {
        query: "机器学习".to_string(),
        persona_uid: None,
        top_k: 10,
        filter_share: false,
    };
    let results = r.search(&req, None);
    assert!(!results.is_empty(), "BM25 与图谱通道均应有命中");
    assert!(
        matches!(&results[0].doc_id, DocId::Graph(_)),
        "修复前图谱项被按 bm25_weight 计权，图谱独有命中无法因高 graph_weight 排首位"
    );
}
