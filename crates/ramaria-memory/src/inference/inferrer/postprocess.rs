//! crates/ramaria-memory/src/inference/inferrer/postprocess.rs - 推断结果后处理（差异计算）
//!
//! 设计特点:
//! - 将新推断 trait 与已有 trait 逐条比对，产出 Add/Update/Deprecate/Keep
//! - 简化版按 trait_label 精确匹配；接入 embedding 后可替换为语义匹配
//! - 旧 accent trait 在新推断中消失即标记废弃
//! - 纯计算，不写库

use ramaria_core::{PersonalityTrait, TraitLayer, TraitStatus};

use super::types::{DiffAction, InferenceResult, PostProcessResult, TraitDiff};

// =========================================================
// 输出后处理
// =========================================================

/// 将新推断的 trait 与已有 trait 做差异比较。
///
/// 策略（简化版——基于 trait_label 精确匹配）:
/// - 新 trait 的 label 在旧 trait 中找不到 → Add
/// - 新 trait 的 label 与旧 trait 匹配但 layer 不同 → Update
/// - 旧 accent trait 在新推断中消失 → Deprecate
/// - 其他 → Keep
///
/// 注意: 接入 embedding 后应替换为语义匹配。
///
/// 参数:
/// - `new_traits`: 新推断的 trait 列表。
/// - `old_traits`: 数据库中已有的 trait 列表。
/// - `persona_uid`: 目标人格标识。
///
/// 返回:
/// - PostProcessResult。
pub fn compute_trait_diff(
    new_traits: &[PersonalityTrait],
    old_traits: &[PersonalityTrait],
    _persona_uid: &str,
) -> PostProcessResult {
    let mut to_add = Vec::new();
    let mut to_update = Vec::new();
    let mut to_deprecate = Vec::new();
    let mut diffs = Vec::new();

    // 构建旧 trait 的 label→(id, trait) 映射
    let old_map: std::collections::HashMap<String, (i64, &PersonalityTrait)> = old_traits
        .iter()
        .filter(|t| t.status == TraitStatus::Active)
        .map(|t| (t.trait_label.clone(), (t.id, t)))
        .collect();

    let mut old_matched: std::collections::HashSet<i64> = std::collections::HashSet::new();

    // 遍历新 trait
    for new_t in new_traits {
        if let Some(&(old_id, old_t)) = old_map.get(&new_t.trait_label) {
            old_matched.insert(old_id);
            if new_t.layer != old_t.layer || new_t.meaning != old_t.meaning {
                // layer 或 meaning 变化 → Update
                to_update.push((old_id, new_t.clone()));
                diffs.push(TraitDiff {
                    action: DiffAction::Update,
                    new_trait: Some(new_t.clone()),
                    old_trait_id: Some(old_id),
                    old_label: Some(old_t.trait_label.clone()),
                });
            } else {
                // 无变化 → Keep
                diffs.push(TraitDiff {
                    action: DiffAction::Keep,
                    new_trait: None,
                    old_trait_id: Some(old_id),
                    old_label: Some(old_t.trait_label.clone()),
                });
            }
        } else {
            // 新 trait → Add
            to_add.push(new_t.clone());
            diffs.push(TraitDiff {
                action: DiffAction::Add,
                new_trait: Some(new_t.clone()),
                old_trait_id: None,
                old_label: None,
            });
        }
    }

    // 未被匹配的旧 accent trait → 标记废弃
    for old_t in old_traits {
        if old_t.status == TraitStatus::Active
            && old_t.layer == TraitLayer::Accent
            && !old_matched.contains(&old_t.id)
        {
            to_deprecate.push(old_t.id);
            diffs.push(TraitDiff {
                action: DiffAction::Deprecate,
                new_trait: None,
                old_trait_id: Some(old_t.id),
                old_label: Some(old_t.trait_label.clone()),
            });
        }
    }

    PostProcessResult {
        to_add,
        to_update,
        to_deprecate,
        diffs,
    }
}

/// 对推断结果执行后处理。
///
/// 参数:
/// - `result`: 推断结果。
/// - `old_traits`: 已存在的 trait 列表。
/// - `persona_uid`: 目标人格标识。
///
/// 返回:
/// - PostProcessResult。
pub fn post_process_inference(
    result: &InferenceResult,
    old_traits: &[PersonalityTrait],
    persona_uid: &str,
) -> PostProcessResult {
    compute_trait_diff(&result.traits, old_traits, persona_uid)
}
