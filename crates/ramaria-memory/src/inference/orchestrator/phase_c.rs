//! crates/ramaria-memory/src/inference/orchestrator/phase_c.rs - Phase C 置信度更新 + 漂移检测编排
//!
//! 设计特点:
//! - run_phase_c_update: 加载活跃 traits 与证据 → 按语义匹配度分配新事件 → 置信度更新 → 持久化 → 漂移检测。
//! - Manual 来源 trait 豁免：不参与证据分配 / 不做证据查询 / 不更新置信度 / 不挂证据（单一过滤点覆盖）。
//! - detect_and_summarize_drift: 对比 cluster_snapshots 旧分布（上一轮已吸收分布）与当前事件分布，产出 DriftSummary。
//! - 真实分布恢复未启用 / 旧分布缺失 / 无判别信息的分类显式跳过，带 skipped_count 标注，不生成占位假数据。
//! - restore_old_distribution: 从快照 samples JSON 恢复真实旧分布（valence/share 按 n_eff 展开）。
//! - 事件-trait 匹配度基于最长公共子串的轻量语义估计，无额外 LLM/embedding 调用。

use ramaria_core::{
    RamariaResult,
    traits::StorageBackend,
    types::{
        EvidenceDirection, MemoryEvent, PersonalityTrait, TraitEvidence, TraitSource, TraitStatus,
        now_ms,
    },
};
use tracing::{debug, error, info, warn};

use crate::inference::{
    confidence::{ConfidenceConfig, run_confidence_update},
    drift::DriftConfig,
};

use super::types::PhaseCResult;

// =========================================================
// Phase C: 置信度更新 + 漂移检测编排
// =========================================================

/// 执行 Phase C 更新：置信度更新 + 漂移检测 + 持久化。
///
/// 流程:
/// 1. 从 DB 加载当前 persona 的活跃 trait 及其证据记录。
/// 2. 使用新事件数据计算置信度更新（E_total、C、conf）。
/// 3. 执行漂移检测（对比 cluster_snapshots 中的旧分布与新事件分布）。
/// 4. 首轮推断跳过漂移检测（无旧分布可对比）。
/// 5. 持久化更新后的置信度和新增证据记录。
///
/// 说明:
/// - Manual 来源的 trait 豁免证据分配 / 置信度更新 / 证据挂载（保持零证据人工锚点）；
/// - `PhaseCResult.traits_updated` 只计实际更新（Manual 不在内）。
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

    // 只处理活跃 trait；Manual 来源豁免（不参与证据分配 / 不更新置信度 / 不挂证据）。
    // 豁免只停增量：命中前已挂的历史证据不清理，查询仍可见。
    let active_traits: Vec<&PersonalityTrait> = stored_traits
        .iter()
        .filter(|t| t.status == TraitStatus::Active && t.source != TraitSource::Manual)
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
        // 事件贡献 = (event.confidence, event.start)
        let event_data = (event.confidence, event.start);

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
                occurred_at: Some(event.start),
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

    let drift_skipped = drift_summary.as_ref().map(|s| s.skipped_count).unwrap_or(0);
    info!(
        persona_uid = %persona_owned,
        traits_updated,
        evidence_saved,
        has_significant_drift,
        drift_skipped,
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
mod drift;

use drift::detect_and_summarize_drift;
#[cfg(test)]
pub(crate) use drift::restore_old_distribution;

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

// =========================================================
// 单元测试（内存 SQLite，真实 StorageBackend 快照语义）
// =========================================================

#[cfg(test)]
mod tests;
