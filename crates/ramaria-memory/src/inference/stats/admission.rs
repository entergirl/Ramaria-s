//! crates/ramaria-memory/src/inference/stats/admission.rs - Ramaria A1 三轨准入与 tentative 自动提升模块
//!
//! 设计特点:
//! - 三轨动态准入: classify_event 按 confidence 分入 confirmed/tentative/discarded 三轨道。
//! - 向后兼容: prefilter_events 保留并委托给三轨分类（use_calibrated_weights=false 时按旧阈值硬截断）。
//! - tentative 跨批次复现自动提升: 关键词簇互证（簇大小 + 跨批次 + Jaccard 相似度）满足时提升为 confirmed。
//! - 内部关键词相似度统一收敛到 crate::similarity::jaccard_similarity。
//!
//! 可见性说明:
//! - are_different_batches 为 pub(super): 仅供 stats 模块根级测试套件（tests.rs）直接调用，非外部 API。
//! - keyword_jaccard / should_promote_cluster 保持私有，仅本模块内部使用。

use super::category::group_by_category;
use super::config::{AdmissionTrack, ClassifiedEvents, StatsConfig};
use ramaria_core::types::MemoryEvent;

// =========================================================
// A1: 准入轨道分类
// =========================================================

/// 将单个事件分类到准入轨道。
///
/// 参数:
/// - `event`: 待分类的事件。
///
/// 返回:
/// - `AdmissionTrack` 枚举值。
///
/// 说明:
/// - 边界值处理: confidence == 0.6 → Confirmed, confidence == 0.45 → Tentative。
/// - 负值/NaN 防御: confidence < 0.0 或 NaN → Discarded。
pub fn classify_event(event: &MemoryEvent) -> AdmissionTrack {
    if event.confidence.is_nan() || event.confidence < 0.0 {
        return AdmissionTrack::Discarded;
    }
    if event.confidence >= 0.6 {
        AdmissionTrack::Confirmed
    } else if event.confidence >= 0.45 {
        AdmissionTrack::Tentative
    } else {
        AdmissionTrack::Discarded
    }
}

/// 将事件列表分类到三个准入轨道。
///
/// 参数:
/// - `events`: 完整的事件列表。
///
/// 返回:
/// - `ClassifiedEvents`，包含三个轨道的分类结果。
pub fn classify_events(events: &[MemoryEvent]) -> ClassifiedEvents {
    let mut confirmed = Vec::new();
    let mut tentative = Vec::new();
    let mut discarded_count = 0usize;

    for event in events {
        match classify_event(event) {
            AdmissionTrack::Confirmed => confirmed.push(event.clone()),
            AdmissionTrack::Tentative => tentative.push(event.clone()),
            AdmissionTrack::Discarded => discarded_count += 1,
        }
    }

    ClassifiedEvents {
        confirmed,
        tentative,
        discarded_count,
    }
}

// =========================================================
// A1 兼容: 预过滤（向后兼容，委托给三轨分类）
// =========================================================

/// 预过滤事件：排除 confidence 低于阈值的推测性事件。
///
/// 说明:
/// - 内部委托给 `classify_event`，使用配置中的 `confidence_threshold` 做硬截断。
/// - 当 `use_calibrated_weights=true` 时，调用方应优先使用 `run_phase_a_stats`，
///   它会自动使用三轨分类。
///
/// 参数:
/// - `events`: 完整事件列表。
/// - `config`: 统计配置（使用其中的 confidence_threshold）。
///
/// 返回:
/// - 两次结果：(通过过滤的事件列表, 被排除的事件数)。
pub fn prefilter_events(events: &[MemoryEvent], config: &StatsConfig) -> (Vec<MemoryEvent>, usize) {
    let total = events.len();
    let filtered: Vec<MemoryEvent> = events
        .iter()
        .filter(|e| classify_event(e) != AdmissionTrack::Discarded)
        // 当 use_calibrated_weights=false 时，额外按旧阈值硬截断以保持完全兼容
        .filter(|e| {
            if config.use_calibrated_weights {
                true
            } else {
                e.confidence >= config.confidence_threshold
            }
        })
        .cloned()
        .collect();
    let excluded = total - filtered.len();
    (filtered, excluded)
}

// =========================================================
// A1 扩展: Tentative 跨批次复现自动提升
// =========================================================

/// Tentative 事件跨批次复现自动提升配置。
///
/// 职责:
/// - 控制 tentative 事件自动提升为 confirmed 的条件阈值。
///
/// 字段约定:
/// - `min_cluster_size`: 同一关键词簇中至少需 N 条 tentative 事件才考虑提升，默认 2。
/// - `min_batch_interval_hours`: 判定为"不同批次"的最小时间间隔（小时），默认 6.0。
/// - `keyword_similarity_threshold`: 簇内事件间关键词 Jaccard 相似度阈值，默认 0.4。
/// - `promoted_confidence`: 提升后的置信度值，默认 0.6（刚好进入 confirmed 轨道）。
#[derive(Debug, Clone)]
pub struct TentativePromotionConfig {
    /// 最小簇大小（至少 N 条 tentative 事件在同一关键词簇中）
    pub min_cluster_size: usize,
    /// 不同批次的最小时间间隔（小时）
    pub min_batch_interval_hours: f64,
    /// 关键词 Jaccard 相似度阈值（用于簇内互证）
    pub keyword_similarity_threshold: f64,
    /// 提升后的置信度值
    pub promoted_confidence: f64,
}

impl Default for TentativePromotionConfig {
    fn default() -> Self {
        Self {
            min_cluster_size: 2,
            min_batch_interval_hours: 6.0,
            keyword_similarity_threshold: 0.4,
            promoted_confidence: 0.6,
        }
    }
}

/// Tentative 事件提升结果。
///
/// 职责:
/// - 返回提升后的 confirmed 事件列表和未提升的 tentative 事件列表。
/// - 调用方应将 promoted 事件合并到 confirmed 轨道参与后续 Phase A 统计。
#[derive(Debug, Clone)]
pub struct TentativePromotionResult {
    /// 提升为 confirmed 的事件列表（confidence 已设为 promoted_confidence）
    pub promoted: Vec<MemoryEvent>,
    /// 未提升的 tentative 事件（保持原 confidence，继续以半权重参与统计）
    pub remaining_tentative: Vec<MemoryEvent>,
    /// 被提升的事件数
    pub promoted_count: usize,
    /// 未提升的事件数
    pub remaining_count: usize,
}

/// 计算两个事件的关键词 Jaccard 相似度。
///
/// 公式: J(A,B) = |A ∩ B| / |A ∪ B|
///
/// 参数:
/// - `a_keywords`: 事件 A 的关键词集合。
/// - `b_keywords`: 事件 B 的关键词集合。
///
/// 返回:
/// - Jaccard 相似度 [0.0, 1.0]。任一方关键词为空时返回 0.0。
///
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::jaccard_similarity`。
fn keyword_jaccard(a_keywords: &str, b_keywords: &str) -> f64 {
    let a_set: std::collections::HashSet<&str> = a_keywords
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let b_set: std::collections::HashSet<&str> = b_keywords
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    crate::similarity::jaccard_similarity(a_set, b_set)
}

/// 判断两个事件是否来自不同批次。
///
/// 策略:
/// - 比较 `created_at` 时间戳（Unix 毫秒），差值超过 `min_batch_interval_hours` 小时视为不同批次。
/// - 若任一事件的 `created_at` 为 0（未初始化），保守视为同批次。
///
/// 参数:
/// - `a`: 事件 A。
/// - `b`: 事件 B。
/// - `config`: 提升配置。
///
/// 返回:
/// - true 表示来自不同批次。
pub(super) fn are_different_batches(
    a: &MemoryEvent,
    b: &MemoryEvent,
    config: &TentativePromotionConfig,
) -> bool {
    if a.created_at == 0 || b.created_at == 0 {
        return false;
    }
    let diff_ms = (a.created_at - b.created_at).abs() as f64;
    let diff_hours = diff_ms / (1000.0 * 3600.0);
    diff_hours >= config.min_batch_interval_hours
}

/// 判断一个 tentative 事件簇是否满足互证条件并应提升。
///
/// 判断标准（所有条件必须同时满足）:
/// 1. 簇大小 ≥ `min_cluster_size`。
/// 2. 至少存在一对事件来自不同批次。
/// 3. 簇内事件对的关键词 Jaccard 相似度均值 ≥ `keyword_similarity_threshold`。
///
/// 参数:
/// - `cluster`: 同一关键词簇中的 tentative 事件。
/// - `config`: 提升配置。
///
/// 返回:
/// - true 表示该簇应被提升。
fn should_promote_cluster(cluster: &[MemoryEvent], config: &TentativePromotionConfig) -> bool {
    if cluster.len() < config.min_cluster_size {
        return false;
    }

    // 条件 2: 至少一对事件来自不同批次
    let has_cross_batch = (0..cluster.len()).any(|i| {
        ((i + 1)..cluster.len()).any(|j| are_different_batches(&cluster[i], &cluster[j], config))
    });

    if !has_cross_batch {
        return false;
    }

    // 条件 3: 簇内关键词 Jaccard 相似度均值 ≥ 阈值
    let mut total_sim = 0.0f64;
    let mut pair_count = 0usize;
    for i in 0..cluster.len() {
        for j in (i + 1)..cluster.len() {
            let a_kw = cluster[i].keywords.as_deref().unwrap_or("");
            let b_kw = cluster[j].keywords.as_deref().unwrap_or("");
            total_sim += keyword_jaccard(a_kw, b_kw);
            pair_count += 1;
        }
    }

    if pair_count == 0 {
        return false;
    }

    let avg_sim = total_sim / pair_count as f64;
    avg_sim >= config.keyword_similarity_threshold
}

/// 对 tentative 事件执行跨批次复现自动提升。
///
/// 算法:
/// 1. 按主分类将 tentative 事件分组为关键词簇。
/// 2. 对每个簇调用 `should_promote_cluster` 判断是否应提升。
/// 3. 满足条件的簇内所有事件 confidence 设为 `promoted_confidence`，归入 promoted。
/// 4. 不满足条件的簇内事件保持原 confidence，归入 remaining_tentative。
///
/// 说明:
/// - 本函数为纯数值逻辑，不执行 I/O。调用方负责将提升后的事件写入存储。
/// - `confirmed` 参数保留以供未来扩展（如与 confirmed 事件做交叉验证），当前版本仅用于签名兼容。
/// - 当 embedding 可用时，调用方可在提升前额外过滤：对 `should_promote_cluster` 返回 true 的簇，
///   使用 `paraphrase` 或 `summary` 字段的 embedding 做余弦相似度验证（> 0.7）。
///
/// 参数:
/// - `tentative`: tentative 轨道的事件列表。
/// - `confirmed`: confirmed 轨道的事件列表（保留供扩展，当前仅用于签名兼容）。
/// - `config`: 提升配置。
///
/// 返回:
/// - `TentativePromotionResult`，包含 promoted 和 remaining 两部分。
pub fn promote_tentative_events(
    tentative: &[MemoryEvent],
    confirmed: &[MemoryEvent],
    config: &TentativePromotionConfig,
) -> TentativePromotionResult {
    // 允许 unused 参数以保持签名扩展性
    let _ = confirmed;

    if tentative.is_empty() {
        return TentativePromotionResult {
            promoted: Vec::new(),
            remaining_tentative: Vec::new(),
            promoted_count: 0,
            remaining_count: 0,
        };
    }

    // Step 1: 按主分类分组（关键词簇）
    let grouped = group_by_category(tentative);

    let mut promoted = Vec::new();
    let mut remaining_tentative = Vec::new();

    // Step 2: 对每个簇判断是否应提升
    for (_category, cluster) in grouped {
        if should_promote_cluster(&cluster, config) {
            // 提升: 将簇内所有事件的 confidence 设为 promoted_confidence
            for mut event in cluster {
                event.confidence = config.promoted_confidence;
                promoted.push(event);
            }
        } else {
            // 不满足条件: 保持原样
            remaining_tentative.extend(cluster);
        }
    }

    let promoted_count = promoted.len();
    let remaining_count = remaining_tentative.len();

    TentativePromotionResult {
        promoted,
        remaining_tentative,
        promoted_count,
        remaining_count,
    }
}
