//! crates/ramaria-memory/src/inference/causal.rs - A8 因果链特征提取
//!
//! 设计特点:
//! - 从 event_relations 表（CausedBy 关系）构建有向图，DFS 计算最长因果路径
//! - 循环模式探测：识别重复出现的因果链序列，指向稳定的行为脚本
//! - 时延分布与情绪沿链走势为扩展特征（独立开关，无数据时为空缺省形态）
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
/// - 扩展特征（时延分布、情绪沿链走势）在无有效数据时保持为空缺省形态，
///   格式化层据此跳过对应段落，从而兼容旧调用路径的输出。
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
    /// 因果边时延分布（扩展特征；空表无有效采样）
    pub latency_stats: CausalLatencyStats,
    /// 沿最长因果路径的情绪走势（扩展特征；空表无有效采样）
    pub emotion_trend: CausalEmotionTrend,
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

/// 因果边时延分布统计。
///
/// 职责:
/// - 描述相邻 CausedBy 事件之间的时间间隔，用于识别"即时连锁"与"延迟触发"。
/// - 对每条边取 `effect.start - cause.start`；时延为负或事件时间缺失的边剔除。
///
/// 字段约定:
/// - 时间戳使用事件 `start`（Unix 毫秒，事件发生起点）。
///   当前写入侧按簇共享同一时间窗，簇内边的时延常为 0；该特征在
///   跨簇/跨会话因果边出现后更有区分度，此处按事件发生时间如实计算。
/// - 时间缺省形态：`sampled_edge_count == 0` 时均值为 None、档位计数为 0。
#[derive(Debug, Clone, Default)]
pub struct CausalLatencyStats {
    /// 参与统计（时延非负且两端事件存在）的因果边数
    pub sampled_edge_count: usize,
    /// 因负时延或时间缺失被剔除的因果边数（仅日志诊断，不参与统计）
    pub excluded_edge_count: usize,
    /// 时延均值（毫秒）
    pub mean_ms: Option<f64>,
    /// 时延中位数（毫秒；偶数样本取中间两值平均）
    pub median_ms: Option<f64>,
    /// 时延最小值（毫秒）
    pub min_ms: Option<f64>,
    /// 时延最大值（毫秒）
    pub max_ms: Option<f64>,
    /// ≤1 天的边数
    pub within_1d_count: usize,
    /// 1-7 天（不含 1 天含 7 天）的边数
    pub within_7d_count: usize,
    /// >7 天的边数
    pub over_7d_count: usize,
}

impl CausalLatencyStats {
    /// 是否无有效采样（无有效样本时不渲染对应文本段落）。
    pub fn is_empty(&self) -> bool {
        self.sampled_edge_count == 0
    }
}

/// 沿最长因果路径的情绪走势。
///
/// 职责:
/// - 采样路径上每节点事件的 valence，刻画"情绪沿因果链逐级演变"的方向与幅度。
/// - 采样路径取所有源→汇路径中最长的一条（同长取节点 ID 字典序最小，保证确定性）。
///
/// 字段约定:
/// - 有效采样节点数 < 2 时（含无 valence 事件）返回空缺省形态（`is_empty() == true`）。
/// - 方向描述基于首末 delta 与正负翻转次数：净变化显著→增强/衰减，
///   净变化小但正负反复→波动，其余→平稳。
#[derive(Debug, Clone, Default)]
pub struct CausalEmotionTrend {
    /// 沿链采样的事件节点数（≥2 才构成有效走势）
    pub sampled_node_count: usize,
    /// 采样节点 valence 均值
    pub mean_valence: Option<f64>,
    /// 路径末端与首端 valence 之差（末 - 首）
    pub head_tail_delta: Option<f64>,
    /// 线性趋势斜率（最小二乘拟合，x=节点序号 0..n-1，y=valence）
    pub linear_slope: Option<f64>,
    /// valence 正负符号沿链翻转次数（0 视为中性不计翻转）
    pub polarity_flips: usize,
    /// 主导方向描述（"逐级增强/逐级衰减/波动/平稳"）
    pub direction: String,
}

impl CausalEmotionTrend {
    /// 是否无有效走势（不渲染对应文本段落）。
    pub fn is_empty(&self) -> bool {
        self.sampled_node_count == 0
    }
}

// =========================================================
// 核心提取函数
// =========================================================

/// 因果核心分析结果（旧四特征 + 供扩展特征使用的中间数据）。
struct CausalCore {
    chain_length: usize,
    cyclic_patterns: Vec<CyclePattern>,
    total_causal_events: usize,
    total_causal_edges: usize,
    /// 全部源→汇简单路径（每路径含起点，供情绪沿链采样）
    paths: Vec<Vec<i64>>,
    /// CausedBy 边对（from_id → to_id，供时延统计）
    edge_pairs: Vec<(i64, i64)>,
}

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
const MS_PER_DAY: i64 = 86_400_000;

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
/// - 因果边时延分布（扩展段，`latency_stats` 为空时不渲染）
/// - 情绪沿链走势（扩展段，`emotion_trend` 为空时不渲染）
/// - 解读提示
///
/// 参数:
/// - `features`: 因果链特征。
///
/// 返回:
/// - 格式化后的中文段落文本。若 chain_length=0 且无循环模式且无扩展特征，
///   返回空字符串。
pub fn format_causal_features_text(features: &CausalChainFeatures) -> String {
    if features.chain_length == 0
        && features.cyclic_patterns.is_empty()
        && features.latency_stats.is_empty()
        && features.emotion_trend.is_empty()
    {
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

    // ---- 扩展段: 因果边时延分布 ----
    if !features.latency_stats.is_empty() {
        let s = &features.latency_stats;
        text.push_str("\n**因果边时延分布:**\n");
        text.push_str(&format!(
            "  有效采样 {} 条边（剔除 {} 条时间缺失/负时延边）。\n",
            s.sampled_edge_count, s.excluded_edge_count
        ));
        let days = |ms: f64| ms / MS_PER_DAY as f64;
        if let (Some(mean), Some(median)) = (s.mean_ms, s.median_ms) {
            text.push_str(&format!(
                "  时延均值约 {:.1} 天，中位数约 {:.1} 天，",
                days(mean),
                days(median)
            ));
        }
        if let (Some(min), Some(max)) = (s.min_ms, s.max_ms) {
            text.push_str(&format!("范围 {:.1} ~ {:.1} 天。\n", days(min), days(max)));
        } else {
            text.push('\n');
        }
        text.push_str(&format!(
            "  分档: ≤1 天 {} 条、1-7 天 {} 条、>7 天 {} 条。\n",
            s.within_1d_count, s.within_7d_count, s.over_7d_count
        ));
    }

    // ---- 扩展段: 情绪沿链走势 ----
    if !features.emotion_trend.is_empty() {
        let t = &features.emotion_trend;
        text.push_str("\n**情绪沿链走势:**\n");
        text.push_str(&format!(
            "  沿最长因果链采样 {} 个事件节点。\n",
            t.sampled_node_count
        ));
        if let Some(mean) = t.mean_valence {
            text.push_str(&format!("  valence 均值 {:.2}，", mean));
        }
        if let Some(delta) = t.head_tail_delta {
            text.push_str(&format!("首末变化 {:.2}，", delta));
        }
        if let Some(slope) = t.linear_slope {
            text.push_str(&format!("每步趋势斜率 {:.3}，", slope));
        }
        text.push_str(&format!("正负翻转 {} 次。\n", t.polarity_flips));
        text.push_str(&format!("  情绪整体呈\"{}\"沿链演变。\n", t.direction));
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

    /// 创建测试用 MemoryEvent，支持自定义发生时间与 valence。
    fn make_event_ts(id: i64, start_ms: i64, valence: f64) -> MemoryEvent {
        let mut ev = make_event(id, "");
        ev.start = start_ms;
        ev.end = start_ms + 60_000;
        ev.valence = valence;
        ev
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

    /// extract_causal_features 无 CausedBy 关系时的默认结果验证。
    #[test]
    fn no_causal_relations_returns_default() {
        // 空关系列表
        let events = vec![make_event(1, "工作")];
        let relations: Vec<EventRelation> = vec![];
        let features = extract_causal_features(&events, &relations);
        assert_eq!(features.chain_length, 0);
        assert!(features.cyclic_patterns.is_empty());
        assert_eq!(features.total_causal_events, 0);
        // 仅 RelatedTo 关系（非因果）
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
            latency_stats: CausalLatencyStats::default(),
            emotion_trend: CausalEmotionTrend::default(),
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
            latency_stats: CausalLatencyStats::default(),
            emotion_trend: CausalEmotionTrend::default(),
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
            latency_stats: CausalLatencyStats::default(),
            emotion_trend: CausalEmotionTrend::default(),
        };
        let text = format_causal_features_text(&features);
        assert!(text.contains("1 跳"));
        assert!(text.contains("较为局部"));
        assert!(!text.contains("主动驱动者"));
    }

    // =========================================================
    // extract_causal_features_extended 测试
    // =========================================================

    #[test]
    fn extended_empty_relations_returns_default() {
        let events = vec![make_event_ts(1, 1000, 0.2)];
        let relations: Vec<EventRelation> = vec![];
        let features = extract_causal_features_extended(&events, &relations);
        assert_eq!(features.chain_length, 0);
        assert!(features.latency_stats.is_empty());
        assert!(features.emotion_trend.is_empty());
        // 与旧 extract 路径完全一致
        let legacy = extract_causal_features(&events, &relations);
        assert_eq!(legacy.chain_length, features.chain_length);
        assert!(features.latency_stats.sampled_edge_count == 0);
        assert_eq!(
            format_causal_features_text(&legacy),
            format_causal_features_text(&features)
        );
    }

    #[test]
    fn extended_missing_time_no_panic() {
        // start=0 视为时间缺失 + valence 中性场景：不 panic，扩展字段合理缺省
        let events = vec![
            make_event_ts(1, 0, 0.0),
            make_event_ts(2, 0, 0.0),
            make_event_ts(3, 0, 0.0),
        ];
        let relations = vec![make_causal(1, 2)];
        let features = extract_causal_features_extended(&events, &relations);
        assert_eq!(features.chain_length, 1);
        // start=0 被过滤为时间缺失 → 时延无有效采样
        assert!(features.latency_stats.is_empty());
        assert!(features.latency_stats.excluded_edge_count >= 1);
        // 路径上 valence 全为 0，仍构成 2 节点采样
        assert_eq!(features.emotion_trend.sampled_node_count, 2);
    }

    #[test]
    fn extended_latency_stats_correct() {
        // 链 1→2→3→4，时延分别为 1 天 / 2 天 / 7 天。
        // 时间起点取 MS_PER_DAY 起（>0 保证不被当作时间缺失剔除）。
        let events = vec![
            make_event_ts(1, MS_PER_DAY, 0.1),
            make_event_ts(2, 2 * MS_PER_DAY, 0.2),
            make_event_ts(3, 4 * MS_PER_DAY, 0.3),
            make_event_ts(4, 11 * MS_PER_DAY, 0.4),
        ];
        let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(3, 4)];
        let features = extract_causal_features_extended(&events, &relations);
        assert_eq!(features.total_causal_edges, 3);
        let s = &features.latency_stats;
        assert_eq!(s.sampled_edge_count, 3);
        assert_eq!(s.excluded_edge_count, 0);
        assert_eq!(s.min_ms, Some(MS_PER_DAY as f64));
        assert_eq!(s.max_ms, Some((7 * MS_PER_DAY) as f64));
        assert_eq!(s.median_ms, Some((2 * MS_PER_DAY) as f64));
        let expected_mean = (MS_PER_DAY + 2 * MS_PER_DAY + 7 * MS_PER_DAY) as f64 / 3.0;
        assert!((s.mean_ms.unwrap() - expected_mean).abs() < 1.0);
        assert_eq!(s.within_1d_count, 1);
        assert_eq!(s.within_7d_count, 2);
        assert_eq!(s.over_7d_count, 0);
    }

    #[test]
    fn extended_latency_drops_negative_and_missing() {
        // 1→2 时延接近 1 天（有效）；2→1 负时延剔除；2→4 事件缺失剔除。
        let events = vec![
            make_event_ts(1, 5_000, 0.1),
            make_event_ts(2, MS_PER_DAY, 0.2),
            make_event_ts(3, 0, 0.3), // start=0 → 时间缺失（但未参与出边）
        ];
        let relations = vec![
            make_causal(1, 2),
            make_causal(2, 1), // 负时延（2 晚于 1，反向则负）
            make_causal(2, 4), // to_id=4 不在 events → 缺失
        ];
        let features = extract_causal_features_extended(&events, &relations);
        let s = &features.latency_stats;
        assert_eq!(s.sampled_edge_count, 1);
        assert_eq!(s.excluded_edge_count, 2);
        assert_eq!(s.min_ms, Some((MS_PER_DAY - 5_000) as f64));
        assert_eq!(s.max_ms, Some((MS_PER_DAY - 5_000) as f64));
    }

    #[test]
    fn extended_emotion_trend_increasing() {
        // 链 valence: -0.6 → -0.2 → 0.3 → 0.7（逐级增强）
        let events = vec![
            make_event_ts(1, 0, -0.6),
            make_event_ts(2, MS_PER_DAY, -0.2),
            make_event_ts(3, 2 * MS_PER_DAY, 0.3),
            make_event_ts(4, 3 * MS_PER_DAY, 0.7),
        ];
        let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(3, 4)];
        let features = extract_causal_features_extended(&events, &relations);
        let t = &features.emotion_trend;
        assert_eq!(t.sampled_node_count, 4);
        assert!(t.mean_valence.unwrap() > 0.0);
        assert!(t.head_tail_delta.unwrap() > 1.2);
        assert!(t.linear_slope.unwrap() > 0.3);
        assert_eq!(t.direction, "逐级增强");
        assert_eq!(t.polarity_flips, 1);
    }

    #[test]
    fn extended_emotion_trend_decreasing() {
        // 链 valence: 0.8 → 0.4 → -0.2 → -0.6（逐级衰减）
        let events = vec![
            make_event_ts(1, 0, 0.8),
            make_event_ts(2, MS_PER_DAY, 0.4),
            make_event_ts(3, 2 * MS_PER_DAY, -0.2),
            make_event_ts(4, 3 * MS_PER_DAY, -0.6),
        ];
        let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(3, 4)];
        let features = extract_causal_features_extended(&events, &relations);
        let t = &features.emotion_trend;
        assert_eq!(t.direction, "逐级衰减");
        assert!(t.head_tail_delta.unwrap() < -1.2);
    }

    #[test]
    fn extended_emotion_trend_flapping() {
        // 链 valence: 0.5 → -0.6 → 0.7 → -0.5 → 0.4（净变化小、多次翻转 → 波动）
        let events = vec![
            make_event_ts(1, 0, 0.5),
            make_event_ts(2, MS_PER_DAY, -0.6),
            make_event_ts(3, 2 * MS_PER_DAY, 0.7),
            make_event_ts(4, 3 * MS_PER_DAY, -0.5),
            make_event_ts(5, 4 * MS_PER_DAY, 0.4),
        ];
        let relations = vec![
            make_causal(1, 2),
            make_causal(2, 3),
            make_causal(3, 4),
            make_causal(4, 5),
        ];
        let features = extract_causal_features_extended(&events, &relations);
        let t = &features.emotion_trend;
        assert_eq!(t.polarity_flips, 4);
        assert_eq!(t.direction, "波动");
    }

    #[test]
    fn extended_emotion_path_too_short_is_empty() {
        // 只有 1 条边 = 2 节点，已满足最小采样；但若事件缺失使有效节点不足则空
        let events = vec![make_event_ts(1, 0, 0.5)];
        let relations = vec![make_causal(1, 2)]; // id2 无事件 → 有效节点 1 个
        let features = extract_causal_features_extended(&events, &relations);
        assert!(features.emotion_trend.is_empty());
        // 时延同样因 to 端缺失而为空
        assert!(features.latency_stats.is_empty());
    }

    #[test]
    fn extended_shortest_path_pick_longest_deterministic() {
        // 分叉图: 1→2→3 与 1→4。情绪应沿最长路径 1→2→3 采样。
        let events = vec![
            make_event_ts(1, 0, -0.5),
            make_event_ts(2, MS_PER_DAY, -0.2),
            make_event_ts(3, 2 * MS_PER_DAY, 0.3),
            make_event_ts(4, MS_PER_DAY, 0.8),
        ];
        let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(1, 4)];
        let features = extract_causal_features_extended(&events, &relations);
        assert_eq!(features.chain_length, 2);
        let t = &features.emotion_trend;
        assert_eq!(t.sampled_node_count, 3);
        assert!(t.head_tail_delta.unwrap() > 0.7);
        assert_eq!(t.direction, "逐级增强");
    }

    // =========================================================
    // format 扩展段渲染测试
    // =========================================================

    #[test]
    fn format_with_empty_extended_omits_sections() {
        let features = CausalChainFeatures {
            chain_length: 2,
            total_causal_events: 3,
            total_causal_edges: 2,
            cyclic_patterns: vec![],
            latency_stats: CausalLatencyStats::default(),
            emotion_trend: CausalEmotionTrend::default(),
        };
        let text = format_causal_features_text(&features);
        assert!(text.contains("因果链分析"));
        assert!(!text.contains("因果边时延分布"));
        assert!(!text.contains("情绪沿链走势"));
    }

    #[test]
    fn format_with_latency_renders_section() {
        let features = CausalChainFeatures {
            chain_length: 1,
            total_causal_events: 2,
            total_causal_edges: 2,
            cyclic_patterns: vec![],
            latency_stats: CausalLatencyStats {
                sampled_edge_count: 1,
                excluded_edge_count: 1,
                mean_ms: Some(2.0 * MS_PER_DAY as f64),
                median_ms: Some(2.0 * MS_PER_DAY as f64),
                min_ms: Some(MS_PER_DAY as f64),
                max_ms: Some(3.0 * MS_PER_DAY as f64),
                within_1d_count: 1,
                within_7d_count: 0,
                over_7d_count: 0,
            },
            emotion_trend: CausalEmotionTrend::default(),
        };
        let text = format_causal_features_text(&features);
        assert!(text.contains("因果边时延分布"));
        assert!(text.contains("剔除 1 条"));
        assert!(!text.contains("情绪沿链走势"));
    }

    #[test]
    fn format_with_emotion_renders_section() {
        let features = CausalChainFeatures {
            chain_length: 1,
            total_causal_events: 2,
            total_causal_edges: 1,
            cyclic_patterns: vec![],
            latency_stats: CausalLatencyStats::default(),
            emotion_trend: CausalEmotionTrend {
                sampled_node_count: 2,
                mean_valence: Some(0.1),
                head_tail_delta: Some(0.6),
                linear_slope: Some(0.6),
                polarity_flips: 0,
                direction: "逐级增强".to_string(),
            },
        };
        let text = format_causal_features_text(&features);
        assert!(!text.contains("因果边时延分布"));
        assert!(text.contains("情绪沿链走势"));
        assert!(text.contains("逐级增强"));
    }

    #[test]
    fn format_extended_full_features_from_chain() {
        let events = vec![
            make_event_ts(1, 0, -0.6),
            make_event_ts(2, MS_PER_DAY, -0.2),
            make_event_ts(3, 3 * MS_PER_DAY, 0.3),
            make_event_ts(4, 10 * MS_PER_DAY, 0.7),
        ];
        let relations = vec![make_causal(1, 2), make_causal(2, 3), make_causal(3, 4)];
        let features = extract_causal_features_extended(&events, &relations);
        let text = format_causal_features_text(&features);
        assert!(text.contains("因果边时延分布"));
        assert!(text.contains("情绪沿链走势"));
        assert!(text.contains("逐级增强"));
    }

    /// v1.7 等价锁定：旧路径（extract_causal_features）产出文本不含扩展段，
    /// 且与 v1.7 旧格式逐字节一致（未因扩展段渲染引入任何前缀/后缀改动）。
    #[test]
    fn legacy_format_text_snapshot_unchanged() {
        // 旧路径仅计算基础特征，扩展字段恒为空 → format 输出不含扩展段
        let events = vec![
            make_event(1, "工作压力"),
            make_event(2, "拖延"),
            make_event(3, "自责"),
        ];
        let relations = vec![make_causal(1, 2), make_causal(2, 3)];
        let legacy = extract_causal_features(&events, &relations);
        let text = format_causal_features_text(&legacy);
        assert_eq!(legacy.chain_length, 2);
        assert!(legacy.latency_stats.is_empty());
        assert!(legacy.emotion_trend.is_empty());
        assert!(!text.contains("因果边时延分布"));
        assert!(!text.contains("情绪沿链走势"));

        // 逐字节快照：2 跳、2 条边、无循环模式的旧格式文本
        let expected = "## 因果链分析 (A8)\n\n因果网络概况: 3 个事件通过 2 条因果关系连接，最长因果链为 2 跳。\n解读提示: 中等因果链提示用户行为有一定连锁效应。\n\n";
        assert_eq!(text, expected);
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
