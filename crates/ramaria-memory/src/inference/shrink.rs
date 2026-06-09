//! rust/crates/ramaria-memory/src/inference/shrink.rs - Phase A 经验贝叶斯小样本收缩
//!
//! 设计特点:
//! - A5 小样本收缩估计: 当分类有效样本量 n_eff 过小时，将极端估计值向全局均值收缩
//! - Valence: 标准经验贝叶斯收缩（无界连续量，对称分布）
//! - Share: logit 变换 → 收缩 → sigmoid（有界 [0,1]）
//! - Presentation: Dirichlet-Multinomial 共轭（三比例和为 1 的组合数据）
//! - γ 动态公式: γ = 3 + 30 / max(n_total_eff, 30)，随总样本量自适应调整
//! - 纯数值计算，零 I/O，不依赖数据库或异步运行时

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
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- γ 动态计算 ----

    #[test]
    fn gamma_small_sample() {
        let config = ShrinkConfig::default();
        let gamma = compute_dynamic_gamma(10.0, &config);
        // γ = 3 + 30/30 = 4.0（因为 max(10,30)=30）
        assert!((gamma - 4.0).abs() < 1e-10);
    }

    #[test]
    fn gamma_large_sample() {
        let config = ShrinkConfig::default();
        let gamma = compute_dynamic_gamma(300.0, &config);
        // γ = 3 + 30/300 = 3.1
        assert!((gamma - 3.1).abs() < 1e-10);
    }

    #[test]
    fn gamma_very_large_sample() {
        let config = ShrinkConfig::default();
        let gamma = compute_dynamic_gamma(10_000.0, &config);
        // γ = 3 + 30/10000 = 3.003
        assert!((gamma - 3.003).abs() < 0.001);
    }

    // ---- Valence 收缩 ----

    #[test]
    fn shrink_valence_large_n_eff() {
        // n_eff 很大时收缩接近原始值
        let result = shrink_valence(0.8, 100.0, 0.2, 4.0);
        // μ = (100/104)*0.8 + (4/104)*0.2 ≈ 0.769 + 0.008 ≈ 0.777
        assert!((result - 0.777).abs() < 0.01);
    }

    #[test]
    fn shrink_valence_small_n_eff() {
        // n_eff 很小时收缩接近全局均值
        let result = shrink_valence(0.8, 1.0, 0.2, 4.0);
        // μ = (1/5)*0.8 + (4/5)*0.2 = 0.16 + 0.16 = 0.32
        assert!((result - 0.32).abs() < 1e-10);
    }

    #[test]
    fn shrink_valence_zero_n_eff() {
        let result = shrink_valence(0.8, 0.0, 0.2, 4.0);
        assert!((result - 0.2).abs() < 1e-10); // 完全依赖先验
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
        assert!(s0 >= 0.0 && s0 <= 1.0);
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
        use crate::inference::stats::compute_category_stats;
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

        let cat1 = compute_category_stats("工作", &events1);
        let cat2 = compute_category_stats("社交", &events2);
        let cats = vec![cat1, cat2];

        let (gv, gs, go, gsu, gm, n_total) = compute_global_stats(&cats);
        assert!(n_total > 0.0);
        // 全局值应在各分类值之间
        assert!(gv >= -0.2 && gv <= 0.5);
        assert!(gs >= 0.5 && gs <= 0.9);
        let pres_sum = go + gsu + gm;
        assert!((pres_sum - 1.0).abs() < 1e-10, "全局 presentation 和应为1");
    }

    // ---- 批量收缩 ----

    #[test]
    fn shrink_category_updates_all_fields() {
        use crate::inference::stats::compute_category_stats;
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
        let mut cat = compute_category_stats("工作", &events);
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
        use crate::inference::stats::compute_category_stats;
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

        let cat1 = compute_category_stats("工作", &events_work);
        let cat2 = compute_category_stats("社交", &events_social);
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
}
