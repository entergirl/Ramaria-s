//! crates/ramaria-memory/src/event/batcher/similarity.rs - 语义/关键词相似度与孤立节点吸附
//!
//! 设计特点:
//! - `compute_semantic_score`: 关键词 Jaccard 与 embedding 余弦按 α 融合
//! - 余弦/Jaccard 实现统一收敛到 `crate::similarity`，本处为薄包装
//! - `absorb_orphans`: 将孤立节点语义吸附到多节点簇，失败者交由 Pending Buffer
//! - `compute_centroid`: 计算簇内节点 embedding 均值（语义中心）
//! - 纯计算，无 I/O 与 LLM 依赖

use ramaria_core::keyword::KeywordToken;

use super::graph;

// =========================================================
// 语义相似度计算
// =========================================================

/// 计算两个 L1Item 的组合相似度得分。
///
/// 公式: `score = α × sim_kw + (1-α) × sim_sem`
///
/// 说明:
/// - `sim_kw`: 关键词 Jaccard 相似度（由调用方预先计算）。
/// - `sim_sem`: 基于 L1 embedding 的余弦相似度。
/// - 若任一 embedding 为 None，返回 None（调用方应降级为 α=1.0 纯关键词）。
///
/// 参数:
/// - `sim_kw`: 已计算的关键词 Jaccard 相似度。
/// - `alpha`: 关键词权重，1.0=纯关键词，0.0=纯语义。
///
/// 返回:
/// - `Some(score)`: 融合相似度得分。
/// - `None`: 缺少 embedding，无法计算语义部分。
pub fn compute_semantic_score(
    embedding_a: Option<&[f32]>,
    embedding_b: Option<&[f32]>,
    sim_kw: f64,
    alpha: f64,
) -> Option<f64> {
    let emb_a = embedding_a?;
    let emb_b = embedding_b?;
    let sim_sem = cosine_similarity(emb_a, emb_b);
    Some(alpha * sim_kw + (1.0 - alpha) * sim_sem)
}

/// 计算两个等长向量的余弦相似度。
///
/// 公式: `cos(θ) = (A·B) / (||A|| × ||B||)`
///
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::cosine_similarity`，本函数为薄包装。
///
/// 返回:
/// - 余弦相似度值，范围 [-1.0, 1.0]。
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    crate::similarity::cosine_similarity(a, b)
}

// =========================================================
// Jaccard 相似度计算
// =========================================================

/// 计算两个关键词列表的 Jaccard 相似度。
///
/// 公式: `J(A, B) = |A ∩ B| / |A ∪ B|`
///
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::jaccard_similarity`，本函数为薄包装。
/// - 统一语义: 基于集合去重（重复关键词不影响结果）；任一侧为空（含两侧皆空）→ 0.0。
///
/// 参数:
/// - `kw_a`: 第一个关键词列表。
/// - `kw_b`: 第二个关键词列表。
///
/// 返回:
/// - Jaccard 相似度，范围 [0.0, 1.0]。
pub fn jaccard_similarity(kw_a: &[KeywordToken], kw_b: &[KeywordToken]) -> f64 {
    crate::similarity::jaccard_similarity(kw_a.iter(), kw_b.iter())
}

// =========================================================
// 孤立节点语义吸附
// =========================================================

/// 尝试将孤立节点（单节点连通分量）语义吸附到已有簇中。
///
/// 算法:
/// 1. 分离多节点簇（≥ 2）和孤立节点（= 1）。
/// 2. 对每个多节点簇，计算其语义中心向量（各节点 embedding 的均值）。
/// 3. 对每个孤立节点，计算其 embedding 与各簇语义中心的余弦相似度。
/// 4. 若最大相似度 ≥ θ_attach，将孤立节点并入该簇。
/// 5. 无法吸附的孤立节点（无 embedding 或相似度不足）返回到 `remaining_orphans`，
///    由上层送入 Pending Buffer。
///
/// 参数:
/// - `graph`: 关键词 Jaccard 图（含节点 embedding）。
/// - `components`: 连通分量列表（来自 `split_large_components`）。
/// - `theta_attach`: 语义吸附相似度阈值，默认 0.3。
///
/// 返回:
/// - `(clusters, remaining_orphans)`:
///   - `clusters`: 仅多节点簇（可能经语义吸附扩充）。
///   - `remaining_orphans`: 所有未被吸附的孤立节点（含无 embedding 者），由上层送 Pending Buffer。
///
/// 降级策略:
/// - 孤立节点无 embedding: 送入 `remaining_orphans`（交由 Pending Buffer 按关键词逻辑处理）。
/// - 无多节点簇可供吸附: 所有孤立节点送入 `remaining_orphans`。
/// - 簇无节点有 embedding（全簇 embedding=None）: 该簇跳过，不参与吸附。
pub fn absorb_orphans(
    graph: &graph::KeywordGraph,
    components: Vec<Vec<usize>>,
    theta_attach: f64,
) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
    // 分离多节点簇和孤立节点
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut orphans: Vec<Vec<usize>> = Vec::new();

    for comp in components {
        if comp.len() >= 2 {
            clusters.push(comp);
        } else {
            orphans.push(comp);
        }
    }

    // 无孤立节点或无可吸附目标 → 所有孤儿送入 remaining_orphans
    if orphans.is_empty() || clusters.is_empty() {
        return (clusters, orphans);
    }

    // 计算各簇的语义中心
    let centroids: Vec<Option<Vec<f32>>> = clusters
        .iter()
        .map(|comp| compute_centroid(graph, comp))
        .collect();

    let mut remaining_orphans: Vec<Vec<usize>> = Vec::new();

    for orphan in orphans {
        let node_idx = orphan[0];
        let node_emb = match &graph.nodes[node_idx].embedding {
            Some(emb) => emb,
            None => {
                // 无 embedding → 送入 remaining_orphans，由 Pending Buffer 按关键词逻辑处理
                remaining_orphans.push(orphan);
                continue;
            }
        };

        let mut best_sim: f64 = 0.0;
        let mut best_cluster: Option<usize> = None;

        for (ci, centroid_opt) in centroids.iter().enumerate() {
            if let Some(centroid) = centroid_opt {
                let sim = cosine_similarity(node_emb, centroid);
                if sim > best_sim {
                    best_sim = sim;
                    best_cluster = Some(ci);
                }
            }
        }

        if best_sim >= theta_attach
            && let Some(ci) = best_cluster
        {
            tracing::debug!(
                orphan_idx = node_idx,
                target_cluster = ci,
                similarity = best_sim,
                "孤立节点语义吸附成功"
            );
            clusters[ci].push(node_idx);
            continue;
        }

        // 相似度不足，送入 remaining_orphans
        tracing::debug!(
            orphan_idx = node_idx,
            best_similarity = best_sim,
            theta_attach,
            "孤立节点语义相似度不足，送入 Pending Buffer"
        );
        remaining_orphans.push(orphan);
    }

    (clusters, remaining_orphans)
}

/// 计算一个簇内所有节点的 embedding 均值向量（语义中心）。
///
/// 参数:
/// - `graph`: 关键词 Jaccard 图。
/// - `component`: 簇的节点索引列表。
///
/// 返回:
/// - `Some(centroid)`: 簇内所有有 embedding 的节点的均值向量。
/// - `None`: 簇内无节点有 embedding（全为 None）。
fn compute_centroid(graph: &graph::KeywordGraph, component: &[usize]) -> Option<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    let mut count: usize = 0;

    for &idx in component {
        if let Some(emb) = &graph.nodes[idx].embedding {
            count += 1;
            match &mut sum {
                None => sum = Some(emb.clone()),
                Some(s) => {
                    for (si, &ei) in s.iter_mut().zip(emb.iter()) {
                        *si += ei;
                    }
                }
            }
        }
    }

    if count == 0 {
        return None;
    }

    sum.map(|s| s.into_iter().map(|v| v / count as f32).collect())
}
