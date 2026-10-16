//! crates/ramaria-channel/src/transport/onebot/event.rs - OneBot 事件解析模块
//!
//! 设计特点:
//! - 事件 JSON 手动提取：NapCat 扩展字段（message_seq / real_id 等）自动容忍，
//!   不做强类型反序列化，避免扩展字段引发整帧失败
//! - 类型归一：`self_id` / `user_id` / `group_id` / `time` 为数字，
//!   `at.qq` / `reply.id` 为字符串；统一转字符串比较与存储
//! - message 事件解析为规范入站事件；notice 撤回事件解析为撤回通知；
//!   meta_event / request 及其他 notice 静默忽略
//! - At 段在解析层折叠为文本（`@qq` / `@全体成员`）；reply 段只用于判定不进正文
//! - 表情段（face / mface / bface）折叠为 `[表情]` 占位文本，避免表情消息整条丢弃
//! - 图片段保留下载引用（url / size / sub_type），正文与附件在组装层产出

use ramaria_core::types::{InboundSender, MemberRole, now_ms};
use serde_json::Value;

use super::api::{SentMessageCache, value_to_string};
use crate::events::RecallNotice;
use crate::transport::{InboundSegment, TransportEvent};

// =========================================================
// 事件入口
// =========================================================

/// 解析一条 OneBot 上报帧为规范入站事件。
///
/// 规则:
/// - `post_type != "message"`（meta_event / notice / request 等）→ None；
/// - `message_type` 为 `private` / `group` 之外 → None；
/// - 缺失 `message` 数组（非数组）→ None（无内容可写）；
/// - JSON 解析失败 / 关键字段缺失 → None（记日志，不 panic）。
///
/// 参数:
/// - `json`: 上报帧原文。
/// - `sent_ids`: 本机已发消息 ID 缓存（判定「回复本机消息」）。
///
/// 返回:
/// - 规范入站事件；非 message 事件或无内容时为 None。
pub(crate) fn parse_event(json: &str, sent_ids: &SentMessageCache) -> Option<TransportEvent> {
    let root: Value = match serde_json::from_str(json) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(channel = "qq", error = %error, "OneBot 上报帧 JSON 解析失败，忽略");
            return None;
        }
    };

    if root.get("post_type").and_then(Value::as_str) != Some("message") {
        tracing::debug!(channel = "qq", "非 message 事件，忽略");
        return None;
    }

    let message_type = root.get("message_type").and_then(Value::as_str);
    let self_id = root.get("self_id").and_then(value_to_string);
    let created_at = parse_created_at(&root);

    // 会话外部标识（前缀隔离私聊与群聊的 ID 空间）
    let external_ref = match message_type {
        Some("private") => match root.get("user_id").and_then(value_to_string) {
            Some(user_id) if !user_id.trim().is_empty() => format!("c2c:{}", user_id.trim()),
            _ => {
                tracing::warn!(channel = "qq", "私聊事件缺少发送者账号，忽略");
                return None;
            }
        },
        Some("group") => match root.get("group_id").and_then(value_to_string) {
            Some(group_id) if !group_id.trim().is_empty() => format!("group:{}", group_id.trim()),
            _ => {
                tracing::warn!(channel = "qq", "群聊事件缺少群号，忽略");
                return None;
            }
        },
        other => {
            tracing::warn!(
                channel = "qq",
                message_type = other.unwrap_or("none"),
                "未知消息类型，忽略"
            );
            return None;
        }
    };

    let Some(segments_value) = root.get("message") else {
        tracing::warn!(channel = "qq", "message 事件缺少消息段数组，忽略");
        return None;
    };
    let Some(segment_array) = segments_value.as_array() else {
        tracing::warn!(channel = "qq", "message 字段不是数组，忽略");
        return None;
    };

    let user_id = root.get("user_id").and_then(value_to_string);
    let sender = parse_sender(root.get("sender"), user_id.as_deref());

    let mut segments = Vec::with_capacity(segment_array.len());
    let mut reply_to: Option<String> = None;
    let mut at_self = false;
    for segment in segment_array {
        parse_segment(
            segment,
            self_id.as_deref(),
            &mut segments,
            &mut reply_to,
            &mut at_self,
        );
    }

    let reply_to_self = reply_to
        .as_deref()
        .is_some_and(|message_id| sent_ids.contains(message_id));
    let should_respond = match message_type {
        Some("private") => true,
        _ => at_self || reply_to_self,
    };

    let platform_message_id = root.get("message_id").and_then(value_to_string);
    Some(TransportEvent {
        external_ref,
        sender,
        platform_message_id,
        reply_to,
        created_at,
        segments,
        should_respond,
        self_id,
    })
}

/// 解析一条 OneBot 上报帧为撤回通知。
///
/// 规则:
/// - JSON 解析失败 → None（记日志，不 panic）；
/// - `post_type != "notice"` → None；
/// - `notice_type` 仅 `friend_recall` / `group_recall`，其余（如 notify）→ None；
/// - `message_id` 必填（缺失 / 空白视为缺失）；
/// - `friend_recall`: 取 `user_id` 作为会话账号与撤回操作者；
/// - `group_recall`: 取 `group_id` 作为会话群号，操作者优先取 `operator_id`，
///   缺失回退 `user_id`；
/// - `time`（秒）缺失 / 非正 → 当前时刻。
///
/// 参数:
/// - `json`: 上报帧原文。
///
/// 返回:
/// - 规范撤回通知；非撤回事件或关键字段缺失时为 None。
pub(crate) fn parse_notice(json: &str) -> Option<RecallNotice> {
    let root: Value = match serde_json::from_str(json) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(channel = "qq", error = %error, "OneBot 上报帧 JSON 解析失败，忽略");
            return None;
        }
    };

    if root.get("post_type").and_then(Value::as_str) != Some("notice") {
        return None;
    }
    let notice_type = root.get("notice_type").and_then(Value::as_str);
    if !matches!(notice_type, Some("friend_recall") | Some("group_recall")) {
        tracing::debug!(
            channel = "qq",
            notice_type = notice_type.unwrap_or("none"),
            "非撤回通知，忽略"
        );
        return None;
    }

    let Some(message_id) = id_field(&root, "message_id") else {
        tracing::warn!(channel = "qq", "撤回通知缺少消息 ID，忽略");
        return None;
    };

    let (external_ref, operator_id) = match notice_type {
        Some("friend_recall") => {
            let Some(user_id) = id_field(&root, "user_id") else {
                tracing::warn!(channel = "qq", "私聊撤回通知缺少账号，忽略");
                return None;
            };
            (format!("c2c:{user_id}"), Some(user_id))
        }
        _ => {
            let Some(group_id) = id_field(&root, "group_id") else {
                tracing::warn!(channel = "qq", "群聊撤回通知缺少群号，忽略");
                return None;
            };
            let operator = id_field(&root, "operator_id").or_else(|| id_field(&root, "user_id"));
            (format!("group:{group_id}"), operator)
        }
    };

    Some(RecallNotice {
        channel: "qq",
        external_ref,
        platform_message_id: message_id,
        operator_id,
        self_id: root.get("self_id").and_then(value_to_string),
        occurred_at: parse_created_at(&root),
    })
}

// =========================================================
// 字段解析
// =========================================================

/// 读取必填标识字段：归一为字符串并去两侧空白，缺失或空白返回 None。
fn id_field(root: &Value, key: &str) -> Option<String> {
    root.get(key)
        .and_then(value_to_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 解析消息时间：`time` 为秒，转 Unix 毫秒；缺失或非正数回退当前时间。
fn parse_created_at(root: &Value) -> i64 {
    let seconds = root
        .get("time")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0);
    match seconds {
        Some(seconds) => seconds.saturating_mul(1000),
        None => now_ms(),
    }
}

/// 解析发送者身份（整段缺失时全部取默认值）。
///
/// 口径:
/// - `platform_id` / `uin` 取顶层 `user_id`；
/// - `display_name` 优先群名片 `card`，其次昵称 `nickname`（都缺为空串）；
/// - `group_nickname` 仅在 `card` 非空时有值；`role` 解析失败为 None。
fn parse_sender(sender: Option<&Value>, user_id: Option<&str>) -> InboundSender {
    let account = user_id.unwrap_or_default().trim().to_string();
    let card = sender
        .and_then(|value| value.get("card"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let nickname = sender
        .and_then(|value| value.get("nickname"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let display_name = if card.is_empty() { nickname } else { card };
    let group_nickname = if card.is_empty() {
        None
    } else {
        Some(card.to_string())
    };
    let role = sender
        .and_then(|value| value.get("role"))
        .and_then(Value::as_str)
        .and_then(MemberRole::parse);
    InboundSender {
        platform_id: account.clone(),
        uin: Some(account),
        display_name: display_name.to_string(),
        group_nickname,
        role,
    }
}

/// 解析单个消息段，追加到段序列并更新 reply / @ 判定。
fn parse_segment(
    segment: &Value,
    self_id: Option<&str>,
    segments: &mut Vec<InboundSegment>,
    reply_to: &mut Option<String>,
    at_self: &mut bool,
) {
    let data = segment.get("data");
    match segment.get("type").and_then(Value::as_str) {
        Some("text") => {
            if let Some(text) = data
                .and_then(|value| value.get("text"))
                .and_then(Value::as_str)
            {
                segments.push(InboundSegment::Text(text.to_string()));
            }
        }
        Some("image") => {
            let url = data
                .and_then(|value| value.get("url"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if url.is_empty() {
                // 无下载地址的图片段：信息不全无法下载，按纯文本占位
                segments.push(InboundSegment::Text("[图片]".to_string()));
                return;
            }
            segments.push(InboundSegment::Image {
                url: url.to_string(),
                size: data
                    .and_then(|value| value.get("file_size"))
                    .and_then(value_as_u64),
                sub_type: data
                    .and_then(|value| value.get("sub_type"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        Some("at") => {
            let Some(qq) = data
                .and_then(|value| value.get("qq"))
                .and_then(value_to_string)
            else {
                return;
            };
            let qq = qq.trim();
            if qq.is_empty() {
                return;
            }
            if qq == "all" {
                segments.push(InboundSegment::Text("@全体成员".to_string()));
                return;
            }
            if self_id.is_some_and(|self_id| self_id.trim() == qq) {
                *at_self = true;
            }
            segments.push(InboundSegment::Text(format!("@{qq}")));
        }
        Some("reply") => {
            // 只取首个 reply 段（后续重复段不覆盖）；空 ID 视为未提供
            *reply_to = reply_to.take().or_else(|| {
                data.and_then(|value| value.get("id"))
                    .and_then(value_to_string)
                    .filter(|id| !id.trim().is_empty())
            });
        }
        // 表情段（经典 / 商城 / 原创）：折叠为占位文本，
        // 使纯表情消息正文非空（不被空正文丢弃）
        Some("face") | Some("mface") | Some("bface") => {
            segments.push(InboundSegment::Text("[表情]".to_string()));
        }
        // 其余段（record / video / file 等）忽略
        _ => {}
    }
}

/// 归一 JSON 取值为 u64：数字原样，字符串尝试解析；其余为 None。
fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
