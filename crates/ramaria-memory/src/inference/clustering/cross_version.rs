//! crates/ramaria-memory/src/inference/clustering/cross_version.rs - 跨版本簇匹配
//!
//! 设计特点:
//! - 按簇语义标签与代表样本相似度匹配上一版快照
//! - 历史快照为只读投影，缺失时返回空匹配
//! - 仅做匹配与统计，不写库

use super::*;

// =========================================================
// 跨版本簇匹配
// =========================================================

/// 跨版本簇匹配结果——单个历史快照的匹配信息。
#[derive(Debug, Clone)]
pub struct CrossVersionMatch {
    /// 历史快照的 id
    pub snapshot_id: i64,
    /// 历史快照的语义标签文本
    pub semantic_label: String,
    /// 历史快照的分类
    pub category: String,
    /// 余弦相似度 (0..1)
    pub similarity: f64,
    /// 是否匹配（similarity ≥ match_threshold）
    pub is_match: bool,
    /// 历史快照的创建时间（Unix 毫秒）
    pub snapshot_created_at: i64,
}

/// 跨版本簇匹配的聚合结果。
#[derive(Debug, Clone, Default)]
pub struct CrossVersionMatchResult {
    /// 所有匹配项（相似度降序）
    pub matches: Vec<CrossVersionMatch>,
    /// 最佳匹配项（相似度最高者）
    pub best_match: Option<CrossVersionMatch>,
    /// 被查询的历史快照总数
    pub total_historical: usize,
    /// 匹配到的快照数
    pub matched_count: usize,
}

/// 跨版本簇匹配的语义余弦相似度阈值（统一 0.85）。
///
/// 语义标签 embedding 与历史快照的余弦相似度 ≥ 该值才判定为同一性格倾向的延续，
/// 低于 0.85 视为不同倾向，避免将语义相近但不同的簇误判为跨版本延续。
pub const CROSS_VERSION_MATCH_THRESHOLD: f64 = 0.85;

/// 执行跨版本簇匹配。
///
/// 算法:
/// 1. 对当前簇的语义标签 embedding 与每个历史快照的 embedding 计算余弦相似度。
/// 2. 相似度 ≥ `match_threshold`（默认 0.85）视为匹配。
/// 3. 返回按相似度降序排列的匹配列表。
///
/// 参数:
/// - `current_embedding`: 当前簇语义标签的 embedding 向量。
/// - `historical_snapshots`: 该 persona 的历史快照列表（需含 `semantic_label_embedding`）。
/// - `match_threshold`: 匹配阈值，默认 0.85。
///
/// 返回:
/// - CrossVersionMatchResult，含匹配列表和统计信息。
pub fn match_clusters_cross_version(
    current_embedding: &[f32],
    historical_snapshots: &[HistoricalSnapshot],
    match_threshold: f64,
) -> CrossVersionMatchResult {
    let total = historical_snapshots.len();
    if current_embedding.is_empty() || total == 0 {
        return CrossVersionMatchResult {
            matches: Vec::new(),
            best_match: None,
            total_historical: total,
            matched_count: 0,
        };
    }

    let mut matches: Vec<CrossVersionMatch> = historical_snapshots
        .iter()
        .filter_map(|snap| {
            let hist_emb = snap.semantic_label_embedding.as_ref()?;
            let hist_vec = ramaria_core::types::ClusterSnapshot::deserialize_embedding(hist_emb)?;
            if hist_vec.len() != current_embedding.len() {
                return None; // 维度不匹配，跳过
            }
            let sim = cosine_similarity(current_embedding, &hist_vec);
            Some(CrossVersionMatch {
                snapshot_id: snap.id,
                semantic_label: snap.semantic_label.clone().unwrap_or_default(),
                category: snap.category.clone(),
                similarity: sim,
                is_match: sim >= match_threshold,
                snapshot_created_at: snap.created_at,
            })
        })
        .collect();

    // 按相似度降序排列
    matches.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let matched_count = matches.iter().filter(|m| m.is_match).count();
    let best_match = matches.first().cloned();

    CrossVersionMatchResult {
        matches,
        best_match,
        total_historical: total,
        matched_count,
    }
}

/// 历史快照的轻量表示（用于跨版本匹配输入）。
///
/// 职责:
/// - 从 `ClusterSnapshot` 中提取跨版本匹配所需的最小字段集。
/// - 避免在纯计算函数中依赖完整的 `ClusterSnapshot` 类型。
#[derive(Debug, Clone)]
pub struct HistoricalSnapshot {
    /// 快照 id
    pub id: i64,
    /// 语义标签文本
    pub semantic_label: Option<String>,
    /// 语义标签 embedding BLOB
    pub semantic_label_embedding: Option<Vec<u8>>,
    /// 分类
    pub category: String,
    /// 创建时间
    pub created_at: i64,
}

impl From<&ramaria_core::types::ClusterSnapshot> for HistoricalSnapshot {
    fn from(s: &ramaria_core::types::ClusterSnapshot) -> Self {
        Self {
            id: s.id,
            semantic_label: s.semantic_label.clone(),
            semantic_label_embedding: s.semantic_label_embedding.clone(),
            category: s.category.clone(),
            created_at: s.created_at,
        }
    }
}
