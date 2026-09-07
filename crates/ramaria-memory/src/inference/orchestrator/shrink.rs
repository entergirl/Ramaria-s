//! crates/ramaria-memory/src/inference/orchestrator/shrink.rs - 分层先验收缩集成
//!
//! 设计特点:
//! - build_layer_hints_from_traits: 从已持久化 traits 构建 trait_label → TraitLayer 映射。
//! - apply_layered_shrinkage: 读取上轮 Active traits → 构建 layer hints → run_shrinkage_layered。
//! - 跨用户冷启动先验：按开关从存储聚合系统内其他 persona 的事件级经验分布并真实传入。
//! - 空 hints（首轮推断）退化为全局先验收缩；DB 读取/聚合失败仅 warn 降级不阻塞。
//! - 收缩后同步更新 StatsSummary 的叙事一致性指标。

use std::collections::HashMap;

use ramaria_core::{
    traits::StorageBackend,
    types::{PersonalityTrait, TraitLayer, TraitStatus},
};
use tracing::{debug, info, warn};

use crate::inference::{
    shrink::{ShrinkConfig, build_cross_user_prior, run_shrinkage_layered},
    stats::StatsSummary,
};

// =========================================================
// 分层先验收缩集成
// =========================================================

/// 从已持久化的人格特质中构建分层先验提示映射。
///
/// 策略:
/// - 仅读取 `Active` 状态的 trait，忽略 `Deprecated` / `Pending`。
/// - 从每条 trait 的 `trait_label`（如"工作""社交"）映射到其 `layer`（Base/Primary/Accent）。
/// - 同一 `trait_label` 出现多次时，按优先级 Base > Primary > Accent 保留最保守的层。
///
/// 说明:
/// - `trait_label` 通常与 Phase A 的 `category` 名称一致，这是两者关联的桥梁。
/// - 若 traits 列表为空（首轮推断），返回空 HashMap，`run_shrinkage_layered` 将退化为全局先验。
///
/// 参数:
/// - `traits`: 从 DB 读取的已有 PersonalityTrait 列表。
///
/// 返回:
/// - trait_label → TraitLayer 的映射。
pub fn build_layer_hints_from_traits(traits: &[PersonalityTrait]) -> HashMap<String, TraitLayer> {
    let mut hints: HashMap<String, TraitLayer> = HashMap::new();

    for t in traits {
        if t.status != TraitStatus::Active {
            continue;
        }
        let label = t.trait_label.clone();
        let layer = t.layer;
        // 优先级: Base > Primary > Accent（数字越小越保守）
        let priority = match layer {
            TraitLayer::Base => 0u8,
            TraitLayer::Primary => 1,
            TraitLayer::Accent => 2,
            _ => 3, // 未知 layer 最低优先级
        };
        hints
            .entry(label)
            .and_modify(|existing| {
                let existing_priority = match *existing {
                    TraitLayer::Base => 0,
                    TraitLayer::Primary => 1,
                    TraitLayer::Accent => 2,
                    _ => 3,
                };
                if priority < existing_priority {
                    *existing = layer;
                }
            })
            .or_insert(layer);
    }

    hints
}

/// 对 Phase A 统计结果应用分层先验收缩。
///
/// 流程:
/// 1. 从 DB 读取该 persona 的上一轮 Active traits。
/// 2. 调用 `build_layer_hints_from_traits` 构建 layer 提示映射。
/// 3. 若启用跨用户冷启动先验，从存储聚合系统内其他 persona 的事件级经验分布，
///    经样本量阈值判定后构造跨用户经验先验。
/// 4. 调用 `run_shrinkage_layered`：hints 非空时使用分层先验收缩；
///    跨用户先验可用时作为 Base/Primary 的全局先验，否则回退当前 persona 内先验。
/// 5. hints 为空（首轮推断）时退化为全局先验收缩（旧 `run_shrinkage` 已删除）。
/// 6. 收缩结果直接写入 `stats_summary.categories`（in-place 修改），
///    并重算叙事一致性指标。
///
/// 说明:
/// - 本函数应在 Phase A 统计完成后、Phase B 推断前调用。
/// - DB 读取 / 跨用户聚合失败不阻塞管线：分别降级为全局先验收缩与当前 persona
///   内先验，仅记录 warn 日志。
/// - 关闭跨用户开关、聚合失败、或系统内无足够经验来源时，跨用户先验恒为 `None`，
///   行为与 v1.7 等价（回退当前 persona 内先验）。
///
/// 参数:
/// - `storage`: 存储后端，用于读取已有 traits 与聚合其他 persona 事件经验分布。
/// - `stats_summary`: Phase A 统计摘要（可变引用，categories 将被 in-place 收缩）。
/// - `persona_uid`: 目标人格标识。
/// - `shrink_config`: 收缩配置（γ 参数等）。
/// - `enable_cross_user_prior`: 是否启用跨用户冷启动先验（对应
///   `[inference.upgrade].cold_start_cross_user_prior`）。
///
/// 返回:
/// - 使用的 γ 值（供日志记录）。categories 为空时返回 0.0。
pub async fn apply_layered_shrinkage(
    storage: &dyn StorageBackend,
    stats_summary: &mut StatsSummary,
    persona_uid: &str,
    shrink_config: &ShrinkConfig,
    enable_cross_user_prior: bool,
) -> f64 {
    if stats_summary.categories.is_empty() {
        debug!(
            persona_uid = %persona_uid,
            "Phase A shrinkage: categories 为空，跳过收缩"
        );
        return 0.0;
    }

    // 1. 读取上一轮的 traits
    let old_traits = match storage.list_traits_by_persona(persona_uid).await {
        Ok(traits) => traits,
        Err(e) => {
            warn!(
                persona_uid = %persona_uid,
                error = %e,
                "Phase A shrinkage: 读取已有 traits 失败，降级为全局先验收缩"
            );
            // 降级: 全局先验收缩（无 layer hints 与跨用户先验，回退当前 persona 内先验）
            return run_shrinkage_layered(
                &mut stats_summary.categories,
                shrink_config,
                &HashMap::new(), // 空 hints → 所有分类使用全局先验
                None,
            );
        }
    };

    // 2. 构建 layer hints
    let layer_hints = build_layer_hints_from_traits(&old_traits);

    if layer_hints.is_empty() {
        info!(
            persona_uid = %persona_uid,
            "Phase A shrinkage: 首轮推断，使用全局先验收缩"
        );
    } else {
        let accent_count = layer_hints
            .values()
            .filter(|l| matches!(l, TraitLayer::Accent))
            .count();
        info!(
            persona_uid = %persona_uid,
            hint_count = layer_hints.len(),
            accent_count,
            "Phase A shrinkage: 加载上轮 layer 提示，执行分层收缩"
        );
    }

    // 3. 构造跨用户经验先验（可选）。
    // 来源：系统内其他已有人格画像对 memory_events 的聚合分布。
    // 判定：样本量阈值在 shrink::build_cross_user_prior 内完成；
    //       无足够来源时回退当前 persona 内先验，不引入中性默认先验冒充经验先验。
    let cross_user_prior = if enable_cross_user_prior {
        match storage.aggregate_persona_event_priors(persona_uid).await {
            Ok(aggregates) if !aggregates.is_empty() => match build_cross_user_prior(&aggregates) {
                Some(prior) => {
                    info!(
                        persona_uid = %persona_uid,
                        source_personas = aggregates.len(),
                        n_total_eff = prior.n_total_eff,
                        "Phase A shrinkage: 加载系统内跨用户经验先验"
                    );
                    Some(prior)
                }
                None => {
                    info!(
                        persona_uid = %persona_uid,
                        aggregate_rows = aggregates.len(),
                        "Phase A shrinkage: 其他 persona 事件经验不足，回退当前 persona 内先验"
                    );
                    None
                }
            },
            Ok(_) => {
                info!(
                    persona_uid = %persona_uid,
                    "Phase A shrinkage: 无跨用户经验来源，回退当前 persona 内先验"
                );
                None
            }
            Err(e) => {
                warn!(
                    persona_uid = %persona_uid,
                    error = %e,
                    "Phase A shrinkage: 聚合跨用户经验先验失败，降级为当前 persona 内先验"
                );
                None
            }
        }
    } else {
        None
    };

    // 4. 执行分层收缩（空 hints 时退化为全局先验）。
    let gamma = run_shrinkage_layered(
        &mut stats_summary.categories,
        shrink_config,
        &layer_hints,
        cross_user_prior.as_ref(),
    );

    // 5. 收缩后更新叙事一致性（presentation 分布向先验收缩后一致性提高）
    stats_summary.cross_category.narrative_consistency =
        crate::inference::stats::compute_narrative_consistency(&stats_summary.categories);

    debug!(
        persona_uid = %persona_uid,
        gamma,
        narrative_consistency = stats_summary.cross_category.narrative_consistency,
        "Phase A shrinkage: 分层收缩完成"
    );

    gamma
}

// =========================================================
// 单元测试：跨用户先验接线行为
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::stats::{CategoryStats, CrossCategoryMetrics, StatsSummary};
    use crate::l1::mock::MockStorage;
    use ramaria_core::types::PersonaEventAggregate;

    fn make_agg(persona_uid: &str, n_events: u64, valence: f64) -> PersonaEventAggregate {
        PersonaEventAggregate::new(
            persona_uid,
            n_events,
            valence,
            0.5,
            1.0 / 3.0,
            1.0 / 3.0,
            1.0 / 3.0,
        )
    }

    /// 构造"单个小样本极端分类"的统计摘要。
    ///
    /// 语义: 分类 valence=0.8、n_eff=1，若跨用户先验可用会被显著拉低；
    /// 回退自身先验（唯一分类 = 0.8）时几乎不收缩。
    fn make_single_extreme_stats() -> StatsSummary {
        StatsSummary {
            total_events_in: 1,
            total_events_filtered: 1,
            confirmed_count: 1,
            tentative_count: 0,
            discarded_count: 0,
            category_count: 1,
            categories: vec![CategoryStats {
                category: "工作".into(),
                event_count: 1,
                n_eff: 1.0,
                valence_mean: 0.8,
                valence_std: 0.2,
                valence_positive_ratio: 1.0,
                share_mean: 0.7,
                share_std: 0.1,
                presentation_objective_ratio: 0.5,
                presentation_subjective_ratio: 0.3,
                presentation_mixed_ratio: 0.2,
                group_weight: 1.0,
            }],
            cross_category: CrossCategoryMetrics {
                emotional_stability: 0.0,
                narrative_consistency: 1.0,
                attitude_contradiction_count: 0,
                share_skewness: 0.0,
                share_kurtosis: 0.0,
            },
            representative_events: Vec::new(),
            motive_stats: Vec::new(),
        }
    }

    /// 开启跨用户先验且系统内有足够其他 persona 经验 → 收缩显著向跨用户先验靠拢。
    #[tokio::test]
    async fn enabled_uses_cross_user_prior() {
        let storage = MockStorage::new();
        // 其他 persona char-a 有 40 条、valence 均值 0.0 的经验分布
        storage.set_persona_event_aggregates(vec![make_agg("char-a", 40, 0.0)]);

        let mut enabled_stats = make_single_extreme_stats();
        let gamma = apply_layered_shrinkage(
            &storage,
            &mut enabled_stats,
            "user-target",
            &ShrinkConfig::default(),
            true,
        )
        .await;
        assert!(gamma > 0.0);

        // 小样本分类向跨用户先验 0.0 收缩：n_eff=1、γ≈3.75 → ~0.168
        assert!(
            enabled_stats.categories[0].valence_mean < 0.4,
            "启用跨用户先验后应显著收缩，实际={}",
            enabled_stats.categories[0].valence_mean
        );
    }

    /// 无跨用户来源（系统内无其他 persona）时回退自身先验，行为与关闭开关一致且不 panic。
    #[tokio::test]
    async fn no_cross_user_source_falls_back_to_own_prior() {
        let storage = MockStorage::new(); // 默认聚合返回空

        let mut enabled_stats = make_single_extreme_stats();
        let gamma_enabled = apply_layered_shrinkage(
            &storage,
            &mut enabled_stats,
            "user-target",
            &ShrinkConfig::default(),
            true,
        )
        .await;
        assert!(gamma_enabled > 0.0);

        let mut disabled_stats = make_single_extreme_stats();
        let gamma_disabled = apply_layered_shrinkage(
            &storage,
            &mut disabled_stats,
            "user-target",
            &ShrinkConfig::default(),
            false,
        )
        .await;
        assert!(gamma_disabled > 0.0);

        // 唯一分类自身先验 = 0.8 → 收缩前后基本不变；两路径结果一致
        assert!((enabled_stats.categories[0].valence_mean - 0.8).abs() < 0.2);
        assert!(
            (enabled_stats.categories[0].valence_mean - disabled_stats.categories[0].valence_mean)
                .abs()
                < 1e-9
        );
    }

    /// 聚合失败（DB 错误）仅 warn 降级不 panic，回退自身先验。
    #[tokio::test]
    async fn aggregate_failure_degrades_to_own_prior() {
        let storage = MockStorage::new();
        storage.set_persona_event_aggregate_error("模拟聚合失败");

        let mut stats = make_single_extreme_stats();
        let gamma = apply_layered_shrinkage(
            &storage,
            &mut stats,
            "user-target",
            &ShrinkConfig::default(),
            true,
        )
        .await;
        assert!(gamma > 0.0);
        // 回退自身先验（0.8），基本不收缩
        assert!(
            (stats.categories[0].valence_mean - 0.8).abs() < 0.2,
            "聚合失败应回退当前 persona 内先验，实际={}",
            stats.categories[0].valence_mean
        );
    }

    /// 关闭开关 → 恒不查跨用户聚合，与 v1.7（None 路径）等价。
    #[tokio::test]
    async fn disabled_never_queries_cross_user_aggregate() {
        // 即使 mock 预设了丰富聚合数据，关闭开关也不应消费
        let storage = MockStorage::new();
        storage.set_persona_event_aggregates(vec![make_agg("char-a", 40, 0.0)]);

        let mut stats = make_single_extreme_stats();
        let _gamma = apply_layered_shrinkage(
            &storage,
            &mut stats,
            "user-target",
            &ShrinkConfig::default(),
            false,
        )
        .await;
        assert!(
            (stats.categories[0].valence_mean - 0.8).abs() < 0.2,
            "关闭开关时小样本分类保持自身先验收缩，实际={}",
            stats.categories[0].valence_mean
        );
    }

    /// categories 为空 → 跳过收缩返回 0.0。
    #[tokio::test]
    async fn empty_categories_returns_zero() {
        let storage = MockStorage::new();
        let mut stats = make_single_extreme_stats();
        stats.categories.clear();

        let gamma = apply_layered_shrinkage(
            &storage,
            &mut stats,
            "user-target",
            &ShrinkConfig::default(),
            true,
        )
        .await;
        assert!((gamma - 0.0).abs() < 1e-12);
    }
}
