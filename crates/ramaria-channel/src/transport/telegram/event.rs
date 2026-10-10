//! crates/ramaria-channel/src/transport/telegram/event.rs - Telegram 事件解析模块
//!
//! 设计特点:
//! - 更新 JSON 手动提取：平台新增字段自动容忍，不做强类型反序列化，
//!   避免扩展字段引发整帧失败
//! - 只处理顶层 `message`（`edited_message` / `channel_post` / `callback_query` 等静默忽略）
//! - 群语义：隐私模式下天然只收 @ 与回复；@ 判定读 `entities`（mention / text_mention /
//!   bot_command 三种形态），被回复判定读 `reply_to_message.from` 与 Bot 身份比对
//! - 实体切片按 UTF-16 单元偏移提取（平台口径），非法边界不 panic
//! - 段序列：文本 → 文本段；图片（photo 最大尺寸项 / 图片类 document）→ 图片段，
//!   caption 追加为文本段（先图后文）；其余媒体忽略
//! - 图片段的下载引用填放置文件标识（下载经 `getFile` 两步，媒体下载器据此工作）

use ramaria_core::types::{InboundSender, now_ms};
use serde_json::Value;

use super::BotIdentity;
use crate::transport::{InboundSegment, TransportEvent};

// =========================================================
// 更新入口
// =========================================================

/// 解析一条 `getUpdates` 更新为规范入站事件。
///
/// 规则:
/// - 顶层无 `message`（编辑消息 / 频道消息 / 回调查询等）→ None；
/// - 聊天类型为 `private` / `group` / `supergroup` 之外 → None；
/// - 缺少发送者身份或聊天 ID → None；
/// - 无文本也无可下载媒体（服务消息等）→ None。
///
/// 参数:
/// - `update`: 单条更新（含 `update_id` 的原始对象）。
/// - `bot`: Bot 身份（@ 提及与被动回复判定）。
///
/// 返回:
/// - 规范入站事件；非消息类更新或无内容时为 None。
pub(crate) fn parse_update(update: &Value, bot: &BotIdentity) -> Option<TransportEvent> {
    let Some(message) = update.get("message") else {
        tracing::debug!(channel = "telegram", "非 message 更新，忽略");
        return None;
    };

    let chat = message.get("chat")?;
    let chat_id = chat.get("id").and_then(Value::as_i64)?;
    let external_ref = match chat.get("type").and_then(Value::as_str) {
        Some("private") => format!("c2c:{chat_id}"),
        Some("group") | Some("supergroup") => format!("group:{chat_id}"),
        other => {
            tracing::warn!(
                channel = "telegram",
                chat_type = other.unwrap_or("none"),
                "未知聊天类型，忽略"
            );
            return None;
        }
    };

    let sender = parse_sender(message.get("from"))?;
    let created_at = parse_created_at(message);
    let platform_message_id = message.get("message_id").and_then(value_to_string);
    let reply_to = message
        .get("reply_to_message")
        .and_then(|reply| reply.get("message_id"))
        .and_then(value_to_string);

    let is_private = external_ref.starts_with("c2c:");
    let should_respond = if is_private {
        true
    } else {
        mention_self(message, bot) || reply_to_self(message, bot)
    };

    let segments = parse_segments(message);
    if segments.is_empty() {
        tracing::debug!(channel = "telegram", "消息无可展示内容，忽略");
        return None;
    }

    Some(TransportEvent {
        external_ref,
        sender,
        platform_message_id,
        reply_to,
        created_at,
        segments,
        should_respond,
        self_id: Some(bot.id.clone()),
    })
}

// =========================================================
// 字段解析
// =========================================================

/// 解析消息时间：`date` 为秒，转 Unix 毫秒；缺失或非正数回退当前时间。
fn parse_created_at(message: &Value) -> i64 {
    let seconds = message
        .get("date")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0);
    match seconds {
        Some(seconds) => seconds.saturating_mul(1000),
        None => now_ms(),
    }
}

/// 解析发送者身份（缺失发送者 ID 时返回 None）。
///
/// 口径:
/// - `platform_id` / `uin` 取 `from.id`（数字归一为字符串）；
/// - `display_name` 为 `first_name` 与 `last_name` 的 trim 拼接（单个缺失时不留多余空格）；
/// - `group_nickname` / `role` 平台无对应概念，恒为 None。
fn parse_sender(from: Option<&Value>) -> Option<InboundSender> {
    let from = from?;
    let account = from
        .get("id")
        .and_then(value_to_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let first_name = from
        .get("first_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let last_name = from
        .get("last_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let display_name = match (first_name.is_empty(), last_name.is_empty()) {
        (true, true) => String::new(),
        (false, true) => first_name.to_string(),
        (true, false) => last_name.to_string(),
        (false, false) => format!("{first_name} {last_name}"),
    };
    Some(InboundSender {
        platform_id: account.clone(),
        uin: Some(account),
        display_name,
        group_nickname: None,
        role: None,
    })
}

/// @ 提及判定（群聊）：mention / text_mention / bot_command 三种实体形态任一命中。
fn mention_self(message: &Value, bot: &BotIdentity) -> bool {
    let text = message.get("text").and_then(Value::as_str).unwrap_or("");
    let Some(entities) = message.get("entities").and_then(Value::as_array) else {
        return false;
    };
    let mention = format!("@{}", bot.username);
    for entity in entities {
        let entity_type = entity.get("type").and_then(Value::as_str).unwrap_or("");
        match entity_type {
            // 用户名提及：实体切片即 `@{username}`（大小写不敏感）
            "mention" => {
                if bot.username.is_empty() {
                    continue;
                }
                let slice = entity_slice(text, entity);
                if slice.is_some_and(|slice| slice.eq_ignore_ascii_case(&mention)) {
                    return true;
                }
            }
            // 文本提及：实体携带用户对象，比对 Bot ID
            "text_mention" => {
                let user_id = entity
                    .get("user")
                    .and_then(|user| user.get("id"))
                    .and_then(value_to_string);
                if user_id.as_deref() == Some(bot.id.as_str()) {
                    return true;
                }
            }
            // 群命令（`/start@bot`）：实体切片以 `@{username}` 结尾
            "bot_command" => {
                if bot.username.is_empty() {
                    continue;
                }
                let slice = entity_slice(text, entity);
                if slice.is_some_and(|slice| ends_with_ignore_ascii_case(slice, &mention)) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// 被动回复判定：被回复消息的发送者为 Bot 自身。
fn reply_to_self(message: &Value, bot: &BotIdentity) -> bool {
    message
        .get("reply_to_message")
        .and_then(|reply| reply.get("from"))
        .and_then(|from| from.get("id"))
        .and_then(value_to_string)
        .is_some_and(|user_id| user_id == bot.id)
}

/// 实体切片：按 UTF-16 单元偏移与长度从文本提取；非法边界返回 None。
fn entity_slice<'a>(text: &'a str, entity: &Value) -> Option<&'a str> {
    let offset = entity.get("offset").and_then(Value::as_u64)? as usize;
    let length = entity.get("length").and_then(Value::as_u64)? as usize;
    slice_utf16(text, offset, length)
}

/// 按 UTF-16 单元偏移与长度精确切片；任一边界不在字符边界上返回 None。
fn slice_utf16(text: &str, offset: usize, length: usize) -> Option<&str> {
    let end_units = offset.checked_add(length)?;
    let mut units = 0usize;
    let mut start = (offset == 0).then_some(0);
    let mut end = (end_units == 0).then_some(0);
    for (index, ch) in text.char_indices() {
        units += ch.len_utf16();
        let boundary = index + ch.len_utf8();
        if units == offset {
            start = Some(boundary);
        }
        if units == end_units {
            end = Some(boundary);
        }
    }
    let (start, end) = (start?, end?);
    (start <= end).then(|| &text[start..end])
}

/// 判断文本是否以指定后缀结尾（ASCII 大小写不敏感；尾部非字符边界返回 false）。
fn ends_with_ignore_ascii_case(text: &str, suffix: &str) -> bool {
    text.len()
        .checked_sub(suffix.len())
        .and_then(|start| text.get(start..))
        .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
}

// =========================================================
// 段序列
// =========================================================

/// 解析消息段序列（文本与可下载媒体；先图后文）。
///
/// 规则:
/// - `text` 非空 → 文本段；
/// - `photo` 非空数组 → 取 `file_size` 最大项的 `file_id` 为图片段；
/// - `document` 且 `mime_type` 以 `image/` 开头 → 图片段；
/// - 图片段存在且 `caption` 非空 → 追加文本段（先图后文）；
/// - 其余媒体（非图片 document / video / voice / animation / sticker 等）忽略。
fn parse_segments(message: &Value) -> Vec<InboundSegment> {
    let mut segments = Vec::new();

    let text = message.get("text").and_then(Value::as_str).unwrap_or("");
    if !text.is_empty() {
        segments.push(InboundSegment::Text(text.to_string()));
    }

    let image = parse_photo(message).or_else(|| parse_image_document(message));
    if let Some(segment) = image {
        segments.push(segment);
        let caption = message
            .get("caption")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !caption.is_empty() {
            segments.push(InboundSegment::Text(caption.to_string()));
        }
    }

    segments
}

/// photo 数组取 `file_size` 最大项为图片段。
fn parse_photo(message: &Value) -> Option<InboundSegment> {
    let photos = message.get("photo").and_then(Value::as_array)?;
    let best = photos.iter().max_by_key(|photo| {
        photo
            .get("file_size")
            .and_then(Value::as_u64)
            .unwrap_or_default()
    })?;
    let file_id = best
        .get("file_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(InboundSegment::Image {
        url: file_id.to_string(),
        size: best.get("file_size").and_then(Value::as_u64),
        sub_type: None,
    })
}

/// 图片类 document（`mime_type` 以 `image/` 开头）为图片段。
fn parse_image_document(message: &Value) -> Option<InboundSegment> {
    let document = message.get("document")?;
    let mime_type = document
        .get("mime_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !mime_type.starts_with("image/") {
        return None;
    }
    let file_id = document
        .get("file_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(InboundSegment::Image {
        url: file_id.to_string(),
        size: document.get("file_size").and_then(Value::as_u64),
        sub_type: None,
    })
}

/// 归一 JSON 取值为字符串：数字转十进制串、字符串原样；其余为 None。
fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
