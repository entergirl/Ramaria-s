//! crates/ramaria-cli/src/commands/probe/report/stats.rs - 探针 report 配对非参与效应量统计
//!
//! 设计特点:
//! - Wilcoxon 符号秩 / Cohen's d（配对与合并 SD）/ 正态 CDF / erf 近似 / BH-FDR 校正
//! - 按维度收集配对差分，供消融对比与 TOST 复用
//! - 纯函数实现，无运行期状态，便于单元测试

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

/// 合并标准差标准化的 Cohen's d（d_av）。
///
/// 说明:
/// - `sd_av = sqrt((var_base + var_ablated) / 2)`（样本方差，n≥2）。
/// - 该口径反映"两个条件各自的离散度"，是等效边界的自然标尺；
///   显著性判定仍用配对 d_z（`cohens_d_paired`），两者语义不同、不可互替。
/// - 样本不足（n<2）或合并 SD 为 0 时返回 0.0。
pub(crate) fn cohens_d_pooled(base: &[f64], ablated: &[f64]) -> f64 {
    let n = base.len().min(ablated.len());
    if n < 2 {
        return 0.0;
    }
    let mean = |xs: &[f64]| xs.iter().take(n).sum::<f64>() / n as f64;
    let var = |xs: &[f64]| {
        let m = mean(xs);
        xs.iter().take(n).map(|x| (x - m) * (x - m)).sum::<f64>() / (n as f64 - 1.0)
    };
    let sd_av = ((var(base) + var(ablated)) / 2.0).sqrt();
    if sd_av < 1e-12 {
        return 0.0;
    }
    let diff_mean = ablated
        .iter()
        .take(n)
        .zip(base.iter().take(n))
        .map(|(a, b)| a - b)
        .sum::<f64>()
        / n as f64;
    diff_mean / sd_av
}

/// 配对 Wilcoxon 符号秩检验双尾 p 值（正态近似，无零差分）。
///
/// 算法:
/// - 剔除零差分后取绝对值排序，相同绝对值取平均秩；
/// - W+ = 正差分秩和；W 均值/方差（不含结校正的近似）→ z → 双尾 p。
/// - 样本量过小（n<5）时近似偏保守/不可靠，返回 `None`（调用方按 p=1.0 处理）。
pub(crate) fn wilcoxon_signed_rank_p(diffs: &[f64]) -> Option<f64> {
    // 剔除零差分
    let mut abs_pairs: Vec<(f64, bool)> = diffs
        .iter()
        .filter(|d| d.abs() > 1e-12)
        .map(|d| (d.abs(), *d > 0.0))
        .collect();
    let n = abs_pairs.len();
    if n < 5 {
        return None; // 样本过小，正态近似不可靠
    }
    abs_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // 平均秩（处理相同绝对值）
    let mut w_plus = 0.0f64;
    let mut i = 0usize;
    while i < n {
        let mut j = i;
        while j + 1 < n && (abs_pairs[j + 1].0 - abs_pairs[i].0).abs() < 1e-12 {
            j += 1;
        }
        let rank_avg = (i + j + 2) as f64 / 2.0; // 1-based 位置平均
        for pair in &abs_pairs[i..=j] {
            if pair.1 {
                w_plus += rank_avg;
            }
        }
        i = j + 1;
    }

    // 无结近似：mean = n(n+1)/4，var = n(n+1)(2n+1)/24
    let n_f = n as f64;
    let mean = n_f * (n_f + 1.0) / 4.0;
    let variance = n_f * (n_f + 1.0) * (2.0 * n_f + 1.0) / 24.0;
    if variance <= 0.0 {
        return None;
    }
    let z = (w_plus - mean) / variance.sqrt();
    Some(2.0 * (1.0 - normal_cdf(z.abs())))
}

/// 标准正态分布 CDF（erf 近似）。
pub(crate) fn normal_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf_approx(z / std::f64::consts::SQRT_2))
}

/// erf 近似（Abramowitz–Stegun 7.1.26，最大误差 ~1.5e-7）。
pub(crate) fn erf_approx(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    if x > 6.0 {
        return sign;
    }
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    sign * (1.0 - poly * (-x * x).exp())
}

/// 配对 Cohen's d（d_z = mean(diff) / sd(diff)）。
///
/// 说明: 差分为零（sd≈0）且均值非零时以 ±10 标记"远超效应量阈值"
/// （避免 inf 破坏判定与序列化）；均值亦为零 → 0.0。
pub(crate) fn cohens_d_paired(diffs: &[f64]) -> f64 {
    let n = diffs.len();
    if n == 0 {
        return 0.0;
    }
    let n_f = n as f64;
    let mean = diffs.iter().sum::<f64>() / n_f;
    if n == 1 {
        return if mean.abs() < 1e-12 {
            0.0
        } else {
            mean.signum() * 10.0
        };
    }
    let variance = diffs.iter().map(|d| (d - mean) * (d - mean)).sum::<f64>() / (n_f - 1.0);
    let sd = variance.sqrt();
    if sd < 1e-12 {
        if mean.abs() < 1e-12 {
            0.0
        } else {
            mean.signum() * 10.0
        }
    } else {
        mean / sd
    }
}

/// Benjamini–Hochberg FDR 校正。
///
/// 返回与输入等长的校正后 q 值；空输入返回空。
pub(crate) fn bh_fdr_adjust(p_values: &[f64]) -> Vec<f64> {
    let m = p_values.len();
    if m == 0 {
        return Vec::new();
    }
    // 索引排序（小 → 大）
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|a, b| {
        p_values[*a]
            .partial_cmp(&p_values[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut q = vec![1.0f64; m];
    // 从最大 p 反向累计取最小
    let mut running_min = f64::INFINITY;
    for (rank_idx, &orig_idx) in order.iter().enumerate().rev() {
        let raw = p_values[orig_idx];
        let adjusted = (raw * m as f64 / (rank_idx + 1) as f64).min(1.0);
        running_min = running_min.min(adjusted);
        q[orig_idx] = running_min;
    }
    q
}
