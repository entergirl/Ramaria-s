//! crates/ramaria-service/src/persona/update.rs - Ramaria 人格基本信息更新与系统用户人格模块
//!
//! 设计特点:
//! - 信息更新按"None 保持、Some 覆盖"语义传递；空描述表达清空（与 None 行为不同）
//! - 配置内容（`config`）不由本模块修改（由人格文件导入通道管理），显式保持旧值
//! - 更新后回读完整视图；更新成功后记录消失属异常状态，显式报错而非静默
//! - 系统用户人格（user-0001）保障为幂等用例：已存在时不做任何写入
//! - 隐私：日志中的个人标识经 `mask_id` 脱敏

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::types::{Persona, PersonaKind};

use crate::engine::Engine;
use crate::types::{PersonaFullView, PersonaUpdateRequest};

use super::view::full_view;

// =========================================================
// 人格管理用例（基本信息更新 / 系统用户人格）
// =========================================================

/// 更新人格基本信息（名称 / 头像 / 描述）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 人格 uid（不可变更）。
/// - `req`: 更新请求（各字段可选；`None` 保持旧值，空描述表达清空）。
///
/// 返回:
/// - 更新后回读的完整视图；
/// - uid 为空 / 人格不存在返回 `Validation` 错误。
///
/// 说明:
/// - 配置内容（`config`）不由本用例修改（由人格文件导入通道管理），显式保持旧值。
pub(crate) async fn update_info(
    engine: &Engine,
    uid: &str,
    req: PersonaUpdateRequest,
) -> RamariaResult<PersonaFullView> {
    if uid.trim().is_empty() {
        return Err(RamariaError::validation("人格 UID 不能为空"));
    }

    let storage = engine.storage_ref();
    let existing = match storage.get_persona_by_uid(uid).await? {
        Some(persona) => persona,
        None => {
            return Err(RamariaError::validation(format!("人格不存在: uid={uid}")));
        }
    };

    // 名称未提供时沿用旧值；头像 / 描述按"None 保持、Some 覆盖"语义传递
    let new_name = req.name.as_deref().unwrap_or(&existing.name);
    let new_avatar = req.avatar.as_deref();
    let new_config: Option<&str> = None;
    let new_description = req.description.as_deref();

    storage
        .update_persona(uid, new_name, new_avatar, new_config, new_description)
        .await?;

    tracing::info!(
        uid = %mask_id(uid),
        name_changed = req.name.is_some(),
        avatar_changed = req.avatar.is_some(),
        description_changed = req.description.is_some(),
        "人格信息已更新"
    );

    let updated = match storage.get_persona_by_uid(uid).await? {
        Some(persona) => persona,
        None => {
            // 更新成功后记录消失属异常状态，显式报错而非静默
            return Err(RamariaError::storage(format!(
                "更新后 persona 意外不存在: uid={uid}"
            )));
        }
    };
    Ok(full_view(updated))
}

/// 确保系统用户人格（user-0001）存在（幂等）。
///
/// 返回:
/// - `Ok(true)`: 本次创建了 user-0001；
/// - `Ok(false)`: 已存在，未做任何写入。
pub(crate) async fn ensure_user(engine: &Engine) -> RamariaResult<bool> {
    let storage = engine.storage_ref();
    if storage.get_persona_by_uid("user-0001").await?.is_some() {
        tracing::debug!("user-0001 已存在，跳过创建");
        return Ok(false);
    }

    let user = Persona::new(
        "user-0001".to_string(),
        "用户".to_string(),
        PersonaKind::User,
        1,
        "system".to_string(),
    );
    storage.create_persona(&user).await?;
    tracing::info!("已创建 persona: user-0001 (用户)");
    Ok(true)
}
