//! crates/ramaria-cli/src/commands/probe/report/pairing.rs - 探针 report 配对取样
//!
//! 设计特点:
//! - 按维度收集逐题分数（仅成功题），供消融对比配对
//! - 按 item_id 对齐两档位，差分为 ablated − base
//! - 只做配对与均值汇总，统计判定由评测层统计原语完成

use super::super::evaluate::VariantEvaluation;

/// 供消融配对的逐维度"item_id → 分数"索引。
pub(crate) type VariantDimScores = std::collections::HashMap<String, f64>;

/// 从评分数值档位提取某维度的逐题分数（仅成功题）。
///
/// 说明:
/// - tone 维 judge 分 1~5 直接作连续分使用；fact/emotion 维 0~1。
/// - `fact_norm` / `fact_point` 为事实维的重算口径，取自同一 fact 子评分的
///   `score_norm` / `score_point`；旧产物缺该字段的题不参与配对。
pub(crate) fn collect_variant_dim_scores(ev: &VariantEvaluation, dim: &str) -> VariantDimScores {
    let mut map = VariantDimScores::new();
    for item in &ev.items {
        if item.error.is_some() {
            continue;
        }
        let score = match dim {
            "fact" => item.fact.as_ref().map(|s| s.score),
            "fact_norm" => item.fact.as_ref().and_then(|s| s.score_norm),
            "fact_point" => item.fact.as_ref().and_then(|s| s.score_point),
            "tone" => item.tone.as_ref().map(|s| s.score as f64),
            "emotion" => item.emotion.as_ref().map(|s| s.score),
            _ => None,
        };
        if let Some(s) = score {
            map.insert(item.item_id.clone(), s);
        }
    }
    map
}

/// 按题目配对两个档位在某维度的差分样本与配对分数。
///
/// 配对规则: 仅取两端都成功评分的 item_id（同一题目），
/// `diffs = ablated − base`；两端任一缺失的题不参与配对。
/// 返回 (diffs, base_mean, ablated_mean, base_scores, ablated_scores)。
pub(crate) fn pair_dimension_diffs(
    ablated: &VariantDimScores,
    base: &VariantDimScores,
) -> (Vec<f64>, f64, f64, Vec<f64>, Vec<f64>) {
    let mut diffs = Vec::new();
    let mut base_scores = Vec::new();
    let mut ablated_scores = Vec::new();
    for (item_id, base_score) in base {
        if let Some(ablated_score) = ablated.get(item_id) {
            diffs.push(ablated_score - base_score);
            base_scores.push(*base_score);
            ablated_scores.push(*ablated_score);
        }
    }
    let n = diffs.len() as f64;
    if n == 0.0 {
        return (diffs, 0.0, 0.0, base_scores, ablated_scores);
    }
    let base_mean = base_scores.iter().sum::<f64>() / n;
    let ablated_mean = ablated_scores.iter().sum::<f64>() / n;
    (diffs, base_mean, ablated_mean, base_scores, ablated_scores)
}
