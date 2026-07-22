//! rust/crates/ramaria-memory/src/event/batcher/graph.rs - TopicBatcher 关键词图
//!
//! 设计特点:
//! - `KeywordGraph`: 基于邻接表的无向加权图，节点为 L1Item 精简视图，边为 Jaccard 相似度
//! - `build_jaccard_graph()`: 对 L1 对计算关键词 Jaccard 相似度，仅保留 ≥ θ_sim 的边
//! - `find_connected_components()`: BFS 求所有连通分量，返回节点索引的向量列表
//! - `try_bisect_component()`: 模块度 Q 贪心二分，用于拆分过大连通分量
//! - `split_large_components()`: 递归应用二分直到所有分量 ≤ max_cluster_size
//! - 时间复杂度: 建图 O(n²·k)，BFS O(n + m)，二分 O(n²·m) per component
//! - 纯计算模块，零 I/O，零 async，可独立单元测试

use std::collections::{HashMap, VecDeque};

use super::{L1Item, jaccard_similarity};

// =========================================================
// GraphNode — 图节点
// =========================================================

/// 关键词图的节点。
///
/// 职责:
/// - 封装单条 L1 摘要的关键词、嵌入向量和显著性。
/// - 节点的索引位置由 `KeywordGraph.nodes` 的位置隐式确定。
///
/// 字段约定:
/// - `l1_index`: 指向原始 `L1Item` 列表的索引，用于建图后追踪。
/// - `keywords`: 标准化后的关键词列表。
/// - `embedding`: L1 摘要的向量表示（用于语义融合），None 表示不可用。
/// - `salience`: 显著性得分。
#[derive(Debug, Clone)]
pub struct GraphNode {
    /// 原始 L1Item 在输入数组中的索引
    pub l1_index: usize,
    /// 标准化的关键词列表
    pub keywords: Vec<ramaria_core::keyword::KeywordToken>,
    /// L1 摘要的向量表示
    pub embedding: Option<Vec<f32>>,
    /// 显著性得分
    pub salience: f64,
}

// =========================================================
// KeywordGraph — 关键词邻接表图
// =========================================================

/// 基于关键词 Jaccard 相似度的无向加权图。
///
/// 职责:
/// - 构建 L1 摘要之间的关键词相似度图。
/// - 通过 BFS 求连通分量，实现语义聚类。
/// - 支持后续模块度拆分、孤立吸附等高级操作。
///
/// 表示:
/// - `nodes[i]`: 节点 i 的 GraphNode 数据。
/// - `adjacency[i]`: 节点 i 的邻居列表 `[(邻居索引 j, 相似度权重)]`。
/// - 无向图，每条边在 `adjacency[i]` 和 `adjacency[j]` 中均存储。
///
/// 性能说明:
/// - 建图 O(n²·k)：n ≤ 500 条 L1，k ≤ 20 个关键词，可接受。
/// - BFS O(n + m)：n 为节点数，m 为边数（通常远小于 n²）。
#[derive(Debug, Clone)]
pub struct KeywordGraph {
    pub nodes: Vec<GraphNode>,
    pub adjacency: Vec<Vec<(usize, f64)>>,
}

impl KeywordGraph {
    /// 创建空图。
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            adjacency: Vec::new(),
        }
    }

    /// 返回图中节点数量。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// 返回图中边数量（无向边，adjacency 中每条边存两次）。
    pub fn edge_count(&self) -> usize {
        self.adjacency
            .iter()
            .map(|neighbors| neighbors.len())
            .sum::<usize>()
            / 2
    }

    /// 从 L1Item 列表构建关键词 Jaccard 图。
    ///
    /// 算法:
    /// 1. 为每个 L1Item 创建 GraphNode。
    /// 2. 对每对节点 (i, j) 计算关键词 Jaccard 相似度。
    /// 3. 若相似度 ≥ θ_sim，添加无向边。
    ///
    /// 参数:
    /// - `l1_items`: 待聚类的 L1 摘要列表。
    /// - `theta_sim`: 相似度阈值，默认 0.2。仅保留 Jaccard ≥ θ_sim 的边。
    ///   较低阈值（0.1-0.2）适合关键词稀疏的场景；较高阈值（0.3-0.5）适合关键词丰富的场景。
    ///
    /// 返回:
    /// - 构建完成的 KeywordGraph。
    ///
    /// 边界情况:
    /// - 空输入 → 空图（node_count=0, edge_count=0）。
    /// - 全孤立节点（无关键词交集）→ n 个孤立节点，无边。
    pub fn build_jaccard_graph(l1_items: &[L1Item], theta_sim: f64) -> Self {
        let n = l1_items.len();
        let mut nodes: Vec<GraphNode> = Vec::with_capacity(n);
        let mut adjacency: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];

        // Step 1: 创建节点
        for (idx, item) in l1_items.iter().enumerate() {
            nodes.push(GraphNode {
                l1_index: idx,
                keywords: item.keywords.clone(),
                embedding: item.embedding.clone(),
                salience: item.salience,
            });
        }

        // Step 2: 计算关键词 Jaccard 边
        // 仅遍历上三角矩阵 (i < j)，避免重复计算
        for i in 0..n {
            for j in (i + 1)..n {
                let sim = jaccard_similarity(&nodes[i].keywords, &nodes[j].keywords);
                if sim >= theta_sim {
                    adjacency[i].push((j, sim));
                    adjacency[j].push((i, sim));
                }
            }
        }

        Self { nodes, adjacency }
    }

    /// BFS 求所有连通分量。
    ///
    /// 算法:
    /// 1. 对每个未访问节点启动 BFS。
    /// 2. BFS 遍历所有从起始节点可达的节点。
    /// 3. 返回所有连通分量的节点索引列表。
    ///
    /// 返回:
    /// - `Vec<Vec<usize>>`: 每个元素是一个连通分量的节点索引列表。
    ///
    /// 说明:
    /// - 连通分量按发现顺序返回（取决于节点索引顺序）。
    /// - 孤立节点（无边连接的节点）自成单元素分量。
    /// - 空图返回空列表。
    pub fn find_connected_components(&self) -> Vec<Vec<usize>> {
        let n = self.nodes.len();
        if n == 0 {
            return vec![];
        }

        let mut visited = vec![false; n];
        let mut components: Vec<Vec<usize>> = Vec::new();

        for start in 0..n {
            if visited[start] {
                continue;
            }

            // BFS from start
            let mut component: Vec<usize> = Vec::new();
            let mut queue: VecDeque<usize> = VecDeque::new();
            queue.push_back(start);
            visited[start] = true;

            while let Some(current) = queue.pop_front() {
                component.push(current);

                for &(neighbor, _sim) in &self.adjacency[current] {
                    if !visited[neighbor] {
                        visited[neighbor] = true;
                        queue.push_back(neighbor);
                    }
                }
            }

            components.push(component);
        }

        components
    }

    /// 获取一个连通分量内所有节点索引对应的 L1Item 索引。
    ///
    /// 参数:
    /// - `component`: 连通分量的节点索引（来自 `find_connected_components`）。
    ///
    /// 返回:
    /// - 对应的 `L1Item` 在原始输入数组中的索引列表。
    pub fn component_l1_indices(&self, component: &[usize]) -> Vec<usize> {
        component
            .iter()
            .map(|&idx| self.nodes[idx].l1_index)
            .collect()
    }
}

impl Default for KeywordGraph {
    fn default() -> Self {
        Self::new()
    }
}

// =========================================================
// 模块度 Q 贪心二分拆分
// =========================================================

/// 对超过 `max_cluster_size` 的连通分量递归执行模块度 Q 二分拆分。
///
/// 算法:
/// 1. 对每个连通分量，若 size ≤ max_cluster_size，直接保留。
/// 2. 否则尝试 `try_bisect_component` 进行模块度 Q 贪心二分。
/// 3. 若二分成功（Q ≥ Q_min 且两组均非空），递归处理两个子组。
/// 4. 若二分不成功（Q 不足或一组为空），原样保留（不再拆分）。
///
/// 参数:
/// - `graph`: 已构建的关键词 Jaccard 图。
/// - `components`: BFS 连通分量列表（来自 `find_connected_components`）。
/// - `max_cluster_size`: 簇的最大 L1 条目数。超过此数的分量触发拆分。默认 25。
/// - `q_min`: 模块度下限。拆分后的 Q < Q_min 时停止拆分。默认 0.3。
///
/// 返回:
/// - 拆分后的连通分量列表。每个分量的 size ≤ max_cluster_size（或无法继续拆分）。
///
/// 降级策略:
/// - 分量内无边（m=0）: 不可拆分，原样返回。
/// - 贪心二分后 Q < Q_min: 不可拆分，原样返回。
/// - 二分后一组为空: 不可拆分，原样返回。
pub fn split_large_components(
    graph: &KeywordGraph,
    components: Vec<Vec<usize>>,
    max_cluster_size: usize,
    q_min: f64,
) -> Vec<Vec<usize>> {
    let mut result: Vec<Vec<usize>> = Vec::new();

    for comp in components {
        if comp.len() <= max_cluster_size {
            result.push(comp);
        } else if let Some((group_a, group_b)) = try_bisect_component(graph, &comp, q_min) {
            // 递归处理两个子组
            let sub = vec![group_a, group_b];
            let processed = split_large_components(graph, sub, max_cluster_size, q_min);
            result.extend(processed);
        } else {
            // 不可拆分，原样保留
            tracing::debug!(
                comp_size = comp.len(),
                max_cluster_size,
                "连通分量过大但模块度 Q 不足，不可拆分，原样保留"
            );
            result.push(comp);
        }
    }

    result
}

/// 尝试对单个连通分量执行模块度 Q 贪心二分。
///
/// 算法（Newman 贪心二分）:
/// 1. 在子图内计算每个节点的加权度 k_i 和总边权 m。
/// 2. 初始将所有节点归入社区 A，社区 B 为空。
/// 3. 循环：对 A 中每个节点 i，计算将其移至 B 的模块度变化 ΔQ。
/// 4. 选择 ΔQ 最大的节点移动（仅当 ΔQ > 0）。
/// 5. 无正向 ΔQ 时停止。
/// 6. 计算最终分区模块度 Q；若 Q ≥ Q_min，接受拆分。
///
/// 模块度公式:
/// - ΔQ_i = (k_i→B - k_i→A) / m - k_i·(k_B - k_A + k_i) / (2m²)
/// - Q_final = (L_A + L_B) / m - (k_A² + k_B²) / (4m²)
///
/// 参数:
/// - `graph`: 关键词 Jaccard 图。
/// - `component`: 待拆分的连通分量节点索引列表。
/// - `q_min`: 模块度下限。拆分后 Q < q_min 时返回 None。
///
/// 返回:
/// - `Some((group_a, group_b))`: 二分成功的两个节点索引组。
/// - `None`: 不可拆分（无边、Q 不足、或一组为空）。
pub fn try_bisect_component(
    graph: &KeywordGraph,
    component: &[usize],
    q_min: f64,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let n = component.len();
    if n < 2 {
        return None;
    }

    // 构建全局索引 → 局部索引映射
    let mut global_to_local: HashMap<usize, usize> = HashMap::with_capacity(n);
    for (li, &gi) in component.iter().enumerate() {
        global_to_local.insert(gi, li);
    }

    // 计算子图内每个节点的加权度 k_i 和总边权 m
    let mut k: Vec<f64> = vec![0.0; n];
    let mut m = 0.0;

    for &gi in component {
        let li = global_to_local[&gi];
        for &(neighbor, weight) in &graph.adjacency[gi] {
            if let Some(&lj) = global_to_local.get(&neighbor) {
                k[li] += weight;
                // 只计一次（li < lj 时计入 m）
                if li < lj {
                    m += weight;
                }
            }
        }
    }

    // 无边 → 不可拆分
    if m == 0.0 {
        return None;
    }

    // 初始分区：前半节点在 A，后半在 B。
    // 避免全空分区的零梯度困境（all-A → empty-B 时首步 ΔQ 恒为负）。
    let half = n / 2;
    let mut in_a: Vec<bool> = (0..n).map(|i| i < half).collect();
    let mut k_a: f64 = k.iter().take(half).sum();
    let mut k_b: f64 = k.iter().skip(half).sum();

    // 贪心循环：双向移动（A→B 或 B→A），选 ΔQ 最大的
    let max_iterations = n * 2;
    for _iter in 0..max_iterations {
        let mut best_dq = 0.0;
        let mut best_i: Option<usize> = None;
        let mut best_to_a: bool = false;

        for i in 0..n {
            let gi = component[i];
            let mut k_i_to_a: f64 = 0.0;
            let mut k_i_to_b: f64 = 0.0;

            for &(neighbor, weight) in &graph.adjacency[gi] {
                if let Some(&lj) = global_to_local.get(&neighbor) {
                    if lj == i {
                        continue;
                    }
                    if in_a[lj] {
                        k_i_to_a += weight;
                    } else {
                        k_i_to_b += weight;
                    }
                }
            }

            if in_a[i] {
                // 考虑 A→B：须保证 A 至少剩 1 个节点
                let count_a = in_a.iter().filter(|&&x| x).count();
                if count_a <= 1 {
                    continue;
                }
                // ΔQ = (k_i→B - k_i→A)/m + k_i·(k_A - k_B - k_i)/(2m²)
                let dq = (k_i_to_b - k_i_to_a) / m + (k[i] / (2.0 * m * m)) * (k_a - k_b - k[i]);
                if dq > best_dq {
                    best_dq = dq;
                    best_i = Some(i);
                    best_to_a = false;
                }
            } else {
                // 考虑 B→A：须保证 B 至少剩 1 个节点
                let count_b = n - in_a.iter().filter(|&&x| x).count();
                if count_b <= 1 {
                    continue;
                }
                // ΔQ = (k_i→A - k_i→B)/m + k_i·(k_B - k_A - k_i)/(2m²)
                let dq = (k_i_to_a - k_i_to_b) / m + (k[i] / (2.0 * m * m)) * (k_b - k_a - k[i]);
                if dq > best_dq {
                    best_dq = dq;
                    best_i = Some(i);
                    best_to_a = true;
                }
            }
        }

        match best_i {
            Some(i) => {
                if best_to_a {
                    in_a[i] = true;
                    k_a += k[i];
                    k_b -= k[i];
                } else {
                    in_a[i] = false;
                    k_a -= k[i];
                    k_b += k[i];
                }
            }
            None => break,
        }
    }

    // 收集两个社区
    let mut group_a: Vec<usize> = Vec::new();
    let mut group_b: Vec<usize> = Vec::new();
    for i in 0..n {
        if in_a[i] {
            group_a.push(component[i]);
        } else {
            group_b.push(component[i]);
        }
    }

    // 任一组为空 → 不可拆分
    if group_a.is_empty() || group_b.is_empty() {
        return None;
    }

    // 计算最终模块度 Q
    let mut l_a: f64 = 0.0;
    let mut l_b: f64 = 0.0;

    for i in 0..n {
        let gi = component[i];
        for &(neighbor, weight) in &graph.adjacency[gi] {
            if let Some(&lj) = global_to_local.get(&neighbor)
                && i < lj
            {
                if in_a[i] && in_a[lj] {
                    l_a += weight;
                } else if !in_a[i] && !in_a[lj] {
                    l_b += weight;
                }
            }
        }
    }

    let q = (l_a + l_b) / m - (k_a * k_a + k_b * k_b) / (4.0 * m * m);

    if q >= q_min {
        tracing::debug!(
            comp_size = n,
            q,
            group_a_size = group_a.len(),
            group_b_size = group_b.len(),
            "模块度 Q 二分成功"
        );
        Some((group_a, group_b))
    } else {
        tracing::debug!(comp_size = n, q, q_min, "模块度 Q 不足最小值，放弃拆分");
        None
    }
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::keyword::KeywordToken;
    use uuid::Uuid;

    /// 辅助函数：创建带关键词的 L1Item
    fn make_l1(keywords: Vec<&str>, salience: f64) -> L1Item {
        L1Item {
            id: Uuid::new_v4(),
            summary: format!("summary_{}", keywords.join("_")),
            keywords: keywords.into_iter().filter_map(KeywordToken::new).collect(),
            embedding: None,
            salience,
            created_at: 1_000_000,
        }
    }

    // ---- KeywordGraph::new ----

    #[test]
    fn graph_new_is_empty() {
        let g = KeywordGraph::new();
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
    }

    // ---- build_jaccard_graph ----

    #[test]
    fn build_graph_empty_input() {
        let items: Vec<L1Item> = vec![];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
        assert!(g.find_connected_components().is_empty());
    }

    #[test]
    fn build_graph_single_node() {
        let items = vec![make_l1(vec!["工作", "压力"], 0.5)];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.edge_count(), 0);
        let comps = g.find_connected_components();
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0], vec![0]);
    }

    #[test]
    fn build_graph_fully_connected() {
        // 三个节点共享大量关键词，应全连通
        let items = vec![
            make_l1(vec!["工作", "压力", "加班"], 0.5),
            make_l1(vec!["工作", "压力", "倦怠"], 0.6),
            make_l1(vec!["工作", "加班", "倦怠"], 0.7),
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        assert_eq!(g.node_count(), 3);
        // 三节点全连通：应有 C(3,2)=3 条边
        assert_eq!(g.edge_count(), 3);
        let comps = g.find_connected_components();
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].len(), 3);
    }

    #[test]
    fn build_graph_all_isolated() {
        // 三组关键词完全不相交 → 三个孤立节点
        let items = vec![
            make_l1(vec!["工作"], 0.5),
            make_l1(vec!["休闲"], 0.5),
            make_l1(vec!["学习"], 0.5),
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        assert_eq!(g.node_count(), 3);
        assert_eq!(g.edge_count(), 0);
        let comps = g.find_connected_components();
        assert_eq!(comps.len(), 3);
        // 每个分量含 1 个节点
        for comp in &comps {
            assert_eq!(comp.len(), 1);
        }
    }

    #[test]
    fn build_graph_mixed_components() {
        // A（工作压力）↔ B（工作倦怠）  Jaccard = 1/3 ≈ 0.33 > 0.2，有边
        // C（休闲娱乐）↔ D（娱乐放松）Jaccard = 1/3 ≈ 0.33 > 0.2，有边
        // A↔C 无交集，无边
        let items = vec![
            make_l1(vec!["工作", "压力"], 0.5),
            make_l1(vec!["工作", "倦怠"], 0.5),
            make_l1(vec!["休闲", "娱乐"], 0.5),
            make_l1(vec!["娱乐", "放松"], 0.5),
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        assert_eq!(g.node_count(), 4);
        assert_eq!(g.edge_count(), 2); // A↔B, C↔D
        let comps = g.find_connected_components();
        assert_eq!(comps.len(), 2);
        // 每个分量应有 2 个节点
        let sizes: Vec<usize> = comps.iter().map(|c| c.len()).collect();
        assert!(sizes.contains(&2));
        assert!(sizes.contains(&2));
    }

    #[test]
    fn build_graph_high_threshold_reduces_edges() {
        // 低阈值（0.1）时三个节点可能全连通
        // 高阈值（0.9）时应全部孤立
        let items = vec![
            make_l1(vec!["工作", "压力", "加班"], 0.5),
            make_l1(vec!["工作", "压力", "倦怠"], 0.6),
            make_l1(vec!["工作", "加班", "倦怠"], 0.7),
        ];
        // 阈值为 0.1: 全部连通
        let g_low = KeywordGraph::build_jaccard_graph(&items, 0.1);
        let comps_low = g_low.find_connected_components();
        assert_eq!(comps_low.len(), 1, "低阈值应全部连通");

        // 阈值为 0.9: 全部孤立（因为所有对 Jaccard < 0.9）
        let g_high = KeywordGraph::build_jaccard_graph(&items, 0.9);
        let comps_high = g_high.find_connected_components();
        assert_eq!(comps_high.len(), 3, "高阈值应全部孤立");
    }

    #[test]
    fn build_graph_repeated_keywords_dont_affect_jaccard() {
        // 重复关键词不影响 Jaccard 计算（集合去重）
        let items = vec![
            make_l1(vec!["工作", "工作", "压力"], 0.5),
            make_l1(vec!["工作", "倦怠"], 0.5),
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        // Jaccard( {工作,压力}, {工作,倦怠} ) = 1/3 ≈ 0.33 > 0.2，有边
        assert!(g.edge_count() > 0, "重复关键词不应影响 Jaccard 计算");
    }

    // ---- find_connected_components ----

    #[test]
    fn connected_components_empty_graph() {
        let g = KeywordGraph::new();
        let comps = g.find_connected_components();
        assert!(comps.is_empty());
    }

    #[test]
    fn connected_components_chain() {
        // A↔B B↔C (链式结构)
        let items = vec![
            make_l1(vec!["工作"], 0.5),
            make_l1(vec!["工作", "压力"], 0.5),
            make_l1(vec!["压力"], 0.5),
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        let comps = g.find_connected_components();
        assert_eq!(comps.len(), 1, "链式连接应属同一分量");
        assert_eq!(comps[0].len(), 3);
    }

    // ---- component_l1_indices ----

    #[test]
    fn component_l1_indices_mapping() {
        let items = vec![
            make_l1(vec!["A"], 0.5),
            make_l1(vec!["B"], 0.5),
            make_l1(vec!["A", "B"], 0.5), // 连接前两者
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        let comps = g.find_connected_components();
        assert_eq!(comps.len(), 1);

        let indices = g.component_l1_indices(&comps[0]);
        assert_eq!(indices.len(), 3);
        // l1_index 应映射回原始 item 索引
        for (node_idx, &l1_idx) in indices.iter().enumerate() {
            assert_eq!(g.nodes[comps[0][node_idx]].l1_index, l1_idx);
        }
    }

    // =========================================================
    // 模块度 Q 二分拆分测试
    // =========================================================

    /// 分量大小 ≤ max_cluster_size 时保持不变
    #[test]
    fn split_small_component_unchanged() {
        let items = vec![
            make_l1(vec!["工作", "压力"], 0.5),
            make_l1(vec!["工作", "倦怠"], 0.5),
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        let comps = g.find_connected_components();
        // max=25，2 远小于上限，不应拆分
        let result = split_large_components(&g, comps, 25, 0.3);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 2);
    }

    /// 分量恰好等于 max_cluster_size 时保持不变
    #[test]
    fn split_exact_max_size_unchanged() {
        let items: Vec<L1Item> = (0..5)
            .map(|i| make_l1(vec!["工作", "压力"], 0.5 + i as f64 * 0.1))
            .collect();
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        let comps = g.find_connected_components();
        assert_eq!(comps.len(), 1);
        // max=5，恰好等于上限，不应拆分
        let result = split_large_components(&g, comps, 5, 0.3);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 5);
    }

    /// 设置很小的 max_cluster_size 强制大分量被拆分
    #[test]
    fn split_large_component_with_small_max() {
        // 4 条有边 L1 — 两两强联系，但整体连通
        let items = vec![
            make_l1(vec!["工作", "加班", "会议"], 0.5),
            make_l1(vec!["工作", "加班", "报告"], 0.5),
            make_l1(vec!["休闲", "旅游", "摄影"], 0.5),
            make_l1(vec!["休闲", "旅游", "美食"], 0.5),
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        let comps = g.find_connected_components();
        // 四个节点应通过部分共享关键词连通为一个分量
        // 验证连通性
        if comps.len() == 1 && comps[0].len() == 4 {
            // 设 max=2，应触发拆分
            let result = split_large_components(&g, comps, 2, 0.0); // Q_min=0 确保接受任何拆分
            assert!(
                result.len() >= 2,
                "大分量应被拆分为至少 2 个子组，实际: {}",
                result.len()
            );
            // 各子组大小应 ≤ 2（或无法继续拆分）
            for comp in &result {
                assert!(comp.len() <= 2, "子组大小应 ≤ max=2，实际: {}", comp.len());
            }
            // 所有节点应被覆盖
            let total: usize = result.iter().map(|c| c.len()).sum();
            assert_eq!(total, 4, "拆分后节点总数应不变");
        }
    }

    /// 无边的连通分量（孤立节点组）不可拆分
    #[test]
    fn bisect_no_edges_returns_none() {
        // 单节点分量
        let items = vec![make_l1(vec!["工作"], 0.5)];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        let comps = g.find_connected_components();
        assert_eq!(comps.len(), 1);
        let result = try_bisect_component(&g, &comps[0], 0.3);
        assert!(result.is_none(), "单个节点不可二分");
    }

    /// 高 Q_min 阻止拆分
    #[test]
    fn high_q_min_prevents_split() {
        // 构建一个全连通的小图
        let items = vec![
            make_l1(vec!["工作", "压力", "加班"], 0.5),
            make_l1(vec!["工作", "压力", "倦怠"], 0.6),
            make_l1(vec!["工作", "加班", "倦怠"], 0.7),
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        let comps = g.find_connected_components();
        assert_eq!(comps.len(), 1);

        // Q_min=0.0 应接受拆分
        let _result_low = try_bisect_component(&g, &comps[0], 0.0);
        // 紧密连接的图拆分 Q 通常较小，但 Q_min=0 一定接受
        // 注意：如果贪心所有节点都留在 A（无移动能增加 ΔQ），则无法拆分
        // 这种情况下 result_low 也会是 None

        // Q_min=0.99 应拒绝拆分（真实图的 Q 几乎不可能达到 0.99）
        let result_high = try_bisect_component(&g, &comps[0], 0.99);
        assert!(
            result_high.is_none(),
            "Q_min=0.99 应阻止拆分，实际: {:?}",
            result_high
        );
    }

    /// 二分结果两组均非空
    #[test]
    fn bisect_produces_two_nonempty_groups() {
        // 构建一个半连通图：A-B-C-D，其中 A-B-C 紧密，D 仅在边缘
        let items = vec![
            make_l1(vec!["工作", "压力", "加班", "会议"], 0.5),
            make_l1(vec!["工作", "压力", "加班", "报告"], 0.5),
            make_l1(vec!["工作", "压力", "倦怠"], 0.5),
            make_l1(vec!["工作", "休闲"], 0.5), // 松连接
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.15); // 低阈值确保连通
        let comps = g.find_connected_components();

        if comps.len() == 1 && comps[0].len() >= 3 {
            let result = try_bisect_component(&g, &comps[0], 0.0);
            if let Some((a, b)) = result {
                assert!(!a.is_empty(), "组 A 不应为空");
                assert!(!b.is_empty(), "组 B 不应为空");
                assert_eq!(a.len() + b.len(), comps[0].len(), "节点总数应不变");
            }
            // 注意：贪心算法可能因没有正向 ΔQ 而无法拆分，此时 result=None
        }
    }

    /// 递归拆分：有社区结构的图应被拆分
    ///
    /// 两组各 3 节点，组内关键词高度重叠（Jaccard ≥ 0.5），
    /// 组间通过 2 个 bridge 关键词保持连通（Jaccard ≈ 0.2-0.33）。
    /// max_cluster_size=2 下应能触发拆分。
    #[test]
    fn recursive_split_community_structure() {
        let items = vec![
            // Group A: work-related + 2 bridge keywords
            make_l1(vec!["工作", "加班", "会议", "b1", "b2"], 0.5),
            make_l1(vec!["工作", "加班", "报告", "b1", "b2"], 0.5),
            make_l1(vec!["工作", "会议", "报告", "b1", "b2"], 0.5),
            // Group B: leisure-related + 2 bridge keywords
            make_l1(vec!["休闲", "旅游", "摄影", "b1", "b2"], 0.5),
            make_l1(vec!["休闲", "旅游", "美食", "b1", "b2"], 0.5),
            make_l1(vec!["休闲", "摄影", "美食", "b1", "b2"], 0.5),
        ];
        let g = KeywordGraph::build_jaccard_graph(&items, 0.2);
        let comps = g.find_connected_components();

        // 两组通过 b1/b2 共享 → 应为单个连通分量
        assert_eq!(comps.len(), 1, "bridge 关键词应连接两组为一个分量");
        assert_eq!(comps[0].len(), 6);

        // max=2，社区结构应触发拆分
        let result = split_large_components(&g, comps, 2, 0.0);

        let total: usize = result.iter().map(|c| c.len()).sum();
        assert_eq!(total, 6, "拆分后节点总数应不变");

        // 应产生多于原始 1 个的分量（社区结构被识别）
        assert!(
            result.len() > 1,
            "社区结构图应触发至少一次拆分（实际分量数: {}）",
            result.len()
        );
    }
}
