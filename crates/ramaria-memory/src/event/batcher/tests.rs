//! crates/ramaria-memory/src/event/batcher/tests.rs - TopicBatcher 单元测试
//!
//! 设计特点:
//! - 覆盖 L1Item 转换、TopicCluster 聚合、配置钳制、语义/关键词相似度
//! - 覆盖孤立节点语义吸附与 build_clusters 五步编排
//! - 使用合成输入，不依赖真实 LLM/embedding

use super::*;
use ramaria_core::keyword::KeywordToken;
use ramaria_core::types::{EvidenceNote, MemoryL1};
use uuid::Uuid;

// ---- L1Item::from ----

fn make_memory_l1(summary: &str, keywords: Option<&str>, salience: f64) -> MemoryL1 {
    MemoryL1 {
        id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        summary: summary.into(),
        keywords: keywords.map(|s| s.into()),
        time_period: None,
        atmosphere: None,
        valence: 0.0,
        salience,
        absorbed: false,
        created_at: 1_700_000_000_000,
        last_accessed_at: None,
        persona_uid: None,
        context_json: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    }
}

/// L1Item::from 各关键词输入参数化验证。
#[test]
fn l1_item_from_memory_l1_cases() {
    // 有关键词 → 解析为 3 个 token，保留 salience
    let item = L1Item::from(&make_memory_l1("测试摘要", Some("工作, 压力, 倦怠"), 0.75));
    assert_eq!(item.keywords.len(), 3);
    assert!((item.salience - 0.75).abs() < f64::EPSILON);
    assert!(item.embedding.is_none());
    // 无关键词 → 空
    let item = L1Item::from(&make_memory_l1("无关键词摘要", None, 0.5));
    assert!(item.keywords.is_empty());
    // 空关键词字符串 → 空
    let item = L1Item::from(&make_memory_l1("空关键词", Some(""), 0.5));
    assert!(item.keywords.is_empty());
}

/// v1.4 M4：L1Item::from 携带结构化 evidence_notes；
/// 缺失时为默认空 Vec（不产生 None 分支，下游消费形态稳定）。
#[test]
fn l1_item_from_memory_l1_carries_evidence_notes() {
    // MemoryL1 带结构化线索 → L1Item 完整复制
    let mut l1 = make_memory_l1("用户讨论项目延期", Some("项目,延期"), 0.5);
    l1.evidence_notes = Some(vec![EvidenceNote {
        text: "用户提到项目延期到月底".into(),
        time: Some("上周三".into()),
        who: Some("用户".into()),
        cause: Some("需求变更频繁".into()),
    }]);
    let item = L1Item::from(&l1);
    assert_eq!(item.evidence_notes.len(), 1);
    assert_eq!(item.evidence_notes[0].text, "用户提到项目延期到月底");
    assert_eq!(
        item.evidence_notes[0].cause.as_deref(),
        Some("需求变更频繁")
    );

    // MemoryL1 缺失 → 空 Vec
    let l1 = make_memory_l1("无线索摘要", None, 0.5);
    let item = L1Item::from(&l1);
    assert!(
        item.evidence_notes.is_empty(),
        "缺失 evidence_notes 时应为默认空 Vec"
    );
}

// ---- semantic_text（S2 语义增强输入组装，v3.1 §5）----

/// semantic_text 完整输入：summary + evidence_notes（含槽位）+ keywords 三段齐全。
#[test]
fn semantic_text_includes_summary_evidence_and_keywords() {
    let item = L1Item {
        id: Uuid::new_v4(),
        summary: "用户讨论项目延期安排".into(),
        keywords: vec![
            KeywordToken::new("项目").unwrap(),
            KeywordToken::new("延期").unwrap(),
        ],
        evidence_notes: vec![EvidenceNote {
            text: "用户提到项目延期到月底".into(),
            time: Some("上周三".into()),
            who: Some("用户".into()),
            cause: Some("需求变更频繁".into()),
        }],
        embedding: None,
        salience: 0.5,
        created_at: 1000,
    };
    let text = item.semantic_text();
    assert!(text.contains("用户讨论项目延期安排"), "应含 summary");
    assert!(text.contains("用户提到项目延期到月底"), "应含证据文本");
    assert!(text.contains("time: 上周三"), "应含 time 槽位");
    assert!(text.contains("who: 用户"), "应含 who 槽位");
    assert!(text.contains("cause: 需求变更频繁"), "应含 cause 槽位");
    assert!(text.contains("项目 延期"), "应含关键词段");
}

/// semantic_text 退化路径：无 evidence_notes / 无 keywords 时各段自动省略，
/// 保持"summary + 可用段"的稳定形态（embedding 输入不因缺数据而失真）。
#[test]
fn semantic_text_degrades_gracefully() {
    // 无线索 + 无关键词 → 仅 summary
    let item = L1Item {
        id: Uuid::new_v4(),
        summary: "仅摘要文本".into(),
        keywords: vec![],
        evidence_notes: vec![],
        embedding: None,
        salience: 0.5,
        created_at: 1000,
    };
    let text = item.semantic_text();
    assert_eq!(text, "仅摘要文本");

    // 无线索 + 有关键词 → summary + keywords
    let item = L1Item {
        id: Uuid::new_v4(),
        summary: "摘要".into(),
        keywords: vec![KeywordToken::new("工作").unwrap()],
        evidence_notes: vec![],
        embedding: None,
        salience: 0.5,
        created_at: 1000,
    };
    let text = item.semantic_text();
    assert!(text.contains("摘要"));
    assert!(text.contains("工作"));
    assert!(!text.contains("cause:"), "无线索时不应出现槽位标记");
}

/// semantic_text 多条线索以分隔符拼接（供 embedding 感知跨线索语义）。
#[test]
fn semantic_text_joins_multiple_evidence_notes() {
    let item = L1Item {
        id: Uuid::new_v4(),
        summary: "摘要".into(),
        keywords: vec![],
        evidence_notes: vec![
            EvidenceNote::new("用户提到项目延期到月底"),
            EvidenceNote {
                text: "用户表示压力很大".into(),
                cause: Some("工作量增加".into()),
                time: None,
                who: None,
            },
        ],
        embedding: None,
        salience: 0.5,
        created_at: 1000,
    };
    let text = item.semantic_text();
    assert!(text.contains(" ; "), "多条线索应以分隔符拼接");
    assert!(text.contains("用户提到项目延期到月底"));
    assert!(text.contains("用户表示压力很大"));
    assert!(text.contains("cause: 工作量增加"));
}

// ---- TopicCluster ----

#[test]
fn topic_cluster_basic() {
    let items = vec![
        L1Item {
            id: Uuid::new_v4(),
            summary: "s1".into(),
            keywords: vec![KeywordToken::new("工作").unwrap()],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: 2000,
        },
        L1Item {
            id: Uuid::new_v4(),
            summary: "s2".into(),
            keywords: vec![KeywordToken::new("压力").unwrap()],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.8,
            created_at: 1000,
        },
    ];
    let cluster = TopicCluster::new(items);
    assert_eq!(cluster.len(), 2);
    // 应按 created_at 正序排列
    assert_eq!(cluster.l1_items[0].created_at, 1000);
    assert_eq!(cluster.l1_items[1].created_at, 2000);
    assert!((cluster.avg_salience - 0.65).abs() < f64::EPSILON);
    assert_eq!(cluster.time_span, (1000, 2000));
}

#[test]
fn topic_cluster_deduplicates_keywords() {
    let items = vec![
        L1Item {
            id: Uuid::new_v4(),
            summary: "s1".into(),
            keywords: vec![
                KeywordToken::new("工作").unwrap(),
                KeywordToken::new("压力").unwrap(),
            ],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: 1000,
        },
        L1Item {
            id: Uuid::new_v4(),
            summary: "s2".into(),
            keywords: vec![
                KeywordToken::new("工作").unwrap(),
                KeywordToken::new("倦怠").unwrap(),
            ],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: 2000,
        },
    ];
    let cluster = TopicCluster::new(items);
    // 去重后应有 3 个唯一关键词：工作、压力、倦怠
    assert_eq!(cluster.cluster_keywords.len(), 3);
}

// ---- TopicBatcherConfig ----

/// TopicBatcherConfig 默认值 / builder / 边界钳制验证。
#[test]
fn config_cases() {
    // 默认值
    let c = TopicBatcherConfig::default();
    assert_eq!(c.min_cluster_size, 3);
    assert_eq!(c.max_cluster_size, 25);
    assert!((c.similarity_threshold - 0.2).abs() < f64::EPSILON);
    assert!((c.alpha - 0.5).abs() < f64::EPSILON);
    assert!((c.modularity_min - 0.3).abs() < f64::EPSILON);
    // builder 链
    let c = TopicBatcherConfig::new()
        .with_min_cluster_size(5)
        .with_max_cluster_size(30)
        .with_similarity_threshold(0.3)
        .with_alpha(0.7)
        .with_modularity_min(0.25);
    assert_eq!(c.min_cluster_size, 5);
    assert_eq!(c.max_cluster_size, 30);
    assert!((c.similarity_threshold - 0.3).abs() < f64::EPSILON);
    assert!((c.alpha - 0.7).abs() < f64::EPSILON);
    assert!((c.modularity_min - 0.25).abs() < f64::EPSILON);
    // 超界输入被钳制
    let c = TopicBatcherConfig::new()
        .with_similarity_threshold(1.5) // 超上限
        .with_alpha(-0.5) // 超下限
        .with_modularity_min(2.0); // 超上限
    assert!((c.similarity_threshold - 1.0).abs() < f64::EPSILON);
    assert!((c.alpha - 0.0).abs() < f64::EPSILON);
    assert!((c.modularity_min - 1.0).abs() < f64::EPSILON);
}

// ---- compute_semantic_score ----

/// compute_semantic_score 各 alpha/embedding 组合参数化验证。
#[test]
fn semantic_score_cases() {
    let emb_a = vec![1.0f32, 0.0];
    let emb_b = vec![1.0f32, 0.0]; // cos=1.0
    // score = 0.5 * 0.4 + 0.5 * 1.0 = 0.7
    let score = compute_semantic_score(Some(&emb_a), Some(&emb_b), 0.4, 0.5).unwrap();
    assert!((score - 0.7).abs() < 0.001);
    let emb_b = vec![0.0f32, 1.0]; // cos=0.0
    // score = 1.0 * 0.4 + 0.0 * 0.0 = 0.4
    let score = compute_semantic_score(Some(&emb_a), Some(&emb_b), 0.4, 1.0).unwrap();
    assert!((score - 0.4).abs() < 0.001);
    // 一边缺少 embedding → None
    assert!(compute_semantic_score(None, Some(&emb_a), 0.4, 0.5).is_none());
    assert!(compute_semantic_score(Some(&emb_a), None, 0.4, 0.5).is_none());
}

// =========================================================
// 孤立节点语义吸附测试
// =========================================================

/// 构造带 embedding 的 L1Item 辅助函数
fn make_l1_with_emb(keywords: Vec<&str>, embedding: Option<Vec<f32>>, salience: f64) -> L1Item {
    L1Item {
        id: Uuid::new_v4(),
        summary: format!("s_{}", keywords.join("_")),
        keywords: keywords.into_iter().filter_map(KeywordToken::new).collect(),
        evidence_notes: vec![],
        embedding,
        salience,
        created_at: 1_000_000,
    }
}

/// 无孤立节点 → 全部保留，无 remaining_orphans
#[test]
fn absorb_no_orphans_all_clusters() {
    let items = vec![
        make_l1_with_emb(vec!["工作", "压力"], Some(vec![1.0, 0.0]), 0.5),
        make_l1_with_emb(vec!["工作", "倦怠"], Some(vec![0.9, 0.1]), 0.5),
    ];
    let g = graph::KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();
    // 两个节点有关键词交集 → 1 个连通分量
    assert_eq!(comps.len(), 1);
    assert_eq!(comps[0].len(), 2);

    let (clusters, remaining) = absorb_orphans(&g, comps, 0.3);
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0].len(), 2);
    assert!(remaining.is_empty());
}

/// 孤立节点 embedding 相似度 ≥ 阈值 → 被吸附
#[test]
fn absorb_orphan_high_similarity() {
    // 簇: 两个相似节点
    let items = vec![
        make_l1_with_emb(vec!["工作", "压力"], Some(vec![1.0, 0.0]), 0.5),
        make_l1_with_emb(vec!["工作", "倦怠"], Some(vec![0.95, 0.05]), 0.5),
        // 孤立节点: 与簇的语义中心相似
        make_l1_with_emb(vec!["休闲"], Some(vec![0.9, 0.1]), 0.5),
    ];
    let g = graph::KeywordGraph::build_jaccard_graph(&items, 0.2);

    // items[0] 和 items[1] 共享"工作" → 连通
    // items[2] 关键词"休闲"无交集 → 孤立
    let comps = g.find_connected_components();

    let (clusters, remaining) = absorb_orphans(&g, comps, 0.3);
    // 孤立节点应与簇语义中心相似度 > 0.3 → 应被吸附
    // 结果应该只有 1 个簇（原簇 + 吸附的孤立节点）
    let total_in_clusters: usize = clusters.iter().map(|c| c.len()).sum();
    assert_eq!(total_in_clusters, 3, "孤立节点应被吸附到簇中");
    assert!(remaining.is_empty(), "不应有剩余孤立节点");
}

/// 孤立节点 embedding 相似度 < 阈值 → 进入 remaining_orphans
#[test]
fn absorb_orphan_low_similarity_goes_to_remaining() {
    let items = vec![
        make_l1_with_emb(vec!["工作", "压力"], Some(vec![1.0, 0.0]), 0.5),
        make_l1_with_emb(vec!["工作", "倦怠"], Some(vec![0.95, 0.05]), 0.5),
        // 孤立节点的 embedding 与簇完全相反
        make_l1_with_emb(vec!["休闲"], Some(vec![-1.0, 0.0]), 0.5),
    ];
    let g = graph::KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();

    let (clusters, remaining) = absorb_orphans(&g, comps, 0.3);
    // 孤立节点与簇 cosine ≈ -1.0 < 0.3 → 应进入 remaining
    assert_eq!(clusters.len(), 1, "原簇应保留");
    assert_eq!(remaining.len(), 1, "一个孤立节点应进入 remaining");
    assert_eq!(remaining[0].len(), 1);
}

/// 孤立节点无 embedding → 送入 remaining_orphans（由 Pending Buffer 处理）
#[test]
fn absorb_orphan_no_embedding_to_remaining() {
    let items = vec![
        make_l1_with_emb(vec!["工作", "压力"], Some(vec![1.0, 0.0]), 0.5),
        make_l1_with_emb(vec!["工作", "倦怠"], Some(vec![0.9, 0.1]), 0.5),
        // 孤立节点无 embedding
        make_l1_with_emb(vec!["休闲"], None, 0.5),
    ];
    let g = graph::KeywordGraph::build_jaccard_graph(&items, 0.2);
    let comps = g.find_connected_components();

    let (clusters, remaining) = absorb_orphans(&g, comps, 0.3);
    // 原簇保留在 clusters
    assert_eq!(clusters.len(), 1, "多节点簇应保留");
    assert_eq!(clusters[0].len(), 2);
    // 无 embedding 的孤立节点 → remaining_orphans
    assert_eq!(
        remaining.len(),
        1,
        "无 embedding 孤立节点应进入 remaining_orphans"
    );
    assert_eq!(remaining[0].len(), 1);
}

// =========================================================
// TopicBatcher::build_clusters 编排测试
// =========================================================

/// 空列表 → 返回空
#[test]
fn build_clusters_empty() {
    let mut batcher = TopicBatcher::new(TopicBatcherConfig::default());
    let (clusters, expired) = batcher.build_clusters(vec![], 1_000_000);
    assert!(clusters.is_empty());
    assert!(expired.is_empty());
}

/// 单条 L1 且 min_cluster_size=1 → 直接返回簇
#[test]
fn build_clusters_single_item() {
    let mut batcher = TopicBatcher::new(TopicBatcherConfig::new().with_min_cluster_size(1));
    let items = vec![make_l1_with_emb(vec!["工作"], None, 0.5)];
    let (clusters, expired) = batcher.build_clusters(items, 1_000_000);
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0].len(), 1);
    assert!(expired.is_empty());
}

/// 单条 L1 不足 min_cluster_size=3 → 进 Pending Buffer
#[test]
fn build_clusters_single_item_to_buffer() {
    let mut batcher = TopicBatcher::new(TopicBatcherConfig::default());
    let items = vec![make_l1_with_emb(vec!["工作"], None, 0.5)];
    let (clusters, expired) = batcher.build_clusters(items, 1_000_000);
    // min=3，1 条不足 → 无正式簇
    assert!(clusters.is_empty());
    assert!(expired.is_empty());
    assert_eq!(batcher.pending_buffer.total_items(), 1);
}

/// 连通分量直接成为簇（无需拆分/吸附）
#[test]
fn build_clusters_connected_component() {
    let mut batcher = TopicBatcher::new(
        TopicBatcherConfig::new()
            .with_min_cluster_size(2)
            .with_max_cluster_size(25),
    );
    let items = vec![
        make_l1_with_emb(vec!["工作", "压力"], None, 0.5),
        make_l1_with_emb(vec!["工作", "倦怠"], None, 0.5),
        make_l1_with_emb(vec!["工作", "加班"], None, 0.5),
    ];
    let (clusters, expired) = batcher.build_clusters(items, 1_000_000);
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0].len(), 3);
    assert!(expired.is_empty());
}

/// 关键词完全不相交 → 全部孤立，进 Pending Buffer
#[test]
fn build_clusters_all_isolated() {
    let mut batcher = TopicBatcher::new(TopicBatcherConfig::new().with_min_cluster_size(3));
    let items = vec![
        make_l1_with_emb(vec!["工作"], None, 0.5),
        make_l1_with_emb(vec!["休闲"], None, 0.5),
        make_l1_with_emb(vec!["学习"], None, 0.5),
    ];
    let (clusters, expired) = batcher.build_clusters(items, 1_000_000);
    // 全部孤立且不足 min=3 → 无正式簇
    assert!(clusters.is_empty());
    assert!(expired.is_empty());
    // 3 条都在缓冲区
    assert_eq!(batcher.pending_buffer.total_items(), 3);
}

/// 部分连通部分孤立：连通簇被提升，孤立节点进缓冲区
#[test]
fn build_clusters_mixed_connected_and_isolated() {
    let mut batcher = TopicBatcher::new(TopicBatcherConfig::new().with_min_cluster_size(2));
    let items = vec![
        make_l1_with_emb(vec!["工作", "压力"], None, 0.5),
        make_l1_with_emb(vec!["工作", "倦怠"], None, 0.5),
        make_l1_with_emb(vec!["休闲", "旅游"], None, 0.5),
    ];
    let (clusters, expired) = batcher.build_clusters(items, 1_000_000);
    // 前两条连通 → 1 个簇（2 条）；第三条孤立
    assert_eq!(clusters.len(), 1, "连通簇应被提升");
    assert_eq!(clusters[0].len(), 2);
    assert_eq!(batcher.pending_buffer.total_items(), 1, "孤立节点进缓冲区");
    assert!(expired.is_empty());
}

/// 簇按 avg_salience 降序排列
#[test]
fn build_clusters_sorted_by_salience() {
    let mut batcher = TopicBatcher::new(
        TopicBatcherConfig::new()
            .with_min_cluster_size(2)
            .with_max_cluster_size(25),
    );
    let items = vec![
        make_l1_with_emb(vec!["工作", "加班"], None, 0.3),
        make_l1_with_emb(vec!["工作", "会议"], None, 0.3),
        make_l1_with_emb(vec!["成就", "喜悦"], None, 0.9),
        make_l1_with_emb(vec!["成就", "成功"], None, 0.9),
    ];
    let (clusters, _) = batcher.build_clusters(items, 1_000_000);
    assert_eq!(clusters.len(), 2);
    // 高 salience 簇（"成就"）应排在前面
    assert!(
        clusters[0].avg_salience >= clusters[1].avg_salience,
        "簇应按 avg_salience 降序排列"
    );
}
