//! crates/ramaria-channel/src/transport/onebot/api/tests.rs - OneBot 出站 API 单元测试
//!
//! 设计特点:
//! - 请求构建为纯函数断言：动作 / 参数 / 消息段 / echo 逐项核对
//! - 非法会话标识的分类断言（协议错误，不 panic）
//! - 应答分类矩阵：成功（含 message_id 归一）/ retcode 非零 / 缺 retcode
//! - 已发消息 ID 缓存：容量淘汰与重复插入

use super::*;

/// 私聊请求：动作与参数（账号归一为数字）。
#[test]
fn private_request_builds_action_and_params() {
    let request = build_text_request("c2c:10001", "你好", "ramaria-1").expect("构建应成功");
    assert_eq!(request["action"], ACTION_SEND_PRIVATE);
    assert_eq!(request["params"]["user_id"], 10001);
    assert_eq!(request["params"]["message"][0]["type"], "text");
    assert_eq!(request["params"]["message"][0]["data"]["text"], "你好");
    assert_eq!(request["echo"], "ramaria-1");
}

/// 群聊请求：动作与参数（群号归一为数字）。
#[test]
fn group_request_builds_action_and_params() {
    let request = build_text_request("group:20002", "在吗", "ramaria-2").expect("构建应成功");
    assert_eq!(request["action"], ACTION_SEND_GROUP);
    assert_eq!(request["params"]["group_id"], 20002);
    assert_eq!(request["params"]["message"][0]["data"]["text"], "在吗");
    assert_eq!(request["echo"], "ramaria-2");
}

/// 会话标识两侧空白容忍。
#[test]
fn external_ref_whitespace_tolerated() {
    let request = build_text_request("  c2c:10001  ", "hi", "e").expect("构建应成功");
    assert_eq!(request["action"], ACTION_SEND_PRIVATE);
    assert_eq!(request["params"]["user_id"], 10001);
}

/// 非法会话标识：空 / 未知前缀 / 非数字账号均为协议错误。
#[test]
fn invalid_external_ref_is_protocol_error() {
    let empty = action_and_params("   ").expect_err("空标识应失败");
    assert!(matches!(empty, ChannelError::Protocol(_)));

    let unknown = action_and_params("channel:1").expect_err("未知前缀应失败");
    assert!(matches!(unknown, ChannelError::Protocol(_)));

    let non_numeric = action_and_params("c2c:abc").expect_err("非数字账号应失败");
    assert!(matches!(non_numeric, ChannelError::Protocol(_)));

    let bad_group = action_and_params("group:x").expect_err("非数字群号应失败");
    assert!(matches!(bad_group, ChannelError::Protocol(_)));
}

/// 应答分类：retcode=0 成功且 message_id 归一为字符串。
#[test]
fn response_ok_returns_message_id() {
    let value = json!({"status": "ok", "retcode": 0, "data": {"message_id": 12345}});
    let message_id = classify_response(&value).expect("应判成功");
    assert_eq!(message_id.as_deref(), Some("12345"));
}

/// 应答分类：字符串 message_id 原样保留；data 缺失时为 None。
#[test]
fn response_ok_message_id_variants() {
    let string_id = json!({"retcode": 0, "data": {"message_id": "m-1"}});
    assert_eq!(
        classify_response(&string_id).expect("应判成功").as_deref(),
        Some("m-1")
    );

    let no_data = json!({"retcode": 0});
    assert_eq!(classify_response(&no_data).expect("应判成功"), None);
}

/// 应答分类：retcode 非零为协议错误（携带 retcode 与服务端文案）。
#[test]
fn response_retcode_failure_is_protocol_error() {
    let value = json!({"status": "failed", "retcode": 1400, "wording": "bad request"});
    let error = classify_response(&value).expect_err("应判失败");
    let text = error.to_string();
    assert!(matches!(error, ChannelError::Protocol(_)));
    assert!(text.contains("1400"), "错误文案应携带 retcode: {text}");
    assert!(
        text.contains("bad request"),
        "错误文案应携带服务端文案: {text}"
    );
}

/// 应答分类：retcode 0 但 status 非 ok 时宽容按成功处理。
#[test]
fn response_ok_status_tolerated() {
    let value = json!({"status": "async", "retcode": 0, "data": {"message_id": 1}});
    assert!(classify_response(&value).is_ok());
}

/// 应答分类：缺 retcode 为协议错误。
#[test]
fn response_missing_retcode_is_protocol_error() {
    let value = json!({"status": "ok", "data": {}});
    let error = classify_response(&value).expect_err("缺 retcode 应失败");
    assert!(matches!(error, ChannelError::Protocol(_)));
}

/// 已发消息 ID 缓存：命中 / 未命中 / 空值忽略。
#[test]
fn sent_message_cache_contains_inserted() {
    let cache = SentMessageCache::new();
    assert!(!cache.contains("1"));
    cache.insert("1");
    assert!(cache.contains("1"));
    assert!(!cache.contains("2"));
    cache.insert("");
    assert!(!cache.contains(""));
}

/// 已发消息 ID 缓存：超出容量淘汰最旧，重复插入不重复占位。
#[test]
fn sent_message_cache_evicts_oldest() {
    let cache = SentMessageCache::new();
    cache.insert("dup");
    cache.insert("dup");
    for index in 0..SENT_MESSAGE_CACHE {
        cache.insert(format!("id-{index}"));
    }
    // 容量填满时最旧的 "dup" 已被淘汰；id-0 仍在（队首）
    assert!(!cache.contains("dup"));
    assert!(cache.contains("id-0"));
    assert!(cache.contains(&format!("id-{}", SENT_MESSAGE_CACHE - 1)));

    cache.insert("newest");
    assert!(cache.contains("newest"));
    assert!(!cache.contains("id-0"), "继续插入应淘汰队首 id-0");
    assert!(cache.contains("id-1"));
}

// =========================================================
// base64 编码
// =========================================================

/// 已知向量：空 / 各余数长度逐条核对（含 `=` 补齐）。
#[test]
fn base64_known_vectors() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
}

/// 中文 UTF-8 字节编码（非 ASCII 载荷）。
#[test]
fn base64_multibyte_bytes() {
    assert_eq!(base64_encode("中文".as_bytes()), "5Lit5paH");
}

// =========================================================
// 出站消息构建（文本 / 图片）
// =========================================================

/// 仅文本：单文本段。
#[test]
fn message_request_text_only() {
    let request = build_message_request("c2c:10001", "你好", None, "e-1").expect("构建应成功");
    let message = request["params"]["message"].as_array().expect("应为数组");
    assert_eq!(message.len(), 1);
    assert_eq!(message[0]["type"], "text");
    assert_eq!(message[0]["data"]["text"], "你好");
}

/// 仅图片：单图片段（file 原样透传）。
#[test]
fn message_request_image_only() {
    let request =
        build_message_request("group:20002", "", Some("base64://Zm9v"), "e-2").expect("构建应成功");
    let message = request["params"]["message"].as_array().expect("应为数组");
    assert_eq!(message.len(), 1);
    assert_eq!(message[0]["type"], "image");
    assert_eq!(message[0]["data"]["file"], "base64://Zm9v");
}

/// 文本 + 图片：按序两段（文本在前、图片在后）。
#[test]
fn message_request_text_and_image() {
    let request = build_message_request(
        "c2c:10001",
        "看这个",
        Some("https://example.com/a.png"),
        "e-3",
    )
    .expect("构建应成功");
    let message = request["params"]["message"].as_array().expect("应为数组");
    assert_eq!(message.len(), 2);
    assert_eq!(message[0]["type"], "text");
    assert_eq!(message[1]["type"], "image");
    assert_eq!(message[1]["data"]["file"], "https://example.com/a.png");
    assert_eq!(request["echo"], "e-3");
}

/// 两段皆无（空文本 + 无图 / 空白图片引用）：协议错误。
#[test]
fn message_request_empty_is_protocol_error() {
    let no_parts = build_message_request("c2c:10001", "   ", None, "e-4").expect_err("应失败");
    assert!(matches!(no_parts, ChannelError::Protocol(_)));

    let blank_image =
        build_message_request("c2c:10001", "", Some("  "), "e-5").expect_err("应失败");
    assert!(matches!(blank_image, ChannelError::Protocol(_)));
}
