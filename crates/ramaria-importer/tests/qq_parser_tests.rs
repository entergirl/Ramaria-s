//! rust/crates/ramaria-importer/tests/qq_parser_tests.rs - QQ JSON 解析器集成测试
//! 设计特点:
//! - 仅测试 qq-chat-exporter v5.x JSON 格式
//! - 完整覆盖 11 种消息类型：type_1/3/6/7/8/9/10/11/19 + system + recalled
//! - 测试 session 切割逻辑
//! - 测试格式检测功能
//! - 测试指纹确定性
//! - 所有测试使用临时文件，不依赖真实 QQ 数据

use std::io::Write;
use std::path::Path;

use ramaria_importer::qq::parser;

// =========================================================
// 测试辅助函数
// =========================================================

/// 创建临时 JSON 文件并返回路径。
fn create_temp_json(name: &str, content: &str) -> String {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("ramaria_test_{name}.json"));
    let mut f = std::fs::File::create(&path).expect("创建临时文件失败");
    f.write_all(content.as_bytes()).expect("写入临时文件失败");
    path.display().to_string()
}

/// 清理临时文件。
fn cleanup(path: &str) {
    let _ = std::fs::remove_file(path);
}

/// 生成一条 qce v5.x 格式的 JSON 消息片段（供后续扩展测试使用）。
#[allow(dead_code)]
fn json_msg(
    id: &str,
    timestamp: i64,
    msg_type: &str,
    text: &str,
    recalled: bool,
    system: bool,
    sender_uid: &str,
    sender_name: &str,
    extra_elements: &str,
) -> String {
    format!(
        r#"{{
            "id": "{id}",
            "timestamp": {timestamp},
            "type": "{msg_type}",
            "recalled": {recalled},
            "system": {system},
            "content": {{
                "text": "{text}",
                "elements": [{extra_elements}]
            }},
            "sender": {{"uid": "{sender_uid}", "name": "{sender_name}"}}
        }}"#
    )
}

// =========================================================
// 格式检测测试
// =========================================================

#[test]
fn detect_json_format_valid() {
    let content = r#"{"chatInfo":{"selfUid":"u_test","selfName":"测试","name":"好友","type":"private"},"messages":[{"id":"1","timestamp":1700000000000,"type":"type_1","content":{"text":"你好","elements":[]},"sender":{"uid":"u_test","name":"测试"},"recalled":false,"system":false}]}"#;
    let path = create_temp_json("detect_json_valid", content);

    let result = parser::detect_qq_format(Path::new(&path));
    cleanup(&path);

    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[test]
fn detect_unknown_format_rejected() {
    let content = r#"{"foo":"bar"}"#;
    let path = create_temp_json("detect_unknown", content);

    let result = parser::detect_qq_format(Path::new(&path));
    cleanup(&path);

    assert!(result.is_ok());
    assert!(!result.unwrap());
}

// =========================================================
// JSON 格式解析测试 — 基础
// =========================================================

#[test]
fn parse_json_basic() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfName": "我自己",
            "name": "好友A",
            "type": "private"
        },
        "messages": [
            {
                "id": "msg_1",
                "timestamp": 1704067200000,
                "type": "type_1",
                "recalled": false,
                "system": false,
                "content": {"text": "你好！", "elements": []},
                "sender": {"uid": "u_self", "name": "我自己"}
            },
            {
                "id": "msg_2",
                "timestamp": 1704067260000,
                "type": "type_1",
                "recalled": false,
                "system": false,
                "content": {"text": "你好呀！", "elements": []},
                "sender": {"uid": "u_friend", "name": "好友A"}
            }
        ]
    }"#;
    let path = create_temp_json("parse_json_basic", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].messages.len(), 2);
    assert_eq!(report.success_text, 2);
    assert_eq!(report.self_name, "我自己");
    assert_eq!(report.chat_name, "好友A");

    // 第一条是自己发的
    assert_eq!(sessions[0].messages[0].role, "user");
    assert_eq!(sessions[0].messages[0].content, "[我自己] 你好！");

    // 第二条是好友发的，应有名称前缀
    assert_eq!(sessions[0].messages[1].role, "assistant");
    assert_eq!(sessions[0].messages[1].content, "[好友A] 你好呀！");
}

// =========================================================
// 跳过规则测试
// =========================================================

#[test]
fn parse_json_with_recalled_and_empty() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "群聊", "type": "group"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"system":false,"content":{"text":"正常消息","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_1","recalled":true,"system":false,"content":{"text":"已撤回","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"3","timestamp":1704067320000,"type":"type_1","recalled":false,"system":false,"content":{"text":"","elements":[]},"sender":{"uid":"u_other","name":"他人"}},
            {"id":"4","timestamp":1704067380000,"type":"type_1","recalled":false,"system":false,"content":{"text":"第二条正常","elements":[]},"sender":{"uid":"u_other","name":"他人"}}
        ]
    }"#;
    let path = create_temp_json("parse_recalled", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.total_raw, 4);
    assert_eq!(report.skipped_recalled, 1);
    assert_eq!(report.skipped_empty, 1);
    assert_eq!(sessions[0].messages.len(), 2);
}

#[test]
fn parse_json_system_messages_skipped() {
    // 系统消息有两种：system==true 的，以及 sender.uin=="0" 的
    // 应优先使用 system 字段判断
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"system":false,"content":{"text":"正常消息","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_1","recalled":false,"system":true,"content":{"text":"[17]","elements":[{"type":"text","data":{"text":"[17]"}}]},"sender":{"uid":"u_I5Q7jwgQApoZIy8cBGOopA","uin":"0","name":"0"}},
            {"id":"3","timestamp":1704067320000,"type":"type_1","recalled":false,"system":false,"content":{"text":"又一条正常","elements":[]},"sender":{"uid":"u_self","name":"我"}}
        ]
    }"#;
    let path = create_temp_json("parse_system", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.total_raw, 3);
    assert_eq!(report.skipped_system, 1);
    assert_eq!(sessions[0].messages.len(), 2);
}

// =========================================================
// 降级类型测试 — 已有类型 (type_6/7/9/11)
// =========================================================

#[test]
fn parse_json_degraded_types() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "测试", "type": "private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"system":false,"content":{"text":"文本","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_6","recalled":false,"system":false,"content":{"text":"[语音 1秒]","elements":[]},"sender":{"uid":"u_other","name":"对方"}},
            {"id":"3","timestamp":1704067320000,"type":"type_9","recalled":false,"system":false,"content":{"text":"[视频: test.mp4]","elements":[]},"sender":{"uid":"u_other","name":"对方"}},
            {"id":"4","timestamp":1704067380000,"type":"type_7","recalled":false,"system":false,"content":{"text":"[卡片消息: ...]","elements":[]},"sender":{"uid":"u_other","name":"对方"}},
            {"id":"5","timestamp":1704067440000,"type":"type_11","recalled":false,"system":false,"content":{"text":"[合并转发: 30条]","elements":[]},"sender":{"uid":"u_other","name":"对方"}}
        ]
    }"#;
    let path = create_temp_json("parse_degraded", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();

    assert_eq!(report.success_text, 1);
    assert_eq!(report.degraded_audio, 1);
    assert_eq!(report.degraded_video, 1);
    assert_eq!(report.degraded_card, 1);
    assert_eq!(report.degraded_forward, 1);
}

// =========================================================
// P0 降级类型测试 — (type_8 文件/type_10 红包/type_19 通话)
// =========================================================

#[test]
fn parse_json_type_8_file() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_8","recalled":false,"system":false,"content":{"text":"[文件: test.pdf]","elements":[{"type":"text","data":{"text":"[文件: test.pdf]"}},{"type":"file","data":{"filename":"test.pdf","size":1024}}]},"sender":{"uid":"u_self","name":"我"}}
        ]
    }"#;
    let path = create_temp_json("parse_type8", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.degraded_file, 1);
    assert_eq!(report.total_degraded(), 1);
    // 文件消息保留原始 text（含文件名）
    assert!(sessions[0].messages[0].content.contains("[文件: test.pdf]"));
}

#[test]
fn parse_json_type_10_red_envelope() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_10","recalled":false,"system":false,"content":{"text":"[UNKNOWN_9消息]","elements":[{"type":"text","data":{"text":"[UNKNOWN_9消息]"}}]},"sender":{"uid":"u_other","name":"好友"}}
        ]
    }"#;
    let path = create_temp_json("parse_type10", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.degraded_red_envelope, 1);
    assert_eq!(report.total_degraded(), 1);
    assert!(sessions[0].messages[0].content.contains("[红包/转账]"));
}

#[test]
fn parse_json_type_19_call() {
    // type_19 通话记录：content.text 为空，elements 为空
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_19","recalled":false,"system":false,"content":{"text":"","elements":[]},"sender":{"uid":"u_other","name":"好友"}}
        ]
    }"#;
    let path = create_temp_json("parse_type19", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();

    assert_eq!(report.degraded_qce_unsupported, 1);
    assert_eq!(report.total_degraded(), 1);
    assert_eq!(report.skipped_empty, 0); // type_19 不应计入 skipped_empty
}

// =========================================================
// 回复消息测试
// =========================================================

#[test]
fn parse_json_reply_message() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private"},
        "messages": [
            {
                "id": "1",
                "timestamp": 1704067200000,
                "type": "type_3",
                "recalled": false,
                "system": false,
                "content": {
                    "text": "这是回复的正文",
                    "elements": [
                        {
                            "type": "reply",
                            "data": {
                                "senderName": "好友",
                                "content": "之前的消息内容"
                            }
                        }
                    ]
                },
                "sender": {"uid": "u_self", "name": "我"}
            }
        ]
    }"#;
    let path = create_temp_json("parse_reply", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.success_reply, 1);
    let msg = &sessions[0].messages[0];
    assert!(msg.content.contains("「回复 好友: 之前的消息内容」"));
    assert!(msg.content.contains("这是回复的正文"));
}

// =========================================================
// 图片消息测试
// =========================================================

#[test]
fn parse_json_with_image() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private"},
        "messages": [
            {
                "id": "1",
                "timestamp": 1704067200000,
                "type": "type_1",
                "recalled": false,
                "system": false,
                "content": {
                    "text": "看这张图 [图片: abc123def456.jpg]",
                    "elements": [
                        {"type": "image", "data": {}}
                    ]
                },
                "sender": {"uid": "u_self", "name": "我"}
            }
        ]
    }"#;
    let path = create_temp_json("parse_image", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.success_image, 1);
    let msg = &sessions[0].messages[0];
    // 图片占位符应替换为 [图片]
    assert!(msg.content.contains("[图片]"));
    assert!(!msg.content.contains("abc123def456"));
}

// =========================================================
// 未知类型测试
// =========================================================

#[test]
fn parse_json_unknown_type_skipped() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "测试", "type": "private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_99","recalled":false,"system":false,"content":{"text":"未知类型","elements":[]},"sender":{"uid":"u_self","name":"我"}}
        ]
    }"#;
    let path = create_temp_json("parse_unknown", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();

    assert_eq!(report.skipped_unknown, 1);
    assert!(report.unknown_types.contains(&"type_99".to_string()));
}

// =========================================================
// Session 切割测试
// =========================================================

#[test]
fn parse_json_session_split() {
    // 两条消息间隔超过 10 分钟，应切割为两个 session
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"system":false,"content":{"text":"消息1","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_1","recalled":false,"system":false,"content":{"text":"消息2","elements":[]},"sender":{"uid":"u_other","name":"好友"}},
            {"id":"3","timestamp":1704070000000,"type":"type_1","recalled":false,"system":false,"content":{"text":"消息3（很久后）","elements":[]},"sender":{"uid":"u_self","name":"我"}}
        ]
    }"#;
    let path = create_temp_json("parse_split", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].messages.len(), 2);
    assert_eq!(sessions[1].messages.len(), 1);
    assert_eq!(report.session_count, 2);
}

// =========================================================
// 边界情况测试
// =========================================================

#[test]
fn parse_json_empty_file_returns_ok_with_empty_sessions() {
    let content =
        r#"{"chatInfo":{"selfUid":"u","selfName":"n","name":"c","type":"private"},"messages":[]}"#;
    let path = create_temp_json("parse_empty", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    // 空文件不应报错，而是返回空 sessions + 含警告的报告
    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();
    assert!(sessions.is_empty());
    assert!(report.warnings.iter().any(|w| w.contains("未解析出")));
}

#[test]
fn parse_json_all_skipped_returns_ok_with_empty_sessions() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "测试", "type": "private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":true,"system":false,"content":{"text":"已撤回","elements":[]},"sender":{"uid":"u_self","name":"我"}}
        ]
    }"#;
    let path = create_temp_json("parse_all_skipped", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    // 全部跳过不应报错，返回空 sessions + 含统计的报告
    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();
    assert!(sessions.is_empty());
    assert_eq!(report.skipped_recalled, 1);
}

#[test]
fn parse_json_file_not_found() {
    let result = parser::parse_qq_export(Path::new("non_existent_file_12345.json"), 10);
    assert!(result.is_err());
}

// =========================================================
// 指纹确定性测试
// =========================================================

/// 通过完整 JSON 解析验证指纹一致性。
#[test]
fn fingerprints_are_deterministic_across_parses() {
    let content = r#"{
        "chatInfo": {"selfUid":"u","selfName":"n","name":"c","type":"private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"system":false,"content":{"text":"测试","elements":[]},"sender":{"uid":"u","name":"n"}}
        ]
    }"#;

    let path1 = create_temp_json("fp_test_1", content);
    let path2 = create_temp_json("fp_test_2", content);

    let result1 = parser::parse_qq_export(Path::new(&path1), 10).unwrap();
    let result2 = parser::parse_qq_export(Path::new(&path2), 10).unwrap();

    cleanup(&path1);
    cleanup(&path2);

    assert_eq!(
        result1.0[0].messages[0].fingerprint,
        result2.0[0].messages[0].fingerprint
    );
}

// =========================================================
// 导入报告测试
// =========================================================

#[test]
fn import_report_summary_contains_key_info() {
    let content = r#"{
        "chatInfo": {"selfUid":"u","selfName":"我","name":"好友","type":"private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好呀","elements":[]},"sender":{"uid":"other","name":"好友"}}
        ]
    }"#;
    let path = create_temp_json("report_test", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    let (_sessions, report) = result.unwrap();
    let summary = report.summary();

    assert!(summary.contains("我"));
    assert!(summary.contains("好友"));
    assert!(summary.contains("2024-01-01"));
    assert!(summary.contains("成功"));
}

/// 验证 新增字段出现在 summary 中。
#[test]
fn import_report_summary_contains_v11_fields() {
    let content = r#"{
        "chatInfo": {"selfUid":"u","selfName":"我","name":"好友","type":"private"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_8","recalled":false,"system":false,"content":{"text":"[文件: a.txt]","elements":[]},"sender":{"uid":"u","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_10","recalled":false,"system":false,"content":{"text":"[UNKNOWN_9消息]","elements":[]},"sender":{"uid":"other","name":"好友"}},
            {"id":"3","timestamp":1704067320000,"type":"type_19","recalled":false,"system":false,"content":{"text":"","elements":[]},"sender":{"uid":"other","name":"好友"}},
            {"id":"4","timestamp":1704067380000,"type":"type_1","recalled":false,"system":true,"content":{"text":"[17]","elements":[]},"sender":{"uid":"other","uin":"0","name":"0"}}
        ]
    }"#;
    let path = create_temp_json("report_v11", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    let (_sessions, report) = result.unwrap();
    let summary = report.summary();

    // 应有 新增字段
    assert!(summary.contains("文件"));
    assert!(summary.contains("红包/转账"));
    assert!(summary.contains("qce未解析"));
    assert!(summary.contains("系统"));

    // 统计应准确
    assert_eq!(report.degraded_file, 1);
    assert_eq!(report.degraded_red_envelope, 1);
    assert_eq!(report.degraded_qce_unsupported, 1);
    assert_eq!(report.skipped_system, 1);
}

// =========================================================
// uin 提取测试（T-V11-5B-001）
// =========================================================

#[test]
fn parse_json_extracts_self_uin_from_chat_info() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfUin": "123456789",
            "selfName": "我自己",
            "name": "好友A",
            "type": "private"
        },
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好！","elements":[]},"sender":{"uid":"u_self","name":"我自己"}}
        ]
    }"#;
    let path = create_temp_json("parse_uin_self", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();
    assert_eq!(report.self_uin.as_deref(), Some("123456789"));
}

#[test]
fn parse_json_extracts_sender_uin_from_messages() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfUin": "10001",
            "selfName": "我",
            "name": "好友",
            "type": "private"
        },
        "messages": [
            {"id":"1","timestamp":1704067200001,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_self","name":"我","uin":"10001"}},
            {"id":"2","timestamp":1704067200002,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好呀","elements":[]},"sender":{"uid":"u_other","name":"好友","uin":"123456789"}}
        ]
    }"#;
    let path = create_temp_json("parse_uin_sender", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, _report) = result.unwrap();
    assert_eq!(sessions.len(), 1);
    // 导出者的消息
    let self_msg = &sessions[0].messages[0];
    assert_eq!(self_msg.sender_uid, "u_self");
    assert_eq!(self_msg.sender_uin.as_deref(), Some("10001"));
    assert_eq!(self_msg.sender_name, "我");
    // 对方的消息
    let other_msg = &sessions[0].messages[1];
    assert_eq!(other_msg.sender_uid, "u_other");
    assert_eq!(other_msg.sender_uin.as_deref(), Some("123456789"));
    assert_eq!(other_msg.sender_name, "好友");
}

#[test]
fn parse_json_sender_uin_missing_is_none() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfName": "我",
            "name": "好友",
            "type": "private"
        },
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_self","name":"我"}}
        ]
    }"#;
    let path = create_temp_json("parse_uin_missing", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();
    // selfUin 不存在
    assert_eq!(report.self_uin, None);
}

// =========================================================
// 双前缀角色映射测试（T-V11-5B-004）
// =========================================================

#[test]
fn parse_json_dual_prefix_both_parties_have_name_prefix() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfName": "烧酒",
            "name": "omkidaso",
            "type": "private"
        },
        "messages": [
            {"id":"1","timestamp":1704067200001,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_self","name":"烧酒","uin":"10001"}},
            {"id":"2","timestamp":1704067200002,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好呀","elements":[]},"sender":{"uid":"u_other","name":"omkidaso","uin":"123456789"}}
        ]
    }"#;
    let path = create_temp_json("parse_dual_prefix", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, _report) = result.unwrap();
    assert_eq!(sessions.len(), 1);
    // 导出者消息应有 [烧酒] 前缀
    assert_eq!(sessions[0].messages[0].role, "user");
    assert_eq!(sessions[0].messages[0].content, "[烧酒] 你好");
    // 对方消息也有 [omkidaso] 前缀
    assert_eq!(sessions[0].messages[1].role, "assistant");
    assert_eq!(sessions[0].messages[1].content, "[omkidaso] 你好呀");
}

#[test]
fn parse_json_dual_prefix_export_with_empty_self_name_uses_wo() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfName": "",
            "name": "好友",
            "type": "private"
        },
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_self","name":""}}
        ]
    }"#;
    let path = create_temp_json("parse_empty_self_name", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, _report) = result.unwrap();
    // self_name 为空时，导出者消息使用 [我] 前缀
    assert_eq!(sessions[0].messages[0].content, "[我] 你好");
}

// =========================================================
// ImportReport other_* 字段测试（T-V11-5B-003）
// =========================================================

#[test]
fn parse_json_report_populates_other_fields() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfUin": "10001",
            "selfName": "烧酒",
            "name": "omkidaso",
            "type": "private"
        },
        "messages": [
            {"id":"1","timestamp":1704067200001,"type":"type_1","recalled":false,"system":false,"content":{"text":"先发","elements":[]},"sender":{"uid":"u_self","name":"烧酒","uin":"10001"}},
            {"id":"2","timestamp":1704067200002,"type":"type_1","recalled":false,"system":false,"content":{"text":"后回","elements":[]},"sender":{"uid":"u_other","name":"omkidaso","uin":"123456789"}}
        ]
    }"#;
    let path = create_temp_json("parse_other_fields", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();

    assert_eq!(report.self_uin.as_deref(), Some("10001"));
    assert_eq!(report.other_uid, "u_other");
    assert_eq!(report.other_uin.as_deref(), Some("123456789"));
    assert_eq!(report.other_name, "omkidaso");

    // summary 应包含双方标识信息
    let summary = report.summary();
    assert!(summary.contains("QQ号=10001"));
    assert!(summary.contains("omkidaso"));
    assert!(summary.contains("QQ号=123456789"));
}

// =========================================================
// 指纹——双前缀不影响跨批次一致性
// =========================================================

#[test]
fn parse_json_fingerprint_consistent_with_dual_prefix() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfName": "烧酒",
            "name": "好友",
            "type": "private"
        },
        "messages": [
            {"id":"1","timestamp":1704067200001,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_self","name":"烧酒"}},
            {"id":"2","timestamp":1704067200002,"type":"type_1","recalled":false,"system":false,"content":{"text":"你好呀","elements":[]},"sender":{"uid":"u_other","name":"好友"}}
        ]
    }"#;

    let path1 = create_temp_json("fp_dual_1", content);
    let path2 = create_temp_json("fp_dual_2", content);

    let result1 = parser::parse_qq_export(Path::new(&path1), 10).unwrap();
    let result2 = parser::parse_qq_export(Path::new(&path2), 10).unwrap();

    cleanup(&path1);
    cleanup(&path2);

    // 双方前缀一致，指纹应相同
    assert_eq!(
        result1.0[0].messages[0].fingerprint,
        result2.0[0].messages[0].fingerprint
    );
    assert_eq!(
        result1.0[0].messages[1].fingerprint,
        result2.0[0].messages[1].fingerprint
    );
}
