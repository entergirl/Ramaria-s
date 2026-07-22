//! rust/crates/ramaria-memory/src/inference/causal.rs - A8 因果链特征提取
//!
//! 设计特点:
//! - 从 event_relations 表（CausedBy 关系）构建有向图，DFS 计算最长因果路径
//! - 循环模式探测：识别重复出现的因果链序列，指向稳定的行为脚本
//! - 纯函数设计：不依赖 DB 或 LLM，输入 MemoryEvent + EventRelation 即可运算
//! - 因果链特征以结构化文本注入 Phase B Step 1 Prompt
//! - 无 CausedBy 关系时返回空特征，不阻塞管线

use ramaria_core::types::{EventRelation, EventRelationKind, MemoryEvent};
use std::collections::{HashMap, HashSet};
use tracing::debug;

// =========================================================
// 数据结构
// =========================================================

/// A8 因果链特征提取结果。
///
/// 职责:
/// - 汇总从 event_relations 推导的行为因果拓扑特征。
/// - 供 Phase B Step 1 Prompt 注入，帮助 LLM 识别"主动驱动者"vs"被动卷入者"。
#[derive(Debug, Clone, Default)]
pub struct CausalChainFeatures {
    /// 最长因果链的跳数（0 表示无 CausedBy 关系或全部孤立）
    pub chain_length: usize,
    /// 重复出现的循环模式列表（按出现次数降序）
    pub cyclic_patterns: Vec<CyclePattern>,
    /// 参与因果链的事件总数
    pub total_causal_events: usize,
    /// CausedBy 边总数
    pub total_causal_edges: usize,
}

/// 循环模式——同一类因果链反复出现。
///
/// 职责:
/// - 描述重复出现的行为脚本（如"压力 → 拖延 → 自责"）。
/// - 出现次数越多→该模式越可能是稳定的人格特征。
#[derive(Debug, Clone)]
pub struct CyclePattern {
    /// 模式描述（如"工作压力 → 拖延 → 自责"）
    pub description: String,
    /// 该模式出现的次数
    pub occurrences: usize,
    /// 模式中涉及的事件类别序列
    pub event_categories: Vec<String>,
    /// 模式内关系类型的序列（当前固定为 CausedBy 重复）
    pub relation_types: Vec<String>,
}

// =========================================================
// 核心提取函数
// =========================================================

/// 从事件和关系中提取因果链特征。
///
/// 算法:
/// 1. 构建 CausedBy 有向邻接表。
/// 2. 从所有源节点（入度=0）出发 DFS，寻最长简单路径。
/// 3. 提取所有因果路径的事件类别序列，检测重复模式。
///
/// 参数:
/// - `events`: 目标 persona 的所有 MemoryEvent。
/// - `relations`: 该 persona 的所有 EventRelation（仅使用 CausedBy 类型）。
///
/// 返回:
/// - CausalChainFeatures：链长度 + 循环模式 + 统计信息。
/// - 无 CausedBy 关系时返回默认值（chain_length=0，空模式）。
pub fn extract_causal_features(
    events: &[MemoryEvent],
    relations: &[EventRelation],
) -> CausalChainFeatures {
    // ---- 1. 构建事件 ID → 类别名 映射 ----
    let event_category: HashMap<i64, String> = events
        .iter()
        .map(|ev| {
            let cat = ev
                .keywords
                .as_ref()
                .and_then(|kw| kw.split(',').next())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| format!("event_{}", ev.id));
            (ev.id, cat)
        })
        .collect();

    // ---- 2. 筛选 CausedBy 边，构建邻接表 ----
    let causal_edges: Vec<&EventRelation> = relations
        .iter()
        .filter(|r| r.kind == EventRelationKind::CausedBy)
        .collect();

    if causal_edges.is_empty() {
        debug!("因果链特征: 无 CausedBy 关系，返回空特征");
        return CausalChainFeatures::default();
    }

    // 邻接表: from_id → [(to_id, weight)]
    let mut adjacency: HashMap<i64, Vec<(i64, f64)>> = HashMap::new();
    // 入度统计: 用于识别源节点
    let mut in_degree: HashMap<i64, usize> = HashMap::new();
    // 所有出现的节点
    let mut all_nodes: HashSet<i64> = HashSet::new();

    for rel in &causal_edges {
        adjacency
            .entry(rel.from_id)
            .or_default()
            .push((rel.to_id, rel.weight));
        *in_degree.entry(rel.to_id).or_default() += 1;
        in_degree.entry(rel.from_id).or_default(); // 确保 from 也在入度表中有条目
        all_nodes.insert(rel.from_id);
        all_nodes.insert(rel.to_id);
    }

    // ---- 3. DFS 寻最长因果路径 ----
    let mut longest_path_len: usize = 0;
    let mut all_paths: Vec<Vec<i64>> = Vec::new(); // 收集所有源→汇路径用于循环检测

    // 源节点: 入度=0 的节点
    let sources: Vec<i64> = all_nodes
        .iter()
        .filter(|n| in_degree.get(n).copied().unwrap_or(0) == 0)
        .copied()
        .collect();

    if sources.is_empty() {
        // 没有明确的源节点（可能是环形结构），从所有节点出发
        debug!("因果链特征: 无明确源节点（可能为环形），从所有节点出发 DFS");
        for &node in &all_nodes {
            let paths_from_node = dfs_all_paths(node, &adjacency, &event_category);
            for path in &paths_from_node {
                longest_path_len = longest_path_len.max(path.len().saturating_sub(1));
            }
            all_paths.extend(paths_from_node);
        }
    } else {
        for &source in &sources {
            let paths_from_source = dfs_all_paths(source, &adjacency, &event_category);
            for path in &paths_from_source {
                longest_path_len = longest_path_len.max(path.len().saturating_sub(1));
            }
            all_paths.extend(paths_from_source);
        }
    }

    // ---- 4. 循环模式探测 ----
    let cyclic_patterns = detect_cycle_patterns(&all_paths, &event_category);

    debug!(
        chain_length = longest_path_len,
        total_events = all_nodes.len(),
        total_edges = causal_edges.len(),
        cycle_count = cyclic_patterns.len(),
        "因果链特征提取完成"
    );

    CausalChainFeatures {
        chain_length: longest_path_len,
        cyclic_patterns,
        total_causal_events: all_nodes.len(),
        total_causal_edges: causal_edges.len(),
    }
}

// =========================================================
// DFS 路径遍历
// =========================================================

/// 从给定节点出发，DFS 遍历所有简单路径（无环）。
///
/// 使用迭代栈防止栈溢出（路径深度无硬限制，但受图结构约束）。
/// 每条路径以节点 ID 序列表示，包含起点。
fn dfs_all_paths(
    start: i64,
    adjacency: &HashMap<i64, Vec<(i64, f64)>>,
    _event_category: &HashMap<i64, String>,
) -> Vec<Vec<i64>> {
    let mut all_paths: Vec<Vec<i64>> = Vec::new();

    // 栈元素: (当前节点, 当前路径, 路径上已访问节点集合)
    let mut stack: Vec<(i64, Vec<i64>, HashSet<i64>)> = Vec::new();
    let initial_path = vec![start];
    let mut initial_visited = HashSet::new();
    initial_visited.insert(start);
    stack.push((start, initial_path, initial_visited));

    while let Some((current, path, visited)) = stack.pop() {
        // 如果当前节点没有后继（汇节点），记录此路径
        let neighbors = adjacency.get(&current);
        if neighbors.is_none() || neighbors.unwrap().is_empty() {
            all_paths.push(path.clone());
            continue;
        }

        let mut has_unvisited_neighbor = false;
        for &(next, _weight) in neighbors.unwrap() {
            if !visited.contains(&next) {
                has_unvisited_neighbor = true;
                let mut new_path = path.clone();
                new_path.push(next);
                let mut new_visited = visited.clone();
                new_visited.insert(next);
                stack.push((next, new_path, new_visited));
            }
        }

        // 所有邻居都已访问过（遇到环），当前路径也是有效路径
        if !has_unvisited_neighbor {
            all_paths.push(path);
        }
    }

    all_paths
}

// =========================================================
// 循环模式探测
// =========================================================

/// 检测因果路径中重复出现的模式。
///
/// 策略:
/// - 将每条路径映射为"事件类别序列"。
/// - 按类别序列分组，出现 ≥ 2 次的视为循环模式。
/// - 模式按出现次数降序排列。
fn detect_cycle_patterns(
    paths: &[Vec<i64>],
    event_category: &HashMap<i64, String>,
) -> Vec<CyclePattern> {
    if paths.len() < 2 {
        return Vec::new();
    }

    // 将每条路径转为类别序列
    let category_paths: Vec<Vec<String>> = paths
        .iter()
        .map(|path| {
            path.iter()
                .map(|id| {
                    event_category
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| format!("event_{}", id))
                })
                .collect()
        })
        .collect();

    // 按类别序列分组计数
    let mut pattern_counts: HashMap<Vec<String>, usize> = HashMap::new();
    for cat_path in &category_paths {
        // 只考虑长度 ≥ 2 的路径（单节点不算因果链）
        if cat_path.len() >= 2 {
            *pattern_counts.entry(cat_path.clone()).or_default() += 1;
        }
    }

    // 也检测长度为 2 的子路径（相邻边对）
    for cat_path in &category_paths {
        for window in cat_path.windows(2) {
            let sub: Vec<String> = window.to_vec();
            *pattern_counts.entry(sub).or_default() += 1;
        }
    }

    // 长度为 3 的子路径
    for cat_path in &category_paths {
        for window in cat_path.windows(3) {
            let sub: Vec<String> = window.to_vec();
            *pattern_counts.entry(sub).or_default() += 1;
        }
    }

    // 筛选出现 ≥ 2 次的模式，去重（长路径包含短路径的，优先保留长的）
    let mut patterns: Vec<CyclePattern> = pattern_counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|(categories, occurrences)| {
            let description = categories.join(" → ");
            let relation_types: Vec<String> = (0..categories.len().saturating_sub(1))
                .map(|_| "CausedBy".to_string())
                .collect();
            CyclePattern {
                description,
                occurrences,
                event_categories: categories,
                relation_types,
            }
        })
        .collect();

    // 去重：如果长模式包含了短模式的内容，保留长模式
    patterns = deduplicate_patterns(patterns);

    // 按出现次数降序
    patterns.sort_by_key(|p| std::cmp::Reverse(p.occurrences));

    // 最多保留 5 个模式（避免 Prompt 过长）
    patterns.truncate(5);

    patterns
}

/// 去重循环模式：较长的模式优先保留，短模式如果被长模式完全覆盖则移出。
fn deduplicate_patterns(mut patterns: Vec<CyclePattern>) -> Vec<CyclePattern> {
    // 按类别序列长度降序，长的优先
    patterns.sort_by_key(|p| std::cmp::Reverse(p.event_categories.len()));

    let mut result: Vec<CyclePattern> = Vec::new();
    for p in patterns {
        // 检查是否被已保留的模式完全包含
        let is_subsumed = result.iter().any(|kept| {
            if kept.event_categories.len() < p.event_categories.len() {
                return false;
            }
            // 检查 p 的序列是否是 kept 序列的连续子序列
            kept.event_categories
                .windows(p.event_categories.len())
                .any(|window| window == p.event_categories.as_slice())
        });

        if !is_subsumed {
            result.push(p);
        }
    }

    result
}

// =========================================================
// 文本格式化（供 Prompt 注入）
// =========================================================

/// 将因果链特征格式化为 Phase B Prompt 可注入的结构化文本。
///
/// 格式:
/// - 因果网络概况（参与事件数、边数、最长链长度）
/// - 循环模式列表（如有）
/// - 解读提示
///
/// 参数:
/// - `features`: 因果链特征。
///
/// 返回:
/// - 格式化后的中文段落文本。若 chain_length=0 且无循环模式，返回空字符串。
pub fn format_causal_features_text(features: &CausalChainFeatures) -> String {
    if features.chain_length == 0 && features.cyclic_patterns.is_empty() {
        return String::new();
    }

    let mut text = String::new();
    text.push_str("## 因果链分析 (A8)\n\n");

    text.push_str(&format!(
        "因果网络概况: {} 个事件通过 {} 条因果关系连接",
        features.total_causal_events, features.total_causal_edges
    ));

    if features.chain_length > 0 {
        text.push_str(&format!("，最长因果链为 {} 跳。\n", features.chain_length));
        // 解读提示
        let driver_hint = if features.chain_length >= 3 {
            "长因果链提示用户可能是事件的\"主动驱动者\"（行为产生连锁影响）。"
        } else if features.chain_length >= 2 {
            "中等因果链提示用户行为有一定连锁效应。"
        } else {
            "短因果链提示用户行为影响较为局部。"
        };
        text.push_str(&format!("解读提示: {}\n", driver_hint));
    } else {
        text.push_str("。\n");
    }

    if !features.cyclic_patterns.is_empty() {
        text.push_str("\n**重复出现的行为脚本（循环模式）:**\n");
        for (i, pattern) in features.cyclic_patterns.iter().enumerate() {
            text.push_str(&format!(
                "  {}. \"{}\" — 出现 {} 次\n",
                i + 1,
                pattern.description,
                pattern.occurrences
            ));
        }
        text.push_str("注意: 循环模式指向稳定的行为脚本，应在性格推断中优先考虑。\n");
    }

    text.push('\n');
    text
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::EventRelationKind;
    use ramaria_core::types::MemoryEvent;
    use ramaria_core::types::Presentation;
    use ramaria_core::types::now_ms;

    /// 创建测试用 MemoryEvent（最小字段集）。
    fn make_event(id: i64, keywords: &str) -> MemoryEvent {
        let now = now_ms();
        MemoryEvent {
            id,
            persona_uid: "test-persona".into(),
            title: format!("Event {}", id),
            summary: format!("Summary of event {}", id),
            keywords: if keywords.is_empty() {
                None
            } else {
                Some(keywords.to_string())
            },
            participants: None,
            start: now,
            end: now,
            confidence: 0.8,
            salience: 0.7,
            valence: -0.3,
            presentation: Presentation::Mixed,
            share: 0.5,
            attitude: None,
            paraphrase: None,
            absorbed: 0,
            situation_strength: Some(3),
            motives: None,
            created_at: now,
            last_accessed_at: None,
            indexed_at: None,
            index_version: None,
        }
    }

    /// 创建 CausedBy 关系。
    fn make_causal(from_id: i64, to_id: i64) -> EventRelation {
        EventRelation {
            id: 0,
            from_id,
            to_id,
            kind: EventRelationKind::CausedBy,
            weight: 0.7,
            created_at: 1000,
        }
    }

    /// 创建非 CausedBy 关系（应被过滤）。
    fn make_related(from_id: i64, to_id: i64) -> EventRelation {
        EventRelation {
            id: 0,
            from_id,
            to_id,
            kind: EventRelationKind::RelatedTo,
            weight: 0.5,
            created_at: 1000,
        }
    }

    // =========================================================
    // extract_causal_features 测试
    // =========================================================

    #[test]
    fn empty_relations_returns_default() {
        let events = vec![make_event(1, "工作")];
        let relations: Vec<EventRelation> = vec![];
        let features = extract_causal_features(&events, &relations);
        assert_eq!(features.chain_length, 0);
        assert!(features.cyclic_patterns.is_empty());
        assert_eq!(features.total_causal_events, 0);
    }

    #[test]
    fn no_causedby_relations_returns_default() {
        let events = vec![make_event(1, "工作"), make_event(2, "生活")];
        let relations = vec![make_related(1, 2)];
        let features = extract_causal_features(&events, &relations);
        assert_eq!(features.chain_length, 0);
        assert!(features.cyclic_patterns.is_empty());
    }

    #[test]
    fn single_causal_link_chain_length_1() {
        // 压力 → 拖延
        let events = vec![make_event(1, "工作压力"), make_event(2, "拖延")];
        let relations = vec![make_causal(1, 2)];
        let features = extract_causal_features(&events, &relations);
        assert_eq!(features.chain_length, 1);
        assert_eq!(features.total_causal_events, 2);
        assert_eq!(features.total_causal_edges, 1);
    }

    #[test]
    fn chain_of_three_length_2() {
        // 压力 → 拖延 → 自责
        let events = vec![
            make_event(1, "工作压力"),
            make_event(2, "拖延"),
            make_event(3, "自责"),
        ];
        let relations = vec![make_causal(1, 2), make_causal(2, 3)];
        let features = extract_causal_features(&events, &relations);
        assert_eq!(features.chain_length, 2);
    }

    #[test]
    fn branching_chain_takes_longest() {
        //     1→2→3 (length 2)
        //     1→4     (length 1)
        let events = vec![
            make_event(1, "压力"),
            make_event(2, "拖延"),
            make_event(3, "自责"),
            make_event(4, "爆发"),
        ];
        let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(1, 4)];
        let features = extract_causal_features(&events, &relations);
        assert_eq!(features.chain_length, 2);
    }

    #[test]
    fn cycle_detected() {
        // 压力 → 拖延 → 自责
        // 压力 → 拖延 → 自责 (第二次重复)
        let events = vec![
            make_event(1, "工作压力"),
            make_event(2, "拖延"),
            make_event(3, "自责"),
            make_event(4, "工作压力"),
            make_event(5, "拖延"),
            make_event(6, "自责"),
        ];
        let relations = vec![
            make_causal(1, 2),
            make_causal(2, 3),
            make_causal(4, 5),
            make_causal(5, 6),
        ];
        let features = extract_causal_features(&events, &relations);
        // 应该有循环模式被检测到
        assert!(
            !features.cyclic_patterns.is_empty(),
            "应该检测到重复的因果链模式"
        );
        assert!(features.cyclic_patterns.iter().any(|p| p.occurrences >= 2));
    }

    #[test]
    fn non_causal_relations_filtered() {
        // CausedBy + RelatedTo 混合，只计 CausedBy
        let events = vec![
            make_event(1, "压力"),
            make_event(2, "拖延"),
            make_event(3, "发泄"),
        ];
        let relations = vec![
            make_causal(1, 2),
            make_related(2, 3), // 非因果，应被过滤
        ];
        let features = extract_causal_features(&events, &relations);
        assert_eq!(features.chain_length, 1);
        assert_eq!(features.total_causal_edges, 1);
    }

    #[test]
    fn no_source_nodes_all_nodes_as_start() {
        // 环形: 1→2→1 (CausedBy 双向)
        let events = vec![make_event(1, "压力"), make_event(2, "拖延")];
        let relations = vec![make_causal(1, 2), make_causal(2, 1)];
        let features = extract_causal_features(&events, &relations);
        // 应该能找到路径
        assert!(features.chain_length >= 1);
    }

    #[test]
    fn event_without_keywords_uses_fallback() {
        let event = make_event(1, ""); // empty → keywords=None via make_event
        let events = vec![event, make_event(2, "拖延")];
        let relations = vec![make_causal(1, 2)];
        let features = extract_causal_features(&events, &relations);
        assert_eq!(features.chain_length, 1);
    }

    // =========================================================
    // format_causal_features_text 测试
    // =========================================================

    #[test]
    fn format_empty_features_returns_empty() {
        let features = CausalChainFeatures::default();
        let text = format_causal_features_text(&features);
        assert!(text.is_empty());
    }

    #[test]
    fn format_with_chain_length() {
        let features = CausalChainFeatures {
            chain_length: 3,
            total_causal_events: 5,
            total_causal_edges: 4,
            cyclic_patterns: vec![],
        };
        let text = format_causal_features_text(&features);
        assert!(text.contains("因果链分析"));
        assert!(text.contains("3 跳"));
        assert!(text.contains("主动驱动者"));
    }

    #[test]
    fn format_with_cycle_patterns() {
        let features = CausalChainFeatures {
            chain_length: 2,
            total_causal_events: 6,
            total_causal_edges: 4,
            cyclic_patterns: vec![CyclePattern {
                description: "工作压力 → 拖延 → 自责".into(),
                occurrences: 2,
                event_categories: vec!["工作压力".into(), "拖延".into(), "自责".into()],
                relation_types: vec!["CausedBy".into(), "CausedBy".into()],
            }],
        };
        let text = format_causal_features_text(&features);
        assert!(text.contains("循环模式"));
        assert!(text.contains("工作压力 → 拖延 → 自责"));
        assert!(text.contains("出现 2 次"));
    }

    #[test]
    fn format_short_chain_no_driver_hint() {
        let features = CausalChainFeatures {
            chain_length: 1,
            total_causal_events: 2,
            total_causal_edges: 1,
            cyclic_patterns: vec![],
        };
        let text = format_causal_features_text(&features);
        assert!(text.contains("1 跳"));
        assert!(text.contains("较为局部"));
        assert!(!text.contains("主动驱动者"));
    }

    // =========================================================
    // dfs_all_paths 测试
    // =========================================================

    #[test]
    fn dfs_single_node_no_edges() {
        let adjacency: HashMap<i64, Vec<(i64, f64)>> = HashMap::new();
        let cat: HashMap<i64, String> = [(1, "work".into())].into();
        let paths = dfs_all_paths(1, &adjacency, &cat);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], vec![1]);
    }

    #[test]
    fn dfs_linear_chain() {
        let adjacency: HashMap<i64, Vec<(i64, f64)>> =
            HashMap::from([(1, vec![(2, 0.7)]), (2, vec![(3, 0.8)])]);
        let cat: HashMap<i64, String> = HashMap::new();
        let paths = dfs_all_paths(1, &adjacency, &cat);
        assert!(paths.iter().any(|p| p == &vec![1, 2, 3]));
    }

    #[test]
    fn dfs_branching() {
        let adjacency: HashMap<i64, Vec<(i64, f64)>> =
            HashMap::from([(1, vec![(2, 0.7), (3, 0.8)])]);
        let cat: HashMap<i64, String> = HashMap::new();
        let paths = dfs_all_paths(1, &adjacency, &cat);
        assert!(paths.iter().any(|p| p == &vec![1, 2]));
        assert!(paths.iter().any(|p| p == &vec![1, 3]));
    }

    // =========================================================
    // detect_cycle_patterns 测试
    // =========================================================

    #[test]
    fn no_cycle_with_single_path() {
        let paths = vec![vec![1, 2, 3]];
        let cat: HashMap<i64, String> = [(1, "A".into()), (2, "B".into()), (3, "C".into())].into();
        let patterns = detect_cycle_patterns(&paths, &cat);
        assert!(patterns.is_empty());
    }

    #[test]
    fn detects_repeated_pattern() {
        // 两个完全相同的路径
        let paths = vec![vec![1, 2, 3], vec![4, 5, 6]];
        let cat: HashMap<i64, String> = [
            (1, "压力".into()),
            (2, "拖延".into()),
            (3, "自责".into()),
            (4, "压力".into()),
            (5, "拖延".into()),
            (6, "自责".into()),
        ]
        .into();
        let patterns = detect_cycle_patterns(&paths, &cat);
        assert!(!patterns.is_empty());
        assert!(patterns.iter().any(|p| p.occurrences >= 2));
    }

    #[test]
    fn empty_paths_returns_empty() {
        let paths: Vec<Vec<i64>> = vec![];
        let cat: HashMap<i64, String> = HashMap::new();
        let patterns = detect_cycle_patterns(&paths, &cat);
        assert!(patterns.is_empty());
    }
}
