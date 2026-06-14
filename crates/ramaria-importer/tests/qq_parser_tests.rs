//! rust/crates/ramaria-importer/tests/qq_parser_tests.rs - QQ 解析器集成测试
//!
//! 设计特点:
//! - 测试 JSON（qq-chat-exporter）和 .txt 两种格式的解析
//! - 覆盖正常消息、撤回、空消息、未知类型等边界情况
//! - 测试 session 切割逻辑
//! - 测试格式检测功能
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

/// 创建临时 TXT 文件并返回路径。
fn create_temp_txt(name: &str, content: &str) -> String {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("ramaria_test_{name}.txt"));
    let mut f = std::fs::File::create(&path).expect("创建临时文件失败");
    f.write_all(content.as_bytes()).expect("写入临时文件失败");
    path.display().to_string()
}

/// 清理临时文件。
fn cleanup(path: &str) {
    let _ = std::fs::remove_file(path);
}

// =========================================================
// 格式检测测试
// =========================================================

#[test]
fn detect_json_format() {
    let content = r#"{"chatInfo":{"selfUid":"u_test","selfName":"测试","name":"好友","type":"friend"},"messages":[{"id":"1","timestamp":1700000000000,"type":"type_1","content":{"text":"你好","elements":[]},"sender":{"uid":"u_test","name":"测试"},"recalled":false}]}"#;
    let path = create_temp_json("detect_json", content);

    let result = parser::detect_qq_format(Path::new(&path));
    cleanup(&path);

    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[test]
fn detect_txt_format() {
    let content = "2024-01-01 12:00:00 张三\n你好\n\n2024-01-01 12:01:00 李四\n你好呀";
    let path = create_temp_txt("detect_txt", content);

    let result = parser::detect_qq_format(Path::new(&path));
    cleanup(&path);

    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[test]
fn detect_unknown_format() {
    let content = "这是一段普通文本，不是QQ聊天记录";
    let path = create_temp_txt("detect_unknown", content);

    let result = parser::detect_qq_format(Path::new(&path));
    cleanup(&path);

    assert!(result.is_ok());
    assert!(!result.unwrap());
}

// =========================================================
// JSON 格式解析测试
// =========================================================

#[test]
fn parse_json_basic() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfName": "我自己",
            "name": "好友A",
            "type": "friend"
        },
        "messages": [
            {
                "id": "msg_1",
                "timestamp": 1704067200000,
                "time": "2024-01-01 12:00:00",
                "type": "type_1",
                "recalled": false,
                "content": {
                    "text": "你好！",
                    "elements": []
                },
                "sender": {
                    "uid": "u_self",
                    "name": "我自己"
                }
            },
            {
                "id": "msg_2",
                "timestamp": 1704067260000,
                "time": "2024-01-01 12:01:00",
                "type": "type_1",
                "recalled": false,
                "content": {
                    "text": "你好呀！",
                    "elements": []
                },
                "sender": {
                    "uid": "u_friend",
                    "name": "好友A"
                }
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
    assert_eq!(sessions[0].messages[0].content, "你好！");

    // 第二条是好友发的，应有名称前缀
    assert_eq!(sessions[0].messages[1].role, "assistant");
    assert_eq!(sessions[0].messages[1].content, "[好友A] 你好呀！");
}

#[test]
fn parse_json_with_recalled_and_empty() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "群聊", "type": "group"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"content":{"text":"正常消息","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_1","recalled":true,"content":{"text":"已撤回","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"3","timestamp":1704067320000,"type":"type_1","recalled":false,"content":{"text":"","elements":[]},"sender":{"uid":"u_other","name":"他人"}},
            {"id":"4","timestamp":1704067380000,"type":"type_1","recalled":false,"content":{"text":"第二条正常","elements":[]},"sender":{"uid":"u_other","name":"他人"}}
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
    assert_eq!(sessions[0].messages.len(), 2); // 只有两条正常消息
}

#[test]
fn parse_json_degraded_types() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "测试", "type": "friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"content":{"text":"文本","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_6","recalled":false,"content":{"text":"语音描述","elements":[]},"sender":{"uid":"u_other","name":"对方"}},
            {"id":"3","timestamp":1704067320000,"type":"type_9","recalled":false,"content":{"text":"视频描述","elements":[]},"sender":{"uid":"u_other","name":"对方"}},
            {"id":"4","timestamp":1704067380000,"type":"type_7","recalled":false,"content":{"text":"卡片","elements":[]},"sender":{"uid":"u_other","name":"对方"}},
            {"id":"5","timestamp":1704067440000,"type":"type_11","recalled":false,"content":{"text":"转发","elements":[]},"sender":{"uid":"u_other","name":"对方"}}
        ]
    }"#;
    let path = create_temp_json("parse_degraded", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.success_text, 1);
    assert_eq!(report.degraded_audio, 1);
    assert_eq!(report.degraded_video, 1);
    assert_eq!(report.degraded_card, 1);
    assert_eq!(report.degraded_forward, 1);

    // 验证降级消息的内容
    let msgs = &sessions[0].messages;
    let audio_msg = msgs.iter().find(|m| m.content.contains("[语音]")).unwrap();
    assert!(audio_msg.content.contains("[语音]"));
    assert!(audio_msg.role == "assistant");

    let video_msg = msgs.iter().find(|m| m.content.contains("[视频]")).unwrap();
    assert!(video_msg.content.contains("[视频]"));

    let card_msg = msgs
        .iter()
        .find(|m| m.content.contains("[卡片消息]"))
        .unwrap();
    assert!(card_msg.content.contains("[卡片消息]"));

    let forward_msg = msgs
        .iter()
        .find(|m| m.content.contains("[转发消息]"))
        .unwrap();
    assert!(forward_msg.content.contains("[转发消息]"));
}

#[test]
fn parse_json_unknown_type_skipped() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "测试", "type": "friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_99","recalled":false,"content":{"text":"未知类型","elements":[]},"sender":{"uid":"u_self","name":"我"}}
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

#[test]
fn parse_json_reply_message() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "friend"},
        "messages": [
            {
                "id": "1",
                "timestamp": 1704067200000,
                "type": "type_3",
                "recalled": false,
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

#[test]
fn parse_json_with_image() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "friend"},
        "messages": [
            {
                "id": "1",
                "timestamp": 1704067200000,
                "type": "type_1",
                "recalled": false,
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

#[test]
fn parse_json_session_split() {
    // 两条消息间隔超过 10 分钟，应切割为两个 session
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"content":{"text":"消息1","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_1","recalled":false,"content":{"text":"消息2","elements":[]},"sender":{"uid":"u_other","name":"好友"}},
            {"id":"3","timestamp":1704070000000,"type":"type_1","recalled":false,"content":{"text":"消息3（很久后）","elements":[]},"sender":{"uid":"u_self","name":"我"}}
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

#[test]
fn parse_json_empty_file_returns_ok_with_empty_sessions() {
    let content =
        r#"{"chatInfo":{"selfUid":"u","selfName":"n","name":"c","type":"f"},"messages":[]}"#;
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
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "测试", "type": "friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":true,"content":{"text":"已撤回","elements":[]},"sender":{"uid":"u_self","name":"我"}}
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

// =========================================================
// TXT 格式解析测试
// =========================================================

#[test]
fn parse_txt_basic() {
    let content = "2024-01-01 12:00:00 张三\n你好！\n\n2024-01-01 12:01:00 李四\n你好呀！\n\n2024-01-01 12:02:00 张三\n今天天气不错";
    let path = create_temp_txt("parse_txt_basic", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].messages.len(), 3);
    assert_eq!(report.chat_type, "txt_export");
    // 第一条被识别为导出者（张三）
    assert_eq!(sessions[0].messages[0].role, "user");
    assert_eq!(sessions[0].messages[0].content, "你好！");
    // 第二条是对方（李四）
    assert_eq!(sessions[0].messages[1].role, "assistant");
    assert!(sessions[0].messages[1].content.contains("[李四]"));
}

#[test]
fn parse_txt_multiline_message() {
    let content =
        "2024-01-01 12:00:00 张三\n第一行\n第二行\n第三行\n\n2024-01-01 12:01:00 李四\n回复消息";
    let path = create_temp_txt("parse_txt_multiline", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, _report) = result.unwrap();

    assert_eq!(sessions[0].messages.len(), 2);
    assert_eq!(sessions[0].messages[0].content, "第一行\n第二行\n第三行");
}

#[test]
fn parse_txt_slash_date_format() {
    let content = "2024/01/01 12:00:00 张三\n你好\n\n2024/01/01 12:01:00 李四\n你好呀";
    let path = create_temp_txt("parse_txt_slash", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, _report) = result.unwrap();
    assert_eq!(sessions[0].messages.len(), 2);
}

#[test]
fn parse_txt_no_messages_returns_ok_with_empty_sessions() {
    let content = "这是一段普通文本\n没有任何时间戳行";
    let path = create_temp_txt("parse_txt_nomsg", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    // 无法解析出消息，但应返回 Ok 含空 sessions 和警告
    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();
    assert!(sessions.is_empty());
    assert!(report.warnings.iter().any(|w| w.contains("未解析出")));
}

#[test]
fn parse_txt_file_not_found() {
    let result = parser::parse_qq_export(Path::new("non_existent_file_12345.txt"), 10);
    assert!(result.is_err());
}

// =========================================================
// 指纹确定性测试
// =========================================================

/// 通过完整 JSON 解析验证指纹一致性。
#[test]
fn fingerprints_are_deterministic_across_parses() {
    let content = r#"{
        "chatInfo": {"selfUid":"u","selfName":"n","name":"c","type":"f"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"content":{"text":"测试","elements":[]},"sender":{"uid":"u","name":"n"}}
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
        "chatInfo": {"selfUid":"u","selfName":"我","name":"好友","type":"friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_1","recalled":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_1","recalled":false,"content":{"text":"你好呀","elements":[]},"sender":{"uid":"other","name":"好友"}}
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
    assert!(summary.contains("2")); // total success
}
