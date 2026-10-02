//! crates/ramaria-service/src/browse/profile.rs - Ramaria L3 三层画像与画像数据状态
//!
//! 设计特点:
//! - 三层画像：按 base / primary / accent 分组，仅保留生效（Active）标签，层内按 seq 升序
//! - 画像状态：有效样本量 = 生效标签 evidence 之和，三档阈值（5 / 20）输出可信度文本
//! - 人格存在性校验：不存在或空白 uid 返回业务校验错误
//! - 只读：不修改任何状态

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{PersonalityTrait, TraitLayer, TraitStatus};

use crate::engine::Engine;
use crate::types::{PersonalityProfileView, ProfileStatusView};

use super::view::{require_persona_uid, trait_detail_view};

// =========================================================
// L3 三层画像
// =========================================================

/// L3 三层性格画像（按 base / primary / accent 分组）。
///
/// 流程:
/// 1. 校验人格 uid 并确认人格存在（不存在 → Validation 错误）；
/// 2. 读取该人格全部性格标签，仅保留生效（Active）标签；
/// 3. 按分层归组，层内按 `seq` 升序。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona_uid`: 目标人格 uid。
///
/// 返回:
/// - 三层画像视图；无人格画像时三层均为空数组（非错误）。
pub(crate) async fn personality_profile(
    engine: &Engine,
    persona_uid: &str,
) -> RamariaResult<PersonalityProfileView> {
    let uid = require_persona_uid(persona_uid)?;
    let storage = engine.storage_ref();
    if storage.get_persona_by_uid(&uid).await?.is_none() {
        return Err(RamariaError::validation(format!("人格不存在: uid={uid}")));
    }

    let traits = storage.list_traits_by_persona(&uid).await?;
    let mut base = Vec::new();
    let mut primary = Vec::new();
    let mut accent = Vec::new();
    for t in &traits {
        if t.status != TraitStatus::Active {
            continue;
        }
        let view = trait_detail_view(t);
        match t.layer {
            TraitLayer::Base => base.push(view),
            TraitLayer::Primary => primary.push(view),
            TraitLayer::Accent => accent.push(view),
            // 未知分层（non_exhaustive）走此分支静默跳过
            _ => {}
        }
    }
    base.sort_by_key(|v| v.seq);
    primary.sort_by_key(|v| v.seq);
    accent.sort_by_key(|v| v.seq);

    tracing::debug!(
        %uid,
        base = base.len(),
        primary = primary.len(),
        accent = accent.len(),
        "L3 三层画像读取完成"
    );
    Ok(PersonalityProfileView {
        persona_uid: uid,
        base,
        primary,
        accent,
    })
}

// =========================================================
// 画像数据状态
// =========================================================

/// 画像数据状态（有效样本量与可信度区间）。
///
/// 判定口径:
/// - `n_total_eff` = 生效标签的 `evidence` 之和；
/// - `< 5` → `insufficient`；`5..20` → `preliminary`；`≥ 20` → `trusted`。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona_uid`: 目标人格 uid。
///
/// 返回:
/// - 有效样本量、生效标签数与状态文本；人格不存在时返回 Validation 错误。
pub(crate) async fn profile_status(
    engine: &Engine,
    persona_uid: &str,
) -> RamariaResult<ProfileStatusView> {
    let uid = require_persona_uid(persona_uid)?;
    let storage = engine.storage_ref();
    if storage.get_persona_by_uid(&uid).await?.is_none() {
        return Err(RamariaError::validation(format!("人格不存在: uid={uid}")));
    }

    let traits = storage.list_traits_by_persona(&uid).await?;
    let active: Vec<&PersonalityTrait> = traits
        .iter()
        .filter(|t| matches!(t.status, TraitStatus::Active))
        .collect();
    let n_total_eff: f64 = active.iter().map(|t| t.evidence).sum();
    let active_count = active.len();

    let (status, status_text) = if n_total_eff < 5.0 {
        (
            "insufficient",
            format!("数据不足（有效样本量: {n_total_eff:.1}）—— 继续对话以积累更多数据"),
        )
    } else if n_total_eff < 20.0 {
        (
            "preliminary",
            format!("初步画像（有效样本量: {n_total_eff:.1}）—— 画像有一定参考价值，建议继续积累"),
        )
    } else {
        (
            "trusted",
            format!("可信画像（有效样本量: {n_total_eff:.1}，共 {active_count} 项性格标签）"),
        )
    };

    tracing::debug!(%uid, n_total_eff, active_count, status, "画像数据状态读取完成");
    Ok(ProfileStatusView {
        persona_uid: uid,
        n_total_eff,
        active_trait_count: active_count,
        status: status.to_string(),
        status_text,
    })
}
