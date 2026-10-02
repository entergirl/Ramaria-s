//! crates/ramaria-memory/src/inference/causal/graph.rs - 因果图遍历与循环模式探测
//!
//! 设计特点:
//! - `dfs_all_paths`: 迭代栈 DFS 枚举源→汇简单路径，避免递归栈溢出
//! - `detect_cycle_patterns`: 以事件类别序列分组，识别重复出现的行为脚本
//! - `deduplicate_patterns`: 长模式优先，移除被完全覆盖的短模式
//! - 纯函数，输入邻接表 / 路径列表 + 类别映射，零 I/O

use std::collections::{HashMap, HashSet};

use super::types::CyclePattern;

// =========================================================
// DFS 路径遍历
// =========================================================

/// 从给定节点出发，DFS 遍历所有简单路径（无环）。
///
/// 使用迭代栈防止栈溢出（路径深度无硬限制，但受图结构约束）。
/// 每条路径以节点 ID 序列表示，包含起点。
pub(super) fn dfs_all_paths(
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
pub(super) fn detect_cycle_patterns(
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
pub(super) fn deduplicate_patterns(mut patterns: Vec<CyclePattern>) -> Vec<CyclePattern> {
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
