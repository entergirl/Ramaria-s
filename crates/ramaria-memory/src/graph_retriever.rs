//! rust/crates/ramaria-memory/src/graph_retriever.rs — 知识图谱检索通道
//!
//! 设计特点:
//! - 从查询中提取潜在实体名 → 匹配 graph_nodes → 遍历 1-hop 边 → 评分
//! - 支持 7 种关系类型的权重配置（TASK_STATUS/OBSTACLE 等权重高于一般 TIMELINE）
//! - 返回结构化 GraphHit，包含实体名、关系链和置信度
//! - 不直接访问数据库——通过闭包/回调注入存储操作，保持模块零 I/O
//! - 对接 retriever.rs：返回 (label, score) 供 RRF 融合
//!
//! 图检索评分公式:
//! score = entity_match_score × relation_boost
//! entity_match_score = matched_chars / max(entity_chars, query_chars) (Jaccard-like)
//! relation_boost = 基础权重 × (1 + 0.5 × 边数量)

use std::collections::HashMap;

// =========================================================
// 数据类型
// =========================================================

/// 知识图谱中的一个实体节点（内存表示）。
#[derive(Debug, Clone)]
pub struct GraphNode {
    /// 数据库主键
    pub id: i64,
    /// 实体名称
    pub entity_name: String,
    /// 实体类型：person / project / module / concept / time
    pub entity_type: String,
}

/// 知识图谱中的一条关系边（内存表示）。
#[derive(Debug, Clone)]
pub struct GraphEdge {
    /// 数据库主键
    pub id: i64,
    /// 源节点 id
    pub source_node_id: i64,
    /// 目标节点 id
    pub target_node_id: i64,
    /// 关系类型
    pub relation_type: String,
}

/// 知识图谱检索结果。
#[derive(Debug, Clone)]
pub struct GraphHit {
    /// 命中的实体名称
    pub entity_name: String,
    /// 实体类型
    pub entity_type: String,
    /// 图检索分数 0.0..1.0
    pub score: f64,
    /// 1-hop 关联的邻居实体名列表
    pub related_entities: Vec<String>,
    /// 关联边的类型列表（与 related_entities 对应）
    pub relation_types: Vec<String>,
}

// =========================================================
// 图谱检索配置
// =========================================================

/// 图谱检索配置。
#[derive(Debug, Clone)]
pub struct GraphRetrieverConfig {
    /// 返回的最大实体数
    pub max_entities: usize,
    /// 1-hop 扩展的最大边数（避免明星节点爆炸）
    pub max_edges_per_node: usize,
    /// 各种关系类型的基础权重
    pub relation_weights: HashMap<String, f64>,
    /// 实体匹配的最小相似度阈值
    pub min_match_ratio: f64,
    /// 图谱通道在 RRF 中的权重
    pub rrf_weight: f64,
}

impl Default for GraphRetrieverConfig {
    fn default() -> Self {
        let mut weights = HashMap::new();
        // 工作/任务相关 → 高权重（反映用户当前关注）
        weights.insert("TASK_STATUS".to_string(), 1.0);
        weights.insert("OBSTACLE".to_string(), 1.0);
        // 依赖/归属 → 中高权重
        weights.insert("USES_DEPENDS".to_string(), 0.9);
        weights.insert("BELONGS_TO".to_string(), 0.8);
        // 情绪/社交 → 中等权重
        weights.insert("EMOTION_STATE".to_string(), 0.7);
        weights.insert("SOCIAL_EVENT".to_string(), 0.6);
        // 时间锚点 → 较低权重
        weights.insert("TIME_ANCHOR".to_string(), 0.4);
        // 默认权重（未知类型）
        weights.insert("__default__".to_string(), 0.5);

        Self {
            max_entities: 5,
            max_edges_per_node: 10,
            relation_weights: weights,
            min_match_ratio: 0.0,
            rrf_weight: 0.8,
        }
    }
}

// =========================================================
// 图谱检索器
// =========================================================

/// 知识图谱检索器。
///
/// 职责:
/// - 管理图数据的内存镜像（节点 + 边）
/// - 执行实体匹配 + 1-hop 遍历检索
/// - 纯内存计算，零 I/O
#[derive(Debug, Clone)]
pub struct GraphRetriever {
    /// 实体名 → 节点
    nodes: HashMap<String, GraphNode>,
    /// 节点 id → 节点
    nodes_by_id: HashMap<i64, GraphNode>,
    /// 源节点 id → 出边列表
    edges_from: HashMap<i64, Vec<GraphEdge>>,
    /// 目标节点 id → 入边列表（预建索引，避免检索时 O(E) 全表扫描）
    edges_to: HashMap<i64, Vec<GraphEdge>>,
}

impl GraphRetriever {
    /// 创建空的图谱检索器。
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            nodes_by_id: HashMap::new(),
            edges_from: HashMap::new(),
            edges_to: HashMap::new(),
        }
    }

    /// 从外部数据加载图谱节点和边。
    ///
    /// 同时构建出边（edges_from）和入边（edges_to）索引，使检索阶段
    /// 收集邻居边的复杂度从 O(E) 降为 O(1) per entity。
    ///
    /// 参数:
    /// - `nodes`: (id, entity_name, entity_type) 列表
    /// - `edges`: (id, source_node_id, target_node_id, relation_type) 列表
    pub fn load(&mut self, nodes: &[(i64, String, String)], edges: &[(i64, i64, i64, String)]) {
        self.nodes.clear();
        self.nodes_by_id.clear();
        self.edges_from.clear();
        self.edges_to.clear();

        for (id, name, etype) in nodes {
            let node = GraphNode {
                id: *id,
                entity_name: name.clone(),
                entity_type: etype.clone(),
            };
            self.nodes.insert(name.clone(), node.clone());
            self.nodes_by_id.insert(*id, node);
        }

        for (id, src, tgt, rel_type) in edges {
            let edge = GraphEdge {
                id: *id,
                source_node_id: *src,
                target_node_id: *tgt,
                relation_type: rel_type.clone(),
            };
            self.edges_from.entry(*src).or_default().push(edge.clone());
            self.edges_to.entry(*tgt).or_default().push(edge);
        }
    }

    /// 添加单个节点。
    pub fn add_node(&mut self, node: GraphNode) {
        self.nodes.insert(node.entity_name.clone(), node.clone());
        self.nodes_by_id.insert(node.id, node);
    }

    /// 添加单条边。
    ///
    /// 同时更新出边（edges_from）和入边（edges_to）索引，
    /// 保持增量添加后检索性能不变。
    pub fn add_edge(&mut self, edge: GraphEdge) {
        self.edges_from
            .entry(edge.source_node_id)
            .or_default()
            .push(edge.clone());
        self.edges_to
            .entry(edge.target_node_id)
            .or_default()
            .push(edge);
    }

    /// 图谱中的节点数。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// 图谱中的边数。
    pub fn edge_count(&self) -> usize {
        self.edges_from.values().map(|v| v.len()).sum()
    }

    /// 清空图谱。
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.nodes_by_id.clear();
        self.edges_from.clear();
        self.edges_to.clear();
    }

    /// 从查询文本中提取候选实体。
    ///
    /// 策略:
    /// - 将查询文本与所有 graph_nodes 的 entity_name 做子串匹配
    /// - 支持中文长实体（如"机器学习项目"能匹配到"机器学习"和"项目"）
    /// - 返回匹配的实体名列表，按匹配长度降序
    pub fn extract_entities(&self, query: &str) -> Vec<(&str, f64)> {
        if query.is_empty() || self.nodes.is_empty() {
            return Vec::new();
        }

        let q_chars: Vec<char> = query.chars().collect();
        let q_len = q_chars.len();
        let mut matches: Vec<(&str, f64)> = Vec::new();

        for entity_name in self.nodes.keys() {
            let e_chars: Vec<char> = entity_name.chars().collect();
            let e_len = e_chars.len();

            if e_len == 0 {
                continue;
            }

            // 子串匹配
            let match_len = if q_len >= e_len {
                // 查询比实体长：实体是否是查询的子串
                contains_subsequence(&q_chars, &e_chars) as usize * e_len
            } else {
                // 实体比查询长：查询是否是实体的子串
                contains_subsequence(&e_chars, &q_chars) as usize * q_len
            };

            if match_len > 0 {
                let ratio = match_len as f64 / e_len.max(q_len) as f64;
                matches.push((entity_name.as_str(), ratio));
            }
        }

        // 按匹配比例降序
        matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        matches
    }

    /// 执行图谱检索。
    ///
    /// 流程:
    /// 1. 从查询中提取候选实体
    /// 2. 对每个匹配实体：收集 1-hop 邻居边
    /// 3. 综合评分 = 实体匹配度 × 关系权重 boost
    pub fn search(&self, query: &str, config: &GraphRetrieverConfig) -> Vec<GraphHit> {
        let entities = self.extract_entities(query);
        if entities.is_empty() {
            return Vec::new();
        }

        let mut hits: Vec<GraphHit> = Vec::new();

        for (entity_name, match_score) in &entities {
            let node = match self.nodes.get(*entity_name) {
                Some(n) => n,
                None => continue,
            };

            if *match_score < config.min_match_ratio {
                continue;
            }

            // 收集 1-hop 邻居
            let mut related = Vec::new();
            let mut rel_types = Vec::new();
            let mut total_boost = 1.0_f64;

            // 出边
            if let Some(out_edges) = self.edges_from.get(&node.id) {
                let edge_count = out_edges.len().min(config.max_edges_per_node);
                for edge in out_edges.iter().take(edge_count) {
                    if let Some(target) = self.nodes_by_id.get(&edge.target_node_id) {
                        related.push(target.entity_name.clone());
                        rel_types.push(edge.relation_type.clone());

                        // 关系权重加成
                        let weight = config
                            .relation_weights
                            .get(&edge.relation_type)
                            .unwrap_or_else(|| {
                                config.relation_weights.get("__default__").unwrap_or(&0.5)
                            });
                        total_boost += *weight * 0.1;
                    }
                }
            }

            // 入边（其他节点指向此实体）—— 使用预建索引 eds_to，O(1) 直接定位
            if let Some(in_edges) = self.edges_to.get(&node.id) {
                let edge_count = in_edges.len().min(config.max_edges_per_node);
                for edge in in_edges.iter().take(edge_count) {
                    if let Some(source) = self.nodes_by_id.get(&edge.source_node_id) {
                        // 避免重复
                        if !related.contains(&source.entity_name) {
                            related.push(source.entity_name.clone());
                            rel_types.push(format!("←{}", edge.relation_type));
                        }
                    }
                }
            }

            // clamp boost
            total_boost = total_boost.clamp(0.5, 2.0);

            let score = (*match_score * total_boost).clamp(0.0, 1.0);
            hits.push(GraphHit {
                entity_name: node.entity_name.clone(),
                entity_type: node.entity_type.clone(),
                score,
                related_entities: related,
                relation_types: rel_types,
            });
        }

        // 按 score 降序，截取 max_entities
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if hits.len() > config.max_entities {
            hits.truncate(config.max_entities);
        }

        hits
    }
}

impl Default for GraphRetriever {
    fn default() -> Self {
        Self::new()
    }
}

// =========================================================
// 辅助函数
// =========================================================

/// 检查 needle 是否是 haystack 的连续子序列。
///
/// 用于实体子串匹配。
fn contains_subsequence(haystack: &[char], needle: &[char]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }

    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// 从查询结果中提取用于 RRF 融合的 (label, score) 对。
///
/// label 格式: "graph:{entity_name}"
pub fn graph_hits_to_rrf_pairs(hits: &[GraphHit]) -> Vec<(String, f64)> {
    hits.iter()
        .map(|h| (format!("graph:{}", h.entity_name), h.score))
        .collect()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
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
}
