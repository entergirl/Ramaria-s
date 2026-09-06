//! crates/ramaria-memory/src/inference/stats/weighted.rs - Ramaria 通用加权统计原语模块
//!
//! 设计特点:
//! - 加权均值: x̄_w = Σ(w_i·x_i) / Σ w_i。
//! - 加权总体方差: σ²_w = Σ(w_i·(x_i−x̄_w)²) / Σ w_i。
//! - 加权占比: ratio = Σ(indicator_i·w_i) / Σ w_i。
//! - 纯数值 f64 计算；总权重为 0 时安全返回 0.0，供各统计子模块共享。

/// 计算加权均值。
///
/// 公式: x̄_w = Σ(w_i · x_i) / Σ w_i
///
/// 参数:
/// - `values`: 各事件的指标取值。
/// - `weights`: 各事件的权重（需与 values 一一对应）。
///
/// 返回:
/// - 加权均值。若总权重为 0，返回 0.0。
pub fn weighted_mean(values: &[f64], weights: &[f64]) -> f64 {
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let weighted_sum: f64 = values.iter().zip(weights).map(|(v, w)| v * w).sum();
    weighted_sum / total_weight
}

/// 计算加权方差（总体方差，非样本方差）。
///
/// 公式: σ²_w = Σ(w_i · (x_i - x̄_w)²) / Σ w_i
///
/// 参数:
/// - `values`: 各事件的指标取值。
/// - `weights`: 各事件的权重。
/// - `mean`: 已计算的加权均值。
///
/// 返回:
/// - 加权方差。若总权重为 0 或仅 1 个有效样本，返回 0.0。
pub fn weighted_variance(values: &[f64], weights: &[f64], mean: f64) -> f64 {
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let weighted_sq_diff: f64 = values
        .iter()
        .zip(weights)
        .map(|(v, w)| w * (v - mean).powi(2))
        .sum();
    weighted_sq_diff / total_weight
}

/// 计算加权占比（用于正面事件比例和 presentation 分布）。
///
/// 公式: ratio = Σ(indicator_i · w_i) / Σ w_i
///
/// 参数:
/// - `indicators`: 各事件的指示器值（0.0 或 1.0）。
/// - `weights`: 各事件的权重。
///
/// 返回:
/// - 加权占比。若总权重为 0，返回 0.0。
pub fn weighted_ratio(indicators: &[f64], weights: &[f64]) -> f64 {
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let weighted_sum: f64 = indicators.iter().zip(weights).map(|(i, w)| i * w).sum();
    weighted_sum / total_weight
}
