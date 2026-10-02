//! crates/ramaria-importer/src/qq/parser/tests.rs - QQ JSON 解析器单元测试
//!
//! 设计特点:
//! - 覆盖图片占位符清理 / 回复正文提取 / 指纹性质 / 会话切割 / 日期换算
//! - 覆盖 JSON 元素描述提取优先级（description > title > None）
//! - 流式解析与整读参考实现在大导出上的会话/报告快照等价对照
//! - 全程使用临时文件与内存数据，不依赖真实 QQ 数据

use std::collections::HashSet;
use std::path::Path;

use ramaria_core::error::RamariaResult;

use crate::traits::{ImportReport, ImportedSession, ParsedMessage};

use super::*;

// -- 图片占位符清理 --

#[test]
fn clean_image_placeholder_replaces() {
    assert_eq!(clean_image_placeholders("[图片: abc123]"), "[图片]");
    assert_eq!(
        clean_image_placeholders("[图片: 1234567890abcdef.jpg]"),
        "[图片]"
    );
}

#[test]
fn clean_image_placeholder_no_placeholder() {
    assert_eq!(clean_image_placeholders("普通消息"), "普通消息");
}

// -- 回复正文提取 --

/// extract_reply_body 各输入参数化验证。
#[test]
fn extract_reply_body_cases() {
    let cases = [
        ("回复的头部信息\n这是真正的回复正文", "这是真正的回复正文"),
        ("[回复某人] 这是正文", "这是正文"),
        ("", ""),
    ];
    for (input, expected) in cases {
        assert_eq!(extract_reply_body(input), expected, "input={input:?}");
    }
}

// -- 指纹计算 --

/// make_fingerprint 确定性与区分度验证。
#[test]
fn fingerprint_properties() {
    // 同输入 → 同指纹，长度 16
    let fp1 = make_fingerprint(1700000000000, "user", "你好");
    let fp2 = make_fingerprint(1700000000000, "user", "你好");
    assert_eq!(fp1, fp2);
    assert_eq!(fp1.len(), 16);
    // content 不同 → 指纹不同
    let fp3 = make_fingerprint(1700000000000, "user", "再见");
    assert_ne!(fp1, fp3);
    // role 不同 → 指纹不同
    let fp4 = make_fingerprint(1700000000000, "assistant", "你好");
    assert_ne!(fp1, fp4);
}

// -- Session 切割 --

/// split_into_sessions 各消息序列参数化验证。
#[test]
fn split_sessions_cases() {
    // 单会话（间隔 < 60s）
    let msgs = vec![
        make_test_msg("user", "消息1", 1000),
        make_test_msg("assistant", "消息2", 2000),
    ];
    let sessions = split_into_sessions(&msgs, 60000);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].messages.len(), 2);
    // 多会话（间隔 > 60s → 拆为 2 组）
    let msgs = vec![
        make_test_msg("user", "消息1", 1000),
        make_test_msg("assistant", "消息2", 2000),
        make_test_msg("user", "消息3", 602000),
        make_test_msg("assistant", "消息4", 603000),
    ];
    let sessions = split_into_sessions(&msgs, 60000);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].messages.len(), 2);
    assert_eq!(sessions[1].messages.len(), 2);
    // 空输入
    assert!(split_into_sessions(&[], 60000).is_empty());
}

// -- 日期转换 --

/// ts_ms_to_date 各时间戳参数化验证。
#[test]
fn ts_ms_to_date_cases() {
    let cases = [(1704067200000i64, "2024-01-01"), (0i64, "1970-01-01")];
    for (ts, expected) in cases {
        assert_eq!(ts_ms_to_date(ts), expected, "ts={ts}");
    }
}

// -- JSON 元素描述提取 --

/// json_element_description 各元素参数化验证（description > title > None）。
#[test]
fn json_element_description_cases() {
    let cases = [
        (
            serde_json::json!({"type": "json", "data": {"title": "[QQ小程序]示例活动", "description": "示例活动：动画区答题互动..."}}),
            Some("示例活动：动画区答题互动..."),
        ),
        (
            serde_json::json!({"type": "json", "data": {"title": "[QQ小程序]标题文本"}}),
            Some("[QQ小程序]标题文本"),
        ),
        (
            serde_json::json!({"type": "text", "data": {"text": "你好"}}),
            None,
        ),
    ];
    for (element, expected) in cases {
        let desc = json_element_description(&[element]);
        assert_eq!(desc.as_deref(), expected);
    }
}

// -- 辅助函数 --

fn make_test_msg(role: &str, content: &str, created_at: i64) -> ParsedMessage {
    ParsedMessage {
        role: role.to_string(),
        content: content.to_string(),
        created_at,
        fingerprint: make_fingerprint(created_at, role, content),
        sender_uid: String::new(),
        sender_uin: None,
        sender_name: String::new(),
    }
}

// -- 流式解析与整读解析快照等价 --

/// 写临时文件并运行给定解析闭包，返回 (sessions, report)。
fn run_with_file<T>(content: &str, f: impl FnOnce(&Path) -> T) -> T {
    let path = std::env::temp_dir().join(format!("ramaria_stream_eq_{}.json", std::process::id()));
    std::fs::write(&path, content).expect("写入临时文件失败");
    let result = f(&path);
    let _ = std::fs::remove_file(&path);
    result
}

/// 整读解析参考实现（即被流式解析取代前的旧逻辑），仅用于快照对照。
fn parse_qq_export_legacy(
    file_path: &Path,
    gap_minutes: u32,
) -> RamariaResult<(Vec<ImportedSession>, ImportReport)> {
    let gap_ms = (gap_minutes as i64) * 60 * 1000;
    let json_str = std::fs::read_to_string(file_path).expect("读取临时文件失败");
    let raw_data: serde_json::Value = serde_json::from_str(&json_str).expect("整读解析失败");

    let chat_info = raw_data.get("chatInfo").expect("缺少 chatInfo");
    let raw_messages = raw_data
        .get("messages")
        .and_then(|m| m.as_array())
        .expect("缺少 messages 数组");

    let self_uid = chat_info
        .get("selfUid")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let self_name = chat_info
        .get("selfName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let self_uin = chat_info
        .get("selfUin")
        .and_then(|v| v.as_str())
        .filter(|u| !u.is_empty());
    let chat_name = chat_info.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let chat_type = chat_info
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let peer_uid = chat_info
        .get("peerUid")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let peer_uin = chat_info
        .get("peerUin")
        .and_then(|v| v.as_str())
        .filter(|u| !u.is_empty());

    let mut report = ImportReport {
        file_path: file_path.display().to_string(),
        self_id: self_uid.to_string(),
        self_name: self_name.to_string(),
        self_uin: self_uin.map(|s| s.to_string()),
        chat_name: chat_name.to_string(),
        chat_type: chat_type.to_string(),
        other_uid: peer_uid.to_string(),
        other_uin: peer_uin.map(|s| s.to_string()),
        other_name: chat_name.to_string(),
        total_raw: raw_messages.len(),
        gap_minutes,
        ..Default::default()
    };

    let mut seen_keys: HashSet<(String, i64)> = HashSet::new();
    let mut deduped: Vec<&serde_json::Value> = Vec::new();
    for msg in raw_messages {
        let key = (
            msg.get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            msg.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0),
        );
        if seen_keys.contains(&key) {
            report.dedup_removed += 1;
            continue;
        }
        seen_keys.insert(key);
        deduped.push(msg);
    }
    deduped.sort_by_key(|m| m.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0));

    let mut parsed: Vec<ParsedMessage> = Vec::new();
    for msg in &deduped {
        if let Some(p) = parse_json_message(msg, self_uid, self_name, &mut report) {
            parsed.push(p);
        }
    }
    let sessions = split_into_sessions(&parsed, gap_ms);
    report.session_count = sessions.len();
    if !parsed.is_empty() {
        report.time_start = ts_ms_to_date(parsed.first().unwrap().created_at);
        report.time_end = ts_ms_to_date(parsed.last().unwrap().created_at);
    }
    if sessions.is_empty() {
        report
            .warnings
            .push("未解析出任何有效消息（全部被跳过或不支持）".to_string());
    }
    Ok((sessions, report))
}

/// 构造一份含多种消息类型、重复、乱序时间戳的大导出。
fn big_mixed_export() -> String {
    let chat = r#"{"chatInfo":{"selfUid":"u_self","selfName":"小明","selfUin":"10001","name":"小红","type":"private","peerUid":"u_peer","peerUin":"90002"},"messages":["#;
    let mut body = String::new();
    // 基础时间戳基座，按递增制造可排序流；插入重复与乱序。
    let base = 1_700_000_000_000i64;
    for i in 0..1500 {
        let ts = base + i * 1000;
        let sender = if i % 3 == 0 { "u_self" } else { "u_peer" };
        let name = if i % 3 == 0 { "小明" } else { "小红" };
        let content = format!("第 {i} 条消息内容用于快照对比");
        if i > 0 {
            body.push(',');
        }
        // 引入乱序（奇数条比顺序早 1ms）验证稳定排序不改变集合
        let actual_ts = if i % 2 == 1 { ts - 1 } else { ts };
        body.push_str(&format!(
            r#"{{"id":"m_{i}","timestamp":{actual_ts},"type":"text","recalled":false,"system":false,"content":{{"text":"{content}","elements":[]}},"sender":{{"uid":"{sender}","name":"{name}"}}}}"#
        ));
    }
    // 追加重复 id 的消息（同一 (id, ts) 应被去重）
    body.push_str(&format!(
        r#",{{"id":"m_0","timestamp":{},"type":"text","recalled":false,"system":false,"content":{{"text":"重复消息","elements":[]}},"sender":{{"uid":"u_self","name":"小明"}}}}"#,
        base
    ));
    // 追加撤回与未知类型消息
    body.push_str(&format!(
        r#",{{"id":"recalled","timestamp":{},"type":"text","recalled":true,"system":false,"content":{{"text":"撤回","elements":[]}},"sender":{{"uid":"u_self","name":"小明"}}}}"#,
        base + 100_000_000
    ));
    body.push_str(&format!(
        r#",{{"id":"unknown","timestamp":{},"type":"future_type","recalled":false,"system":false,"content":{{"text":"未知","elements":[]}},"sender":{{"uid":"u_peer","name":"小红"}}}}"#,
        base + 200_000_000
    ));
    format!("{chat}{body}]}}")
}

/// 流式解析与整读解析在大导出上的会话/报告快照完全一致。
#[test]
fn streaming_equals_whole_file_snapshot() {
    let content = big_mixed_export();

    let (stream_sessions, stream_report) =
        run_with_file(&content, |p| parse_qq_export(p, 10).expect("流式解析失败"));
    let (legacy_sessions, legacy_report) = run_with_file(&content, |p| {
        parse_qq_export_legacy(p, 10).expect("整读解析失败")
    });

    // 会话结构等价：消息总数与逐条 (role, content, created_at) 一致
    let flatten = |sessions: &[ImportedSession]| {
        let mut v: Vec<(String, String, i64, String)> = Vec::new();
        for s in sessions {
            for m in &s.messages {
                v.push((
                    m.role.clone(),
                    m.content.clone(),
                    m.created_at,
                    m.fingerprint.clone(),
                ));
            }
        }
        v
    };
    assert_eq!(
        flatten(&stream_sessions),
        flatten(&legacy_sessions),
        "流式与整读解析的消息快照应一致"
    );

    // 报告关键统计一致
    assert_eq!(stream_report.session_count, legacy_report.session_count);
    assert_eq!(stream_report.total_raw, legacy_report.total_raw);
    assert_eq!(stream_report.dedup_removed, legacy_report.dedup_removed);
    assert_eq!(stream_report.total_success(), legacy_report.total_success());
    assert_eq!(
        stream_report.total_degraded(),
        legacy_report.total_degraded()
    );
    assert_eq!(stream_report.total_skipped(), legacy_report.total_skipped());
    assert_eq!(stream_report.unknown_types, legacy_report.unknown_types);
    assert_eq!(stream_report.warnings.len(), legacy_report.warnings.len());
    assert_eq!(stream_report.time_start, legacy_report.time_start);
    assert_eq!(stream_report.time_end, legacy_report.time_end);
    assert_eq!(stream_report.other_uid, legacy_report.other_uid);
}
