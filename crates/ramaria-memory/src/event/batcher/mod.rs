//! rust/crates/ramaria-memory/src/event/batcher/mod.rs - TopicBatcher 主题批量构建器
//!
//! 设计特点:
//! - 从未吸收 L1 摘要构建关键词 Jaccard 图，通过连通分量实现语义聚类
//! - L1 embedding 语义增强: 与关键词 Jaccard 按 α 权重融合（默认 α=0.5）
//! - embedding 不可用时自动退化为纯关键词图（α=1.0），保证离线可用性
//! - `L1Item` 为 `MemoryL1` 的聚类专用精简视图，通过 `From` trait 转换
//! - `TopicBatcherConfig` 集中管理所有聚类参数，支持 Builder 模式
//! - 纯计算模块，不依赖 LLM 或数据库，仅依赖 ramaria-core 的 KeywordToken

pub mod buffer;
pub mod graph;

use ramaria_core::keyword::KeywordToken;
use ramaria_core::types::{EvidenceNote, MemoryL1};
use uuid::Uuid;

// =========================================================
// L1Item — 聚类专用精简视图
// =========================================================

/// L1 摘要的聚类专用视图。
///
/// 职责:
/// - 从 `MemoryL1` 精简提取 TopicBatcher 所需的字段，降低聚类过程中的内存占用。
/// - 关键词从逗号分隔字符串解析为 `Vec<KeywordToken>`。
/// - embedding 字段由上层注入（从向量索引查询后挂载）。
///
/// 字段约定:
/// - `keywords`: 标准化后的关键词列表（已通过 `KeywordToken::new()` 过滤）。
/// - `evidence_notes`: 结构化证据线索（v1.4，供 S2 语义增强输入组装）。
/// - `embedding`: L1 摘要文本的向量表示（384 维），None 表示未配置嵌入模型。
#[derive(Debug, Clone)]
pub struct L1Item {
    pub id: Uuid,
    pub summary: String,
    pub keywords: Vec<KeywordToken>,
    /// 结构化证据线索（v1.4 M4 起参与语义增强输入组装）
    pub evidence_notes: Vec<EvidenceNote>,
    pub embedding: Option<Vec<f32>>,
    pub salience: f64,
    pub created_at: i64,
}

impl From<&MemoryL1> for L1Item {
    /// 从 MemoryL1 构造聚类用精简视图。
    ///
    /// 说明:
    /// - 关键词从 `keywords` 字段解析，通过 `KeywordToken::new()` 标准化。
    /// - evidence_notes 直接复制结构化线索（缺失/为空 → 空 Vec）。
    /// - embedding 初始化为 None，由上层在构建图之前注入。
    fn from(l1: &MemoryL1) -> Self {
        let keywords = l1
            .keywords
            .as_deref()
            .map(|kw_str| {
                kw_str
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .filter_map(KeywordToken::new)
                    .collect()
            })
            .unwrap_or_default();

        Self {
            id: l1.id,
            summary: l1.summary.clone(),
            keywords,
            evidence_notes: l1.evidence_notes.clone().unwrap_or_default(),
            embedding: None,
            salience: l1.salience,
            created_at: l1.created_at,
        }
    }
}

impl L1Item {
    /// 组装 S2 语义增强的 embedding 输入文本（v3.1 §5：summary + evidence_notes + keywords）。
    ///
    /// 设计:
    /// - 上层用此文本调用 embedding provider 生成向量后挂载到 `embedding` 字段。
    /// - evidence_notes 为空时自动退化为 `summary + keywords`，保证无线索时输入形态稳定。
    /// - 结构化槽位（time/who/cause）按"槽位名: 值"拼接，供模型感知因果线索语义。
    ///
    /// 返回:
    /// - 拼接后的语义文本（单行，各段以空格分隔）。
    pub fn semantic_text(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(3);
        parts.push(self.summary.trim().to_string());

        // 证据线索段：仅取 text 槽位与可选槽位（time/who/cause），逐条拼接
        let evidence_part: Vec<String> = self
            .evidence_notes
            .iter()
            .map(|note| {
                let mut seg = note.text.trim().to_string();
                if let Some(time) = note.time.as_deref() {
                    seg.push_str(&format!(" time: {time}"));
                }
                if let Some(who) = note.who.as_deref() {
                    seg.push_str(&format!(" who: {who}"));
                }
                if let Some(cause) = note.cause.as_deref() {
                    seg.push_str(&format!(" cause: {cause}"));
                }
                seg
            })
            .collect();
        if !evidence_part.is_empty() {
            parts.push(evidence_part.join(" ; "));
        }

        // 关键词段
        if !self.keywords.is_empty() {
            let kw_joined: Vec<&str> = self.keywords.iter().map(|k| k.as_str()).collect();
            parts.push(kw_joined.join(" "));
        }

        parts.join(" ")
    }
}

// =========================================================
// TopicCluster — 主题簇
// =========================================================

/// 由 TopicBatcher 产出的主题簇。
///
/// 职责:
/// - 封装一组语义相近的 L1 摘要，作为后续事件提取（L1→L2）的批次输入。
/// - 携带簇级别的聚合统计信息（平均显著性、时间跨度、代表性关键词）。
///
/// 字段约定:
/// - `l1_items`: 按 `created_at` 时间正序排列。
/// - `cluster_keywords`: 簇内所有 L1 关键词的去重并集（保留高频词顺序）。
/// - `avg_salience`: 所有 L1 salience 的算术均值。
/// - `time_span`: (最早时间戳, 最晚时间戳)，均为 Unix 毫秒。
#[derive(Debug, Clone)]
pub struct TopicCluster {
    /// 簇内的 L1 条目（按 created_at 正序）
    pub l1_items: Vec<L1Item>,
    /// 簇级别的去重关键词集合
    pub cluster_keywords: Vec<KeywordToken>,
    /// 平均显著性
    pub avg_salience: f64,
    /// 时间跨度 (earliest_ms, latest_ms)
    pub time_span: (i64, i64),
}

impl TopicCluster {
    /// 从一组 L1Item 构造 TopicCluster。
    ///
    /// 说明:
    /// - 自动计算 `cluster_keywords`（去重并集）、`avg_salience`、`time_span`。
    /// - 调用方应保证 `l1_items` 非空。
    pub fn new(mut l1_items: Vec<L1Item>) -> Self {
        // 按时间正序排列
        l1_items.sort_by_key(|item| item.created_at);

        // 收集簇内所有关键词（去重）
        let mut kw_set: std::collections::BTreeMap<String, KeywordToken> =
            std::collections::BTreeMap::new();
        for item in &l1_items {
            for kw in &item.keywords {
                kw_set
                    .entry(kw.as_str().to_string())
                    .or_insert_with(|| kw.clone());
            }
        }
        let cluster_keywords: Vec<KeywordToken> = kw_set.into_values().collect();

        // 平均显著性
        let avg_salience = if l1_items.is_empty() {
            0.0
        } else {
            let sum: f64 = l1_items.iter().map(|i| i.salience).sum();
            sum / l1_items.len() as f64
        };

        // 时间跨度
        let time_span = if l1_items.is_empty() {
            (0, 0)
        } else {
            let earliest = l1_items.first().map(|i| i.created_at).unwrap_or(0);
            let latest = l1_items.last().map(|i| i.created_at).unwrap_or(0);
            (earliest, latest)
        };

        Self {
            l1_items,
            cluster_keywords,
            avg_salience,
            time_span,
        }
    }

    /// 返回簇内 L1 条目数量。
    pub fn len(&self) -> usize {
        self.l1_items.len()
    }

    /// 簇是否为空。
    pub fn is_empty(&self) -> bool {
        self.l1_items.is_empty()
    }
}

// =========================================================
// TopicBatcherConfig — 聚类参数配置
// =========================================================

/// TopicBatcher 配置。
///
/// 职责:
/// - 集中管理所有聚类参数，避免散布的魔法值。
/// - 提供 `default()` 和 builder 模式方便构造。
///
/// 字段约定:
/// - `min_cluster_size`: 簇的最小 L1 条目数。不足此数的簇进入 Pending Buffer。默认 3。
/// - `max_cluster_size`: 簇的最大 L1 条目数。超此数触发模块度 Q 二分拆分。默认 25。
/// - `similarity_threshold` (θ_sim): Jaccard 边的相似度阈值。仅保留 sim ≥ θ_sim 的边。默认 0.2。
/// - `alpha`: 关键词-语义融合权重。α=1.0 为纯关键词图，α=0.0 为纯语义图。默认 0.5。
/// - `modularity_min` (Q_min): 模块度拆分停止阈值。Q < Q_min 时不再继续拆分。默认 0.3。
#[derive(Debug, Clone)]
pub struct TopicBatcherConfig {
    pub min_cluster_size: usize,
    pub max_cluster_size: usize,
    pub similarity_threshold: f64,
    pub alpha: f64,
    pub modularity_min: f64,
}

impl Default for TopicBatcherConfig {
    fn default() -> Self {
        Self {
            min_cluster_size: 3,
            max_cluster_size: 25,
            similarity_threshold: 0.2,
            alpha: 0.5,
            modularity_min: 0.3,
        }
    }
}

impl TopicBatcherConfig {
    /// 创建使用默认值的配置。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置最小簇大小。
    pub fn with_min_cluster_size(mut self, size: usize) -> Self {
        self.min_cluster_size = size;
        self
    }

    /// 设置最大簇大小。
    pub fn with_max_cluster_size(mut self, size: usize) -> Self {
        self.max_cluster_size = size;
        self
    }

    /// 设置 Jaccard 相似度阈值。
    pub fn with_similarity_threshold(mut self, threshold: f64) -> Self {
        self.similarity_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// 设置关键词-语义融合权重 α。
    ///
    /// 参数:
    /// - `alpha`: 0.0..1.0，1.0=纯关键词图，0.0=纯语义图。
    pub fn with_alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }

    /// 设置模块度最小值 Q_min。
    pub fn with_modularity_min(mut self, q_min: f64) -> Self {
        self.modularity_min = q_min.clamp(0.0, 1.0);
        self
    }
}

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
/// ```ignore
/// let mut batcher = TopicBatcher::new(TopicBatcherConfig::default());
/// let (clusters, expired) = batcher.build_clusters(l1_items, now_ms);
/// for cluster in &clusters {
///     // 每个 cluster 送入 LLM 做事件提取
/// }
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

// =========================================================
// 语义相似度计算
// =========================================================

/// 计算两个 L1Item 的组合相似度得分。
///
/// 公式: `score = α × sim_kw + (1-α) × sim_sem`
///
/// 说明:
/// - `sim_kw`: 关键词 Jaccard 相似度（由调用方预先计算）。
/// - `sim_sem`: 基于 L1 embedding 的余弦相似度。
/// - 若任一 embedding 为 None，返回 None（调用方应降级为 α=1.0 纯关键词）。
///
/// 参数:
/// - `sim_kw`: 已计算的关键词 Jaccard 相似度。
/// - `alpha`: 关键词权重，1.0=纯关键词，0.0=纯语义。
///
/// 返回:
/// - `Some(score)`: 融合相似度得分。
/// - `None`: 缺少 embedding，无法计算语义部分。
pub fn compute_semantic_score(
    embedding_a: Option<&[f32]>,
    embedding_b: Option<&[f32]>,
    sim_kw: f64,
    alpha: f64,
) -> Option<f64> {
    let emb_a = embedding_a?;
    let emb_b = embedding_b?;
    let sim_sem = cosine_similarity(emb_a, emb_b);
    Some(alpha * sim_kw + (1.0 - alpha) * sim_sem)
}

/// 计算两个等长向量的余弦相似度。
///
/// 公式: `cos(θ) = (A·B) / (||A|| × ||B||)`
///
/// 说明:
/// - 若任一向量范数为零，返回 0.0（零向量无方向，相似度最低）。
/// - 结果钳制到 [-1.0, 1.0]（防御浮点误差）。
/// - 两个向量的长度必须相等，否则返回 0.0 并记录 warn 日志。
///
/// 参数:
/// - `a`: 第一个向量。
/// - `b`: 第二个向量（必须与 a 等长）。
///
/// 返回:
/// - 余弦相似度值，范围 [-1.0, 1.0]。
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        tracing::warn!(
            a_len = a.len(),
            b_len = b.len(),
            "余弦相似度: 向量长度不一致，返回 0.0"
        );
        return 0.0;
    }

    if a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for i in 0..a.len() {
        let ai = a[i] as f64;
        let bi = b[i] as f64;
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    let cos = dot / (norm_a.sqrt() * norm_b.sqrt());
    cos.clamp(-1.0, 1.0)
}

// =========================================================
// Jaccard 相似度计算
// =========================================================

/// 计算两个关键词列表的 Jaccard 相似度。
///
/// 公式: `J(A, B) = |A ∩ B| / |A ∪ B|`
///
/// 说明:
/// - 使用 `KeywordToken` 的 `PartialEq` 进行比较（已标准化）。
/// - 若并集为空（两组关键词均为空），返回 0.0。
/// - 时间复杂度 O(n²)，n 通常 < 20，可接受。
///
/// 参数:
/// - `kw_a`: 第一个关键词列表。
/// - `kw_b`: 第二个关键词列表。
///
/// 返回:
/// - Jaccard 相似度，范围 [0.0, 1.0]。
pub fn jaccard_similarity(kw_a: &[KeywordToken], kw_b: &[KeywordToken]) -> f64 {
    if kw_a.is_empty() && kw_b.is_empty() {
        return 0.0;
    }

    // 计算交集大小
    let intersection = kw_a.iter().filter(|k| kw_b.contains(k)).count();

    // 计算并集大小
    // 使用去重后的关键词数量
    let mut union_set: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for k in kw_a {
        union_set.insert(k.as_str());
    }
    for k in kw_b {
        union_set.insert(k.as_str());
    }
    let union_size = union_set.len();

    if union_size == 0 {
        return 0.0;
    }

    intersection as f64 / union_size as f64
}

// =========================================================
// 孤立节点语义吸附
// =========================================================

/// 尝试将孤立节点（单节点连通分量）语义吸附到已有簇中。
///
/// 算法:
/// 1. 分离多节点簇（≥ 2）和孤立节点（= 1）。
/// 2. 对每个多节点簇，计算其语义中心向量（各节点 embedding 的均值）。
/// 3. 对每个孤立节点，计算其 embedding 与各簇语义中心的余弦相似度。
/// 4. 若最大相似度 ≥ θ_attach，将孤立节点并入该簇。
/// 5. 无法吸附的孤立节点（无 embedding 或相似度不足）返回到 `remaining_orphans`，
///    由上层送入 Pending Buffer。
///
/// 参数:
/// - `graph`: 关键词 Jaccard 图（含节点 embedding）。
/// - `components`: 连通分量列表（来自 `split_large_components`）。
/// - `theta_attach`: 语义吸附相似度阈值，默认 0.3。
///
/// 返回:
/// - `(clusters, remaining_orphans)`:
///   - `clusters`: 仅多节点簇（可能经语义吸附扩充）。
///   - `remaining_orphans`: 所有未被吸附的孤立节点（含无 embedding 者），由上层送 Pending Buffer。
///
/// 降级策略:
/// - 孤立节点无 embedding: 送入 `remaining_orphans`（交由 Pending Buffer 按关键词逻辑处理）。
/// - 无多节点簇可供吸附: 所有孤立节点送入 `remaining_orphans`。
/// - 簇无节点有 embedding（全簇 embedding=None）: 该簇跳过，不参与吸附。
pub fn absorb_orphans(
    graph: &graph::KeywordGraph,
    components: Vec<Vec<usize>>,
    theta_attach: f64,
) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
    // 分离多节点簇和孤立节点
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut orphans: Vec<Vec<usize>> = Vec::new();

    for comp in components {
        if comp.len() >= 2 {
            clusters.push(comp);
        } else {
            orphans.push(comp);
        }
    }

    // 无孤立节点或无可吸附目标 → 所有孤儿送入 remaining_orphans
    if orphans.is_empty() || clusters.is_empty() {
        return (clusters, orphans);
    }

    // 计算各簇的语义中心
    let centroids: Vec<Option<Vec<f32>>> = clusters
        .iter()
        .map(|comp| compute_centroid(graph, comp))
        .collect();

    let mut remaining_orphans: Vec<Vec<usize>> = Vec::new();

    for orphan in orphans {
        let node_idx = orphan[0];
        let node_emb = match &graph.nodes[node_idx].embedding {
            Some(emb) => emb,
            None => {
                // 无 embedding → 送入 remaining_orphans，由 Pending Buffer 按关键词逻辑处理
                remaining_orphans.push(orphan);
                continue;
            }
        };

        let mut best_sim: f64 = 0.0;
        let mut best_cluster: Option<usize> = None;

        for (ci, centroid_opt) in centroids.iter().enumerate() {
            if let Some(centroid) = centroid_opt {
                let sim = cosine_similarity(node_emb, centroid);
                if sim > best_sim {
                    best_sim = sim;
                    best_cluster = Some(ci);
                }
            }
        }

        if best_sim >= theta_attach
            && let Some(ci) = best_cluster
        {
            tracing::debug!(
                orphan_idx = node_idx,
                target_cluster = ci,
                similarity = best_sim,
                "孤立节点语义吸附成功"
            );
            clusters[ci].push(node_idx);
            continue;
        }

        // 相似度不足，送入 remaining_orphans
        tracing::debug!(
            orphan_idx = node_idx,
            best_similarity = best_sim,
            theta_attach,
            "孤立节点语义相似度不足，送入 Pending Buffer"
        );
        remaining_orphans.push(orphan);
    }

    (clusters, remaining_orphans)
}

/// 计算一个簇内所有节点的 embedding 均值向量（语义中心）。
///
/// 参数:
/// - `graph`: 关键词 Jaccard 图。
/// - `component`: 簇的节点索引列表。
///
/// 返回:
/// - `Some(centroid)`: 簇内所有有 embedding 的节点的均值向量。
/// - `None`: 簇内无节点有 embedding（全为 None）。
fn compute_centroid(graph: &graph::KeywordGraph, component: &[usize]) -> Option<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    let mut count: usize = 0;

    for &idx in component {
        if let Some(emb) = &graph.nodes[idx].embedding {
            count += 1;
            match &mut sum {
                None => sum = Some(emb.clone()),
                Some(s) => {
                    for (si, &ei) in s.iter_mut().zip(emb.iter()) {
                        *si += ei;
                    }
                }
            }
        }
    }

    if count == 0 {
        return None;
    }

    sum.map(|s| s.into_iter().map(|v| v / count as f32).collect())
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

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

    /// v1.4 M4（T-V14-4-004）：L1Item::from 携带结构化 evidence_notes；
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

    // ---- jaccard_similarity ----

    /// jaccard_similarity 各输入参数化验证。
    #[test]
    fn jaccard_similarity_cases() {
        fn kw(s: &str) -> KeywordToken {
            KeywordToken::new(s).unwrap()
        }
        let cases: Vec<(Vec<KeywordToken>, Vec<KeywordToken>, f64)> = vec![
            (
                vec![kw("工作"), kw("压力")],
                vec![kw("工作"), kw("压力")],
                1.0,
            ),
            (vec![kw("工作")], vec![kw("休闲")], 0.0),
            // 交集=1（工作），并集=3（工作、压力、倦怠）→ 1/3 ≈ 0.333
            (
                vec![kw("工作"), kw("压力")],
                vec![kw("工作"), kw("倦怠")],
                1.0 / 3.0,
            ),
            (vec![], vec![], 0.0),
            (vec![kw("工作")], vec![], 0.0),
        ];
        for (a, b, expected) in cases {
            assert!(
                (jaccard_similarity(&a, &b) - expected).abs() < 0.001,
                "期望 {expected}"
            );
        }
    }

    // ---- cosine_similarity ----

    /// cosine_similarity 各输入参数化验证（含零向量与长度不一致）。
    #[test]
    fn cosine_similarity_cases() {
        let cases: Vec<(Vec<f32>, Vec<f32>, f64)> = vec![
            (vec![1.0, 0.0, 0.0], vec![1.0, 0.0, 0.0], 1.0),
            (vec![1.0, 0.0], vec![0.0, 1.0], 0.0),
            (vec![1.0, 0.0], vec![-1.0, 0.0], -1.0),
            (vec![0.0, 0.0], vec![1.0, 0.0], 0.0),
            (vec![1.0, 0.0], vec![1.0, 0.0, 0.0], 0.0), // 长度不一致
        ];
        for (a, b, expected) in cases {
            assert!(
                (cosine_similarity(&a, &b) - expected).abs() < f64::EPSILON,
                "期望 {expected}"
            );
        }
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
}
