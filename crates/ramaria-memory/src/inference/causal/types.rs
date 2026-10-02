//! crates/ramaria-memory/src/inference/causal/types.rs - A8 因果链特征数据结构
//!
//! 设计特点:
//! - 汇总因果网络概况、循环模式、时延分布与情绪走势四类特征
//! - 扩展特征（时延/情绪）在无有效数据时保持空缺省形态，格式化层据此跳过段落
//! - 全部为纯数据结构，零 I/O，可独立序列化与测试
//! - `CausalCore` 为内部中间结果，仅供同目录算法子模块复用

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

/// 因果核心分析结果（旧四特征 + 供扩展特征使用的中间数据）。
pub(super) struct CausalCore {
    pub(super) chain_length: usize,
    pub(super) cyclic_patterns: Vec<CyclePattern>,
    pub(super) total_causal_events: usize,
    pub(super) total_causal_edges: usize,
    /// 全部源→汇简单路径（每路径含起点，供情绪沿链采样）
    pub(super) paths: Vec<Vec<i64>>,
    /// CausedBy 边对（from_id → to_id，供时延统计）
    pub(super) edge_pairs: Vec<(i64, i64)>,
}
