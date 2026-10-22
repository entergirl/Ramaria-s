//! crates/ramaria-memory/src/inference/shrink.rs - 经验贝叶斯小样本收缩
//!
//! 设计特点:
//! - A5 小样本收缩估计: 当分类有效样本量 n_eff 过小时，将极端估计值向全局均值收缩
//! - 分层先验: Base/Primary 使用跨领域全局先验，Accent 使用领域/主题簇先验
//! - Valence: 标准经验贝叶斯收缩（无界连续量，对称分布）
//! - Share: logit 变换 → 收缩 → sigmoid（有界 [0,1]）
//! - Presentation: Dirichlet-Multinomial 共轭（三比例和为 1 的组合数据）
//! - γ 动态公式: γ = 3 + 30 / max(n_total_eff, 30)，随总样本量自适应调整
//! - 纯数值计算，零 I/O，不依赖数据库或异步运行时
//! - 现役版本为分层先验 `run_shrinkage_layered()`（旧 `run_shrinkage()` 已删除）

use crate::inference::stats::CategoryStats;

// =========================================================
// 配置类型
// =========================================================

/// 经验贝叶斯收缩配置。
///
/// 职责:
/// - 管理收缩强度参数 γ 的动态计算相关常量。
///
/// 字段约定:
/// - `gamma_base`: γ 公式中的基础偏移量，默认 3。
/// - `gamma_scale`: γ 公式中的缩放因子，默认 30。
/// - `gamma_min_eff`: γ 公式中 max(n_total_eff, gamma_min_eff) 的保底值，默认 30。
#[derive(Debug, Clone)]
pub struct ShrinkConfig {
    /// γ 公式基础偏移量
    pub gamma_base: f64,
    /// γ 公式缩放因子
    pub gamma_scale: f64,
    /// 总有效样本量的保底值
    pub gamma_min_eff: f64,
}

impl Default for ShrinkConfig {
    fn default() -> Self {
        Self {
            gamma_base: 3.0,
            gamma_scale: 30.0,
            gamma_min_eff: 30.0,
        }
    }
}

// =========================================================
// γ 动态计算
// =========================================================

/// 计算动态平滑参数 γ。
///
/// 公式: γ = gamma_base + gamma_scale / max(n_total_eff, gamma_min_eff)
///
/// 说明:
/// - n_total_eff 很小时（如 < 30），γ 较大（更保守收缩），因为全局均值本身也不可靠。
/// - n_total_eff 很大时，γ 趋近 gamma_base=3，此时全局均值可靠，收缩减弱。
///
/// 参数:
/// - `n_total_eff`: 全部事件的 salience 加权有效样本量。
/// - `config`: 收缩配置。
///
/// 返回:
/// - 平滑参数 γ。
pub fn compute_dynamic_gamma(n_total_eff: f64, config: &ShrinkConfig) -> f64 {
    let clamped = n_total_eff.max(config.gamma_min_eff);
    config.gamma_base + config.gamma_scale / clamped
}

// =========================================================
// Valence 收缩（标准经验贝叶斯）
// =========================================================

/// 对 valence 均值执行经验贝叶斯收缩。
///
/// 公式: μ_shrunk = (n_eff / (n_eff + γ)) · x̄_w + (γ / (n_eff + γ)) · μ_prior
///
/// 其中:
/// - n_eff: 该分类的 salience 加权有效样本量。
/// - x̄_w: 该分类的加权均值。
/// - μ_prior: 全局加权均值（先验）。
/// - γ: 平滑强度参数。
///
/// 行为:
/// - n_eff → ∞ : μ_shrunk → x̄_w（完全信任数据）。
/// - n_eff → 0 : μ_shrunk → μ_prior（完全信任先验）。
///
/// 参数:
/// - `category_mean`: 该分类的加权均值。
/// - `category_n_eff`: 该分类的有效样本量。
/// - `global_mean`: 全局加权均值（先验）。
/// - `gamma`: 平滑参数。
///
/// 返回:
/// - 收缩后的均值估计。
pub fn shrink_valence(
    category_mean: f64,
    category_n_eff: f64,
    global_mean: f64,
    gamma: f64,
) -> f64 {
    if category_n_eff + gamma < 1e-12 {
        return global_mean;
    }
    let weight_data = category_n_eff / (category_n_eff + gamma);
    let weight_prior = gamma / (category_n_eff + gamma);
    weight_data * category_mean + weight_prior * global_mean
}

// =========================================================
// Share 收缩（logit 变换）
// =========================================================

/// 将 [0, 1] 有界值通过 logit 变换转为无界连续量。
///
/// 公式: logit(p) = ln(p / (1 - p))
///
/// 说明:
/// - 对边界值做温和处理：p → max(p, ε), p → min(p, 1-ε)，其中 ε = 1e-8。
/// - 避免 ln(0) 和除零错误。
///
/// 参数:
/// - `p`: 原始概率值 0.0..1.0。
///
/// 返回:
/// - logit 变换后的值。
pub fn logit(p: f64) -> f64 {
    let p_clamped = p.clamp(1e-8, 1.0 - 1e-8);
    (p_clamped / (1.0 - p_clamped)).ln()
}

/// 将 logit 值通过 sigmoid（逆 logit）映射回 [0, 1]。
///
/// 公式: sigmoid(x) = 1 / (1 + e^(-x))
///
/// 参数:
/// - `x`: logit 空间中的值。
///
/// 返回:
/// - [0, 1] 范围内的概率值。
pub fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// 对 share 均值执行经验贝叶斯收缩。
///
/// 流程:
/// 1. 将 [0,1] 有界值通过 logit 映射到无界空间。
/// 2. 在无界空间执行标准经验贝叶斯收缩。
/// 3. 通过 sigmoid 映射回 [0,1]。
///
/// 参数:
/// - `category_share_mean`: 该分类的 share 加权均值（[0,1]）。
/// - `category_n_eff`: 该分类的有效样本量。
/// - `global_share_mean`: 全局 share 加权均值（[0,1]）。
/// - `gamma`: 平滑参数。
///
/// 返回:
/// - 收缩后的 share 均值（[0,1]）。
pub fn shrink_share(
    category_share_mean: f64,
    category_n_eff: f64,
    global_share_mean: f64,
    gamma: f64,
) -> f64 {
    let cat_logit = logit(category_share_mean);
    let global_logit = logit(global_share_mean);
    let shrunk_logit = shrink_valence(cat_logit, category_n_eff, global_logit, gamma);
    sigmoid(shrunk_logit)
}

// =========================================================
// Presentation 收缩（Dirichlet-Multinomial 共轭）
// =========================================================

/// 对 presentation 分布执行 Dirichlet-Multinomial 收缩。
///
/// 原理:
/// - 三种 presentation 比例 (objective, subjective, mixed) 和为 1，属于组合数据。
/// - Dirichlet 先验的伪计数来自全局 presentation 分布乘以 γ。
/// - 收缩等价于在各观测计数上加上先验伪计数，自然保持和为 1。
///
/// 公式:
/// - α_k = global_ratio_k · γ + 1（+1 是拉普拉斯平滑，避免零概率）
/// - shrunken_ratio_k = (category_ratio_k · n_eff + α_k - 1) / (n_eff + Σα_k - 3)
///
/// 参数:
/// - `cat_obj / cat_sub / cat_mix`: 该分类的三种 presentation 加权占比，和为 1。
/// - `category_n_eff`: 该分类的有效样本量。
/// - `global_obj / global_sub / global_mix`: 全局 presentation 加权占比，和为 1。
/// - `gamma`: 平滑参数（作为先验强度）。
///
/// 返回:
/// - 收缩后的 (objective_ratio, subjective_ratio, mixed_ratio)，和为 1。
#[allow(clippy::too_many_arguments)]
pub fn shrink_presentation(
    cat_obj: f64,
    cat_sub: f64,
    cat_mix: f64,
    category_n_eff: f64,
    global_obj: f64,
    global_sub: f64,
    global_mix: f64,
    gamma: f64,
) -> (f64, f64, f64) {
    // 先验伪计数: α_k = global_ratio_k · γ + 1
    let alpha_obj = global_obj * gamma + 1.0;
    let alpha_sub = global_sub * gamma + 1.0;
    let alpha_mix = global_mix * gamma + 1.0;

    // 观测计数（按比例反推，category_n_eff 为总"计数"）
    let obs_obj = cat_obj * category_n_eff;
    let obs_sub = cat_sub * category_n_eff;
    let obs_mix = cat_mix * category_n_eff;

    // 后验伪计数 = 观测计数 + 先验伪计数 - 1
    let post_obj = obs_obj + alpha_obj - 1.0;
    let post_sub = obs_sub + alpha_sub - 1.0;
    let post_mix = obs_mix + alpha_mix - 1.0;

    let total = post_obj + post_sub + post_mix;
    if total < 1e-12 {
        // 极端情况：返回全局先验
        return (global_obj, global_sub, global_mix);
    }

    let shrunk_obj = post_obj / total;
    let shrunk_sub = post_sub / total;
    let shrunk_mix = post_mix / total;

    (shrunk_obj, shrunk_sub, shrunk_mix)
}

// =========================================================
// 批量收缩
// =========================================================

/// 对单个分类统计执行全部指标的收缩。
///
/// 参数:
/// - `cat`: 待收缩的分类统计（可变引用，in-place 更新）。
/// - `global_valence_mean`: 全局 valence 加权均值。
/// - `global_share_mean`: 全局 share 加权均值。
/// - `global_obj/sub/mix`: 全局 presentation 加权占比。
/// - `gamma`: 平滑参数。
pub fn shrink_category(
    cat: &mut CategoryStats,
    global_valence_mean: f64,
    global_share_mean: f64,
    global_obj: f64,
    global_sub: f64,
    global_mix: f64,
    gamma: f64,
) {
    cat.valence_mean = shrink_valence(cat.valence_mean, cat.n_eff, global_valence_mean, gamma);
    cat.share_mean = shrink_share(cat.share_mean, cat.n_eff, global_share_mean, gamma);
    let (so, ss, sm) = shrink_presentation(
        cat.presentation_objective_ratio,
        cat.presentation_subjective_ratio,
        cat.presentation_mixed_ratio,
        cat.n_eff,
        global_obj,
        global_sub,
        global_mix,
        gamma,
    );
    cat.presentation_objective_ratio = so;
    cat.presentation_subjective_ratio = ss;
    cat.presentation_mixed_ratio = sm;
}

/// 计算所有分类的全局统计量。
///
/// 参数:
/// - `categories`: 所有分类统计。
///
/// 返回:
/// - (global_valence_mean, global_share_mean, global_obj_ratio, global_sub_ratio, global_mix_ratio, n_total_eff)
pub fn compute_global_stats(categories: &[CategoryStats]) -> (f64, f64, f64, f64, f64, f64) {
    let n_total_eff: f64 = categories.iter().map(|c| c.n_eff).sum();

    if n_total_eff < 1e-12 {
        return (0.0, 0.5, 1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 0.0);
    }

    // 全局加权均值 (= Σ(n_eff_i · mean_i) / Σ n_eff_i)
    let global_valence_mean: f64 = categories
        .iter()
        .map(|c| c.n_eff * c.valence_mean)
        .sum::<f64>()
        / n_total_eff;
    let global_share_mean: f64 = categories
        .iter()
        .map(|c| c.n_eff * c.share_mean)
        .sum::<f64>()
        / n_total_eff;
    let global_obj: f64 = categories
        .iter()
        .map(|c| c.n_eff * c.presentation_objective_ratio)
        .sum::<f64>()
        / n_total_eff;
    let global_sub: f64 = categories
        .iter()
        .map(|c| c.n_eff * c.presentation_subjective_ratio)
        .sum::<f64>()
        / n_total_eff;
    let global_mix: f64 = categories
        .iter()
        .map(|c| c.n_eff * c.presentation_mixed_ratio)
        .sum::<f64>()
        / n_total_eff;

    (
        global_valence_mean,
        global_share_mean,
        global_obj,
        global_sub,
        global_mix,
        n_total_eff,
    )
}
mod prior;
pub use prior::*;

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
