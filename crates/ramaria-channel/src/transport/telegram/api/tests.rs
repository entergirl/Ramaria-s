//! crates/ramaria-channel/src/transport/telegram/api/tests.rs - Telegram 出站 API 单元测试
//!
//! 设计特点:
//! - 会话标识解析矩阵：私聊正数 / 群聊负数 / 空白容忍 / 非法分类断言
//! - 分片算法覆盖边界：恰限长 / 超一单元 / 非 BMP 字符不撕裂 / 换行优先 / 硬切 / 空文本
//! - 应答分类矩阵：成功 / 401 / 403 / 429 / 缺字段 / result 缺 message_id
//! - 请求构建纯函数断言：offset 有无 / 订阅类型 / 聊天 ID 与文本

use super::*;

// =========================================================
// 会话标识解析
// =========================================================

/// 私聊正数与群聊负数（含两侧空白容忍）。
#[test]
fn parse_chat_id_accepts_private_and_group() {
    assert_eq!(parse_chat_id("c2c:10001").expect("私聊应解析"), 10001);
    assert_eq!(
        parse_chat_id("group:-1001234567890").expect("群聊应解析"),
        -1001234567890
    );
    assert_eq!(parse_chat_id("  c2c:7  ").expect("空白应容忍"), 7);
    assert_eq!(parse_chat_id("group:-42").expect("小负数应解析"), -42);
}

/// 空 / 未知前缀 / 非数字均为协议错误。
#[test]
fn parse_chat_id_rejects_invalid() {
    let empty = parse_chat_id("   ").expect_err("空标识应失败");
    assert!(matches!(empty, ChannelError::Protocol(_)));

    let unknown = parse_chat_id("channel:1").expect_err("未知前缀应失败");
    assert!(matches!(unknown, ChannelError::Protocol(_)));

    let non_numeric = parse_chat_id("c2c:abc").expect_err("非数字应失败");
    assert!(matches!(non_numeric, ChannelError::Protocol(_)));

    let bad_group = parse_chat_id("group:x").expect_err("非数字群应失败");
    assert!(matches!(bad_group, ChannelError::Protocol(_)));
}

// =========================================================
// 文本分片
// =========================================================

/// 短文本单片原样。
#[test]
fn split_short_text_single_chunk() {
    let chunks = split_message("你好，世界", MESSAGE_LIMIT);
    assert_eq!(chunks, vec!["你好，世界".to_string()]);
}

/// 恰限长单片（中文字符按 UTF-16 计 1 单元）。
#[test]
fn split_exact_limit_single_chunk() {
    let text = "好".repeat(MESSAGE_LIMIT);
    let chunks = split_message(&text, MESSAGE_LIMIT);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chars().count(), MESSAGE_LIMIT);
}

/// 超一单元切两片（第一片占满，第二片一字符）。
#[test]
fn split_one_over_limit_two_chunks() {
    let text = "好".repeat(MESSAGE_LIMIT + 1);
    let chunks = split_message(&text, MESSAGE_LIMIT);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].chars().count(), MESSAGE_LIMIT);
    assert_eq!(chunks[1], "好");
}

/// ASCII 与 emoji 混合在边界上：不得切在代理对中间（emoji 整字进下一片）。
#[test]
fn split_non_bmp_not_torn_at_boundary() {
    let text = format!("{}😀b", "a".repeat(MESSAGE_LIMIT - 1));
    let chunks = split_message(&text, MESSAGE_LIMIT);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0], "a".repeat(MESSAGE_LIMIT - 1));
    assert_eq!(chunks[1], "😀b");
    for chunk in &chunks {
        assert!(!chunk.is_empty());
    }
}

/// emoji 恰好填满限长时单片（2 单元/字）。
#[test]
fn split_emoji_exact_limit_single_chunk() {
    let text = "😀".repeat(MESSAGE_LIMIT / 2);
    let chunks = split_message(&text, MESSAGE_LIMIT);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chars().count(), MESSAGE_LIMIT / 2);
}

/// 换行优先：若硬切会撕裂超限，则在最近换行后断开（前片含换行）。
#[test]
fn split_prefers_newline_break() {
    let tail = "x".repeat(MESSAGE_LIMIT - 3);
    let text = format!("abc\n{tail}");
    let chunks = split_message(&text, MESSAGE_LIMIT);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0], "abc\n");
    assert_eq!(chunks[1], tail);
    assert_eq!(chunks.concat(), text);
}

/// 无换行长文硬切（无信息丢失、逐片非空）。
#[test]
fn split_long_text_hard_cut() {
    let text = "x".repeat(MESSAGE_LIMIT + 100);
    let chunks = split_message(&text, MESSAGE_LIMIT);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), MESSAGE_LIMIT);
    assert_eq!(chunks[1].len(), 100);
    assert_eq!(chunks.concat(), text);
}

/// 空文本 → 空列表（防御）。
#[test]
fn split_empty_text_no_chunks() {
    assert!(split_message("", MESSAGE_LIMIT).is_empty());
}

// =========================================================
// 应答分类
// =========================================================

/// 成功应答返回 result；result 缺失时为 Null。
#[test]
fn parse_api_response_ok() {
    let value = json!({"ok": true, "result": {"message_id": 12}});
    let result = parse_api_response(&value).expect("应判成功");
    assert_eq!(result["message_id"], 12);

    let empty = json!({"ok": true});
    assert_eq!(parse_api_response(&empty).expect("应判成功"), Value::Null);
}

/// 401 / 403 为认证类错误（权限拒绝不可重试）。
#[test]
fn parse_api_response_auth_errors() {
    let unauthorized = json!({"ok": false, "error_code": 401, "description": "Unauthorized"});
    let error = parse_api_response(&unauthorized).expect_err("401 应失败");
    assert!(
        matches!(error, ChannelError::Auth(_)),
        "应为认证分类: {error:?}"
    );

    let forbidden = json!({"ok": false, "error_code": 403, "description": "Forbidden"});
    let error = parse_api_response(&forbidden).expect_err("403 应失败");
    assert!(
        matches!(error, ChannelError::Auth(_)),
        "应为认证分类: {error:?}"
    );
}

/// 429 等其余错误码为协议错误（携带错误码与描述）。
#[test]
fn parse_api_response_protocol_errors() {
    let rate_limited = json!({
        "ok": false,
        "error_code": 429,
        "description": "Too Many Requests: retry after 30"
    });
    let error = parse_api_response(&rate_limited).expect_err("429 应失败");
    let text = error.to_string();
    assert!(matches!(error, ChannelError::Protocol(_)));
    assert!(text.contains("429"), "文案应携带错误码: {text}");
    assert!(text.contains("retry after"), "文案应携带描述: {text}");
}

/// 缺少 ok 字段为协议错误。
#[test]
fn parse_api_response_missing_ok_is_protocol_error() {
    let value = json!({"result": {}});
    let error = parse_api_response(&value).expect_err("缺 ok 应失败");
    assert!(matches!(error, ChannelError::Protocol(_)));
}

/// result 缺 message_id 时为 None（发送成功但无 ID 可记）。
#[test]
fn extract_message_id_missing_returns_none() {
    assert_eq!(extract_message_id(&json!({})), None);
    assert_eq!(
        extract_message_id(&json!({"message_id": 7})).as_deref(),
        Some("7")
    );
    assert_eq!(
        extract_message_id(&json!({"message_id": "m-1"})).as_deref(),
        Some("m-1")
    );
}

// =========================================================
// 请求构建
// =========================================================

/// 首轮请求体不带 offset；后续带 offset；订阅 message 类更新。
#[test]
fn build_get_updates_body_offset_variants() {
    let first = build_get_updates_body(None, 25);
    assert!(first.get("offset").is_none(), "首轮不应带 offset: {first}");
    assert_eq!(first["timeout"], 25);
    assert_eq!(first["allowed_updates"], json!(["message"]));

    let next = build_get_updates_body(Some(106), 5);
    assert_eq!(next["offset"], 106);
    assert_eq!(next["timeout"], 5);
}

/// 发送请求体：聊天 ID 与文本。
#[test]
fn build_send_message_body_fields() {
    let body = build_send_message_body(-100123, "你好");
    assert_eq!(body["chat_id"], -100123);
    assert_eq!(body["text"], "你好");
}

/// 调用地址拼接：base 尾部斜杠容忍。
#[test]
fn api_url_trims_trailing_slash() {
    assert_eq!(
        api_url("https://api.telegram.org/", "t", "getMe"),
        "https://api.telegram.org/bott/getMe"
    );
    assert_eq!(
        download_url("https://api.telegram.org", "t", "/photos/a.png"),
        "https://api.telegram.org/file/bott/photos/a.png"
    );
}
