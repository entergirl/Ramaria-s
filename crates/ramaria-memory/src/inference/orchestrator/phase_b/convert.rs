//! crates/ramaria-memory/src/inference/orchestrator/phase_b/convert.rs - Phase B 推断结果转换
//!
//! 设计特点:
//! - InferredTrait → PersonalityTrait，layer 字符串映射到 TraitLayer 枚举。
//! - 无法识别的 layer 默认归入 Accent；id=0 表示由 DB 自动分配。
//! - 置信度/证据量/一致性优先取 LLM confidence，缺失时按统计指标动态计算。
//! - 统计指标按 trait_label 前缀匹配分类，无匹配时回退中性默认值。

use ramaria_core::types::{PersonalityTrait, TraitLayer, TraitSource, TraitStatus, now_ms};

use crate::inference::inferrer::InferredTrait;
use crate::inference::stats::{CategoryStats, StatsSummary};

// =========================================================
// 类型转换
// =========================================================

/// 将 InferredTrait 转换为 PersonalityTrait。
///
/// 转换规则:
/// - `layer` 字符串映射到 `TraitLayer` 枚举。
/// - 无法识别的 layer 默认归入 Accent。
/// - id=0 表示由 DB 自动分配。
///
/// 置信度/证据量/一致性不再硬编码 0.5/1.0/0.5。
/// - 优先使用 LLM 推断的 confidence 值。
/// - 若 LLM 未提供 confidence，根据 trait_label 前缀匹配 stats 中
///   对应分类的 n_eff/valence_std/share_std 动态计算。
pub(in crate::inference::orchestrator) fn convert_to_personality_traits(
    inferred: &[InferredTrait],
    persona_uid: &str,
    stats: &StatsSummary,
) -> Vec<PersonalityTrait> {
    let now = now_ms();

    // 根据 n_eff 等统计指标动态计算 evidence/consistency/confidence
    let compute_evidence = |n_eff: f64| n_eff.clamp(0.0, 100.0);
    let compute_consistency = |valence_std: f64, share_std: f64| {
        let avg_std = (valence_std + share_std) / 2.0;
        (1.0 - avg_std).clamp(0.1, 0.95)
    };
    let compute_confidence = |evidence: f64, consistency: f64| {
        if evidence <= 0.0 {
            0.0
        } else {
            consistency * (1.0 - 1.0 / (1.0 + evidence))
        }
    };

    // 按 trait_label 前缀匹配 stats 中对应分类的统计指标
    let find_stats_for_label = |label: &str| -> Option<(&CategoryStats, f64, f64)> {
        stats
            .categories
            .iter()
            .find(|c| label.starts_with(&c.category))
            .map(|cs| {
                let ev = compute_evidence(cs.n_eff);
                let con = compute_consistency(cs.valence_std, cs.share_std);
                (cs, ev, con)
            })
    };

    inferred
        .iter()
        .map(|t| {
            let layer = match t.layer.as_str() {
                "base" => TraitLayer::Base,
                "primary" => TraitLayer::Primary,
                _ => TraitLayer::Accent,
            };

            // 置信度优先取 LLM 的推断值，无则用统计指标计算
            let (confidence, evidence, consistency) = if let Some(llm_conf) = t.confidence {
                // LLM 提供了置信度，用它；evidence/consistency 从 stats 匹配
                let (ev, con) = find_stats_for_label(&t.trait_label)
                    .map(|(_, ev, con)| (ev, con))
                    .unwrap_or((1.0, 0.5));
                tracing::debug!(
                    trait_label = %t.trait_label,
                    llm_conf,
                    computed_conf = ?compute_confidence(ev, con),
                    ev,
                    con,
                    "LLM 推断置信度已解析，evidence/consistency 由统计指标补全"
                );
                (llm_conf, ev, con)
            } else {
                // LLM 未提供置信度，全部由统计指标计算
                let (ev, con, conf) = find_stats_for_label(&t.trait_label)
                    .map(|(_, ev, con)| (ev, con, compute_confidence(ev, con)))
                    .unwrap_or((1.0, 0.5, 0.5));
                tracing::debug!(
                    trait_label = %t.trait_label,
                    ev,
                    con,
                    conf,
                    "LLM 未提供置信度，由统计指标动态计算"
                );
                (conf, ev, con)
            };

            PersonalityTrait {
                id: 0,
                persona_uid: persona_uid.to_string(),
                layer,
                trait_label: t.trait_label.clone(),
                meaning: t.meaning.clone(),
                not_meaning: t.not_meaning.clone(),
                trigger: t.trigger.clone(),
                suppress: t.suppress.clone(),
                related: t.related.clone(),
                seq: t.seq,
                source: TraitSource::Inferred,
                ref_event_id: None,
                ref_l1_id: None,
                confidence,
                evidence,
                consistency,
                status: TraitStatus::Active,
                created_at: now,
                updated_at: now,
            }
        })
        .collect()
}
