//! crates/ramaria-channel/src/transport/onebot/event/tests.rs - OneBot 事件解析单元测试
//!
//! 设计特点:
//! - 样例驱动：覆盖真实 NapCat 事件形态（含扩展字段）与字段类型归一
//! - 响应判定三分支：私聊逐条 / 群 @ / 群被回复；"all" 与未命中不触发
//! - 容错路径断言：坏 JSON / 非 message 事件 / 缺关键字段均返回 None

use super::*;
use ramaria_core::types::now_ms;
use serde_json::json;

/// 无已发消息的解析（多数用例使用）。
fn parse(json: &str) -> Option<TransportEvent> {
    parse_event(json, &SentMessageCache::new())
}

/// 私聊文本事件：会话标识 / 发送者 / 时间 / 段序列逐项核对。
#[test]
fn private_text_event_fields() {
    let json = json!({
        "post_type": "message",
        "message_type": "private",
        "sub_type": "friend",
        "message_id": 123456,
        "user_id": 10001,
        "self_id": 42,
        "time": 1_700_000_000,
        "raw_message": "你好",
        "font": 0,
        "message": [{ "type": "text", "data": { "text": "你好" } }],
        "sender": { "user_id": 10001, "nickname": "小明", "card": "", "role": "member" },
        "message_seq": 1,
        "real_id": 2,
        "real_seq": "3",
        "message_format": "array"
    })
    .to_string();

    let event = parse(&json).expect("应解析成功");
    assert_eq!(event.external_ref, "c2c:10001");
    assert_eq!(event.platform_message_id.as_deref(), Some("123456"));
    assert_eq!(event.created_at, 1_700_000_000_000);
    assert_eq!(event.self_id.as_deref(), Some("42"));
    assert_eq!(event.sender.platform_id, "10001");
    assert_eq!(event.sender.uin.as_deref(), Some("10001"));
    assert_eq!(event.sender.display_name, "小明");
    assert_eq!(event.sender.group_nickname, None);
    assert_eq!(event.sender.role, Some(MemberRole::Member));
    assert_eq!(
        event.segments,
        vec![InboundSegment::Text("你好".to_string())]
    );
    assert!(event.should_respond, "私聊逐条响应");
    assert_eq!(event.reply_to, None);
}

/// 群聊事件：会话标识为 group 前缀；self_id 与 user_id 为数字时归一为字符串。
#[test]
fn group_text_event_fields() {
    let json = json!({
        "post_type": "message",
        "message_type": "group",
        "group_id": 20002,
        "user_id": 10001,
        "self_id": 42,
        "time": 1_700_000_001,
        "message_id": 555,
        "message": [{ "type": "text", "data": { "text": "早上好" } }],
        "sender": { "user_id": 10001, "nickname": "小明", "card": "群名片", "role": "admin" }
    })
    .to_string();

    let event = parse(&json).expect("应解析成功");
    assert_eq!(event.external_ref, "group:20002");
    assert_eq!(event.sender.display_name, "群名片");
    assert_eq!(event.sender.group_nickname.as_deref(), Some("群名片"));
    assert_eq!(event.sender.role, Some(MemberRole::Admin));
    assert!(!event.should_respond, "群聊未 @ 未回复时不响应");
}

/// @ 判定：at.qq 为字符串、self_id 为数字（归一后相等）；At 段折叠为文本。
#[test]
fn group_at_self_triggers_response() {
    let json = json!({
        "post_type": "message",
        "message_type": "group",
        "group_id": 20002,
        "user_id": 10001,
        "self_id": 42,
        "time": 1_700_000_002,
        "message": [
            { "type": "at", "data": { "qq": "42" } },
            { "type": "text", "data": { "text": " 在吗" } }
        ]
    })
    .to_string();

    let event = parse(&json).expect("应解析成功");
    assert!(event.should_respond, "@ 本机应触发响应");
    assert_eq!(
        event.segments,
        vec![
            InboundSegment::Text("@42".to_string()),
            InboundSegment::Text(" 在吗".to_string()),
        ]
    );
}

/// @ 他人：不触发响应；"all" 折叠为「@全体成员」且不算 @ 本机。
#[test]
fn group_at_other_or_all_does_not_trigger() {
    let other = json!({
        "post_type": "message",
        "message_type": "group",
        "group_id": 20002,
        "user_id": 10001,
        "self_id": 42,
        "time": 1,
        "message": [{ "type": "at", "data": { "qq": "99" } }]
    })
    .to_string();
    let event = parse(&other).expect("应解析成功");
    assert!(!event.should_respond);
    assert_eq!(
        event.segments,
        vec![InboundSegment::Text("@99".to_string())]
    );

    let all = json!({
        "post_type": "message",
        "message_type": "group",
        "group_id": 20002,
        "user_id": 10001,
        "self_id": 42,
        "time": 1,
        "message": [{ "type": "at", "data": { "qq": "all" } }]
    })
    .to_string();
    let event = parse(&all).expect("应解析成功");
    assert!(!event.should_respond, "\"all\" 不算 @ 本机");
    assert_eq!(
        event.segments,
        vec![InboundSegment::Text("@全体成员".to_string())]
    );
}

/// reply 判定：命中本机已发消息 ID → 触发响应；reply 段不进正文。
#[test]
fn group_reply_to_sent_message_triggers_response() {
    let sent_ids = SentMessageCache::new();
    sent_ids.insert("998877");

    let json = json!({
        "post_type": "message",
        "message_type": "group",
        "group_id": 20002,
        "user_id": 10001,
        "self_id": 42,
        "time": 1_700_000_003,
        "message": [
            { "type": "reply", "data": { "id": "998877" } },
            { "type": "text", "data": { "text": "收到" } }
        ]
    })
    .to_string();

    let event = parse_event(&json, &sent_ids).expect("应解析成功");
    assert!(event.should_respond, "回复本机消息应触发响应");
    assert_eq!(event.reply_to.as_deref(), Some("998877"));
    assert_eq!(
        event.segments,
        vec![InboundSegment::Text("收到".to_string())],
        "reply 段不产生正文段"
    );
}

/// reply 判定：未命中已发消息 / 无缓存时不触发。
#[test]
fn group_reply_to_unknown_message_does_not_trigger() {
    let json = json!({
        "post_type": "message",
        "message_type": "group",
        "group_id": 20002,
        "user_id": 10001,
        "self_id": 42,
        "time": 1,
        "message": [
            { "type": "reply", "data": { "id": "123123" } },
            { "type": "text", "data": { "text": "回复别人" } }
        ]
    })
    .to_string();

    let event = parse(&json).expect("应解析成功");
    assert!(!event.should_respond);
    assert_eq!(event.reply_to.as_deref(), Some("123123"));
}

/// 图片段：url / file_size / sub_type 归一为图片段引用。
#[test]
fn image_segment_carries_download_reference() {
    let json = json!({
        "post_type": "message",
        "message_type": "private",
        "user_id": 10001,
        "self_id": 42,
        "time": 1_700_000_004,
        "message": [{
            "type": "image",
            "data": {
                "summary": "[图片]",
                "file": "a.jpg",
                "sub_type": "0",
                "url": "https://multimedia.nt.qq.com.cn/download?rkey=x",
                "file_size": 2048
            }
        }]
    })
    .to_string();

    let event = parse(&json).expect("应解析成功");
    assert_eq!(event.segments.len(), 1);
    match &event.segments[0] {
        InboundSegment::Image {
            url,
            size,
            sub_type,
        } => {
            assert!(url.starts_with("https://multimedia.nt.qq.com.cn/"));
            assert_eq!(*size, Some(2048));
            assert_eq!(sub_type.as_deref(), Some("0"));
        }
        other => panic!("应为图片段: {other:?}"),
    }
}

/// 图片段缺 url：降级为文本占位「[图片]」（无附件引用）。
#[test]
fn image_segment_without_url_degrades_to_text() {
    let json = json!({
        "post_type": "message",
        "message_type": "private",
        "user_id": 10001,
        "self_id": 42,
        "time": 1,
        "message": [{ "type": "image", "data": { "file": "a.jpg" } }]
    })
    .to_string();

    let event = parse(&json).expect("应解析成功");
    assert_eq!(
        event.segments,
        vec![InboundSegment::Text("[图片]".to_string())]
    );
}

/// 混合段序列：text + at + image + reply + 未知段（未知忽略、顺序保持）。
#[test]
fn mixed_segments_preserve_order_and_ignore_unknown() {
    let sent_ids = SentMessageCache::new();
    sent_ids.insert("777");

    let json = json!({
        "post_type": "message",
        "message_type": "group",
        "group_id": 20002,
        "user_id": 10001,
        "self_id": 42,
        "time": 1,
        "message": [
            { "type": "text", "data": { "text": "看这个" } },
            { "type": "at", "data": { "qq": "42" } },
            { "type": "image", "data": { "url": "https://example.com/a.png", "file_size": "10" } },
            { "type": "reply", "data": { "id": "777" } },
            { "type": "face", "data": { "id": "1" } }
        ]
    })
    .to_string();

    let event = parse_event(&json, &sent_ids).expect("应解析成功");
    assert!(event.should_respond);
    assert_eq!(event.segments.len(), 3);
    assert_eq!(
        event.segments[0],
        InboundSegment::Text("看这个".to_string())
    );
    assert_eq!(event.segments[1], InboundSegment::Text("@42".to_string()));
    match &event.segments[2] {
        InboundSegment::Image { url, size, .. } => {
            assert_eq!(url, "https://example.com/a.png");
            assert_eq!(*size, Some(10), "file_size 字符串形态应可解析");
        }
        other => panic!("应为图片段: {other:?}"),
    }
}

/// 非 message 事件（meta_event / notice）忽略。
#[test]
fn non_message_events_are_ignored() {
    let meta = json!({
        "post_type": "meta_event",
        "meta_event_type": "lifecycle",
        "sub_type": "connect",
        "time": 1,
        "self_id": 42
    })
    .to_string();
    assert!(parse(&meta).is_none());

    let notice = json!({
        "post_type": "notice",
        "notice_type": "notify",
        "time": 1,
        "self_id": 42
    })
    .to_string();
    assert!(parse(&notice).is_none());
}

/// 未知消息类型忽略。
#[test]
fn unknown_message_type_is_ignored() {
    let json = json!({
        "post_type": "message",
        "message_type": "discuss",
        "user_id": 10001,
        "self_id": 42,
        "time": 1,
        "message": [{ "type": "text", "data": { "text": "hi" } }]
    })
    .to_string();
    assert!(parse(&json).is_none());
}

/// 坏 JSON 不 panic（返回 None）。
#[test]
fn broken_json_returns_none() {
    assert!(parse("{ not json").is_none());
    assert!(parse("").is_none());
}

/// message 数组缺失或非数组：返回 None（无内容可写）。
#[test]
fn missing_or_invalid_message_array_returns_none() {
    let missing = json!({
        "post_type": "message",
        "message_type": "private",
        "user_id": 10001,
        "self_id": 42,
        "time": 1
    })
    .to_string();
    assert!(parse(&missing).is_none());

    let invalid = json!({
        "post_type": "message",
        "message_type": "private",
        "user_id": 10001,
        "self_id": 42,
        "time": 1,
        "message": "not-an-array"
    })
    .to_string();
    assert!(parse(&invalid).is_none());
}

/// 私聊缺 user_id / 群聊缺 group_id：返回 None（无法确定会话）。
#[test]
fn missing_session_identifier_returns_none() {
    let private = json!({
        "post_type": "message",
        "message_type": "private",
        "self_id": 42,
        "time": 1,
        "message": [{ "type": "text", "data": { "text": "hi" } }]
    })
    .to_string();
    assert!(parse(&private).is_none());

    let group = json!({
        "post_type": "message",
        "message_type": "group",
        "user_id": 10001,
        "self_id": 42,
        "time": 1,
        "message": [{ "type": "text", "data": { "text": "hi" } }]
    })
    .to_string();
    assert!(parse(&group).is_none());
}

/// 时间：秒转毫秒；缺失 / 非正数回退当前时间（范围断言）。
#[test]
fn created_at_seconds_to_millis_and_fallback() {
    let with_time = json!({
        "post_type": "message",
        "message_type": "private",
        "user_id": 10001,
        "self_id": 42,
        "time": 1_700_000_005,
        "message": [{ "type": "text", "data": { "text": "hi" } }]
    })
    .to_string();
    assert_eq!(
        parse(&with_time).expect("应解析成功").created_at,
        1_700_000_005_000
    );

    let missing_time = json!({
        "post_type": "message",
        "message_type": "private",
        "user_id": 10001,
        "self_id": 42,
        "message": [{ "type": "text", "data": { "text": "hi" } }]
    })
    .to_string();
    let before = now_ms();
    let event = parse(&missing_time).expect("应解析成功");
    let after = now_ms();
    assert!(
        (before..=after).contains(&event.created_at),
        "缺失时间应回退当前时间: {} not in [{before}, {after}]",
        event.created_at
    );
}

/// sender 缺失：身份字段取默认值（账号仍取顶层 user_id）。
#[test]
fn sender_missing_falls_back_to_defaults() {
    let json = json!({
        "post_type": "message",
        "message_type": "group",
        "group_id": 20002,
        "user_id": 10001,
        "self_id": 42,
        "time": 1,
        "message": [{ "type": "text", "data": { "text": "hi" } }]
    })
    .to_string();

    let event = parse(&json).expect("应解析成功");
    assert_eq!(event.sender.platform_id, "10001");
    assert_eq!(event.sender.uin.as_deref(), Some("10001"));
    assert_eq!(event.sender.display_name, "");
    assert_eq!(event.sender.group_nickname, None);
    assert_eq!(event.sender.role, None);
}
