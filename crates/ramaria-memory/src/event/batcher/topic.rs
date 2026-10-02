//! crates/ramaria-memory/src/event/batcher/topic.rs - TopicBatcher 主题批量构建器
//!
//! 设计特点:
//! - 从未吸收 L1 摘要构建关键词 Jaccard 图，通过连通分量实现语义聚类
//! - 五步编排：图构建 → BFS 连通分量 → 模块度拆分 + 孤立吸附 → 缓冲区处理 → 簇排序
//! - 跨批次碎片管理：未达阈值的碎片留在 PendingBuffer 中，下次批次继续积累
//! - 纯计算编排，不依赖 LLM 或数据库

use super::buffer;
use super::cluster::TopicCluster;
use super::config::TopicBatcherConfig;
use super::graph;
use super::item::L1Item;
use super::similarity::absorb_orphans;

// =========================================================
// TopicBatcher — 主题批量构建器
// =========================================================

/// 主题批量构建器——将未吸收 L1 摘要按语义聚类为 TopicCluster。
///
/// 职责:
/// - 持有 `TopicBatcherConfig` 和 `PendingBuffer`（跨批次持久化碎片状态）。
/// - `build_clusters()`: 五步编排，将 `Vec<L1Item>` 转为 `Vec<TopicCluster>`。
/// - 跨批次碎片管理: 未达阈值的碎片留在 PendingBuffer 中，下次批次继续积累。
///
/// 五步编排（`build_clusters`）:
/// 1. 关键词 Jaccard 图构建: `KeywordGraph::build_jaccard_graph()`
/// 2. BFS 连通分量: `find_connected_components()`
/// 3. 模块度拆分 + 孤立吸附: `split_large_components()` → `absorb_orphans()`
/// 4. 缓冲区处理: 未吸附孤立节点 → `add_fragment()`；`drain_promoted()`；`collect_expired()`
/// 5. 簇排序: 簇内按时间正序，簇间按 avg_salience 降序
///
/// 使用示例:
/// ```
/// use ramaria_memory::event::batcher::{L1Item, TopicBatcher};
/// use ramaria_memory::TopicBatcherConfig;
/// use ramaria_core::keyword::KeywordToken;
/// use uuid::Uuid;
///
/// let item = L1Item {
///     id: Uuid::new_v4(),
///     summary: "用户提到工作压力".into(),
///     keywords: vec![
///         KeywordToken::new("工作").unwrap(),
///         KeywordToken::new("压力").unwrap(),
///     ],
///     evidence_notes: vec![],
///     embedding: None,
///     salience: 0.6,
///     created_at: 1_000,
/// };
/// let mut batcher = TopicBatcher::new(TopicBatcherConfig::default());
/// // 单条 L1 不足 min_cluster_size(3) → 进入 Pending Buffer，不产出正式簇
/// let (clusters, expired) = batcher.build_clusters(vec![item], 2_000);
/// assert!(clusters.is_empty());
/// assert!(expired.is_empty());
/// assert_eq!(batcher.pending_buffer.fragment_count(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct TopicBatcher {
    pub config: TopicBatcherConfig,
    pub pending_buffer: buffer::PendingBuffer,
}

impl TopicBatcher {
    /// 创建新的 TopicBatcher。
    pub fn new(config: TopicBatcherConfig) -> Self {
        let min_cluster = config.min_cluster_size;
        Self {
            config,
            pending_buffer: buffer::PendingBuffer::new(min_cluster, 30),
        }
    }

    /// 五步编排：将 L1 条目列表聚类为主题簇。
    ///
    /// 参数:
    /// - `l1_items`: 待聚类的 L1 条目（通常来自 `list_unabsorbed_l1`）。
    /// - `now_ms`: 当前 Unix 毫秒时间戳，用于碎片超时计算。
    ///
    /// 返回:
    /// - `(clusters, expired_fragments)`:
    ///   - `clusters`: 按 avg_salience 降序排列的正式主题簇。
    ///   - `expired_fragments`: 超时未归并的碎片（由 EventExtractor 降级合并处理）。
    ///
    /// 降级策略:
    /// - L1 条目数 ≤ 1: 直接包装为单元素 TopicCluster，跳过图构建。
    /// - embedding 全部不可用: α 自动退化为 1.0（纯关键词图），不影响离线可用性。
    /// - 全部分量为孤立节点: 跳过模块度拆分和吸附，全部进入 Pending Buffer。
    pub fn build_clusters(
        &mut self,
        l1_items: Vec<L1Item>,
        now_ms: i64,
    ) -> (Vec<TopicCluster>, Vec<buffer::PendingFragment>) {
        let n = l1_items.len();

        // 空列表或单条 L1 → 直接返回
        if n == 0 {
            return (vec![], vec![]);
        }
        if n == 1 {
            let cluster = TopicCluster::new(l1_items);
            if cluster.len() >= self.config.min_cluster_size {
                return (vec![cluster], vec![]);
            } else {
                // 单条 L1 不足 min_cluster_size → 送 Pending Buffer
                let item = cluster.l1_items.into_iter().next().unwrap();
                self.pending_buffer.add_fragment(item, now_ms);
                // 检查是否有碎片因本次添加而达到阈值
                let promoted = self.drain_promoted_as_clusters();
                let expired = self.pending_buffer.collect_expired(now_ms);
                return (promoted, expired);
            }
        }

        // Step 1: 构建关键词 Jaccard 图
        let graph =
            graph::KeywordGraph::build_jaccard_graph(&l1_items, self.config.similarity_threshold);
        tracing::debug!(
            l1_count = n,
            node_count = graph.node_count(),
            edge_count = graph.edge_count(),
            "关键词图构建完成"
        );

        // Step 2: BFS 连通分量
        let components = graph.find_connected_components();
        tracing::debug!(component_count = components.len(), "连通分量发现完成");

        // Step 3: 模块度拆分 + 孤立节点语义吸附
        let split = graph::split_large_components(
            &graph,
            components,
            self.config.max_cluster_size,
            self.config.modularity_min,
        );

        let (multi_node, orphans) = absorb_orphans(&graph, split, 0.3);
        tracing::debug!(
            multi_node_count = multi_node.len(),
            orphan_count = orphans.len(),
            "模块度拆分与孤立吸附完成"
        );

        // Step 4: 缓冲区处理
        // 4a. 无法吸附的孤立节点 → 送入 Pending Buffer
        for orphan_comp in orphans {
            for node_idx in orphan_comp {
                let l1_idx = graph.nodes[node_idx].l1_index;
                if l1_idx < l1_items.len() {
                    let item = l1_items[l1_idx].clone();
                    self.pending_buffer.add_fragment(item, now_ms);
                }
            }
        }

        // 4b. 排出已达标的碎片
        let promoted_clusters = self.drain_promoted_as_clusters();

        // 4c. 收集超时碎片
        let expired = self.pending_buffer.collect_expired(now_ms);

        // Step 5: 将多节点分量转为 TopicCluster 并排序
        let mut clusters: Vec<TopicCluster> = multi_node
            .into_iter()
            .map(|comp| {
                let items: Vec<L1Item> = comp
                    .iter()
                    .filter_map(|&node_idx| {
                        let l1_idx = graph.nodes[node_idx].l1_index;
                        if l1_idx < l1_items.len() {
                            Some(l1_items[l1_idx].clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                TopicCluster::new(items)
            })
            .filter(|c| !c.is_empty())
            .collect();

        // 合并提升的碎片
        clusters.extend(promoted_clusters);

        // 簇间按 avg_salience 降序排列
        clusters.sort_by(|a, b| {
            b.avg_salience
                .partial_cmp(&a.avg_salience)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        tracing::info!(
            l1_input = n,
            cluster_count = clusters.len(),
            expired_count = expired.len(),
            "TopicBatcher 聚类完成"
        );

        (clusters, expired)
    }

    /// 从 PendingBuffer 排出达标碎片并包装为 TopicCluster。
    fn drain_promoted_as_clusters(&mut self) -> Vec<TopicCluster> {
        let promoted = self.pending_buffer.drain_promoted();
        promoted.into_iter().map(TopicCluster::new).collect()
    }
}
