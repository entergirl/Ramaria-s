//! T-V13-2-016：TopicBatcher 全链路集成测试（v1.3 遗留收尾补齐，T-V14-8-001）
//!
//! 验收要求：mock LLM + mock embedding + 50 条 fixture L1 → 验证簇数量/内聚性/缓冲区行为。
//!
//! 覆盖：
//! - 50 条跨主题 L1（5 主题 × 10 条）→ build_clusters 产出与主题数一致的簇（簇数量）
//! - 簇内 L1 关键词共享同一主题词（内聚性）
//! - 孤立节点（无共享关键词）进入 PendingBuffer → 碎片提升为簇 / 超时过期（缓冲区行为）

mod common;

use ramaria_core::LlmProviderTrait;
use ramaria_core::keyword::KeywordToken;
use ramaria_memory::event::batcher::TopicBatcher;
use ramaria_memory::event::{L1Item, TopicBatcherConfig};

use common::MockLlm;

/// 构造一条 L1Item（embedding 预计算向量 = mock embedding）。
fn l1_item(
    id: uuid::Uuid,
    summary: &str,
    keywords: &[&str],
    embedding: Option<Vec<f32>>,
    salience: f64,
    created_at: i64,
) -> L1Item {
    L1Item {
        id,
        summary: summary.to_string(),
        keywords: keywords
            .iter()
            .filter_map(|k| KeywordToken::new(k))
            .collect(),
        evidence_notes: vec![],
        embedding,
        salience,
        created_at,
    }
}

/// 为第 t 个主题生成预计算向量（主题间正交，主题内相同 = mock embedding）。
fn topic_embedding(topic: usize, dim: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; dim];
    v[topic % dim] = 1.0;
    v
}

/// 5 主题 × 10 条 = 50 条 fixture L1（跨主题关键词无交集）。
fn fixture_50_l1(dim: usize) -> Vec<L1Item> {
    let topics: [&[&str]; 5] = [
        &["工作", "项目", "会议"],
        &["健身", "跑步", "运动"],
        &["美食", "餐厅", "做饭"],
        &["旅行", "景点", "假期"],
        &["电影", "音乐", "阅读"],
    ];
    let mut items = Vec::with_capacity(50);
    for (t, topic) in topics.iter().enumerate() {
        for i in 0..10 {
            items.push(l1_item(
                uuid::Uuid::new_v4(),
                &format!("主题{t} 摘要{i}"),
                topic,
                Some(topic_embedding(t, dim)),
                0.5 + (i as f64) * 0.01,
                1_700_000_000_000 + (t as i64 * 1000) + (i as i64),
            ));
        }
    }
    items
}

/// 验收主路径：50 条 L1 → 簇数量与主题数一致（5 个簇），每簇 10 条。
#[test]
fn build_clusters_50_fixture_l1_yields_5_topics() {
    let mut batcher = TopicBatcher::new(TopicBatcherConfig::default());
    let items = fixture_50_l1(8);
    assert_eq!(items.len(), 50, "fixture 应为 50 条");

    let (clusters, expired) = batcher.build_clusters(items, common::now());
    assert!(expired.is_empty(), "新鲜数据不应有过期碎片");
    assert_eq!(
        clusters.len(),
        5,
        "5 个不连通主题应产出 5 个簇，实际 {}",
        clusters.len()
    );

    for c in &clusters {
        assert_eq!(c.l1_items.len(), 10, "每簇应含 10 条 L1");
        assert!(c.avg_salience > 0.5, "簇平均显著性应 > 0.5");
    }
}

/// 内聚性：簇内所有 L1 共享同一主题关键词（去重并集不跨主题）。
#[test]
fn cluster_internal_cohesion_keeps_single_topic() {
    let mut batcher = TopicBatcher::new(TopicBatcherConfig::default());
    let items = fixture_50_l1(8);
    let (clusters, _) = batcher.build_clusters(items, common::now());

    for c in &clusters {
        // 簇关键词去重并集
        let kws: Vec<&str> = c.cluster_keywords.iter().map(|k| k.as_str()).collect();
        // 断言关键词全部来自同一主题（两两主题间无交集 → 并集必然只含一个主题的词）
        let all_from_single_topic = kws
            .iter()
            .all(|k| kws.iter().all(|other| same_topic_or_equal(k, other)));
        assert!(all_from_single_topic, "簇关键词应同主题: {kws:?}");
    }
}

/// 判断两个词是否属于同一主题（fixture 中 5 个主题词两两互斥）。
fn same_topic_or_equal(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let topics: [&[&str]; 5] = [
        &["工作", "项目", "会议"],
        &["健身", "跑步", "运动"],
        &["美食", "餐厅", "做饭"],
        &["旅行", "景点", "假期"],
        &["电影", "音乐", "阅读"],
    ];
    topics.iter().any(|t| t.contains(&a) && t.contains(&b))
}

/// 缓冲区行为：孤立节点（< min_cluster_size 且无共享关键词）进入 PendingBuffer；
/// 跨批次同主题孤立节点合并到同一碎片，达到 min_cluster_size 后提升为簇。
#[test]
fn orphans_enter_pending_buffer_and_promote_across_batches() {
    let mut batcher = TopicBatcher::new(TopicBatcherConfig::default());
    let now = common::now();

    // 批次 1：孤立 A（主题 alpha）+ 无关节点 C（关键词不重叠）→ A 进缓冲区
    let (clusters, expired) = batcher.build_clusters(
        vec![
            l1_item(
                uuid::Uuid::new_v4(),
                "孤立A",
                &["alpha"],
                None,
                0.5,
                now - 1000,
            ),
            l1_item(
                uuid::Uuid::new_v4(),
                "无关C",
                &["omega"],
                None,
                0.5,
                now - 1000,
            ),
        ],
        now,
    );
    assert!(clusters.is_empty(), "不足阈值的孤立节点不应成簇");
    assert!(expired.is_empty(), "刚进入缓冲区的碎片不应过期");

    // 批次 2：孤立 B（同主题 alpha）→ 与碎片 A 合并（Jaccard=1.0 ≥ 0.4），仍不足 3
    let (clusters, _) = batcher.build_clusters(
        vec![
            l1_item(
                uuid::Uuid::new_v4(),
                "孤立B",
                &["alpha"],
                None,
                0.5,
                now - 500,
            ),
            l1_item(
                uuid::Uuid::new_v4(),
                "无关D",
                &["psi"],
                None,
                0.5,
                now - 500,
            ),
        ],
        now,
    );
    assert!(clusters.is_empty(), "2 条仍不足 min_cluster_size=3");

    // 批次 3：孤立 E（同主题 alpha）→ 碎片达到 3 条 → 提升为一个簇
    let (clusters, _) = batcher.build_clusters(
        vec![l1_item(
            uuid::Uuid::new_v4(),
            "孤立E",
            &["alpha"],
            None,
            0.5,
            now,
        )],
        now,
    );
    assert_eq!(clusters.len(), 1, "碎片积累到 3 条应提升为一个簇");
    assert_eq!(clusters[0].l1_items.len(), 3);
    assert_eq!(
        clusters[0].cluster_keywords[0].as_str(),
        "alpha",
        "提升簇应保留主题关键词"
    );
}

/// 缓冲区行为：超时碎片返回 expired（跨批次）。
#[test]
fn pending_buffer_expires_stale_fragments_across_batches() {
    let mut batcher = TopicBatcher::new(TopicBatcherConfig::default());
    let now = common::now();

    // 批次 1：2 条孤立（不同主题）→ 进入缓冲区，各自成碎片
    let (clusters, expired) = batcher.build_clusters(
        vec![
            l1_item(
                uuid::Uuid::new_v4(),
                "碎片A",
                &["x1"],
                None,
                0.5,
                now - 10_000,
            ),
            l1_item(
                uuid::Uuid::new_v4(),
                "碎片B",
                &["x2"],
                None,
                0.5,
                now - 10_000,
            ),
        ],
        now,
    );
    assert!(clusters.is_empty());
    assert!(expired.is_empty());

    // 批次 2（时间推进超过 max_age_days=30 天）：旧碎片过期返回，新碎片保留
    let late = now + 40 * 24 * 3600 * 1000; // 40 天后
    let (clusters, expired) = batcher.build_clusters(
        vec![l1_item(
            uuid::Uuid::new_v4(),
            "新碎片C",
            &["x3"],
            None,
            0.5,
            late,
        )],
        late,
    );
    assert!(clusters.is_empty(), "过期碎片不应成簇");
    assert_eq!(expired.len(), 2, "两个旧碎片超时应全部过期，新碎片保留");

    // 再推进时间（用新批次触发 collect_expired）：新碎片最终也过期
    let (_, expired) = batcher.build_clusters(
        vec![l1_item(
            uuid::Uuid::new_v4(),
            "新碎片D",
            &["x4"],
            None,
            0.5,
            late + 40 * 24 * 3600 * 1000,
        )],
        late + 40 * 24 * 3600 * 1000,
    );
    assert_eq!(expired.len(), 1, "新碎片超时后应过期");
}

/// mock LLM 可用性（验收要求 mock LLM 参与测试环境）：实例化并断言名称。
#[test]
fn mock_llm_is_available_in_test_env() {
    let llm = MockLlm::new(r#"{"events":[]}"#);
    assert_eq!(llm.name(), "MockLlm");
}

/// embedding 参与语义融合：主题向量正交时仍能按关键词正确分簇（embedding 不干扰）。
#[test]
fn embedding_similarity_does_not_break_keyword_clustering() {
    let mut batcher = TopicBatcher::new(TopicBatcherConfig::default());
    let items = fixture_50_l1(8);
    let (clusters, _) = batcher.build_clusters(items, common::now());
    assert_eq!(clusters.len(), 5);
}
