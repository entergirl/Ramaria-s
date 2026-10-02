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

use std::collections::HashMap;

use ramaria_core::types::{PersonaEventAggregate, TraitLayer};

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
/// 1. 确定全局先验：优先使用 `cross_user_prior`（系统内已有人格画像的跨用户经验分布，
///    冷启动校准）；未提供时回退当前 persona 全部分类先验。
/// 2. 根据 layer_hints 识别 Accent 分类，计算领域先验（来自 Accent 分类子集）。
/// 3. 对每个分类：
///    - 查 layer_hints 获取该分类的预期 TraitLayer。
///    - Base/Primary → 使用全局先验（跨用户或当前 persona）。
///    - Accent → 使用领域先验（不可用时 fallback 全局先验）。
///    - 未在 hints 中的分类 → 使用全局先验（保守策略）。
/// 4. 执行标准收缩公式。
///
/// 参数:
/// - `categories`: 所有分类统计（可变引用，in-place 更新）。
/// - `shrink_config`: 收缩配置（γ 参数）。
/// - `layer_hints`: 分类名 → TraitLayer 的映射（来自上一轮 Phase B 的持久化结果）。
///   首次推断时传入空 HashMap，此时所有分类使用全局先验。
/// - `cross_user_prior`: 可选的跨用户经验先验（冷启动校准）。
///   `Some` → 作为 base/primary 的全局先验来源；`None` → 回退当前 persona 内先验。
///
/// 返回:
/// - 使用的 γ 值（供日志记录）。
///
/// 说明:
/// - 当至少 2 个分类标记为 Accent 时才计算领域先验；单个 Accent 分类 fallback 全局先验。
/// - 与全局先验收缩（已删除的 `run_shrinkage`）的差异仅在于先验来源，收缩公式完全一致。
pub fn run_shrinkage_layered(
    categories: &mut [CategoryStats],
    shrink_config: &ShrinkConfig,
    layer_hints: &HashMap<String, TraitLayer>,
    cross_user_prior: Option<&ShrinkPrior>,
) -> f64 {
    // Step 1: 确定全局先验。
    // 跨用户经验先验优先（冷启动校准）；
    // 未提供时回退当前 persona 全部分类先验。
    let own_prior = ShrinkPrior::from_categories(categories);
    let global_prior = cross_user_prior.unwrap_or(&own_prior);

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
            Some(l) => select_shrink_prior(l, global_prior, &domain_prior),
            None => global_prior, // 无 hint 时保守使用全局先验
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
// 跨用户经验先验（冷启动校准）
// =========================================================

/// 统一的冷启动默认先验（首个 persona、系统内无已有人格画像时回退）。
///
/// 语义（中性先验）:
/// - `valence_mean = 0.0`：情感方向无偏（社会赞许偏倚的对抗基线）。
/// - `share_mean = 0.5`：分享意愿中性。
/// - `presentation` 三态均分（objective/subjective/mixed = 1/3）。
///
/// 返回:
/// - 中性先验包。
pub fn unified_default_prior() -> ShrinkPrior {
    ShrinkPrior {
        valence_mean: 0.0,
        share_mean: 0.5,
        obj_ratio: 1.0 / 3.0,
        sub_ratio: 1.0 / 3.0,
        mix_ratio: 1.0 / 3.0,
        n_total_eff: 0.0,
    }
}

/// 合并多个 persona 的经验先验为跨用户先验。
///
/// 算法:
/// - 对每个 persona 的 `ShrinkPrior` 按 `n_total_eff` 加权平均各指标，
///   得到"系统内已有人格画像的跨用户经验分布"。
/// - 输入为空（系统内尚无已有人格画像）时回退统一默认先验 `unified_default_prior()`。
///
/// 参数:
/// - `persona_priors`: 各已有人格画像的收缩先验观察。
///
/// 返回:
/// - 跨用户经验先验包（空输入时回退统一默认）。
pub fn merge_cross_user_prior(persona_priors: &[ShrinkPrior]) -> ShrinkPrior {
    let total_eff: f64 = persona_priors.iter().map(|p| p.n_total_eff).sum();

    if total_eff < 1e-12 {
        return unified_default_prior();
    }

    let weighted = |f: &dyn Fn(&ShrinkPrior) -> f64| -> f64 {
        persona_priors
            .iter()
            .map(|p| p.n_total_eff * f(p))
            .sum::<f64>()
            / total_eff
    };

    ShrinkPrior {
        valence_mean: weighted(&|p| p.valence_mean),
        share_mean: weighted(&|p| p.share_mean),
        obj_ratio: weighted(&|p| p.obj_ratio),
        sub_ratio: weighted(&|p| p.sub_ratio),
        mix_ratio: weighted(&|p| p.mix_ratio),
        n_total_eff: total_eff,
    }
}

// =========================================================
// 聚合行 → 跨用户先验换算
// =========================================================

/// 跨用户经验先验要求的最少"其他 persona 来源"数量（排除目标 persona 后）。
///
/// 说明:
/// - 系统内需存在至少一个已有人格画像作为经验来源，否则不存在"跨用户"语义。
pub const MIN_CROSS_USER_PERSONA_SOURCES: usize = 1;

/// 跨用户经验先验要求的最少总经验事件数。
///
/// 说明:
/// - 口径与 γ 公式的样本量保底（`gamma_min_eff = 30`）对齐：低于该量级的事件
///   分布本身不可靠，不应作为经验先验冒充"已校准分布"。
/// - 事件数不足时回退当前 persona 内先验（既不借用杂讯，也不引入中性默认先验）。
pub const MIN_CROSS_USER_TOTAL_EVENTS: u64 = 30;

/// 将单条 persona 的事件级聚合行换算为收缩先验包。
///
/// 说明:
/// - `n_events` 作为该 persona 在跨用户加权合并中的有效样本量权重。
impl From<&PersonaEventAggregate> for ShrinkPrior {
    fn from(agg: &PersonaEventAggregate) -> Self {
        Self {
            valence_mean: agg.valence_mean,
            share_mean: agg.share_mean,
            obj_ratio: agg.obj_ratio,
            sub_ratio: agg.sub_ratio,
            mix_ratio: agg.mix_ratio,
            n_total_eff: agg.n_events as f64,
        }
    }
}

/// 从存储层聚合行构造跨用户经验先验（含样本量阈值判定）。
///
/// 判定口径:
/// - 可用的"其他 persona 来源"数 ≥ [`MIN_CROSS_USER_PERSONA_SOURCES`]，
///   且这些来源的事件总数 ≥ [`MIN_CROSS_USER_TOTAL_EVENTS`]；
/// - 低于阈值时返回 `None`，由调用方回退当前 persona 内先验。
///
/// 设计约束:
/// - 空 / 样本不足时返回 `None`，绝不回退中性默认先验 [`unified_default_prior`]
///   ——中性先验只用于"系统内完全没有经验来源"的首个 persona 场景，
///   不应在存在少量杂讯时冒充经验先验。
///
/// 参数:
/// - `aggregates`: 存储层返回的其他 persona 聚合行（存储层已排除目标 persona）。
///
/// 返回:
/// - `Some(ShrinkPrior)`: 合并后的跨用户经验先验。
/// - `None`: 经验来源不足，调用方应回退当前 persona 内先验。
pub fn build_cross_user_prior(aggregates: &[PersonaEventAggregate]) -> Option<ShrinkPrior> {
    let usable: Vec<&PersonaEventAggregate> =
        aggregates.iter().filter(|a| a.n_events > 0).collect();

    if usable.len() < MIN_CROSS_USER_PERSONA_SOURCES {
        return None;
    }
    let total_events: u64 = usable.iter().map(|a| a.n_events).sum();
    if total_events < MIN_CROSS_USER_TOTAL_EVENTS {
        return None;
    }

    let priors: Vec<ShrinkPrior> = usable.iter().map(|a| ShrinkPrior::from(*a)).collect();
    Some(merge_cross_user_prior(&priors))
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
