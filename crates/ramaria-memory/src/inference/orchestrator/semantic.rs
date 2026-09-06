//! crates/ramaria-memory/src/inference/orchestrator/semantic.rs - 语义标签持久化与跨版本簇匹配
//!
//! 设计特点:
//! - generate_semantic_labels_for_clusters: 为每个簇生成语义标签。
//! - persist_cluster_snapshots_with_semantic_labels: 语义标签 embedding → ClusterSnapshot 入库 → 跨版本匹配。
//! - query_cross_version_matches: 便捷查询入口（供前端展示历史相似簇）。
//! - 降级策略: embedding 不可用仅存文本标签；单簇失败不影响其他簇；无历史快照跳过匹配。

use ramaria_core::{
    RamariaResult,
    traits::{EmbeddingProvider, StorageBackend},
    types::{ClusterSnapshot, now_ms},
};
use tracing::{debug, info, warn};

use crate::inference::clustering::{
    ClusteringResult, CrossVersionMatchResult, HistoricalSnapshot, generate_semantic_label,
    match_clusters_cross_version,
};

// =========================================================
// 语义标签持久化与跨版本簇匹配
// =========================================================

/// 从聚类结果中为每个簇生成语义标签。
///
/// 对每个簇调用 `generate_semantic_label()`，
/// 从核心样本的 paraphrase 中提取高频共性短语作为语义标签。
///
/// 参数:
/// - `result`: 聚类结果，使用其 `clusters` 字段。
///
/// 返回:
/// - 与 `result.clusters` 顺序一致的语义标签列表。
pub fn generate_semantic_labels_for_clusters(result: &ClusteringResult) -> Vec<String> {
    result
        .clusters
        .iter()
        .map(generate_semantic_label)
        .collect()
}

/// 为语义标签生成 embedding 并持久化聚类快照。
///
/// 完整流程:
/// 1. 为每个簇生成语义标签（调用 `generate_semantic_label`）。
/// 2. 调用 `EmbeddingProvider::embed()` 为每个语义标签生成 embedding 向量。
/// 3. 将 embedding 序列化为 BLOB。
/// 4. 构建 `ClusterSnapshot` 并写入数据库。
/// 5. 查询历史快照 → 执行跨版本匹配 → 记录日志。
///
/// 参数:
/// - `embedding_provider`: 嵌入模型接口。为 `None` 时跳过 embedding，仅保存文本标签。
/// - `storage`: 存储后端（用于读写快照）。
/// - `result`: 聚类结果。
/// - `persona_uid`: 当前人格标识。
/// - `category`: 事件分类标签（工作/社交/家庭等）。
/// - `match_threshold`: 跨版本余弦相似度匹配阈值（默认 0.85）。
///
/// 返回:
/// - 保存的快照数量 + 跨版本匹配结果。
///
/// 降级策略:
/// - Embedding provider 不可用时：仅保存文本语义标签，`semantic_label_embedding` 为 NULL。
/// - Embedding 生成失败时：warn 日志 + 跳过该簇的 embedding（不阻塞其他簇）。
/// - 跨版本匹配无历史数据时：正常保存新快照，跳过匹配。
pub async fn persist_cluster_snapshots_with_semantic_labels(
    embedding_provider: Option<&dyn EmbeddingProvider>,
    storage: &dyn StorageBackend,
    result: &ClusteringResult,
    persona_uid: &str,
    category: &str,
    match_threshold: f64,
) -> RamariaResult<(usize, Option<CrossVersionMatchResult>)> {
    let semantic_labels = generate_semantic_labels_for_clusters(result);

    // 为每个簇生成 embedding 向量
    let mut embeddings: Vec<Option<Vec<f32>>> = Vec::with_capacity(semantic_labels.len());

    if let Some(provider) = embedding_provider {
        if provider.is_available() {
            for label in &semantic_labels {
                match provider.embed(label).await {
                    Ok(vec) => {
                        debug!(
                            persona_uid,
                            label,
                            dim = vec.len(),
                            "语义标签 embedding 生成成功"
                        );
                        embeddings.push(Some(vec));
                    }
                    Err(e) => {
                        warn!(
                            persona_uid,
                            label,
                            error = %e,
                            "语义标签 embedding 生成失败，该簇跳过 embedding"
                        );
                        embeddings.push(None);
                    }
                }
            }
        } else {
            info!(
                persona_uid,
                "Embedding provider 不可用，跳过语义标签 embedding 生成"
            );
            embeddings.resize(semantic_labels.len(), None);
        }
    } else {
        embeddings.resize(semantic_labels.len(), None);
    }

    // 保存快照
    let mut snapshot_count = 0usize;
    for (idx, cluster) in result.clusters.iter().enumerate() {
        let label = &semantic_labels[idx];
        let emb_blob = embeddings[idx]
            .as_ref()
            .map(|v| ClusterSnapshot::serialize_embedding(v));

        let cluster_label = format!("cluster_{}", idx);
        let samples_json = serde_json::json!({
            "core_paraphrases": &cluster.core_paraphrases,
            "edge_paraphrases": &cluster.edge_paraphrases,
            "size": cluster.size,
        });

        let snapshot = ClusterSnapshot {
            id: 0,
            persona_uid: persona_uid.to_string(),
            category: category.to_string(),
            cluster_label,
            samples: Some(samples_json.to_string()),
            count: cluster.size as i32,
            is_current: true,
            created_at: now_ms(),
            semantic_label: Some(label.clone()),
            semantic_label_embedding: emb_blob,
        };

        match storage.save_cluster_snapshot(&snapshot).await {
            Ok(_) => snapshot_count += 1,
            Err(e) => {
                warn!(
                    persona_uid,
                    category,
                    cluster_idx = idx,
                    error = %e,
                    "保存聚类快照失败（跳过该簇，不影响其他簇）"
                );
            }
        }
    }

    info!(
        persona_uid,
        category,
        snapshot_count,
        total_clusters = result.cluster_count,
        "语义标签聚类快照保存完成"
    );

    // 跨版本匹配：加载历史快照并执行匹配
    let cross_version_result = if !embeddings.is_empty() && embeddings.iter().any(|e| e.is_some()) {
        match storage.get_all_snapshots_with_embeddings(persona_uid).await {
            Ok(historical) => {
                if historical.is_empty() {
                    info!(persona_uid, "无历史快照，跳过跨版本匹配");
                    None
                } else {
                    // 转换为轻量 HistoricalSnapshot
                    let hist_snaps: Vec<HistoricalSnapshot> =
                        historical.iter().map(|s| s.into()).collect();

                    // 对每个有 embedding 的簇执行匹配
                    let mut all_matches = CrossVersionMatchResult::default();
                    for (idx, emb_opt) in embeddings.iter().enumerate() {
                        if let Some(emb) = emb_opt {
                            let cluster_matches =
                                match_clusters_cross_version(emb, &hist_snaps, match_threshold);
                            if cluster_matches.matched_count > 0 {
                                debug!(
                                    persona_uid,
                                    cluster_idx = idx,
                                    label = %semantic_labels[idx],
                                    matched = cluster_matches.matched_count,
                                    total_historical = cluster_matches.total_historical,
                                    "跨版本匹配命中"
                                );
                            }
                            // 聚合所有簇的匹配结果
                            all_matches.matches.extend(cluster_matches.matches);
                            all_matches.total_historical = cluster_matches.total_historical;
                            all_matches.matched_count += cluster_matches.matched_count;
                        }
                    }

                    // 重新排序聚合后的匹配
                    all_matches.matches.sort_by(|a, b| {
                        b.similarity
                            .partial_cmp(&a.similarity)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    all_matches.best_match = all_matches.matches.first().cloned();

                    info!(
                        persona_uid,
                        total_historical = all_matches.total_historical,
                        matched = all_matches.matched_count,
                        "跨版本簇匹配完成"
                    );

                    Some(all_matches)
                }
            }
            Err(e) => {
                warn!(
                    persona_uid,
                    error = %e,
                    "加载历史快照失败，跳过跨版本匹配"
                );
                None
            }
        }
    } else {
        info!(persona_uid, "无可用 embedding，跳过跨版本匹配");
        None
    };

    Ok((snapshot_count, cross_version_result))
}

/// 查询该 persona 的历史簇匹配信息（便捷入口）。
///
/// 用于前端展示：某个簇的语义标签在历史上是否出现过类似的倾向。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `persona_uid`: 人格标识。
/// - `current_embedding`: 当前簇的语义标签 embedding。
/// - `match_threshold`: 匹配阈值（默认 0.85）。
///
/// 返回:
/// - CrossVersionMatchResult，含历史匹配列表。
pub async fn query_cross_version_matches(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    current_embedding: &[f32],
    match_threshold: f64,
) -> RamariaResult<CrossVersionMatchResult> {
    let historical = storage
        .get_all_snapshots_with_embeddings(persona_uid)
        .await
        .unwrap_or_else(|e| {
            warn!(persona_uid, error = %e, "查询历史快照失败");
            vec![]
        });

    let hist_snaps: Vec<HistoricalSnapshot> = historical.iter().map(|s| s.into()).collect();
    Ok(match_clusters_cross_version(
        current_embedding,
        &hist_snaps,
        match_threshold,
    ))
}
