//! crates/ramaria-channel/src/transport/telegram/event/tests.rs - Telegram 事件解析单元测试
//!
//! 设计特点:
//! - 样例驱动：覆盖私聊 / 群聊 / 回复 / 命令 / 图片 / 服务消息等真实更新形态
//! - 响应判定矩阵：私聊逐条；群聊 mention（用户名 / 文本提及 / 群命令）与
//!   被动回复三分支；回复他人与普通群消息不触发
//! - UTF-16 切片断言：非 BMP 字符在前时提及偏移仍按单元口径精确命中
//! - 容错路径断言：编辑消息 / 频道消息 / 服务消息 / 空内容 / 缺省字段均安全返回

use super::*;
use ramaria_core::types::now_ms;
use serde_json::json;

/// 固定 Bot 身份（id=42 / username=testbot）。
fn bot() -> BotIdentity {
    BotIdentity {
        id: "42".to_string(),
        username: "testbot".to_string(),
    }
}

/// 解析一条更新。
fn parse(update: Value) -> Option<TransportEvent> {
    parse_update(&update, &bot())
}

/// 私聊文本更新（`entities` 整个字段缺省）。
fn private_text(text: &str, message_id: i64) -> Value {
    json!({
        "update_id": 100,
        "message": {
            "message_id": message_id,
            "from": { "id": 10001, "is_bot": false, "first_name": "小明", "last_name": "王" },
            "chat": { "id": 10001, "first_name": "小明", "type": "private" },
            "date": 1_700_000_000,
            "text": text
        }
    })
}

/// 群聊更新外壳（正文与实体由调用方补充）。
fn group_message(text: &str, extra: Value) -> Value {
    let mut message = json!({
        "message_id": 200,
        "from": { "id": 10001, "is_bot": false, "first_name": "小明" },
        "chat": { "id": -1001234567890i64, "title": "测试群", "type": "supergroup" },
        "date": 1_700_000_001,
        "text": text
    });
    if let (Some(target), Some(source)) = (message.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    json!({ "update_id": 101, "message": message })
}

// =========================================================
// 私聊
// =========================================================

/// 私聊文本：会话标识 / 身份 / 时间 / 段序列逐项核对；entities 缺省容忍。
#[test]
fn private_text_event_fields() {
    let event = parse(private_text("你好", 55)).expect("应解析成功");
    assert_eq!(event.external_ref, "c2c:10001");
    assert_eq!(event.platform_message_id.as_deref(), Some("55"));
    assert_eq!(event.created_at, 1_700_000_000_000);
    assert_eq!(event.self_id.as_deref(), Some("42"));
    assert_eq!(event.sender.platform_id, "10001");
    assert_eq!(event.sender.uin.as_deref(), Some("10001"));
    assert_eq!(event.sender.display_name, "小明 王");
    assert_eq!(event.sender.group_nickname, None);
    assert_eq!(event.sender.role, None);
    assert_eq!(event.reply_to, None);
    assert_eq!(
        event.segments,
        vec![InboundSegment::Text("你好".to_string())]
    );
    assert!(event.should_respond, "私聊逐条响应");
}

/// 显示名拼接：单名、仅姓、全缺失三种形态。
#[test]
fn sender_display_name_variants() {
    let first_only = json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "from": { "id": 7, "first_name": "小明" },
            "chat": { "id": 7, "type": "private" },
            "date": 1,
            "text": "hi"
        }
    });
    let event = parse(first_only).expect("应解析成功");
    assert_eq!(event.sender.display_name, "小明");

    let last_only = json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "from": { "id": 8, "last_name": "王" },
            "chat": { "id": 8, "type": "private" },
            "date": 1,
            "text": "hi"
        }
    });
    let event = parse(last_only).expect("应解析成功");
    assert_eq!(event.sender.display_name, "王");

    let no_name = json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "from": { "id": 9 },
            "chat": { "id": 9, "type": "private" },
            "date": 1,
            "text": "hi"
        }
    });
    let event = parse(no_name).expect("应解析成功");
    assert_eq!(event.sender.display_name, "");
}

/// 缺发送者身份 / 聊天 ID → None。
#[test]
fn missing_sender_or_chat_is_none() {
    let no_from = json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "chat": { "id": 7, "type": "private" },
            "date": 1,
            "text": "hi"
        }
    });
    assert!(parse(no_from).is_none(), "缺发送者应忽略");

    let no_chat_id = json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "from": { "id": 7, "first_name": "小明" },
            "chat": { "type": "private" },
            "date": 1,
            "text": "hi"
        }
    });
    assert!(parse(no_chat_id).is_none(), "缺聊天 ID 应忽略");
}

// =========================================================
// 群语义
// =========================================================

/// 群内用户名提及：mention 实体切片等于 `@username`（大小写不敏感）→ 触发。
#[test]
fn group_mention_username_triggers() {
    let update = group_message(
        "@TESTBOT 在吗",
        json!({ "entities": [{ "offset": 0, "length": 8, "type": "mention" }] }),
    );
    let event = parse(update).expect("应解析成功");
    assert_eq!(event.external_ref, "group:-1001234567890");
    assert!(event.should_respond, "提及 Bot 用户名应触发响应");
    assert_eq!(
        event.segments,
        vec![InboundSegment::Text("@TESTBOT 在吗".to_string())]
    );
}

/// 群内提及他人 / 普通群消息 → 不触发。
#[test]
fn group_mention_other_or_plain_not_trigger() {
    let other = group_message(
        "@otherbot 在吗",
        json!({ "entities": [{ "offset": 0, "length": 9, "type": "mention" }] }),
    );
    let event = parse(other).expect("应解析成功");
    assert!(!event.should_respond, "提及他人不应触发");

    let plain = group_message("大家早上好", json!({}));
    let event = parse(plain).expect("应解析成功");
    assert!(!event.should_respond, "普通群消息不应触发");
}

/// 群内文本提及（text_mention）：用户 ID 等于 Bot ID → 触发；否则不触发。
#[test]
fn group_text_mention_matches_bot_id() {
    let self_mention = group_message(
        "你好",
        json!({
            "entities": [{
                "offset": 0,
                "length": 2,
                "type": "text_mention",
                "user": { "id": 42, "is_bot": true, "first_name": "Ramaria" }
            }]
        }),
    );
    assert!(
        parse(self_mention).expect("应解析成功").should_respond,
        "文本提及 Bot 应触发"
    );

    let other_mention = group_message(
        "你好",
        json!({
            "entities": [{
                "offset": 0,
                "length": 2,
                "type": "text_mention",
                "user": { "id": 77, "is_bot": false, "first_name": "别人" }
            }]
        }),
    );
    assert!(
        !parse(other_mention).expect("应解析成功").should_respond,
        "文本提及他人不应触发"
    );
}

/// 群命令带用户名后缀（`/start@testbot`）→ 触发。
#[test]
fn group_bot_command_with_username_triggers() {
    let update = group_message(
        "/start@testbot",
        json!({ "entities": [{ "offset": 0, "length": 14, "type": "bot_command" }] }),
    );
    let event = parse(update).expect("应解析成功");
    assert!(event.should_respond, "群命令指向 Bot 应触发");
}

/// 群命令不带用户名后缀（`/start`）不直接触发（群内 @ 语义由实体承担）。
#[test]
fn group_bot_command_without_username_not_trigger() {
    let update = group_message(
        "/start",
        json!({ "entities": [{ "offset": 0, "length": 6, "type": "bot_command" }] }),
    );
    assert!(!parse(update).expect("应解析成功").should_respond);
}

/// 群内回复 Bot 消息 → 触发；回复他人 → 不触发但 reply_to 照记。
#[test]
fn group_reply_decision() {
    let reply_to_bot = group_message(
        "回复你",
        json!({
            "reply_to_message": {
                "message_id": 50,
                "from": { "id": 42, "is_bot": true, "first_name": "Ramaria" },
                "date": 1_700_000_000,
                "text": "你好"
            }
        }),
    );
    let event = parse(reply_to_bot).expect("应解析成功");
    assert!(event.should_respond, "回复 Bot 应触发");
    assert_eq!(event.reply_to.as_deref(), Some("50"));

    let reply_to_other = group_message(
        "回复别人",
        json!({
            "reply_to_message": {
                "message_id": 51,
                "from": { "id": 99, "is_bot": false, "first_name": "别人" },
                "date": 1_700_000_000,
                "text": "上一条"
            }
        }),
    );
    let event = parse(reply_to_other).expect("应解析成功");
    assert!(!event.should_respond, "回复他人不应触发");
    assert_eq!(event.reply_to.as_deref(), Some("51"), "回复目标应照记");
}

/// 非 BMP 字符在文本前部时，提及偏移按 UTF-16 单元口径切片（不是字节偏移）。
#[test]
fn mention_offset_is_utf16_units() {
    // "😀" 占 2 个 UTF-16 单元，其后一个空格共 3 单元后是 "@TESTBOT"
    let update = group_message(
        "😀 @TESTBOT 你好",
        json!({ "entities": [{ "offset": 3, "length": 8, "type": "mention" }] }),
    );
    let event = parse(update).expect("应解析成功");
    assert!(event.should_respond, "UTF-16 偏移切片应命中提及");
}

/// 实体偏移越界或落在字符中间：不 panic、不触发。
#[test]
fn illegal_entity_bounds_tolerated() {
    let out_of_range = group_message(
        "hi",
        json!({ "entities": [{ "offset": 99, "length": 5, "type": "mention" }] }),
    );
    assert!(!parse(out_of_range).expect("应解析成功").should_respond);

    let inside_surrogate = group_message(
        "😀hi",
        json!({ "entities": [{ "offset": 1, "length": 1, "type": "mention" }] }),
    );
    assert!(!parse(inside_surrogate).expect("应解析成功").should_respond);
}

// =========================================================
// 媒体与段序列
// =========================================================

/// photo 无 caption：取 file_size 最大项为图片段（下载引用为文件标识）。
#[test]
fn photo_without_caption_picks_largest() {
    let update = json!({
        "update_id": 120,
        "message": {
            "message_id": 300,
            "from": { "id": 10001, "first_name": "小明" },
            "chat": { "id": 10001, "type": "private" },
            "date": 1_700_000_002,
            "photo": [
                { "file_id": "small", "file_unique_id": "u1", "width": 90, "height": 60, "file_size": 1024 },
                { "file_id": "large", "file_unique_id": "u2", "width": 900, "height": 600, "file_size": 20480 }
            ]
        }
    });
    let event = parse(update).expect("应解析成功");
    assert!(event.should_respond, "私聊图片应触发");
    assert_eq!(
        event.segments,
        vec![InboundSegment::Image {
            url: "large".to_string(),
            size: Some(20480),
            sub_type: None,
        }]
    );
}

/// photo 带 caption：先图后文。
#[test]
fn photo_with_caption_image_before_text() {
    let update = json!({
        "update_id": 121,
        "message": {
            "message_id": 301,
            "from": { "id": 10001, "first_name": "小明" },
            "chat": { "id": 10001, "type": "private" },
            "date": 1_700_000_003,
            "photo": [{ "file_id": "mid", "file_size": 4096 }],
            "caption": "看这个"
        }
    });
    let event = parse(update).expect("应解析成功");
    assert_eq!(
        event.segments,
        vec![
            InboundSegment::Image {
                url: "mid".to_string(),
                size: Some(4096),
                sub_type: None,
            },
            InboundSegment::Text("看这个".to_string()),
        ]
    );
}

/// photo 项缺 file_size：取默认 0 比较，仍产出图片段。
#[test]
fn photo_without_size_still_parsed() {
    let update = json!({
        "update_id": 122,
        "message": {
            "message_id": 302,
            "from": { "id": 10001, "first_name": "小明" },
            "chat": { "id": 10001, "type": "private" },
            "date": 1,
            "photo": [{ "file_id": "only" }]
        }
    });
    let event = parse(update).expect("应解析成功");
    assert_eq!(
        event.segments,
        vec![InboundSegment::Image {
            url: "only".to_string(),
            size: None,
            sub_type: None,
        }]
    );
}

/// 图片类 document：产出图片段；非图片 document 忽略后无内容 → None。
#[test]
fn document_image_and_non_image() {
    let image_document = json!({
        "update_id": 123,
        "message": {
            "message_id": 303,
            "from": { "id": 10001, "first_name": "小明" },
            "chat": { "id": 10001, "type": "private" },
            "date": 1,
            "document": {
                "file_name": "a.png",
                "mime_type": "image/png",
                "file_id": "doc-1",
                "file_unique_id": "u3",
                "file_size": 8192
            },
            "caption": "图"
        }
    });
    let event = parse(image_document).expect("应解析成功");
    assert_eq!(
        event.segments,
        vec![
            InboundSegment::Image {
                url: "doc-1".to_string(),
                size: Some(8192),
                sub_type: None,
            },
            InboundSegment::Text("图".to_string()),
        ]
    );

    let pdf_document = json!({
        "update_id": 124,
        "message": {
            "message_id": 304,
            "from": { "id": 10001, "first_name": "小明" },
            "chat": { "id": 10001, "type": "private" },
            "date": 1,
            "document": {
                "file_name": "a.pdf",
                "mime_type": "application/pdf",
                "file_id": "doc-2",
                "file_size": 1024
            }
        }
    });
    assert!(parse(pdf_document).is_none(), "非图片文档应忽略");
}

/// 服务消息（无文本与媒体）→ None；空文本消息 → None。
#[test]
fn service_message_and_empty_text_ignored() {
    let service = json!({
        "update_id": 125,
        "message": {
            "message_id": 305,
            "from": { "id": 42, "is_bot": true, "first_name": "Ramaria" },
            "chat": { "id": -1001234567890i64, "title": "群", "type": "group" },
            "date": 1,
            "new_chat_members": [{ "id": 42, "is_bot": true, "first_name": "Ramaria" }]
        }
    });
    assert!(parse(service).is_none(), "服务消息应忽略");

    assert!(parse(private_text("", 306)).is_none(), "空文本应忽略");
}

/// 编辑消息 / 频道消息 / 回调查询（顶层无 message）→ None。
#[test]
fn non_message_updates_ignored() {
    let edited = json!({
        "update_id": 126,
        "edited_message": {
            "message_id": 307,
            "from": { "id": 10001, "first_name": "小明" },
            "chat": { "id": 10001, "type": "private" },
            "date": 1,
            "text": "改过了"
        }
    });
    assert!(parse(edited).is_none());

    let channel_post = json!({
        "update_id": 127,
        "channel_post": {
            "message_id": 308,
            "chat": { "id": -1009876543210i64, "title": "频道", "type": "channel" },
            "date": 1,
            "text": "公告"
        }
    });
    assert!(parse(channel_post).is_none());

    let callback = json!({
        "update_id": 128,
        "callback_query": { "id": "1", "from": { "id": 1 }, "chat_instance": "x", "data": "d" }
    });
    assert!(parse(callback).is_none());
}

// =========================================================
// 容错
// =========================================================

/// 未知字段容错：扩展字段不引发解析失败。
#[test]
fn unknown_fields_tolerated() {
    let update = json!({
        "update_id": 129,
        "message": {
            "message_id": 309,
            "message_thread_id": 3,
            "is_topic_message": true,
            "from": { "id": 10001, "first_name": "小明", "language_code": "zh-hans", "is_premium": true },
            "chat": { "id": 10001, "type": "private", "username": "xiaoming", "custom": { "x": 1 } },
            "date": 1_700_000_004,
            "text": "你好",
            "custom_extension": [1, 2, 3]
        }
    });
    let event = parse(update).expect("未知字段应容忍");
    assert_eq!(event.created_at, 1_700_000_004_000);
}

/// date 缺失回退当前时间。
#[test]
fn date_missing_falls_back_to_now() {
    let update = json!({
        "update_id": 130,
        "message": {
            "message_id": 310,
            "from": { "id": 10001, "first_name": "小明" },
            "chat": { "id": 10001, "type": "private" },
            "text": "你好"
        }
    });
    let event = parse(update).expect("应解析成功");
    let delta = (event.created_at - now_ms()).abs();
    assert!(delta < 5_000, "时间应回退当前时刻，偏差 {delta}ms");
}

/// 未知聊天类型 → None。
#[test]
fn unknown_chat_type_ignored() {
    let update = json!({
        "update_id": 131,
        "message": {
            "message_id": 311,
            "from": { "id": 10001, "first_name": "小明" },
            "chat": { "id": -1001, "type": "channel" },
            "date": 1,
            "text": "你好"
        }
    });
    assert!(parse(update).is_none());
}

/// 群 ID 负数原样保留在外部标识中。
#[test]
fn negative_group_id_preserved() {
    let update = group_message("早上好", json!({}));
    let event = parse(update).expect("应解析成功");
    assert_eq!(event.external_ref, "group:-1001234567890");
}
