//! crates/ramaria-service/src/withdraw/local.rs - Ramaria 本地消息撤回
//!
//! 设计特点:
//! - 仅本地会话：社交平台会话的消息由平台侧撤回，服务层拒绝本地撤回
//! - 仅本人消息：只允许撤回用户自己发送的消息（助手消息不可撤回）
//! - 窗口约束：与通道反应窗口同口径（2 分钟）
//! - 幂等：已撤回或标记未命中统一按校验错误提示

use tracing::info;
use uuid::Uuid;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{MessageRole, is_social_channel};

use crate::engine::Engine;

use super::RECALL_REACTION_WINDOW_MS;

/// 撤回一条本地消息（仅本地会话、本人消息、窗口内）。
///
/// 流程:
/// 1. 消息定位（不存在 → 校验错误）；
/// 2. 会话校验（不存在 / 社交平台会话 → 校验错误）；
/// 3. 角色校验（仅用户消息可撤回）；
/// 4. 状态校验（已撤回 / 超窗口 → 校验错误）；
/// 5. 标记撤回（行未命中兜底校验错误）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `message_id`: 目标消息 id。
/// - `now`: 撤回时刻（Unix 毫秒）。
///
/// 返回:
/// - 标记成功返回 `Ok(())`；全部校验失败为校验错误。
pub(crate) async fn recall_local_message(
    engine: &Engine,
    message_id: Uuid,
    now: i64,
) -> RamariaResult<()> {
    let storage = engine.storage_ref().as_ref();

    let Some(message) = storage.get_message(message_id).await? else {
        return Err(RamariaError::validation("消息不存在"));
    };
    let Some(session) = storage.get_session(message.session_id).await? else {
        return Err(RamariaError::validation("会话不存在"));
    };
    if is_social_channel(&session.channel) {
        return Err(RamariaError::validation(
            "社交平台会话的消息请在对应平台撤回",
        ));
    }
    if message.role != MessageRole::User {
        return Err(RamariaError::validation("只能撤回自己发送的消息"));
    }
    if message.recalled_at.is_some() {
        return Err(RamariaError::validation("消息已撤回"));
    }
    if now.saturating_sub(message.created_at) > RECALL_REACTION_WINDOW_MS {
        return Err(RamariaError::validation("已超过 2 分钟撤回窗口"));
    }

    if !storage.mark_message_recalled_by_id(message_id, now).await? {
        return Err(RamariaError::validation("消息已撤回"));
    }
    info!(
        session_id = %message.session_id,
        message_id = %message_id,
        "本地消息已撤回"
    );
    Ok(())
}
