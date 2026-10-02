//! crates/ramaria-memory/src/inference/causal/extract.rs - A8 因果链特征提取算法
//!
//! 设计特点:
//! - 从 event_relations 表（CausedBy 关系）构建有向图，DFS 计算最长因果路径
//! - 循环模式探测：识别重复出现的因果链序列，指向稳定的行为脚本
//! - 时延分布与情绪沿链走势为扩展特征（独立开关，无数据时为空缺省形态）
//! - 纯函数设计：不依赖 DB 或 LLM，输入 MemoryEvent + EventRelation 即可运算
//! - 无 CausedBy 关系时返回空特征，不阻塞管线

use ramaria_core::types::{EventRelation, EventRelationKind, MemoryEvent};
use std::collections::{HashMap, HashSet};
use tracing::debug;

use super::graph::{detect_cycle_patterns, dfs_all_paths};
use super::types::{CausalChainFeatures, CausalCore, CausalEmotionTrend, CausalLatencyStats};

// =========================================================
// 核心提取函数
// =========================================================

/// 从事件和关系中提取因果链基础特征（不含时延/情绪扩展段）。
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
/// - CausalChainFeatures：链长度 + 循环模式 + 统计信息（扩展特征为空缺省形态）。
/// - 无 CausedBy 关系时返回默认值（chain_length=0，空模式）。
pub fn extract_causal_features(
    events: &[MemoryEvent],
    relations: &[EventRelation],
) -> CausalChainFeatures {
    let Some(core) = analyze_causal_core(events, relations) else {
        debug!("因果链特征: 无 CausedBy 关系，返回空特征");
        return CausalChainFeatures::default();
    };
    CausalChainFeatures {
        chain_length: core.chain_length,
        cyclic_patterns: core.cyclic_patterns,
        total_causal_events: core.total_causal_events,
        total_causal_edges: core.total_causal_edges,
        latency_stats: CausalLatencyStats::default(),
        emotion_trend: CausalEmotionTrend::default(),
    }
}

/// 从事件和关系中提取因果链特征（含时延分布与情绪沿链走势扩展段）。
///
/// 与 `extract_causal_features` 的关系:
/// - 基础四特征完全一致；本函数额外补齐 `latency_stats` 与 `emotion_trend`。
/// - 无 CausedBy 关系 / 时间缺失 / 无情绪采样路径时，扩展字段保持空缺省形态，
///   使无数据路径与关闭开关路径输出逐字节等价。
///
/// 参数:
/// - `events`: 目标 persona 的所有 MemoryEvent。
/// - `relations`: 该 persona 的所有 EventRelation（仅使用 CausedBy 类型）。
///
/// 返回:
/// - CausalChainFeatures：链长度 + 循环模式 + 统计 + 扩展特征。
/// - 无 CausedBy 关系时返回默认值（全部特征为空，不 panic）。
pub fn extract_causal_features_extended(
    events: &[MemoryEvent],
    relations: &[EventRelation],
) -> CausalChainFeatures {
    let Some(core) = analyze_causal_core(events, relations) else {
        debug!("因果链特征(扩展): 无 CausedBy 关系，返回空特征");
        return CausalChainFeatures::default();
    };
    let latency_stats = compute_latency_stats(&core, events);
    let emotion_trend = compute_emotion_trend(&core, events);
    CausalChainFeatures {
        chain_length: core.chain_length,
        cyclic_patterns: core.cyclic_patterns,
        total_causal_events: core.total_causal_events,
        total_causal_edges: core.total_causal_edges,
        latency_stats,
        emotion_trend,
    }
}

/// 执行因果图分析与路径枚举的内部核心。
///
/// 算法:
/// 1. 构建 CausedBy 有向邻接表。
/// 2. 从所有源节点（入度=0）出发 DFS；无源节点（环形）时从全部节点出发。
/// 3. 枚举全部源→汇简单路径，供循环模式检测与情绪沿链采样共用。
fn analyze_causal_core(events: &[MemoryEvent], relations: &[EventRelation]) -> Option<CausalCore> {
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
        return None;
    }

    // 邻接表: from_id → [(to_id, weight)]
    let mut adjacency: HashMap<i64, Vec<(i64, f64)>> = HashMap::new();
    // 入度统计: 用于识别源节点
    let mut in_degree: HashMap<i64, usize> = HashMap::new();
    // 所有出现的节点
    let mut all_nodes: HashSet<i64> = HashSet::new();
    let mut edge_pairs: Vec<(i64, i64)> = Vec::with_capacity(causal_edges.len());

    for rel in &causal_edges {
        adjacency
            .entry(rel.from_id)
            .or_default()
            .push((rel.to_id, rel.weight));
        *in_degree.entry(rel.to_id).or_default() += 1;
        in_degree.entry(rel.from_id).or_default(); // 确保 from 也在入度表中有条目
        all_nodes.insert(rel.from_id);
        all_nodes.insert(rel.to_id);
        edge_pairs.push((rel.from_id, rel.to_id));
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

    Some(CausalCore {
        chain_length: longest_path_len,
        cyclic_patterns,
        total_causal_events: all_nodes.len(),
        total_causal_edges: causal_edges.len(),
        paths: all_paths,
        edge_pairs,
    })
}

// =========================================================
// 扩展特征：因果边时延分布
// =========================================================

/// 自然日毫秒常量（时延分档依据）。
pub(super) const MS_PER_DAY: i64 = 86_400_000;

/// 统计因果边两端事件的时延分布。
///
/// 算法:
/// - 对每条 CausedBy 边取 `to.start - from.start`（cause → effect 的发生时间差）。
/// - 时间缺失（任一端事件不在 events / start 不可用）或时延为负（因果方向与
///   时间顺序冲突）的边剔除，仅 debug 计数，不 panic、不参与统计。
/// - 分档口径: ≤1 天 / 1-7 天（不含 1 天含 7 天）/ >7 天。
fn compute_latency_stats(core: &CausalCore, events: &[MemoryEvent]) -> CausalLatencyStats {
    // 事件 ID → 事件发生起点（Unix 毫秒）
    let start_by_id: HashMap<i64, i64> = events
        .iter()
        .filter(|e| e.start > 0)
        .map(|e| (e.id, e.start))
        .collect();

    let mut latencies: Vec<i64> = Vec::with_capacity(core.edge_pairs.len());
    let mut excluded: usize = 0;

    for &(from_id, to_id) in &core.edge_pairs {
        let (Some(&from_start), Some(&to_start)) =
            (start_by_id.get(&from_id), start_by_id.get(&to_id))
        else {
            excluded += 1;
            debug!(from_id, to_id, "因果链时延: 事件时间缺失，剔除该边");
            continue;
        };
        let latency_ms = to_start.saturating_sub(from_start);
        if from_start > to_start {
            // 时延为负（effect 早于 cause）：数据异常，剔除并计数
            excluded += 1;
            debug!(from_id, to_id, latency_ms, "因果链时延: 时延为负，剔除该边");
            continue;
        }
        latencies.push(latency_ms);
    }

    if latencies.is_empty() {
        return CausalLatencyStats {
            excluded_edge_count: excluded,
            ..CausalLatencyStats::default()
        };
    }

    latencies.sort_unstable();
    let n = latencies.len();
    let sum: f64 = latencies.iter().map(|&ms| ms as f64).sum();
    let mean_ms = sum / n as f64;
    let median_ms = if n % 2 == 1 {
        latencies[n / 2] as f64
    } else {
        (latencies[n / 2 - 1] as f64 + latencies[n / 2] as f64) / 2.0
    };
    let min_ms = *latencies.first().unwrap() as f64;
    let max_ms = *latencies.last().unwrap() as f64;

    let within_1d_count = latencies.iter().filter(|&&ms| ms <= MS_PER_DAY).count();
    let within_7d_count = latencies
        .iter()
        .filter(|&&ms| ms > MS_PER_DAY && ms <= 7 * MS_PER_DAY)
        .count();
    let over_7d_count = latencies.iter().filter(|&&ms| ms > 7 * MS_PER_DAY).count();

    debug!(
        sampled = n,
        excluded, mean_ms, median_ms, min_ms, max_ms, "因果链时延统计完成"
    );

    CausalLatencyStats {
        sampled_edge_count: n,
        excluded_edge_count: excluded,
        mean_ms: Some(mean_ms),
        median_ms: Some(median_ms),
        min_ms: Some(min_ms),
        max_ms: Some(max_ms),
        within_1d_count,
        within_7d_count,
        over_7d_count,
    }
}

// =========================================================
// 扩展特征：情绪沿链走势
// =========================================================

/// 首末 delta 的显著阈值（|delta| 低于该值视为净走势小）。
const DELTA_EPS: f64 = 0.15;

/// 计算沿最长因果路径的情绪走势。
///
/// 算法:
/// - 采样路径: 全部源→汇路径中最长的一条；同长取节点 ID 字典序最小，保证确定性。
/// - 逐节点取事件 valence，节点数 < 2 时返回空缺省形态。
/// - 线性斜率: 最小二乘拟合（x=节点序号 0..n-1，y=valence）。
/// - 方向: 首末 delta 净变化显著（> DELTA_EPS）→ 增强，< -DELTA_EPS → 衰减；
///   净变化小但正负翻转 ≥2 次 → 波动；其余 → 平稳。
fn compute_emotion_trend(core: &CausalCore, events: &[MemoryEvent]) -> CausalEmotionTrend {
    // 事件 ID → valence
    let valence_by_id: HashMap<i64, f64> = events.iter().map(|e| (e.id, e.valence)).collect();

    // 最长路径（同长取字典序最小 → 确定性输出）
    let max_len = core.paths.iter().map(Vec::len).max().unwrap_or(0);
    let chosen_path = core
        .paths
        .iter()
        .filter(|p| p.len() == max_len)
        .min_by(|a, b| a.cmp(b));

    let Some(path) = chosen_path else {
        return CausalEmotionTrend::default();
    };

    // 采样路径上有 valence 的事件序列
    let vals: Vec<f64> = path
        .iter()
        .filter_map(|id| valence_by_id.get(id).copied())
        .collect();

    if vals.len() < 2 {
        return CausalEmotionTrend::default();
    }

    let n = vals.len();
    let mean_valence = vals.iter().sum::<f64>() / n as f64;
    let head_tail_delta =
        vals.last().copied().unwrap_or(0.0) - vals.first().copied().unwrap_or(0.0);

    // 最小二乘斜率: slope = Σ(x_i - x̄)(y_i - ȳ) / Σ(x_i - x̄)^2
    let linear_slope = {
        let x_mean = (n - 1) as f64 / 2.0;
        let mut num = 0.0;
        let mut den = 0.0;
        for (i, v) in vals.iter().enumerate() {
            let x = i as f64 - x_mean;
            num += x * (v - mean_valence);
            den += x * x;
        }
        if den.abs() < f64::EPSILON {
            None
        } else {
            Some(num / den)
        }
    };

    // 正负翻转次数（0 视为中性，不构成符号翻转）
    let polarity_flips = vals
        .windows(2)
        .filter(|w| w[0].is_sign_positive() != w[1].is_sign_positive())
        .filter(|w| w[0] != 0.0 && w[1] != 0.0)
        .count();

    let direction = if head_tail_delta > DELTA_EPS {
        "逐级增强".to_string()
    } else if head_tail_delta < -DELTA_EPS {
        "逐级衰减".to_string()
    } else if polarity_flips >= 2 {
        "波动".to_string()
    } else {
        "平稳".to_string()
    };

    debug!(
        sampled = n,
        mean_valence,
        head_tail_delta,
        linear_slope = ?linear_slope,
        polarity_flips,
        direction = %direction,
        "因果链情绪走势统计完成"
    );

    CausalEmotionTrend {
        sampled_node_count: n,
        mean_valence: Some(mean_valence),
        head_tail_delta: Some(head_tail_delta),
        linear_slope,
        polarity_flips,
        direction,
    }
}
