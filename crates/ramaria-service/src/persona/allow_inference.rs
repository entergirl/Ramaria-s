//! crates/ramaria-service/src/persona/allow_inference.rs - Ramaria 人格推断接管模块
//!
//! 设计特点:
//! - 显式「允许推断接管」：把选定 Manual 画像的来源改写为 Inferred，此后参与推断管线
//! - 只改 source，不改文本 / 置信度 / 证据；单条改写失败记 warn 跳过，不阻塞其余条目
//! - 候选仅含 Active 的 Manual 行；uid 无人格 / 无匹配 label 返回零改写（非错误）
//! - 幂等：已是 Inferred 的行不在候选内，重复调用结果一致

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{TraitSource, TraitStatus};
use tracing::{info, warn};

use crate::engine::Engine;

// =========================================================
// 允许推断接管
// =========================================================

/// 允许推断接管：将选定 Manual 画像的来源改写为 Inferred。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 目标人格标识。
/// - `labels`: 目标 trait 标签（精确匹配）；空切片 = 全部 Manual 画像。
///
/// 返回:
/// - 成功改写的条数；uid 无人格 / 无匹配 label 返回 0（非错误）。
///
/// 说明:
/// - 候选 = `source == Manual && status == Active`；已废弃的 Manual 行不参与推断管线，不纳入候选；
/// - 单条改写失败记 warn 并跳过（不上抛、不计入），其余条目照常改写。
pub(crate) async fn allow_inference(
    engine: &Engine,
    uid: &str,
    labels: &[String],
) -> RamariaResult<usize> {
    let traits = engine.storage_ref().list_traits_by_persona(uid).await?;

    let candidates: Vec<_> = traits
        .iter()
        .filter(|t| t.source == TraitSource::Manual && t.status == TraitStatus::Active)
        .filter(|t| labels.is_empty() || labels.contains(&t.trait_label))
        .collect();

    let mut updated = 0usize;
    for t in candidates {
        match engine
            .storage_ref()
            .update_trait_source(t.id, TraitSource::Inferred)
            .await
        {
            Ok(_) => updated += 1,
            Err(e) => {
                warn!(
                    trait_id = t.id,
                    trait_label = %t.trait_label,
                    error = %e,
                    "接管改写失败（跳过）"
                );
            }
        }
    }

    info!(
        persona_uid = %uid,
        requested_labels = labels.len(),
        updated,
        "接管完成"
    );

    Ok(updated)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
