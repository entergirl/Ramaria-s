//! rust/crates/ramaria-memory/src/inference/shrink.rs - 经验贝叶斯小样本收缩
//!
//! 设计特点:
//! - A5 小样本收缩估计: 当分类有效样本量 n_eff 过小时，将极端估计值向全局均值收缩
//! - 分层先验: Base/Primary 使用跨领域全局先验，Accent 使用领域/主题簇先验
//! - Valence: 标准经验贝叶斯收缩（无界连续量，对称分布）
//! - Share: logit 变换 → 收缩 → sigmoid（有界 [0,1]）
//! - Presentation: Dirichlet-Multinomial 共轭（三比例和为 1 的组合数据）
//! - γ 动态公式: γ = 3 + 30 / max(n_total_eff, 30)，随总样本量自适应调整
//! - 纯数值计算，零 I/O，不依赖数据库或异步运行时
//! - 向后兼容: `run_shrinkage()` 保留，`run_shrinkage_layered()` 新增

use std::collections::HashMap;

use ramaria_core::types::TraitLayer;

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

/// 执行完整的经验贝叶斯收缩管线。
///
/// 流程:
/// 1. 计算全局加权统计量。
/// 2. 基于 n_total_eff 动态计算 γ。
/// 3. 逐分类执行收缩（valence/logit-share/Dirichlet-presentation）。
///
/// 参数:
/// - `categories`: 所有分类统计（可变引用，in-place 更新）。
/// - `shrink_config`: 收缩配置。
///
/// 返回:
/// - 使用的 γ 值（供日志记录）。
pub fn run_shrinkage(categories: &mut [CategoryStats], shrink_config: &ShrinkConfig) -> f64 {
    let (g_val_mean, g_share_mean, g_obj, g_sub, g_mix, n_total_eff) =
        compute_global_stats(categories);

    let gamma = compute_dynamic_gamma(n_total_eff, shrink_config);

    for cat in categories.iter_mut() {
        shrink_category(cat, g_val_mean, g_share_mean, g_obj, g_sub, g_mix, gamma);
    }

    gamma
}

// =========================================================
// 分层先验收缩
// =========================================================

/// 收缩先验值包（五个先验指标聚合）。
///
/// 职责:
/// - 将原先分散传递的 5 个全局先验值聚合为一个类型。
/// - 支持从 CategoryStats 切片计算先验（全局或领域）。
#[derive(Debug, Clone)]
pub struct ShrinkPrior {
    /// 全局/领域 valence 均值
    pub valence_mean: f64,
    /// 全局/领域 share 均值
    pub share_mean: f64,
    /// 全局/领域 objective 占比
    pub obj_ratio: f64,
    /// 全局/领域 subjective 占比
    pub sub_ratio: f64,
    /// 全局/领域 mixed 占比
    pub mix_ratio: f64,
    /// 用于计算该先验的有效样本量
    pub n_total_eff: f64,
}

impl ShrinkPrior {
    /// 从分类统计切片计算先验。
    ///
    /// 参数:
    /// - `categories`: 参与先验计算的分类统计列表。
    ///
    /// 返回:
    /// - 若 categories 为空，返回中性先验。
    fn from_categories(categories: &[CategoryStats]) -> Self {
        let (gv, gs, go, gsu, gm, n_total) = compute_global_stats(categories);
        Self {
            valence_mean: gv,
            share_mean: gs,
            obj_ratio: go,
            sub_ratio: gsu,
            mix_ratio: gm,
            n_total_eff: n_total,
        }
    }
}

/// 根据 trait_layer 选择应使用的先验值。
///
/// 规则:
/// - `Base` / `Primary`: 始终使用全局先验（跨领域稳定的人格特质）。
/// - `Accent`: 使用领域先验。若领域先验不可用（如首次推断、领域样本不足），则 fallback 到全局先验。
///
/// 说明:
/// - 本函数仅选择单个指标的先验值。完整先验包选择由 `select_shrink_prior` 完成。
///
/// 参数:
/// - `trait_layer`: 人格特质层级。
/// - `global_prior`: 跨领域全局先验值。
/// - `domain_prior`: 领域内先验值（可选，Accent 时使用）。
///
/// 返回:
/// - 选定的先验值。
pub fn select_prior(trait_layer: &TraitLayer, global_prior: f64, domain_prior: Option<f64>) -> f64 {
    match trait_layer {
        TraitLayer::Base | TraitLayer::Primary => global_prior,
        TraitLayer::Accent => domain_prior.unwrap_or(global_prior),
        _ => global_prior, // 未知 layer 保守使用全局先验
    }
}

/// 根据 trait_layer 选择完整的收缩先验包。
///
/// 参数:
/// - `trait_layer`: 人格特质层级。
/// - `global_prior`: 跨领域全局先验包。
/// - `domain_prior`: 领域内先验包（可选，Accent 时使用）。
///
/// 返回:
/// - 选定的先验包引用。
fn select_shrink_prior<'a>(
    trait_layer: &TraitLayer,
    global_prior: &'a ShrinkPrior,
    domain_prior: &'a Option<ShrinkPrior>,
) -> &'a ShrinkPrior {
    match trait_layer {
        TraitLayer::Base | TraitLayer::Primary => global_prior,
        TraitLayer::Accent => domain_prior.as_ref().unwrap_or(global_prior),
        _ => global_prior,
    }
}

/// 对指定子集的分类计算领域先验。
///
/// 策略:
/// - 从 category_indices 指定的分类子集中计算加权先验。
/// - 若子集为空或总 n_eff 过低（< 1.0），返回 None 表示领域先验不可靠。
///
/// 参数:
/// - `categories`: 所有分类统计。
/// - `category_indices`: 属于该领域的分类索引列表。
///
/// 返回:
/// - `Some(ShrinkPrior)` 若领域有足够样本，`None` 表示应 fallback 全局先验。
pub fn compute_domain_prior(
    categories: &[CategoryStats],
    category_indices: &[usize],
) -> Option<ShrinkPrior> {
    if category_indices.is_empty() {
        return None;
    }

    let domain_cats: Vec<CategoryStats> = category_indices
        .iter()
        .filter_map(|&idx| categories.get(idx).cloned())
        .collect();

    if domain_cats.is_empty() {
        return None;
    }

    let prior = ShrinkPrior::from_categories(&domain_cats);

    // 领域样本量过小（< 1.0）时先验不可靠，建议 fallback
    if prior.n_total_eff < 1.0 {
        return None;
    }

    Some(prior)
}

/// 执行分层经验贝叶斯收缩管线。
///
/// 流程:
/// 1. 计算全局先验（来自所有分类）。
/// 2. 根据 layer_hints 识别 Accent 分类，计算领域先验（来自 Accent 分类子集）。
/// 3. 对每个分类：
///    - 查 layer_hints 获取该分类的预期 TraitLayer。
///    - Base/Primary → 使用全局先验。
///    - Accent → 使用领域先验（不可用时 fallback 全局先验）。
///    - 未在 hints 中的分类 → 使用全局先验（保守策略）。
/// 4. 执行标准收缩公式。
///
/// 参数:
/// - `categories`: 所有分类统计（可变引用，in-place 更新）。
/// - `shrink_config`: 收缩配置（γ 参数）。
/// - `layer_hints`: 分类名 → TraitLayer 的映射（来自上一轮 Phase B 的持久化结果）。
///   首次推断时传入空 HashMap，此时所有分类使用全局先验。
///
/// 返回:
/// - 使用的 γ 值（供日志记录）。
///
/// 说明:
/// - 当至少 2 个分类标记为 Accent 时才计算领域先验；单个 Accent 分类 fallback 全局先验。
/// - 与 `run_shrinkage()` 的差异仅在于先验来源，收缩公式完全一致。
pub fn run_shrinkage_layered(
    categories: &mut [CategoryStats],
    shrink_config: &ShrinkConfig,
    layer_hints: &HashMap<String, TraitLayer>,
) -> f64 {
    // Step 1: 计算全局先验（来自所有分类）
    let global_prior = ShrinkPrior::from_categories(categories);

    // Step 2: 识别 Accent 分类索引并计算领域先验
    let accent_indices: Vec<usize> = categories
        .iter()
        .enumerate()
        .filter(|(_, cat)| {
            layer_hints
                .get(&cat.category)
                .map(|layer| matches!(layer, TraitLayer::Accent))
                .unwrap_or(false)
        })
        .map(|(i, _)| i)
        .collect();

    let domain_prior: Option<ShrinkPrior> = if accent_indices.len() >= 2 {
        compute_domain_prior(categories, &accent_indices)
    } else {
        None // 不足 2 个 Accent 分类时，领域先验不可靠
    };

    // Step 3: 动态 γ（基于全局 n_total_eff）
    let gamma = compute_dynamic_gamma(global_prior.n_total_eff, shrink_config);

    // Step 4: 逐分类收缩
    for cat in categories.iter_mut() {
        let layer = layer_hints.get(&cat.category);
        let prior = match layer {
            Some(l) => select_shrink_prior(l, &global_prior, &domain_prior),
            None => &global_prior, // 无 hint 时保守使用全局先验
        };

        shrink_category(
            cat,
            prior.valence_mean,
            prior.share_mean,
            prior.obj_ratio,
            prior.sub_ratio,
            prior.mix_ratio,
            gamma,
        );
    }

    gamma
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- γ 动态计算 ----

    /// compute_dynamic_gamma 各样本量参数化验证。
    #[test]
    fn gamma_cases() {
        let config = ShrinkConfig::default();
        let cases = [
            (10.0, 4.0),       // γ = 3 + 30/30（max(10,30)=30）
            (300.0, 3.1),      // γ = 3 + 30/300
            (10_000.0, 3.003), // γ = 3 + 30/10000
        ];
        for (n, expected) in cases {
            let gamma = compute_dynamic_gamma(n, &config);
            assert!((gamma - expected).abs() < 0.001, "n={n}");
        }
    }

    // ---- Valence 收缩 ----

    /// shrink_valence 各 n_eff 参数化验证。
    #[test]
    fn shrink_valence_cases() {
        let cases = [
            (100.0, 0.777), // n_eff 很大 → 接近原始值
            (1.0, 0.32),    // n_eff 很小 → 接近全局均值
            (0.0, 0.2),     // 完全依赖先验
        ];
        for (n_eff, expected) in cases {
            let result = shrink_valence(0.8, n_eff, 0.2, 4.0);
            assert!((result - expected).abs() < 0.01, "n_eff={n_eff}");
        }
    }

    // ---- Logit / Sigmoid ----

    #[test]
    fn logit_sigmoid_roundtrip() {
        for &p in &[0.1, 0.3, 0.5, 0.7, 0.9] {
            let l = logit(p);
            let s = sigmoid(l);
            assert!((s - p).abs() < 1e-6, "roundtrip failed for p={}", p);
        }
    }

    #[test]
    fn logit_boundaries() {
        // 边界值不应 panic
        let l0 = logit(0.0);
        let l1 = logit(1.0);
        // 应返回有效值
        assert!(l0.is_finite());
        assert!(l1.is_finite());
        // sigmoid 应在 [0,1]
        let s0 = sigmoid(l0);
        assert!((0.0..=1.0).contains(&s0));
    }

    #[test]
    fn sigmoid_center() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-10);
    }

    // ---- Share 收缩 ----

    #[test]
    fn shrink_share_basic() {
        // 分类 share=0.9, n_eff=2, 全局 share=0.5, γ=4
        // cat_logit = ln(0.9/0.1) ≈ 2.197
        // global_logit = ln(0.5/0.5) = 0.0
        // shrunk_logit = (2/6)*2.197 + (4/6)*0.0 ≈ 0.732
        // sigmoid(0.732) ≈ 0.675
        let result = shrink_share(0.9, 2.0, 0.5, 4.0);
        assert!((result - 0.675).abs() < 0.01);
    }

    // ---- Presentation 收缩 ----

    #[test]
    fn shrink_presentation_sum_to_one() {
        let (o, s, m) = shrink_presentation(0.6, 0.3, 0.1, 2.0, 0.33, 0.33, 0.34, 4.0);
        let total = o + s + m;
        assert!(
            (total - 1.0).abs() < 1e-10,
            "收缩后 presentation 比例和应为1，实际={}",
            total
        );
    }

    #[test]
    fn shrink_presentation_small_n_eff() {
        // 小样本时收缩向全局先验靠近
        let (o, s, m) = shrink_presentation(1.0, 0.0, 0.0, 1.0, 0.33, 0.33, 0.34, 4.0);
        // 收缩后不应再是 (1,0,0)，而应更均匀
        assert!(o < 1.0, "小样本时应向先验收缩");
        assert!(s > 0.0);
        assert!(m > 0.0);
    }

    // ---- 全局统计 ----

    #[test]
    fn compute_global_stats_basic() {
        use crate::inference::stats::{CalibratedWeightConfig, compute_category_stats};
        use ramaria_core::types::{MemoryEvent, Presentation, now_ms};

        fn mk(
            title: &str,
            kw: &str,
            salience: f64,
            valence: f64,
            share: f64,
            pres: Presentation,
        ) -> MemoryEvent {
            let now = now_ms();
            let mut ev = MemoryEvent::new(
                "user-0001".into(),
                title.into(),
                "摘要".into(),
                now - 1000,
                now,
            );
            ev.keywords = Some(kw.into());
            ev.confidence = 0.9;
            ev.salience = salience;
            ev.valence = valence;
            ev.share = share;
            ev.presentation = pres;
            ev
        }

        let events1 = vec![
            mk("E1", "工作", 0.8, 0.5, 0.7, Presentation::Objective),
            mk("E2", "工作", 0.6, 0.3, 0.5, Presentation::Subjective),
        ];
        let events2 = vec![mk("E3", "社交", 0.7, -0.2, 0.9, Presentation::Mixed)];

        let wcfg = CalibratedWeightConfig::default();
        let cat1 = compute_category_stats("工作", &events1, None, &wcfg);
        let cat2 = compute_category_stats("社交", &events2, None, &wcfg);
        let cats = vec![cat1, cat2];

        let (gv, gs, go, gsu, gm, n_total) = compute_global_stats(&cats);
        assert!(n_total > 0.0);
        // 全局值应在各分类值之间
        assert!((-0.2..=0.5).contains(&gv));
        assert!((0.5..=0.9).contains(&gs));
        let pres_sum = go + gsu + gm;
        assert!((pres_sum - 1.0).abs() < 1e-10, "全局 presentation 和应为1");
    }

    // ---- 批量收缩 ----

    #[test]
    fn shrink_category_updates_all_fields() {
        use crate::inference::stats::{CalibratedWeightConfig, compute_category_stats};
        use ramaria_core::types::{MemoryEvent, Presentation, now_ms};

        fn mk(salience: f64, valence: f64, share: f64, pres: Presentation) -> MemoryEvent {
            let now = now_ms();
            let mut ev = MemoryEvent::new(
                "user-0001".into(),
                "E".into(),
                "摘要".into(),
                now - 1000,
                now,
            );
            ev.keywords = Some("工作".into());
            ev.confidence = 0.9;
            ev.salience = salience;
            ev.valence = valence;
            ev.share = share;
            ev.presentation = pres;
            ev
        }

        let events = vec![mk(0.5, 0.9, 0.9, Presentation::Objective)];
        let wcfg = CalibratedWeightConfig::default();
        let mut cat = compute_category_stats("工作", &events, None, &wcfg);
        let original_valence = cat.valence_mean;
        let original_share = cat.share_mean;
        let original_obj = cat.presentation_objective_ratio;

        // 单事件 n_eff=0.5，应该明显收缩
        shrink_category(&mut cat, 0.1, 0.4, 0.33, 0.33, 0.34, 4.0);

        // 收缩后值应向先验靠近
        assert!(
            cat.valence_mean < original_valence,
            "valence 应向先验 0.1 方向收缩"
        );
        assert!(
            cat.share_mean < original_share,
            "share 应向先验 0.4 方向收缩"
        );
        assert!(
            cat.presentation_objective_ratio < original_obj,
            "objective ratio 应收缩"
        );

        // presentation 比例和仍为 1
        let sum = cat.presentation_objective_ratio
            + cat.presentation_subjective_ratio
            + cat.presentation_mixed_ratio;
        assert!((sum - 1.0).abs() < 1e-10, "presentation 和应为1");
    }

    #[test]
    fn run_shrinkage_full_pipeline() {
        use crate::inference::stats::{CalibratedWeightConfig, compute_category_stats};
        use ramaria_core::types::{MemoryEvent, Presentation, now_ms};

        let config = ShrinkConfig::default();

        fn mk(
            title: &str,
            kw: &str,
            salience: f64,
            valence: f64,
            share: f64,
            pres: Presentation,
        ) -> MemoryEvent {
            let now = now_ms();
            let mut ev = MemoryEvent::new(
                "user-0001".into(),
                title.into(),
                "摘要".into(),
                now - 1000,
                now,
            );
            ev.keywords = Some(kw.into());
            ev.confidence = 0.9;
            ev.salience = salience;
            ev.valence = valence;
            ev.share = share;
            ev.presentation = pres;
            ev
        }

        let events_work = vec![mk("E1", "工作", 0.8, 0.9, 0.8, Presentation::Objective)];
        let events_social = vec![
            mk("E2", "社交", 0.7, 0.5, 0.9, Presentation::Subjective),
            mk("E3", "社交", 0.6, 0.3, 0.7, Presentation::Mixed),
            mk("E4", "社交", 0.8, 0.6, 0.8, Presentation::Subjective),
        ];

        let wcfg = CalibratedWeightConfig::default();
        let cat1 = compute_category_stats("工作", &events_work, None, &wcfg);
        let cat2 = compute_category_stats("社交", &events_social, None, &wcfg);
        let mut cats = vec![cat1, cat2];

        let gamma = run_shrinkage(&mut cats, &config);
        assert!(gamma > 0.0, "γ 应为正数");

        // 工作（n_eff=0.8）应被明显收缩
        let work = &cats[0];
        // 社交（n_eff=2.1）收缩程度较小
        let social = cats.iter().find(|c| c.category == "社交").unwrap();

        // 两者都应仍在合理范围
        assert!(work.valence_mean >= -1.0 && work.valence_mean <= 1.0);
        assert!(social.valence_mean >= -1.0 && social.valence_mean <= 1.0);
        assert!(work.share_mean >= 0.0 && work.share_mean <= 1.0);
    }

    #[test]
    fn run_shrinkage_empty_categories() {
        let config = ShrinkConfig::default();
        let mut cats: Vec<CategoryStats> = Vec::new();
        let gamma = run_shrinkage(&mut cats, &config);
        assert!((gamma - 4.0).abs() < 1e-10); // n_total_eff=0 → max(0,30)=30 → γ=4
    }

    // =========================================================
    // 分层先验收缩
    // =========================================================

    /// 构造测试用 CategoryStats。
    fn make_cat(
        category: &str,
        n_eff: f64,
        valence_mean: f64,
        share_mean: f64,
        obj: f64,
        sub: f64,
        mix: f64,
    ) -> CategoryStats {
        CategoryStats {
            category: category.into(),
            event_count: n_eff as usize,
            n_eff,
            valence_mean,
            valence_std: 0.2,
            valence_positive_ratio: if valence_mean > 0.0 { 0.7 } else { 0.3 },
            share_mean,
            share_std: 0.1,
            presentation_objective_ratio: obj,
            presentation_subjective_ratio: sub,
            presentation_mixed_ratio: mix,
            group_weight: 1.0,
        }
    }

    /// select_prior 各 (layer, global, domain) 参数化验证。
    #[test]
    fn select_prior_cases() {
        let cases = [
            (TraitLayer::Base, 0.5, Some(0.8), 0.5),    // Base 用全局
            (TraitLayer::Primary, 0.5, Some(0.8), 0.5), // Primary 用全局
            (TraitLayer::Accent, 0.5, Some(0.8), 0.8),  // Accent 用领域
            (TraitLayer::Accent, 0.5, None, 0.5),       // Accent 无领域 → fallback 全局
        ];
        for (layer, global, domain, expected) in cases {
            let result = select_prior(&layer, global, domain);
            assert!(
                (result - expected).abs() < 1e-10,
                "{layer:?} 期望 {expected}"
            );
        }
    }

    /// compute_domain_prior 各索引/n_eff 参数化验证。
    #[test]
    fn compute_domain_prior_cases() {
        // 空索引 → None
        let cats = vec![make_cat("工作", 10.0, 0.5, 0.6, 0.4, 0.3, 0.3)];
        assert!(
            compute_domain_prior(&cats, &[]).is_none(),
            "空索引应返回 None"
        );
        // n_eff < 1.0 → None
        let cats = vec![make_cat("社交", 0.5, 0.8, 0.9, 0.2, 0.5, 0.3)];
        assert!(
            compute_domain_prior(&cats, &[0]).is_none(),
            "n_eff < 1.0 应返回 None"
        );
        // 有效领域 → Some，prior 接近原始值
        let cats = vec![
            make_cat("工作", 8.0, 0.6, 0.7, 0.5, 0.3, 0.2),
            make_cat("社交", 5.0, 0.1, 0.8, 0.2, 0.5, 0.3),
        ];
        let prior = compute_domain_prior(&cats, &[1]).expect("有效领域应返回 Some");
        assert!((prior.valence_mean - 0.1).abs() < 0.01);
        assert!((prior.share_mean - 0.8).abs() < 0.01);
    }

    #[test]
    fn run_shrinkage_layered_base_primary_use_global() {
        let config = ShrinkConfig::default();
        let mut cats = vec![make_cat("工作", 10.0, 0.8, 0.7, 0.5, 0.3, 0.2)];
        let original_valence = cats[0].valence_mean;

        let mut hints = HashMap::new();
        hints.insert("工作".to_string(), TraitLayer::Base);

        let gamma = run_shrinkage_layered(&mut cats, &config, &hints);
        assert!(gamma > 0.0);

        // Base 使用全局先验，n_eff=10 较大，收缩幅度小
        assert!(
            cats[0].valence_mean <= original_valence,
            "应向全局均值方向收缩（全局 valence 可能较低）"
        );
    }

    #[test]
    fn run_shrinkage_layered_accent_uses_domain_prior() {
        let config = ShrinkConfig::default();
        // 两个分类: 工作（Base, n_eff=20, valence=0.8）和 社交（Accent, n_eff=2, valence=-0.5）
        let mut cats = vec![
            make_cat("工作", 20.0, 0.8, 0.7, 0.5, 0.3, 0.2),
            make_cat("社交", 3.0, -0.5, 0.4, 0.2, 0.5, 0.3),
        ];

        // 记录原始值
        let social_original_valence = cats[1].valence_mean;

        // 先用全局先验收缩一次
        let mut cats_global = cats.clone();
        run_shrinkage(&mut cats_global, &config);
        let social_global_shrunk = cats_global[1].valence_mean;

        // 再用分层先验收缩
        let mut hints = HashMap::new();
        hints.insert("工作".to_string(), TraitLayer::Base);
        hints.insert("社交".to_string(), TraitLayer::Accent);
        // 只有 1 个 Accent → 领域先验不可用 → fallback 全局，结果应与 run_shrinkage 一致
        let gamma = run_shrinkage_layered(&mut cats, &config, &hints);
        assert!(gamma > 0.0);

        // 单 Accent 时 fallback 全局，结果应接近全局收缩
        assert!(
            (cats[1].valence_mean - social_global_shrunk).abs() < 0.01,
            "单 Accent fallback 全局时结果应一致"
        );
        // 社交的 n_eff=3 较小，应被明显收缩
        assert!(
            cats[1].valence_mean > social_original_valence,
            "小样本负值应被向全局均值收缩（提升）"
        );
    }

    #[test]
    fn run_shrinkage_layered_multiple_accents() {
        let config = ShrinkConfig::default();
        // 三个分类: 工作(Base), 社交(Accent), 家庭(Accent)
        let mut cats = vec![
            make_cat("工作", 20.0, 0.6, 0.7, 0.4, 0.3, 0.3),
            make_cat("社交", 3.0, -0.4, 0.8, 0.1, 0.6, 0.3),
            make_cat("家庭", 4.0, -0.2, 0.3, 0.3, 0.3, 0.4),
        ];

        // 记录 accent 分类的原始值
        let social_original_valence = cats[1].valence_mean;
        let family_original_valence = cats[2].valence_mean;

        let mut hints = HashMap::new();
        hints.insert("工作".to_string(), TraitLayer::Base);
        hints.insert("社交".to_string(), TraitLayer::Accent);
        hints.insert("家庭".to_string(), TraitLayer::Accent);

        let gamma = run_shrinkage_layered(&mut cats, &config, &hints);
        assert!(gamma > 0.0);

        // 工作（Base, n_eff=20）几乎不变
        assert!((cats[0].valence_mean - 0.6).abs() < 0.1);

        // 社交和家庭（Accent, 小样本）应被收缩
        // 领域先验来自 Accent 子集: valence ≈ (-0.4*3 + -0.2*4)/(3+4) ≈ -0.286
        // 社交收缩: (3*−0.4 + γ*−0.286)/(3+γ) — 应向 -0.286 靠近
        assert!(
            cats[1].valence_mean >= social_original_valence,
            "社交应被向领域均值收缩（领域均值高于原始值）"
        );
        assert!(
            cats[2].valence_mean <= family_original_valence,
            "家庭应被向领域均值收缩（领域均值低于原始值）"
        );
    }

    #[test]
    fn run_shrinkage_layered_empty_hints() {
        let config = ShrinkConfig::default();
        let mut cats = vec![make_cat("工作", 10.0, 0.8, 0.7, 0.5, 0.3, 0.2)];

        let mut cats_expected = cats.clone();
        run_shrinkage(&mut cats_expected, &config);

        let hints = HashMap::new(); // 空 hints
        let gamma = run_shrinkage_layered(&mut cats, &config, &hints);
        assert!(gamma > 0.0);

        // 空 hints 应退化为全局先验=run_shrinkage
        assert!(
            (cats[0].valence_mean - cats_expected[0].valence_mean).abs() < 0.01,
            "空 hints 应与 run_shrinkage 结果一致"
        );
    }

    #[test]
    fn run_shrinkage_layered_empty_categories() {
        let config = ShrinkConfig::default();
        let mut cats: Vec<CategoryStats> = Vec::new();
        let hints = HashMap::new();
        let gamma = run_shrinkage_layered(&mut cats, &config, &hints);
        assert!((gamma - 4.0).abs() < 1e-10);
    }

    #[test]
    fn shrink_prior_from_categories_empty() {
        let cats: Vec<CategoryStats> = Vec::new();
        let prior = ShrinkPrior::from_categories(&cats);
        assert!((prior.valence_mean - 0.0).abs() < 1e-10);
        assert!((prior.share_mean - 0.5).abs() < 1e-10);
        assert!((prior.n_total_eff - 0.0).abs() < 1e-10);
    }

    #[test]
    fn shrink_prior_from_categories_single() {
        let cats = vec![make_cat("工作", 10.0, 0.6, 0.7, 0.4, 0.3, 0.3)];
        let prior = ShrinkPrior::from_categories(&cats);
        assert!((prior.valence_mean - 0.6).abs() < 0.01);
        assert!((prior.share_mean - 0.7).abs() < 0.01);
        assert!((prior.n_total_eff - 10.0).abs() < 0.01);
    }
}
