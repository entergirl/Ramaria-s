//! crates/ramaria-desktop/src/events/tests.rs - Tauri 事件负载序列化单元测试
//!
//! 设计特点:
//! - 覆盖聊天流式（delta / done / error）负载的字段序列化
//! - 覆盖导入进度负载的估算字段省略与透出
//! - 覆盖主动消息负载的常规投递（activated 省略）与点击重播（activated=true）两态

use super::*;

#[test]
fn delta_payload_serialization() {
    let payload = ChatDeltaPayload::new("req-001".to_string(), "你好".to_string());
    let json = serde_json::to_string(&payload).expect("序列化失败");
    assert!(json.contains("req-001"));
    assert!(json.contains("你好"));
}

#[test]
fn done_payload_serialization() {
    let payload = ChatDonePayload::new(
        "req-001".to_string(),
        Some("deepseek".into()),
        42,
        "完整回复".to_string(),
    );
    let json = serde_json::to_string(&payload).expect("序列化失败");
    assert!(json.contains("完整回复"), "应含完整回复文本: {json}");
    assert!(
        json.contains(r#""request_id":"req-001""#),
        "应含 request_id: {json}"
    );
    assert!(
        json.contains(r#""backend_id":"deepseek""#),
        "应含 backend_id: {json}"
    );
    assert!(
        json.contains(r#""total_chars":42"#),
        "应含 total_chars: {json}"
    );
}

#[test]
fn done_payload_empty_content_serializes() {
    let payload = ChatDonePayload::new("req-002".to_string(), None, 0, String::new());
    let json = serde_json::to_string(&payload).expect("序列化失败");
    assert!(
        json.contains(r#""content":"""#),
        "空 content 应序列化为空串字段: {json}"
    );
}

#[test]
fn error_payload_serialization() {
    let payload = ChatErrorPayload::new(
        "req-001".into(),
        "连接失败".into(),
        "请检查网络".into(),
        true,
    );
    let json = serde_json::to_string(&payload).expect("序列化失败");
    assert!(json.contains("连接失败"));
    assert!(json.contains("请检查网络"));
    assert!(json.contains("true"));
}

// ---- 阶段预计总量与 ETA 字段 ----

/// 基础 payload（无估算字段）→ 序列化不含 l1_expected/l2_expected/l3_expected/eta_seconds
/// （向后兼容：旧前端忽略未知字段；旧后端事件不含新字段）。
#[test]
fn basic_payload_omits_estimate_fields() {
    let payload = ImportProgressPayload::new("l1", 1, 10, "进度");
    let json = serde_json::to_string(&payload).expect("序列化失败");
    assert!(
        !json.contains("l1_expected"),
        "未附加估算时不应输出字段: {json}"
    );
    assert!(
        !json.contains("eta_seconds"),
        "未附加估算时不应输出字段: {json}"
    );
}

/// 附加估算字段 → 序列化包含各阶段预计总量与 eta_seconds。
#[test]
fn payload_with_estimates_serializes_fields() {
    let payload = ImportProgressPayload::new("l1", 5, 20, "进度").with_estimates(
        Some(20),
        Some(2),
        Some(2),
        Some(120),
    );
    let json = serde_json::to_string(&payload).expect("序列化失败");
    assert!(
        json.contains(r#""l1_expected":20"#),
        "应含 l1_expected: {json}"
    );
    assert!(
        json.contains(r#""l2_expected":2"#),
        "应含 l2_expected: {json}"
    );
    assert!(
        json.contains(r#""l3_expected":2"#),
        "应含 l3_expected: {json}"
    );
    assert!(
        json.contains(r#""eta_seconds":120"#),
        "应含 eta_seconds: {json}"
    );
}

/// 部分估算字段为 None → 仅序列化非 None 字段。
#[test]
fn payload_with_partial_estimates_serializes_only_some() {
    // L1 阶段：仅 l1_expected 已知，L2/L3 未知
    let payload =
        ImportProgressPayload::new("l1", 0, 20, "进度").with_estimates(Some(20), None, None, None);
    let json = serde_json::to_string(&payload).expect("序列化失败");
    assert!(json.contains(r#""l1_expected":20"#));
    assert!(!json.contains("l2_expected"), "未知阶段不应输出: {json}");
    assert!(!json.contains("eta_seconds"), "无 ETA 不应输出: {json}");
}

// ---- 主动消息事件 ----

/// 常规投递：六字段序列化齐备，activated 不出现。
#[test]
fn proactive_payload_serializes_fields_without_activated() {
    let payload = ProactiveMessagePayload::new(
        "session-001".to_string(),
        "message-001".to_string(),
        "主动消息内容".to_string(),
        "char-0001".to_string(),
        "event".to_string(),
        1_700_000_000_000,
    );
    let json = serde_json::to_string(&payload).expect("序列化失败");
    assert!(
        json.contains(r#""session_id":"session-001""#),
        "应含 session_id: {json}"
    );
    assert!(
        json.contains(r#""message_id":"message-001""#),
        "应含 message_id: {json}"
    );
    assert!(
        json.contains(r#""content":"主动消息内容""#),
        "应含 content: {json}"
    );
    assert!(
        json.contains(r#""persona":"char-0001""#),
        "应含 persona: {json}"
    );
    assert!(json.contains(r#""source":"event""#), "应含 source: {json}");
    assert!(
        json.contains(r#""created_at":1700000000000"#),
        "应含 created_at: {json}"
    );
    assert!(
        !json.contains("activated"),
        "常规投递不应输出 activated: {json}"
    );
}

/// 通知点击重播：activated=true 序列化透出。
#[test]
fn proactive_payload_activated_marker_serializes() {
    let payload = ProactiveMessagePayload::new(
        "session-001".to_string(),
        "message-001".to_string(),
        "主动消息内容".to_string(),
        "char-0001".to_string(),
        "event".to_string(),
        1_700_000_000_000,
    )
    .activated();
    let json = serde_json::to_string(&payload).expect("序列化失败");
    assert!(
        json.contains(r#""activated":true"#),
        "点击重播应含 activated=true: {json}"
    );
}
