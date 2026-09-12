//! crates/ramaria-memory/src/graph_retriever.rs — 知识图谱检索通道
//!
//! 设计特点:
//! - 从查询中提取潜在实体名 → 匹配 graph_nodes → 遍历 1-hop 边 → 评分
//! - 实体匹配先经 bigram 倒排生成候选，避免每次查询扫描全部节点名
//! - 支持 7 种关系类型的权重配置（TASK_STATUS/OBSTACLE 等权重高于一般 TIMELINE）
//! - 返回结构化 GraphHit，包含实体名、关系链和置信度
//! - 不直接访问数据库——通过闭包/回调注入存储操作，保持模块零 I/O
//! - 对接 retriever.rs：返回 (label, score) 供 RRF 融合
//!
//! 图检索评分公式:
//! score = entity_match_score × relation_boost
//! entity_match_score = matched_chars / max(entity_chars, query_chars) (Jaccard-like)
//! relation_boost = 1.0 + Σ(关系权重 × 0.1)，出边与入边对称计入
//!   - 每条被纳入的邻居边按其关系类型权重贡献 0.1 加成（TASK_STATUS=1.0 → 0.10/边，
//!     TIME_ANCHOR=0.4 → 0.04/边），最终钳制到 [0.5, 2.0]。
//!   - 出边与入边行为对称：入边（其他节点指向本实体）同样计入加成，
//!     解决此前"出边计入、入边不计"的不对称问题（决策 D-V17-014-16）。

use std::collections::{HashMap, HashSet};

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
    /// 实体名 bigram 倒排：相邻两字符 → 包含该 bigram 的实体名列表（候选生成用）
    entity_bigrams: HashMap<(char, char), Vec<String>>,
    /// 单字符实体名索引：字符 → 实体名列表（仅收录单字符实体名）
    single_char_entities: HashMap<char, Vec<String>>,
}

impl GraphRetriever {
    /// 创建空的图谱检索器。
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            nodes_by_id: HashMap::new(),
            edges_from: HashMap::new(),
            edges_to: HashMap::new(),
            entity_bigrams: HashMap::new(),
            single_char_entities: HashMap::new(),
        }
    }

    /// 从外部数据加载图谱节点和边。
    ///
    /// 同时构建出边（edges_from）、入边（edges_to）与实体名倒排索引，
    /// 使邻居边收集为 O(1) per entity、实体候选生成为 O(查询字符数)。
    ///
    /// 参数:
    /// - `nodes`: (id, entity_name, entity_type) 列表
    /// - `edges`: (id, source_node_id, target_node_id, relation_type) 列表
    pub fn load(&mut self, nodes: &[(i64, String, String)], edges: &[(i64, i64, i64, String)]) {
        self.nodes.clear();
        self.nodes_by_id.clear();
        self.edges_from.clear();
        self.edges_to.clear();
        self.entity_bigrams.clear();
        self.single_char_entities.clear();

        for (id, name, etype) in nodes {
            let node = GraphNode {
                id: *id,
                entity_name: name.clone(),
                entity_type: etype.clone(),
            };
            self.insert_index_entry(name);
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
    ///
    /// 同名节点覆盖前先清理旧索引项，再按新名重建倒排索引，
    /// 保证索引与节点集合始终同步。
    pub fn add_node(&mut self, node: GraphNode) {
        self.remove_index_entry(&node.entity_name);
        self.insert_index_entry(&node.entity_name);
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
        self.entity_bigrams.clear();
        self.single_char_entities.clear();
    }

    /// 把一个实体名登记进倒排索引（bigram 与单字符各建一路）。
    fn insert_index_entry(&mut self, name: &str) {
        let chars: Vec<char> = name.chars().collect();
        if chars.is_empty() {
            return;
        }

        // 同一 bigram 在同一实体名中可能重复出现，先去重再登记，避免索引项膨胀。
        let mut seen: HashSet<(char, char)> = HashSet::new();
        for pair in chars.windows(2) {
            let key = (pair[0], pair[1]);
            if seen.insert(key) {
                self.entity_bigrams
                    .entry(key)
                    .or_default()
                    .push(name.to_string());
            }
        }

        // 仅单字符实体名进入单字符索引：多字符实体统一由 bigram 路召回。
        if let [c] = chars.as_slice() {
            self.single_char_entities
                .entry(*c)
                .or_default()
                .push(name.to_string());
        }
    }

    /// 从倒排索引中移除一个实体名的全部条目（同名覆盖前调用）。
    fn remove_index_entry(&mut self, name: &str) {
        let chars: Vec<char> = name.chars().collect();

        for pair in chars.windows(2) {
            let key = (pair[0], pair[1]);
            let list_is_empty = match self.entity_bigrams.get_mut(&key) {
                Some(list) => {
                    list.retain(|n| n != name);
                    list.is_empty()
                }
                None => false,
            };
            if list_is_empty {
                self.entity_bigrams.remove(&key);
            }
        }

        if let [c] = chars.as_slice() {
            let list_is_empty = match self.single_char_entities.get_mut(c) {
                Some(list) => {
                    list.retain(|n| n != name);
                    list.is_empty()
                }
                None => false,
            };
            if list_is_empty {
                self.single_char_entities.remove(c);
            }
        }
    }

    /// 从查询文本中提取候选实体。
    ///
    /// 策略:
    /// - 查询长度 ≥ 2 时，先用相邻字符对查 bigram 倒排、查询字符查单字符索引生成候选，
    ///   再对候选做精确子串匹配校验，避免每次查询扫描全部节点名
    /// - 支持中文长实体（如"机器学习项目"能匹配到"机器学习"和"项目"）
    /// - 单字符查询无法用 bigram 生成候选，退回全量扫描
    /// - 返回匹配的实体名列表，按匹配比例降序
    pub fn extract_entities(&self, query: &str) -> Vec<(&str, f64)> {
        if query.is_empty() || self.nodes.is_empty() {
            return Vec::new();
        }

        let q_chars: Vec<char> = query.chars().collect();
        let q_len = q_chars.len();

        // 单字符查询无法用 bigram 生成候选：查询是单个字符时，
        // "查询是实体子串"等价于该字符出现在实体名任意位置，需要全量扫描。
        if q_len < 2 {
            return self.extract_entities_full_scan(&q_chars);
        }

        let candidates = self.candidate_entities(&q_chars);
        let mut matches: Vec<(&str, f64)> = Vec::new();

        for entity_name in candidates {
            let e_chars: Vec<char> = entity_name.chars().collect();
            let e_len = e_chars.len();

            if e_len == 0 {
                continue;
            }

            // 候选只保证部分字符命中索引，仍需按原匹配公式做双向子串校验。
            let match_len = if q_len >= e_len {
                // 查询比实体长：实体是否是查询的子串
                contains_subsequence(&q_chars, &e_chars) as usize * e_len
            } else {
                // 实体比查询长：查询是否是实体的子串
                contains_subsequence(&e_chars, &q_chars) as usize * q_len
            };

            if match_len > 0 {
                let ratio = match_len as f64 / e_len.max(q_len) as f64;
                matches.push((entity_name, ratio));
            }
        }

        // 按匹配比例降序
        matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        matches
    }

    /// 由倒排索引生成候选实体名集合（引用 `nodes` 的 key，天然去重）。
    ///
    /// 两路召回并集:
    /// - bigram 倒排: 查询的每个相邻字符对命中"实体名包含该 bigram"
    /// - 单字符索引: 查询的每个字符命中"单字符实体名"
    fn candidate_entities(&self, q_chars: &[char]) -> HashSet<&str> {
        let mut candidate_names: HashSet<&str> = HashSet::new();

        for pair in q_chars.windows(2) {
            if let Some(names) = self.entity_bigrams.get(&(pair[0], pair[1])) {
                candidate_names.extend(names.iter().map(|n| n.as_str()));
            }
        }
        for c in q_chars {
            if let Some(names) = self.single_char_entities.get(c) {
                candidate_names.extend(names.iter().map(|n| n.as_str()));
            }
        }

        // 索引中的名字统一映射回节点集合的 key 引用，顺带过滤索引与集合的潜在不同步。
        candidate_names
            .into_iter()
            .filter_map(|name| self.nodes.get_key_value(name).map(|(key, _)| key.as_str()))
            .collect()
    }

    /// 全量扫描实体名做子串匹配（无法通过倒排生成候选时的回退路径）。
    ///
    /// 单字符查询无法用 bigram 生成候选：查询是单个字符时，
    /// "查询是实体子串"等价于该字符出现在实体名中，必须检查全部实体名。
    fn extract_entities_full_scan(&self, q_chars: &[char]) -> Vec<(&str, f64)> {
        let q_len = q_chars.len();
        let mut matches: Vec<(&str, f64)> = Vec::new();

        for entity_name in self.nodes.keys() {
            let e_chars: Vec<char> = entity_name.chars().collect();
            let e_len = e_chars.len();

            if e_len == 0 {
                continue;
            }

            let match_len = if q_len >= e_len {
                contains_subsequence(q_chars, &e_chars) as usize * e_len
            } else {
                contains_subsequence(&e_chars, q_chars) as usize * q_len
            };

            if match_len > 0 {
                let ratio = match_len as f64 / e_len.max(q_len) as f64;
                matches.push((entity_name.as_str(), ratio));
            }
        }

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

            // 入边（其他节点指向此实体）—— 使用预建索引 edges_to，O(1) 直接定位。
            // 行为对称性（决策 D-V17-014-16）：入边与出边一样参与关系权重加成，
            // 使"被引用"与"引用"对实体重要度贡献一致，避免评分方向偏差。
            if let Some(in_edges) = self.edges_to.get(&node.id) {
                let edge_count = in_edges.len().min(config.max_edges_per_node);
                for edge in in_edges.iter().take(edge_count) {
                    if let Some(source) = self.nodes_by_id.get(&edge.source_node_id) {
                        // 避免重复
                        if !related.contains(&source.entity_name) {
                            related.push(source.entity_name.clone());
                            rel_types.push(format!("←{}", edge.relation_type));

                            // 关系权重加成（与出边一致：weight × 0.1）
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
}
