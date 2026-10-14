//! crates/ramaria-service/src/chat/social.rs - Ramaria 社交会话判定辅助模块
//!
//! 设计特点:
//! - 社交通道判定与只读拒绝文案单点：QQ / Telegram 会话的历史前缀与生成门禁共用
//! - 发言人前缀口径与 L1 逐目标投影同构（`{sender_name}：` 全角冒号）
//! - 群聊判定对齐通道 external_ref 命名约定（`group:*` 群聊 / `c2c:*` 私聊）

use ramaria_core::error::RamariaError;
use ramaria_core::types::{Message, MessageRole, Session, is_social_channel};

/// 社交群聊外部标识前缀（与通道 external_ref 命名约定对齐：`c2c:*` 私聊 / `group:*` 群聊）。
const SOCIAL_GROUP_PREFIX: &str = "group:";

/// 判断会话是否为社交平台群聊（社交通道 + `group:` 外部标识）。
///
/// 参数:
/// - `session`: 目标会话。
///
/// 返回:
/// - 社交通道且外部标识为群聊形态时 true；私聊 / 非社交通道为 false。
pub(super) fn is_group_conversation(session: &Session) -> bool {
    is_social_channel(&session.channel)
        && session
            .external_ref
            .as_deref()
            .is_some_and(|reference| reference.starts_with(SOCIAL_GROUP_PREFIX))
}

/// 社交会话历史消息的发言人前缀（`{sender_name}：`）。
///
/// 口径:
/// - 仅 `role=User` 且 `sender_name`（trim 后）非空的消息带前缀；
/// - 其余（assistant 消息 / 发言人缺失或空白）返回 None（正文零变化）。
///
/// 参数:
/// - `message`: 历史消息。
///
/// 返回:
/// - 前缀文本（不含尾随正文）；不适用时为 None。
pub(super) fn sender_prefix(message: &Message) -> Option<&str> {
    if message.role != MessageRole::User {
        return None;
    }
    message
        .sender_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

/// 构造社交平台会话的只读拒绝错误（客户端本地生成与绕过通道的调用共用同一文案）。
///
/// 参数:
/// - `channel`: 会话来源通道标识。
///
/// 返回:
/// - `Validation` 错误（提示仅可查看、需在对应平台回复）。
pub(super) fn readonly_error(channel: &str) -> RamariaError {
    RamariaError::validation(format!(
        "会话来自社交平台（{channel}），仅可查看；请在对应平台回复。"
    ))
}
