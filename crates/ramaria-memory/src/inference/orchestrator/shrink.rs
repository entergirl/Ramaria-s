//! crates/ramaria-memory/src/inference/orchestrator/shrink.rs - 分层先验收缩集成
//!
//! 设计特点:
//! - build_layer_hints_from_traits: 从已持久化 traits 构建 trait_label → TraitLayer 映射。
//! - apply_layered_shrinkage: 读取上轮 Active traits → 构建 layer hints → run_shrinkage_layered。
//! - 空 hints（首轮推断）退化为全局先验收缩；DB 读取失败仅 warn 降级不阻塞。
//! - 收缩后同步更新 StatsSummary 的叙事一致性指标。

use std::collections::HashMap;

use ramaria_core::{
    traits::StorageBackend,
    types::{PersonalityTrait, TraitLayer, TraitStatus},
};
use tracing::{debug, info, warn};

use crate::inference::{
    shrink::{ShrinkConfig, run_shrinkage_layered},
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
/// 3. 调用 `run_shrinkage_layered`：hints 非空时使用分层先验收缩。
/// 4. hints 为空（首轮推断）时退化为全局先验收缩（旧 `run_shrinkage` 已删除）。
/// 5. 收缩结果直接写入 `stats_summary.categories`（in-place 修改）。
///
/// 说明:
/// - 本函数应在 Phase A 统计完成后、Phase B 推断前调用。
/// - DB 读取失败不阻塞管线：降级为全局先验收缩，仅记录 warn 日志。
/// - 收缩后需重新计算 `StatsSummary.cross_category` 指标（如有必要，调用方负责）。
///
/// 参数:
/// - `storage`: 存储后端，用于读取已有 traits。
/// - `stats_summary`: Phase A 统计摘要（可变引用，categories 将被 in-place 收缩）。
/// - `persona_uid`: 目标人格标识。
/// - `shrink_config`: 收缩配置（γ 参数等）。
///
/// 返回:
/// - 使用的 γ 值（供日志记录）。失败时返回 0.0。
pub async fn apply_layered_shrinkage(
    storage: &dyn StorageBackend,
    stats_summary: &mut StatsSummary,
    persona_uid: &str,
    shrink_config: &ShrinkConfig,
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
            // 降级: 全局先验收缩（无跨用户先验，回退当前 persona 内先验）
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

    // 3. 执行分层收缩（空 hints 时退化为全局先验）。
    // 跨用户经验先验需从系统内已有人格画像聚合，当前收缩路径尚未接入存储聚合，
    // 传入 None 回退当前 persona 内先验（跨用户先验接入属后续增强）。
    let gamma = run_shrinkage_layered(
        &mut stats_summary.categories,
        shrink_config,
        &layer_hints,
        None,
    );

    // 4. 收缩后更新叙事一致性（presentation 分布向先验收缩后一致性提高）
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
