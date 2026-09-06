//! crates/ramaria-memory/src/inference/orchestrator/phase_c.rs - Phase C 置信度更新 + 漂移检测编排
//!
//! 设计特点:
//! - run_phase_c_update: 加载活跃 traits 与证据 → 按语义匹配度分配新事件 → 置信度更新 → 持久化 → 漂移检测。
//! - detect_and_summarize_drift: 对比 cluster_snapshots 旧分布与当前事件分布，产出 DriftSummary。
//! - restore_old_distribution: 从快照 samples JSON 恢复真实旧分布（valence/share 按 n_eff 展开）。
//! - 事件-trait 匹配度基于最长公共子串的轻量语义估计，无额外 LLM/embedding 调用。

use ramaria_core::{
    RamariaResult,
    traits::StorageBackend,
    types::{EvidenceDirection, MemoryEvent, PersonalityTrait, TraitEvidence, TraitStatus, now_ms},
};
use tracing::{debug, error, info, warn};

use crate::inference::{
    confidence::{ConfidenceConfig, run_confidence_update},
    drift::{CategoryEventData, DriftConfig, DriftSummary, run_drift_detection},
};

use super::types::PhaseCResult;

// =========================================================
// Phase C: 置信度更新 + 漂移检测编排
// =========================================================

/// 执行 Phase C 更新：置信度更新 + 漂移检测 + 持久化。
///
/// 流程:
/// 1. 从 DB 加载当前 persona 的所有活跃 trait 及其证据记录。
/// 2. 使用新事件数据计算置信度更新（E_total、C、conf）。
/// 3. 执行漂移检测（对比 cluster_snapshots 中的旧分布与新事件分布）。
/// 4. 首轮推断跳过漂移检测（无旧分布可对比）。
/// 5. 持久化更新后的置信度和新增证据记录。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `persona_uid`: 目标人格标识。
/// - `new_traits`: Phase B 产出的 trait 列表。
/// - `events`: 本次 L3 推断使用的事件列表（用于计算证据贡献和漂移检测）。
/// - `is_first_round`: 是否为首轮推断（跳过漂移检测）。
///
/// 返回:
/// - PhaseCResult：包含更新数量、证据数量、漂移检测结果。
pub async fn run_phase_c_update(
    confidence_config: &ConfidenceConfig,
    drift_config: &DriftConfig,
    storage: &dyn StorageBackend,
    persona_uid: &str,
    new_traits: &[PersonalityTrait],
    events: &[MemoryEvent],
    is_first_round: bool,
) -> RamariaResult<PhaseCResult> {
    let persona_owned = persona_uid.to_string();
    let now = now_ms();

    if new_traits.is_empty() {
        info!(persona_uid = %persona_owned, "Phase C: 无 trait 需要更新置信度");
        return Ok(PhaseCResult {
            traits_updated: 0,
            evidence_saved: 0,
            has_significant_drift: false,
            drift_categories: vec![],
            confidence_summary: None,
            drift_summary: None,
        });
    }

    // ---- 1. 加载已有 traits 和证据 ----
    let stored_traits = storage
        .list_traits_by_persona(&persona_owned)
        .await
        .map_err(|e| {
            error!(persona_uid = %persona_owned, error = %e, "Phase C: 加载 traits 失败");
            e
        })?;

    // 只处理活跃 trait
    let active_traits: Vec<&PersonalityTrait> = stored_traits
        .iter()
        .filter(|t| t.status == TraitStatus::Active)
        .collect();

    if active_traits.is_empty() {
        info!(persona_uid = %persona_owned, "Phase C: 无活跃 trait，跳过");
        return Ok(PhaseCResult {
            traits_updated: 0,
            evidence_saved: 0,
            has_significant_drift: false,
            drift_categories: vec![],
            confidence_summary: None,
            drift_summary: None,
        });
    }

    // ---- 2. 为每个 trait 加载证据记录 ----
    let mut trait_states: Vec<(i64, f64, Vec<TraitEvidence>)> = Vec::new();
    for t in &active_traits {
        let evidence = storage
            .list_evidence_by_trait(t.id)
            .await
            .unwrap_or_else(|e| {
                warn!(trait_id = t.id, error = %e, "Phase C: 加载 trait 证据失败，使用空列表");
                vec![]
            });
        trait_states.push((t.id, t.confidence, evidence));
    }

    // ---- 3. 准备新事件数据（按语义匹配度分配事件） ----
    // 基于事件关键词与 trait 标签的文本重叠计算匹配度，
    // 仅将匹配度 > 阈值的事件分配给对应 trait，score = valence × relevance。
    let n_traits = active_traits.len();
    let mut new_event_data_by_trait: Vec<Vec<(f64, i64)>> = vec![vec![]; n_traits];
    let mut new_event_scores_by_trait: Vec<Vec<f64>> = vec![vec![]; n_traits];

    for event in events {
        // 事件贡献 = (event.confidence, event.created_at)
        let event_data = (event.confidence, event.created_at);

        // 解析事件关键词
        let event_keywords: Vec<&str> = event
            .keywords
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        for (i, t) in active_traits.iter().enumerate() {
            // 计算该事件与该 trait 的语义匹配度
            // 同时匹配 trait_label（如"尽责"）和 meaning（如"对任务有强烈的完成意愿"）
            let relevance =
                compute_event_trait_relevance(&event_keywords, &t.trait_label, &t.meaning);

            // 所有事件均分配到所有 trait（floor 保证 relevance ≥ 0.3），
            // 但 score = valence × relevance 使高匹配度的 trait 获得更强的证据权重。
            if relevance > 0.0 {
                new_event_data_by_trait[i].push(event_data);
                // score 为带方向的相关性：方向由 valence 符号决定，强度由 relevance 缩放
                let score = (event.valence.clamp(-1.0, 1.0) * relevance).clamp(-1.0, 1.0);
                new_event_scores_by_trait[i].push(score);
            }
        }
    }

    // ---- 4. 执行置信度更新 ----
    let confidence_summary = run_confidence_update(
        &trait_states,
        &new_event_data_by_trait,
        &new_event_scores_by_trait,
        now,
        confidence_config,
    );

    // ---- 5. 持久化置信度更新 ----
    let mut traits_updated = 0usize;
    for update in &confidence_summary.updates {
        match storage
            .update_trait_confidence(
                update.trait_id,
                update.conf_after,
                update.e_total_after,
                update.consistency_after,
            )
            .await
        {
            Ok(_) => {
                traits_updated += 1;
                debug!(
                    trait_id = update.trait_id,
                    conf_before = %update.conf_before,
                    conf_after = %update.conf_after,
                    "Phase C: 置信度更新已持久化"
                );
            }
            Err(e) => {
                warn!(
                    trait_id = update.trait_id,
                    error = %e,
                    "Phase C: 置信度更新持久化失败（跳过）"
                );
            }
        }
    }

    // ---- 6. 保存证据记录 ----
    let mut evidence_saved = 0usize;
    for (i, t) in active_traits.iter().enumerate() {
        let trait_id = t.id;
        for (j, event) in events.iter().enumerate() {
            // 跳过无效的事件 ID
            if event.id == 0 {
                continue;
            }

            let score = new_event_scores_by_trait[i].get(j).copied().unwrap_or(0.0);

            let evidence = TraitEvidence {
                id: 0,
                trait_id,
                event_id: event.id,
                direction: if score >= 0.0 {
                    EvidenceDirection::Support
                } else {
                    EvidenceDirection::Contradict
                },
                score,
                decay: 1.0, // 新证据初始衰减为 1.0
                created_at: now,
            };

            match storage.save_evidence(&evidence).await {
                Ok(_) => evidence_saved += 1,
                Err(e) => {
                    warn!(
                        trait_id,
                        event_id = event.id,
                        error = %e,
                        "Phase C: 证据记录保存失败（跳过）"
                    );
                }
            }
        }
    }

    // ---- 7. 漂移检测 ----
    let (has_significant_drift, drift_categories, drift_summary) = if is_first_round {
        info!(persona_uid = %persona_owned, "Phase C: 首轮推断，跳过漂移检测");
        (false, vec![], None)
    } else {
        match detect_and_summarize_drift(storage, &persona_owned, events, drift_config).await {
            Ok(summary) => {
                let categories: Vec<String> = summary
                    .categories
                    .iter()
                    .filter(|c| c.needs_review)
                    .map(|c| c.category.clone())
                    .collect();
                let has_drift = !categories.is_empty();
                if has_drift {
                    info!(
                        persona_uid = %persona_owned,
                        drift_categories = ?categories,
                        "Phase C: 检测到性格漂移"
                    );
                }
                (has_drift, categories, Some(summary))
            }
            Err(e) => {
                warn!(persona_uid = %persona_owned, error = %e, "Phase C: 漂移检测失败，跳过");
                (false, vec![], None)
            }
        }
    };

    info!(
        persona_uid = %persona_owned,
        traits_updated,
        evidence_saved,
        has_significant_drift,
        "Phase C: 更新完成"
    );

    Ok(PhaseCResult {
        traits_updated,
        evidence_saved,
        has_significant_drift,
        drift_categories,
        confidence_summary: Some(confidence_summary),
        drift_summary,
    })
}

// =========================================================
// 漂移检测辅助
// =========================================================

/// 执行漂移检测：对比 cluster_snapshots 中的旧分布与新事件分布。
///
/// 从 storage 加载当前快照作为旧分布，从 events 中按 category 分组提取新分布。
async fn detect_and_summarize_drift(
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

    let mut category_data: Vec<CategoryEventData> = Vec::new();

    for (category, cat_events) in &categories_map {
        // 加载该分类的旧快照数据
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

        // 从快照提取旧分布。
        // 开启开关时从 `persona_cluster_snapshots.samples` JSON 恢复真实旧分布；
        // 关闭开关（`restore_real_distribution=false`）时回退硬编码占位（全 0 / 0.5）。
        let (old_valences, old_shares, old_saliences) = if drift_config.restore_real_distribution {
            restore_old_distribution(&snapshots)
        } else {
            // 旧版占位：快照数条 0.0 valence / 0.5 share，漂移检测实际不触发（all-zeros 守卫）。
            (
                snapshots.iter().map(|_s| 0.0).collect(),
                snapshots.iter().map(|_s| 0.5).collect(),
                Vec::<f64>::new(),
            )
        };

        // 从当前事件提取新分布
        let new_valences: Vec<f64> = cat_events.iter().map(|e| e.valence).collect();
        let new_shares: Vec<f64> = cat_events.iter().map(|e| e.share).collect();
        let new_saliences: Vec<f64> = cat_events.iter().map(|e| e.salience).collect();
        let new_confidences: Vec<f64> = cat_events.iter().map(|e| e.confidence).collect();

        // 如果旧分布为空（新分类），跳过漂移检测。
        // 快照缺失时记 warn 并跳过（静默降级，不阻塞主流程）。
        if old_valences.is_empty() || old_valences.iter().all(|&v| v == 0.0) {
            if old_valences.is_empty() {
                warn!(
                    persona_uid,
                    category, "Phase C: 快照旧分布为空（samples 缺失或解析失败），跳过漂移检测"
                );
            } else {
                debug!(
                    persona_uid,
                    category, "Phase C: 新分类无旧分布，跳过漂移检测"
                );
            }
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
        });
    }

    Ok(run_drift_detection(&category_data, drift_config))
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
pub(super) fn restore_old_distribution(
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

// =========================================================
// 事件-Trait 语义匹配度计算
// =========================================================

/// 计算事件与性格 trait 的语义匹配度（0.0..1.0）。
///
/// 基于事件的关键词与 trait 的标签和含义描述的文本重叠来估计相关性。
/// 这是 LLM 评估的轻量替代方案，无需额外 API 调用。
///
/// 算法:
/// 1. 对每个事件关键词，分别计算其与 trait_label 和 meaning 的最长公共子串比例。
/// 2. 取两者中较大的匹配度作为该关键词的得分。
/// 3. 综合匹配度 = 匹配关键词数 / 总关键词数。
/// 4. 无关键词时返回中等默认值 0.5。
///
/// 参数:
/// - `event_keywords`: 事件的关键词列表（已按逗号分割并 trim）。
/// - `trait_label`: trait 的标签文本（如"尽责""温和""幽默"）。
/// - `trait_meaning`: trait 的含义描述（自然语言，如"对任务有强烈的完成意愿"）。
///
/// 返回:
/// - 0.0..1.0 的匹配度值。
pub(super) fn compute_event_trait_relevance(
    event_keywords: &[&str],
    trait_label: &str,
    trait_meaning: &str,
) -> f64 {
    if event_keywords.is_empty() {
        // 无关键词时默认中等相关，不排除事件
        return 0.5;
    }

    let label_chars: Vec<char> = trait_label.chars().collect();
    let meaning_chars: Vec<char> = trait_meaning.chars().collect();

    let mut match_count = 0usize;

    for kw in event_keywords {
        let kw_chars: Vec<char> = kw.chars().collect();

        // 分别对 trait_label 和 meaning 计算 LCS 重叠比例
        let label_overlap = longest_common_substring_ratio(&kw_chars, &label_chars);
        let meaning_overlap = if meaning_chars.is_empty() {
            0.0
        } else {
            longest_common_substring_ratio(&kw_chars, &meaning_chars)
        };

        // 取较大的重叠度
        let best_overlap = label_overlap.max(meaning_overlap);

        // 阈值 0.3：至少 30% 的字符重叠才视为相关
        if best_overlap > 0.3 {
            match_count += 1;
        }
    }

    let relevance = match_count as f64 / event_keywords.len() as f64;

    // 边界保护：确保每个事件至少获得最低匹配度（0.3），
    // 使得所有事件都能参与所有 trait 的证据更新，
    // 但匹配度高的 trait 获得更高的 score 权重。
    relevance.max(0.3)
}

/// 计算两个字符序列的最长公共子串长度与较短者长度的比例。
///
/// 使用动态规划 O(m*n) 计算 LCS 长度，返回 lcs_len / min(len_a, len_b)。
pub(super) fn longest_common_substring_ratio(a: &[char], b: &[char]) -> f64 {
    let n = a.len();
    let m = b.len();

    if n == 0 || m == 0 {
        return 0.0;
    }

    // DP: dp[i][j] = 以 a[i-1] 和 b[j-1] 结尾的最长公共后缀长度
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    let mut max_len = 0usize;

    for i in 1..=n {
        for j in 1..=m {
            if a[i - 1] == b[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
                max_len = max_len.max(dp[i][j]);
            }
        }
    }

    max_len as f64 / (n.min(m) as f64)
}
