//! crates/ramaria-service/src/proactive/roster.rs - Ramaria 主动消息名单用例模块
//!
//! 设计特点:
//! - 名单读用例：活跃人格 + 开关状态 + 对话解锁状态 + 生效结论（实时计算，不物化写入）
//! - 名单写用例：值域 / uid 存在 / user 类硬排除三项校验通过后写开关
//! - 生效判定单点：非 user 类 AND 全局总开关 AND 人格开关有效值
//! - 排序沿用人格列表口径（存储层稳定排序 kind, seq）
//! - 日志只记人格标识与开关值等元数据，不记消息内容

use serde::{Deserialize, Serialize};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;

use crate::engine::Engine;

use super::switch::{self, ProactivePersonaMode};

// =========================================================
// 视图
// =========================================================

/// 主动消息名单条目（读用例返回）。
///
/// 字段约定:
/// - `uid` / `name` / `kind`: 人格身份（kind 为稳定字符串标识）；
/// - `has_local_dialogue`: 本地用户消息存在性（"对话一次"解锁判定，排除导入）；
/// - `mode`: 三态文本 auto / on / off；
/// - `effective`: 生效结论（非 user 类 AND 全局总开关 AND 开关有效值）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProactivePersonaView {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub has_local_dialogue: bool,
    pub mode: String,
    pub effective: bool,
}

// =========================================================
// 读用例
// =========================================================

/// 列出活跃人格的主动开关状态（实时计算，不落库）。
///
/// 参数:
/// - `engine`: 服务层引擎（配置快照与存储句柄来源）。
///
/// 返回:
/// - 按存储层人格列表排序（kind, seq）的名单条目；任一查询失败上抛。
pub(crate) async fn list_personas(engine: &Engine) -> RamariaResult<Vec<ProactivePersonaView>> {
    let global_enabled = engine.config().proactive.enabled;
    let storage: &dyn StorageBackend = engine.storage_ref().as_ref();
    let personas = storage.list_personas().await?;
    let mut views = Vec::with_capacity(personas.len());
    for persona in personas {
        let has_local_dialogue = storage
            .has_local_user_message_by_persona(&persona.uid)
            .await?;
        let mode = switch::load_mode(storage, &persona.uid).await?;
        let switch_on = match mode {
            ProactivePersonaMode::On => true,
            ProactivePersonaMode::Off => false,
            // 自动：有本地对话才解锁
            ProactivePersonaMode::Auto => has_local_dialogue,
        };
        // user 类硬排除先于全局总开关与人格开关：即使强开也不生效
        let effective = !switch::is_user_persona(&persona) && global_enabled && switch_on;
        views.push(ProactivePersonaView {
            uid: persona.uid,
            name: persona.name,
            kind: persona.kind.as_str().to_string(),
            has_local_dialogue,
            mode: mode.as_str().to_string(),
            effective,
        });
    }
    tracing::debug!(count = views.len(), "主动消息名单：读取完成");
    Ok(views)
}

// =========================================================
// 写用例
// =========================================================

/// 保存指定人格的主动开关（校验 → 写 settings 键）。
///
/// 校验顺序:
/// - uid 非空 → mode 值域（trim 后精确匹配 auto / on / off）→ uid 存在 → 非 user 类。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 人格业务标识（两侧空白容忍）。
/// - `mode`: 开关文本（auto / on / off，两侧空白容忍）。
///
/// 返回:
/// - 校验失败为业务校验错误（原文直出到前端）；查询 / 写入失败为存储错误。
pub(crate) async fn set_persona_mode(engine: &Engine, uid: &str, mode: &str) -> RamariaResult<()> {
    let uid = uid.trim();
    if uid.is_empty() {
        return Err(RamariaError::validation("人格 UID 不能为空"));
    }
    let Some(parsed) = ProactivePersonaMode::parse(mode) else {
        return Err(RamariaError::validation(format!(
            "主动开关值非法: {mode}（可选 auto / on / off）"
        )));
    };
    let storage: &dyn StorageBackend = engine.storage_ref().as_ref();
    let persona = storage
        .get_persona_by_uid(uid)
        .await?
        .ok_or_else(|| RamariaError::validation(format!("人格不存在: uid={uid}")))?;
    if switch::is_user_persona(&persona) {
        return Err(RamariaError::validation("用户人格不参与主动对话"));
    }
    switch::save_mode(storage, uid, parsed).await?;
    tracing::info!(uid = %uid, mode = parsed.as_str(), "主动消息名单：开关已更新");
    Ok(())
}

#[cfg(test)]
mod tests;
