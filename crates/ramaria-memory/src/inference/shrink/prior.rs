//! crates/ramaria-memory/src/inference/shrink/prior.rs - 分层先验与跨用户经验先验
//!
//! 设计特点:
//! - 先验包聚合：五个指标的样本量加权聚合，保持收缩公式单一来源
//! - 分层选择：Base / Primary 用全局先验，Accent 用领域先验（不足两个 Accent 时回退）
//! - 冷启动校准：跨用户经验先验按有效样本量加权合并，样本不足时回退当前 persona 内先验

use std::collections::HashMap;

use ramaria_core::types::{PersonaEventAggregate, TraitLayer};

use crate::inference::stats::CategoryStats;

use super::{ShrinkConfig, compute_dynamic_gamma, compute_global_stats, shrink_category};

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
    pub(super) fn from_categories(categories: &[CategoryStats]) -> Self {
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
