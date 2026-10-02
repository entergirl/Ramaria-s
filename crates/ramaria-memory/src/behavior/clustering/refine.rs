//! crates/ramaria-memory/src/behavior/clustering/refine.rs - 簇提炼
//!
//! 设计特点:
//! - 关键词并集按频次取 Top-N；簇中心为通道向量均值。
//! - valence 均值/标准差按 salience 加权，权重为 0 时退化为等权。
//! - presentation 分布按频次降序；situation_strength 均值 None 按 3 计。
//! - 簇质量为内聚度（簇内平均相似度）× 一致性（1 − 归一化 valence 标准差）。
//! - 保留簇成员逐事件 start_ms / salience，供近期事件加权证据链使用。

use ramaria_core::behavior::{BehaviorSituation, PresentationFreq};
use ramaria_core::types::Presentation;
use std::collections::HashMap;

use super::sample::BehaviorSample;
use super::similarity::fused_similarity;

// =========================================================
// 簇提炼
// =========================================================

/// 簇成员逐事件信息（供近期事件加权证据链）。
///
/// 职责:
/// - 保留每个成员的 `start_ms` 与 `salience`，使 `build_evidence` 能用真实
///   事件时间计算 recency_factor（修复"恒 1.0"缺陷，D-V16-007）。
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterMember {
    /// 事件 id（锚点可能为负，调用方过滤后写入证据链）
    pub event_id: i64,
    /// 事件开始时间（Unix 毫秒）
    pub start_ms: i64,
    /// 显著性权重（salience 加权）
    pub salience: f64,
}

/// 提炼后的簇（可直接构造 `BehaviorSituation` 持久化）。
#[derive(Debug, Clone, PartialEq)]
pub struct RefinedCluster {
    /// 情境侧特征（含关键词并集/簇中心/valence 分布/presentation 分布等）
    pub situation: BehaviorSituation,
    /// 有效样本量 n_eff（salience 加权）
    pub n_eff: f64,
    /// 内聚度（簇内成员间平均相似度）
    pub cohesion: f64,
    /// 簇质量 = 内聚度 × 一致性（1 − 归一化 valence 标准差）
    pub quality: f64,
    /// 簇内事件 id（证据链引用）
    pub member_event_ids: Vec<i64>,
    /// 簇成员逐事件信息（保留 start/salience，供近期事件加权）。
    ///
    /// 与 `member_event_ids` 一一对应（同索引），顺序一致。
    pub member_events: Vec<ClusterMember>,
}

/// 关键词并集保留的 Top-N 条数。
pub const KEYWORD_TOP_N: usize = 10;

/// 簇提炼（v3.1 §4.2 Step 2.4）。
///
/// 输出:
/// - 关键词并集（频次 Top-N）
/// - 簇中心（情境通道 + 反应通道向量均值）
/// - valence 加权均值与标准差（salience 加权）
/// - presentation 分布
/// - situation_strength 均值（None 按 3）
/// - 时间跨度（天）
/// - n_eff / 内聚度 / 簇质量
pub fn refine_cluster(
    samples: &[BehaviorSample],
    members: &[usize],
    beta1: f64,
    beta2: f64,
) -> RefinedCluster {
    let member_samples: Vec<&BehaviorSample> = members.iter().map(|&i| &samples[i]).collect();

    // ---- 关键词并集（频次 Top-N） ----
    let mut kw_freq: HashMap<&str, usize> = HashMap::new();
    for s in &member_samples {
        for k in &s.situation_keywords {
            *kw_freq.entry(k.as_str()).or_insert(0) += 1;
        }
    }
    let mut kw_sorted: Vec<(&str, usize)> = kw_freq.into_iter().collect();
    kw_sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let keywords: Vec<String> = kw_sorted
        .into_iter()
        .take(KEYWORD_TOP_N)
        .map(|(k, _)| k.to_string())
        .collect();

    // ---- 簇中心（通道向量均值） ----
    let centroid = mean_vector(
        member_samples
            .iter()
            .filter_map(|s| s.situation_vector.as_deref()),
    );
    let response_centroid = mean_vector(
        member_samples
            .iter()
            .filter_map(|s| s.reaction_vector.as_deref()),
    );

    // ---- valence 加权均值/标准差（salience 加权） ----
    let weight_sum: f64 = member_samples.iter().map(|s| s.salience).sum();
    let valence_mean = if weight_sum > 0.0 {
        member_samples
            .iter()
            .map(|s| s.valence * s.salience)
            .sum::<f64>()
            / weight_sum
    } else {
        member_samples.iter().map(|s| s.valence).sum::<f64>() / member_samples.len().max(1) as f64
    };
    let variance = if weight_sum > 0.0 {
        member_samples
            .iter()
            .map(|s| s.salience * (s.valence - valence_mean).powi(2))
            .sum::<f64>()
            / weight_sum
    } else {
        member_samples
            .iter()
            .map(|s| (s.valence - valence_mean).powi(2))
            .sum::<f64>()
            / member_samples.len().max(1) as f64
    };
    let valence_std = variance.sqrt();

    // ---- presentation 分布 ----
    let mut pres_count: HashMap<Presentation, usize> = HashMap::new();
    for s in &member_samples {
        *pres_count.entry(s.presentation).or_insert(0) += 1;
    }
    let total = member_samples.len().max(1);
    let mut presentation_dist: Vec<PresentationFreq> = pres_count
        .into_iter()
        .map(|(p, c)| PresentationFreq {
            presentation: p,
            freq: c as f64 / total as f64,
        })
        .collect();
    presentation_dist.sort_by(|a, b| {
        b.freq
            .partial_cmp(&a.freq)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // ---- situation_strength 均值（None 等效 3） ----
    let strength_mean = member_samples
        .iter()
        .map(|s| s.situation_strength.unwrap_or(3) as f64)
        .sum::<f64>()
        / total as f64;

    // ---- 时间跨度（天） ----
    let min_start = member_samples.iter().map(|s| s.start_ms).min().unwrap_or(0);
    let max_start = member_samples.iter().map(|s| s.start_ms).max().unwrap_or(0);
    let time_span_days = (max_start - min_start) as f64 / 86_400_000.0;

    // ---- 内聚度 / 簇质量 ----
    let n = member_samples.len();
    let cohesion = if n <= 1 {
        1.0
    } else {
        let mut sum = 0.0;
        let mut cnt = 0usize;
        for a in 0..n {
            for b in (a + 1)..n {
                sum += fused_similarity(member_samples[a], member_samples[b], beta1, beta2);
                cnt += 1;
            }
        }
        sum / cnt as f64
    };
    // 一致性 = 1 − 归一化 valence 标准差（valence 范围 [-1,1]，std 上限 2）
    let consistency = (1.0 - (valence_std / 2.0)).clamp(0.0, 1.0);
    let quality = cohesion * consistency;

    // ---- n_eff ----
    let n_eff = if weight_sum > 0.0 {
        weight_sum
    } else {
        member_samples.len() as f64
    };

    RefinedCluster {
        situation: BehaviorSituation {
            keywords,
            centroid,
            response_centroid,
            valence_mean: valence_mean.clamp(-1.0, 1.0),
            valence_std,
            sample_count: member_samples.len(),
            presentation_dist,
            situation_strength_mean: strength_mean,
            time_span_days,
            trait_refs: Vec::new(),
        },
        n_eff,
        cohesion,
        quality,
        member_event_ids: member_samples.iter().map(|s| s.event_id).collect(),
        member_events: member_samples
            .iter()
            .map(|s| ClusterMember {
                event_id: s.event_id,
                start_ms: s.start_ms,
                salience: s.salience,
            })
            .collect(),
    }
}

/// 计算多向量的均值（无向量输入 → None）。
fn mean_vector<'a>(vecs: impl Iterator<Item = &'a [f32]>) -> Option<Vec<f32>> {
    let mut it = vecs;
    let first = it.next()?;
    let dim = first.len();
    if dim == 0 {
        return None;
    }
    let mut sum = vec![0.0f64; dim];
    let mut count = 0usize;
    for v in std::iter::once(first).chain(it) {
        if v.len() == dim {
            for (s, &x) in sum.iter_mut().zip(v.iter()) {
                *s += x as f64;
            }
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    Some(sum.iter().map(|&s| (s / count as f64) as f32).collect())
}
