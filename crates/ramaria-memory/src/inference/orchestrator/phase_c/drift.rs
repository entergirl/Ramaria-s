//! crates/ramaria-memory/src/inference/orchestrator/phase_c/drift.rs - Phase C 漂移检测辅助
//!
//! 设计特点:
//! - 旧分布取自最新一期 cluster_snapshots，缺失或不可解析显式跳过
//! - 真实分布恢复未启用时整体跳过，不生成占位假数据
//! - 新分布从事件按分类分组提取，交 drift 模块判定

use super::*;
use crate::inference::drift::{CategoryEventData, DriftSummary, run_drift_detection};

// =========================================================
// 漂移检测辅助
// =========================================================

/// 执行漂移检测：对比 cluster_snapshots 中旧分布（上一轮已吸收分布）与新事件分布。
///
/// 流程:
/// 1. 从 storage 加载各分类的当前（最新一期）快照作为旧分布来源。
/// 2. `restore_real_distribution=false` 时整体显式跳过——不生成占位假数据。
/// 3. 旧分布缺失 / 解析失败 / 无判别信息的分类显式跳过并累计 `skipped_count`。
/// 4. 从 events 按 category 分组提取新分布，交 `run_drift_detection` 判定。
pub(super) async fn detect_and_summarize_drift(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    events: &[MemoryEvent],
    drift_config: &DriftConfig,
) -> RamariaResult<DriftSummary> {
    use crate::inference::stats::extract_primary_category;

    // 按 category 对事件分组
    let mut categories_map: std::collections::BTreeMap<String, Vec<&MemoryEvent>> =
        std::collections::BTreeMap::new();

    for event in events {
        let cat = extract_primary_category(event);
        categories_map.entry(cat).or_default().push(event);
    }

    if !drift_config.restore_real_distribution {
        warn!(
            persona_uid,
            candidate_categories = categories_map.len(),
            "Phase C: 真实旧分布恢复未启用（restore_real_distribution=false），跳过漂移检测"
        );
        return Ok(DriftSummary {
            categories: vec![],
            review_count: 0,
            any_drift: false,
            skipped_count: categories_map.len(),
        });
    }

    let mut category_data: Vec<CategoryEventData> = Vec::new();
    let mut skipped_count = 0usize;

    for (category, cat_events) in &categories_map {
        // 加载该分类的旧快照（当前/最新一期）
        let snapshots = storage
            .get_current_snapshots(persona_uid, category)
            .await
            .unwrap_or_else(|e| {
                warn!(
                    persona_uid,
                    category,
                    error = %e,
                    "Phase C: 加载快照失败，使用空旧分布"
                );
                vec![]
            });

        // 从快照 samples JSON 恢复真实旧分布（valence/share 按 n_eff 展开）。
        let (old_valences, old_shares, old_saliences) = restore_old_distribution(&snapshots);

        // 从当前事件提取新分布
        let new_valences: Vec<f64> = cat_events.iter().map(|e| e.valence).collect();
        let new_shares: Vec<f64> = cat_events.iter().map(|e| e.share).collect();
        let new_saliences: Vec<f64> = cat_events.iter().map(|e| e.salience).collect();
        let new_confidences: Vec<f64> = cat_events.iter().map(|e| e.confidence).collect();

        // 旧分布无可用信息 → 显式跳过该分类并累计计数（不静默、不阻塞主流程）。
        if old_valences.is_empty() {
            if snapshots.is_empty() {
                warn!(
                    persona_uid,
                    category, "Phase C: 无历史快照（新分类或上一轮快照未写入），跳过漂移检测"
                );
            } else {
                warn!(
                    persona_uid,
                    category, "Phase C: 快照 samples 缺失或解析失败，旧分布为空，跳过漂移检测"
                );
            }
            skipped_count += 1;
            continue;
        }

        // all-zeros 守卫仅防御"解析出的旧分布确无判别信息"的退化场景：
        // valence 与 share 两组展开值同时全零时视为无信息可对比。
        let valences_all_zero = old_valences.iter().all(|&v| v == 0.0);
        let shares_all_zero = old_shares.iter().all(|&s| s == 0.0);
        if valences_all_zero && shares_all_zero {
            warn!(
                persona_uid,
                category, "Phase C: 解析出的旧分布无判别信息（valence/share 全零），跳过漂移检测"
            );
            skipped_count += 1;
            continue;
        }

        category_data.push(CategoryEventData {
            category: category.clone(),
            old_valences,
            new_valences,
            old_shares,
            new_shares,
            old_saliences,
            new_saliences,
            old_confidences: vec![],
            new_confidences,
        });
    }

    if category_data.is_empty() {
        return Ok(DriftSummary {
            categories: vec![],
            review_count: 0,
            any_drift: false,
            skipped_count,
        });
    }

    let mut summary = run_drift_detection(&category_data, drift_config);
    summary.skipped_count += skipped_count;
    Ok(summary)
}

/// 从 `persona_cluster_snapshots` 的 `samples` JSON 恢复真实旧分布。
///
/// 快照 `samples` 为分类级聚合 JSON（由 L3 Phase A 持久化时写入），结构形如：
/// `{"category": "...", "event_count": N, "n_effective": n, "valence_mean": x,
///   "valence_std": s, "share_mean": y}`。
///
/// 漂移检测需要新旧两组样本向量进行 Wasserstein + 置换检验，因此将 `valence_mean`
/// 与 `share_mean` 按 `n_effective`（向下取整）重复展开，作为旧分布的近似样本点。
///
/// 参数:
/// - `snapshots`: 该 persona + 分类的当前快照列表。
///
/// 返回:
/// - (old_valences, old_shares, old_saliences)。samples 缺失或解析失败时返回空向量
///   （调用方据此跳过该分类漂移检测并记 warn）。
pub(crate) fn restore_old_distribution(
    snapshots: &[ramaria_core::types::ClusterSnapshot],
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut valences: Vec<f64> = Vec::new();
    let mut shares: Vec<f64> = Vec::new();

    for snap in snapshots {
        let Some(samples_json) = snap.samples.as_deref() else {
            continue;
        };
        // 解析快照聚合 JSON；解析失败记 debug 并跳过该快照（静默降级）。
        let Ok(v) = serde_json::from_str::<serde_json::Value>(samples_json) else {
            debug!(
                persona_uid = %snap.persona_uid,
                category = %snap.category,
                "Phase C: 快照 samples JSON 解析失败，跳过该快照旧分布恢复"
            );
            continue;
        };

        let valence_mean = v.get("valence_mean").and_then(|x| x.as_f64());
        let share_mean = v.get("share_mean").and_then(|x| x.as_f64());
        // 样本量取 n_effective（向下取整），缺失时回退 event_count。
        // 两者都缺失时 n_eff=0，无法可靠重建旧分布权重，跳过该快照（保守，不臆造样本）。
        let n_eff = v
            .get("n_effective")
            .and_then(|x| x.as_f64())
            .map(|n| n.floor() as usize)
            .or_else(|| {
                v.get("event_count")
                    .and_then(|c| c.as_u64())
                    .map(|c| c as usize)
            })
            .unwrap_or(0);

        if n_eff == 0 {
            debug!(
                persona_uid = %snap.persona_uid,
                category = %snap.category,
                "Phase C: 快照 samples 无样本量字段，跳过该快照旧分布恢复"
            );
            continue;
        }

        // 样本量上限防御（防极端大值拖慢置换检验）
        let n = n_eff.clamp(1, 10_000);
        if let Some(vm) = valence_mean {
            valences.extend(std::iter::repeat_n(vm.clamp(-1.0, 1.0), n));
        }
        if let Some(sm) = share_mean {
            shares.extend(std::iter::repeat_n(sm.clamp(0.0, 1.0), n));
        }
    }

    // salience 快照未持久化，返回空（salience 维度漂移在新旧都无样本时自动不显著）。
    (valences, shares, Vec::<f64>::new())
}
