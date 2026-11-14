//! crates/ramaria-eval/src/report/stats.rs - Ramaria 评测报告配对统计
//!
//! 设计特点:
//! - 配对差值只取两侧都有有效 rubric 分的题，方向固定为 left - right
//! - 置信区间用固定种子的 SplitMix64 重采样，不引入随机依赖，同一输入结果不变
//! - 样本量不足时区间留 `None`，由调用方记缺项并降级，不报错也不填零
//! - 效应量按 d_z = 均值差 / 差值样本标准差；标准差为零时留 `None`
//! - 显著性检验复用统计原语的 Wilcoxon 符号秩实现

use std::collections::BTreeMap;

use crate::stats::wilcoxon_signed_rank_p;

/// bootstrap 重采样次数。
pub(crate) const BOOTSTRAP_RESAMPLES: usize = 2000;

/// bootstrap 固定种子。
const BOOTSTRAP_SEED: u64 = 0x5EED_2026;

/// 置信区间所需的最小样本量（配对差或单组观测）。
pub(crate) const MIN_CI_PAIRS: usize = 10;

/// 显著性阈值（校正后 p）。
pub(crate) const SIGNIFICANCE_LEVEL: f64 = 0.05;

/// 题号 → 有效 rubric 得分率。
pub(crate) type RatioMap = BTreeMap<String, f64>;

/// 一组配对差值的统计量。
///
/// 字段约定:
/// - `n`: 配对题数；
/// - `mean`: 均值差；无配对时为 `None`；
/// - `ci`: 95% 置信区间；少于 `MIN_CI_PAIRS` 对时为 `None`；
/// - `dz`: 配对 Cohen's d_z；样本不足或差值标准差为零时为 `None`；
/// - `p`: Wilcoxon 双尾 p 值；样本过小或无法近似时为 `None`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PairedStats {
    pub n: usize,
    pub mean: Option<f64>,
    pub ci: Option<(f64, f64)>,
    pub dz: Option<f64>,
    pub p: Option<f64>,
}

/// 按题号取交集后计算配对统计量。
///
/// 参数:
/// - `left` / `right`: 两侧的题号 → 得分率映射。
///
/// 返回:
/// - 差分为 `left - right`，按题号字典序累加，保证同一输入结果一致。
pub(crate) fn paired(left: &RatioMap, right: &RatioMap) -> PairedStats {
    let diffs: Vec<f64> = left
        .iter()
        .filter_map(|(question, value)| right.get(question).map(|other| value - other))
        .collect();
    let n = diffs.len();
    let mean = (n > 0).then(|| diffs.iter().sum::<f64>() / n as f64);
    PairedStats {
        n,
        mean,
        ci: bootstrap_ci(&diffs),
        dz: paired_dz(&diffs),
        p: wilcoxon_signed_rank_p(&diffs),
    }
}

/// 配对 Cohen's d_z（样本标准差口径）。
///
/// 返回:
/// - 少于 2 对或差值标准差近似为零时为 `None`（与序列化的空值语义一致）。
fn paired_dz(diffs: &[f64]) -> Option<f64> {
    let n = diffs.len();
    if n < 2 {
        return None;
    }
    let mean = diffs.iter().sum::<f64>() / n as f64;
    let variance = diffs.iter().map(|d| (d - mean) * (d - mean)).sum::<f64>() / (n as f64 - 1.0);
    let sd = variance.sqrt();
    if sd < 1e-12 {
        return None;
    }
    Some(mean / sd)
}

/// 一组样本均值的 95% bootstrap 置信区间（百分位法）。
///
/// 参数:
/// - `values`: 样本值（配对差值或单组观测）。
///
/// 返回:
/// - 少于 `MIN_CI_PAIRS` 个样本时为 `None`；
/// - 其余情况返回 2.5 / 97.5 百分位，线性插值取分位点。
pub(crate) fn bootstrap_ci(values: &[f64]) -> Option<(f64, f64)> {
    let n = values.len();
    if n < MIN_CI_PAIRS {
        return None;
    }
    let mut rng = SplitMix64::new(BOOTSTRAP_SEED);
    let mut means = Vec::with_capacity(BOOTSTRAP_RESAMPLES);
    for _ in 0..BOOTSTRAP_RESAMPLES {
        let mut sum = 0.0;
        for _ in 0..n {
            sum += values[rng.below(n)];
        }
        means.push(sum / n as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some((quantile(&means, 0.025), quantile(&means, 0.975)))
}

/// 已排序样本的分位点（线性插值）。
fn quantile(sorted: &[f64], q: f64) -> f64 {
    let position = q * (sorted.len() - 1) as f64;
    let low = position.floor() as usize;
    let high = position.ceil() as usize;
    if low == high {
        sorted[low]
    } else {
        sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64)
    }
}

/// 线性同余之外的确定性伪随机源（SplitMix64）。
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// 由固定种子构造。
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// 生成下一个 64 位随机数。
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 返回 `[0, bound)` 内的下标（模偏差可忽略，取样仅用于聚合均值）。
    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已知差值序列的均值与 d_z 与手算一致。
    #[test]
    fn paired_mean_and_dz_match_hand_calculation() {
        let left: RatioMap = [("q1", 0.6), ("q2", 0.8), ("q3", 0.4)]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        let right: RatioMap = [("q1", 0.5), ("q2", 0.5), ("q3", 0.3)]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        let stats = paired(&left, &right);
        assert_eq!(stats.n, 3);
        assert!((stats.mean.expect("应有均值") - 0.166_666_666_666_666_63).abs() < 1e-12);
        let dz = stats.dz.expect("应有效应量");
        assert!((dz - 1.443_375_672_920_385_3).abs() < 1e-9, "dz 实际 {dz}");
        assert!(stats.ci.is_none(), "少于 10 对不产出置信区间");
        assert!(stats.p.is_none(), "少于 5 对不产出 p 值");
    }

    /// 题号只在两侧都存在时配对；差值为零时效应量与 p 均留空。
    #[test]
    fn pairing_uses_intersection_only() {
        let left: RatioMap = [("q1", 0.5), ("q2", 0.5), ("q3", 0.9)]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        let right: RatioMap = [("q1", 0.5), ("q2", 0.5)]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        let stats = paired(&left, &right);
        assert_eq!(stats.n, 2, "q3 只有一侧有分，不参与配对");
        assert_eq!(stats.mean, Some(0.0));
        assert_eq!(stats.dz, None);
        assert_eq!(stats.p, None);
    }

    /// 同一输入两次 bootstrap 结果一致，且区间覆盖均值。
    #[test]
    fn bootstrap_is_deterministic_and_contains_mean() {
        let diffs: Vec<f64> = (0..12).map(|index| index as f64 * 0.01).collect();
        let first = bootstrap_ci(&diffs).expect("12 对应有区间");
        let second = bootstrap_ci(&diffs).expect("12 对应有区间");
        assert_eq!(first, second, "固定种子应可复现");
        let mean = diffs.iter().sum::<f64>() / diffs.len() as f64;
        assert!(first.0 <= mean && mean <= first.1, "区间应覆盖均值");
        assert!(first.0 < first.1);
    }
}
