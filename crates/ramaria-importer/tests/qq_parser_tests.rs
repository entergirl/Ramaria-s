//! rust/crates/ramaria-importer/tests/qq_parser_tests.rs - QQ JSON 解析器集成测试
//!
//! 设计特点:
//! - 测试 qq-chat-exporter v6.x JSON 格式（语义化 type 名称）
//! - 完整覆盖 10 种消息类型：text/reply/audio/json/file/video/forward/type_10/type_19/system/recalled
//! - 测试 session 切割逻辑
//! - 测试格式检测功能
//! - 测试指纹确定性
//! - 测试 peerUid 直接提取
//! - 测试 JSON 卡片描述提取优化
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

// =========================================================
// 格式检测测试
// =========================================================

#[test]
fn detect_json_format_valid() {
    let content = r#"{"chatInfo":{"selfUid":"u_test","selfName":"测试","name":"好友","type":"private","peerUid":"u_friend"},"messages":[{"id":"1","timestamp":1700000000000,"type":"text","content":{"text":"你好","elements":[]},"sender":{"uid":"u_test","name":"测试"},"recalled":false,"system":false}]}"#;
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
            "type": "private",
            "peerUid": "u_friend",
            "peerUin": "123456789"
        },
        "messages": [
            {
                "id": "msg_1",
                "timestamp": 1704067200000,
                "type": "text",
                "recalled": false,
                "system": false,
                "content": {"text": "你好！", "elements": []},
                "sender": {"uid": "u_self", "name": "我自己"}
            },
            {
                "id": "msg_2",
                "timestamp": 1704067260000,
                "type": "text",
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
    // v6.x: 直接从 chatInfo 提取对方标识
    assert_eq!(report.other_uid, "u_friend");
    assert_eq!(report.other_uin.as_deref(), Some("123456789"));
    assert_eq!(report.other_name, "好友A");

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
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "群聊", "type": "group", "peerUid": "u_group"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"text","recalled":false,"system":false,"content":{"text":"正常消息","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"text","recalled":true,"system":false,"content":{"text":"已撤回","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"3","timestamp":1704067320000,"type":"text","recalled":false,"system":false,"content":{"text":"","elements":[]},"sender":{"uid":"u_other","name":"他人"}},
            {"id":"4","timestamp":1704067380000,"type":"text","recalled":false,"system":false,"content":{"text":"第二条正常","elements":[]},"sender":{"uid":"u_other","name":"他人"}}
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
    // 系统消息通过 system==true 字段判断，不依赖 sender 字段
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"text","recalled":false,"system":false,"content":{"text":"正常消息","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"system","recalled":false,"system":true,"content":{"text":"烧酒领取了茄子的红包","elements":[{"type":"system","data":{"subType":17}}]},"sender":{"uid":"未知","name":"系统消息"}},
            {"id":"3","timestamp":1704067320000,"type":"text","recalled":false,"system":false,"content":{"text":"又一条正常","elements":[]},"sender":{"uid":"u_self","name":"我"}}
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
// 降级类型测试 — 语义化名称
// =========================================================

#[test]
fn parse_json_degraded_types() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "测试", "type": "private", "peerUid": "u_other"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"text","recalled":false,"system":false,"content":{"text":"文本","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"audio","recalled":false,"system":false,"content":{"text":"[语音:1秒]","elements":[{"type":"audio","data":{"duration":1}}]},"sender":{"uid":"u_other","name":"对方"}},
            {"id":"3","timestamp":1704067320000,"type":"video","recalled":false,"system":false,"content":{"text":"[视频: test.mp4]","elements":[{"type":"video","data":{}}]},"sender":{"uid":"u_other","name":"对方"}},
            {"id":"4","timestamp":1704067380000,"type":"json","recalled":false,"system":false,"content":{"text":"[JSON消息]","elements":[{"type":"json","data":{"title":"测试卡片"}}]},"sender":{"uid":"u_other","name":"对方"}},
            {"id":"5","timestamp":1704067440000,"type":"forward","recalled":false,"system":false,"content":{"text":"[转发消息: 30条]","elements":[{"type":"forward","data":{"messageCount":30}}]},"sender":{"uid":"u_other","name":"对方"}}
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
// 各降级类型专项测试
// =========================================================

#[test]
fn parse_json_type_file() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"file","recalled":false,"system":false,"content":{"text":"[文件: test.pdf]","elements":[{"type":"text","data":{"text":"[文件: test.pdf]"}},{"type":"file","data":{"filename":"test.pdf","size":1024}}]},"sender":{"uid":"u_self","name":"我"}}
        ]
    }"#;
    let path = create_temp_json("parse_type_file", content);

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
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_10","recalled":false,"system":false,"content":{"text":"红包/钱包消息","elements":[{"type":"wallet","data":{}}]},"sender":{"uid":"u_other","name":"好友"}}
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
    // type_19 通话记录：v6.x 中 content.text 为非空 "通话 - 已在其他设备处理"
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_19","recalled":false,"system":false,"content":{"text":"通话 - 已在其他设备处理","elements":[{"type":"av_record","data":{"summary":"通话 - 已在其他设备处理"}}]},"sender":{"uid":"u_other","name":"好友"}}
        ]
    }"#;
    let path = create_temp_json("parse_type19", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();

    assert_eq!(report.degraded_qce_unsupported, 1);
    assert_eq!(report.total_degraded(), 1);
    assert_eq!(report.skipped_empty, 0); // type_19 有非空 text，不计入 skipped_empty
}

#[test]
fn parse_json_type_19_call_empty_text_defensive() {
    // 防御性测试：当 type_19 的 content.text 为空时（异常情况），应降级而非跳过
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_19","recalled":false,"system":false,"content":{"text":"","elements":[]},"sender":{"uid":"u_other","name":"好友"}}
        ]
    }"#;
    let path = create_temp_json("parse_type19_empty", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();

    assert_eq!(report.degraded_qce_unsupported, 1);
    assert_eq!(report.total_degraded(), 1);
    assert_eq!(report.skipped_empty, 0);
}

// =========================================================
// JSON 卡片描述提取优化测试
// =========================================================

#[test]
fn parse_json_type_json_extracts_description() {
    // json 类型有 data.description 时应提取为降级文本
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"json","recalled":false,"system":false,"content":{"text":"[JSON消息]","elements":[{"type":"json","data":{"title":"[QQ小程序]标题","description":"这是卡片的描述文本"}}]},"sender":{"uid":"u_other","name":"好友"}}
        ]
    }"#;
    let path = create_temp_json("parse_json_desc", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.degraded_card, 1);
    // 降级文本应包含 description
    assert!(
        sessions[0].messages[0]
            .content
            .contains("[卡片: 这是卡片的描述文本]")
    );
}

#[test]
fn parse_json_type_json_fallback_to_title() {
    // json 类型无 description 但有 title
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"json","recalled":false,"system":false,"content":{"text":"[JSON消息]","elements":[{"type":"json","data":{"title":"[QQ小程序]只有标题"}}]},"sender":{"uid":"u_other","name":"好友"}}
        ]
    }"#;
    let path = create_temp_json("parse_json_title", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.degraded_card, 1);
    assert!(
        sessions[0].messages[0]
            .content
            .contains("[卡片: [QQ小程序]只有标题]")
    );
}

#[test]
fn parse_json_type_json_no_desc_or_title() {
    // json 类型既无 description 也无 title
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"json","recalled":false,"system":false,"content":{"text":"[JSON消息]","elements":[{"type":"json","data":{}}]},"sender":{"uid":"u_other","name":"好友"}}
        ]
    }"#;
    let path = create_temp_json("parse_json_none", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.degraded_card, 1);
    assert!(sessions[0].messages[0].content.contains("[卡片消息]"));
}

// =========================================================
// 回复消息测试
// =========================================================

#[test]
fn parse_json_reply_message() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {
                "id": "1",
                "timestamp": 1704067200000,
                "type": "reply",
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
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {
                "id": "1",
                "timestamp": 1704067200000,
                "type": "text",
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
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "测试", "type": "private", "peerUid": "u_other"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"unknown_future_type","recalled":false,"system":false,"content":{"text":"未知类型","elements":[]},"sender":{"uid":"u_self","name":"我"}}
        ]
    }"#;
    let path = create_temp_json("parse_unknown", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();

    assert_eq!(report.skipped_unknown, 1);
    assert!(
        report
            .unknown_types
            .contains(&"unknown_future_type".to_string())
    );
}

// =========================================================
// Session 切割测试
// =========================================================

#[test]
fn parse_json_session_split() {
    // 两条消息间隔超过 10 分钟，应切割为两个 session
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"text","recalled":false,"system":false,"content":{"text":"消息1","elements":[]},"sender":{"uid":"u_self","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"text","recalled":false,"system":false,"content":{"text":"消息2","elements":[]},"sender":{"uid":"u_other","name":"好友"}},
            {"id":"3","timestamp":1704070000000,"type":"text","recalled":false,"system":false,"content":{"text":"消息3（很久后）","elements":[]},"sender":{"uid":"u_self","name":"我"}}
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
    let content = r#"{"chatInfo":{"selfUid":"u","selfName":"n","name":"c","type":"private","peerUid":"u_p"},"messages":[]}"#;
    let path = create_temp_json("parse_empty", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    // 空文件不应报错，返回空 sessions + 含警告的报告
    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();
    assert!(sessions.is_empty());
    assert!(report.warnings.iter().any(|w| w.contains("未解析出")));
}

#[test]
fn parse_json_all_skipped_returns_ok_with_empty_sessions() {
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "测试", "type": "private", "peerUid": "u_other"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"text","recalled":true,"system":false,"content":{"text":"已撤回","elements":[]},"sender":{"uid":"u_self","name":"我"}}
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
        "chatInfo": {"selfUid":"u","selfName":"n","name":"c","type":"private","peerUid":"u_p"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"text","recalled":false,"system":false,"content":{"text":"测试","elements":[]},"sender":{"uid":"u","name":"n"}}
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
        "chatInfo": {"selfUid":"u","selfName":"我","name":"好友","type":"private","peerUid":"u_other"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"text","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"text","recalled":false,"system":false,"content":{"text":"你好呀","elements":[]},"sender":{"uid":"other","name":"好友"}}
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

/// 验证全部降级类型出现在 summary 中。
#[test]
fn import_report_summary_contains_all_degraded_types() {
    let content = r#"{
        "chatInfo": {"selfUid":"u","selfName":"我","name":"好友","type":"private","peerUid":"u_other"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"file","recalled":false,"system":false,"content":{"text":"[文件: a.txt]","elements":[]},"sender":{"uid":"u","name":"我"}},
            {"id":"2","timestamp":1704067260000,"type":"type_10","recalled":false,"system":false,"content":{"text":"红包/钱包消息","elements":[]},"sender":{"uid":"other","name":"好友"}},
            {"id":"3","timestamp":1704067320000,"type":"type_19","recalled":false,"system":false,"content":{"text":"通话 - 已在其他设备处理","elements":[]},"sender":{"uid":"other","name":"好友"}},
            {"id":"4","timestamp":1704067380000,"type":"system","recalled":false,"system":true,"content":{"text":"系统消息","elements":[]},"sender":{"uid":"未知","name":"系统消息"}}
        ]
    }"#;
    let path = create_temp_json("report_degraded", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    let (_sessions, report) = result.unwrap();
    let summary = report.summary();

    // 应有各降级字段
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
// uin 提取与 peerUid 测试
// =========================================================

#[test]
fn parse_json_extracts_self_uin_from_chat_info() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfUin": "123456789",
            "selfName": "我自己",
            "name": "好友A",
            "type": "private",
            "peerUid": "u_friend"
        },
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"text","recalled":false,"system":false,"content":{"text":"你好！","elements":[]},"sender":{"uid":"u_self","name":"我自己"}}
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
fn parse_json_peer_uid_directly_from_chat_info() {
    // v6.x: other_uid/other_uin 直接从 chatInfo.peerUid/peerUin 提取
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfUin": "10001",
            "selfName": "烧酒",
            "name": "omkidaso",
            "type": "private",
            "peerUid": "u_I5Q7jwgQApoZIy8cBGOopA",
            "peerUin": "2232537224"
        },
        "messages": [
            {"id":"1","timestamp":1704067200001,"type":"text","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_self","name":"烧酒","uin":"10001"}},
            {"id":"2","timestamp":1704067200002,"type":"text","recalled":false,"system":false,"content":{"text":"你好呀","elements":[]},"sender":{"uid":"u_I5Q7jwgQApoZIy8cBGOopA","name":"omkidaso","uin":"2232537224"}}
        ]
    }"#;
    let path = create_temp_json("parse_peer_uid", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();

    // 对方标识应直接从 chatInfo 提取
    assert_eq!(report.other_uid, "u_I5Q7jwgQApoZIy8cBGOopA");
    assert_eq!(report.other_uin.as_deref(), Some("2232537224"));
    assert_eq!(report.other_name, "omkidaso");

    // summary 应包含双方标识信息
    let summary = report.summary();
    assert!(summary.contains("QQ号=10001"));
    assert!(summary.contains("omkidaso"));
    assert!(summary.contains("QQ号=2232537224"));
}

#[test]
fn parse_json_sender_uin_missing_is_none() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfName": "我",
            "name": "好友",
            "type": "private",
            "peerUid": "u_friend"
        },
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"text","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_self","name":"我"}}
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
// 双前缀角色映射测试
// =========================================================

#[test]
fn parse_json_dual_prefix_both_parties_have_name_prefix() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfName": "烧酒",
            "name": "omkidaso",
            "type": "private",
            "peerUid": "u_other"
        },
        "messages": [
            {"id":"1","timestamp":1704067200001,"type":"text","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_self","name":"烧酒","uin":"10001"}},
            {"id":"2","timestamp":1704067200002,"type":"text","recalled":false,"system":false,"content":{"text":"你好呀","elements":[]},"sender":{"uid":"u_other","name":"omkidaso","uin":"123456789"}}
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
            "type": "private",
            "peerUid": "u_friend"
        },
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"text","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_self","name":""}}
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
// 指纹——双前缀不影响跨批次一致性
// =========================================================

#[test]
fn parse_json_fingerprint_consistent_with_dual_prefix() {
    let content = r#"{
        "chatInfo": {
            "selfUid": "u_self",
            "selfName": "烧酒",
            "name": "好友",
            "type": "private",
            "peerUid": "u_friend"
        },
        "messages": [
            {"id":"1","timestamp":1704067200001,"type":"text","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_self","name":"烧酒"}},
            {"id":"2","timestamp":1704067200002,"type":"text","recalled":false,"system":false,"content":{"text":"你好呀","elements":[]},"sender":{"uid":"u_friend","name":"好友"}}
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

// =========================================================
// v6.x 特有：type_10/type_19 非空 text 正常解析
// =========================================================

#[test]
fn parse_json_type_10_has_readable_text() {
    // v6.x: type_10 的 content.text 为 "红包/钱包消息"（非空）
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_10","recalled":false,"system":false,"content":{"text":"红包/钱包消息","elements":[{"type":"wallet","data":{"summary":"红包/钱包消息"}}]},"sender":{"uid":"u_other","name":"好友"}}
        ]
    }"#;
    let path = create_temp_json("parse_type10_text", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (sessions, report) = result.unwrap();

    assert_eq!(report.degraded_red_envelope, 1);
    // 降级为 [红包/转账]，非原始 text
    assert!(sessions[0].messages[0].content.contains("[红包/转账]"));
    // 不应计入 skipped_empty
    assert_eq!(report.skipped_empty, 0);
}

#[test]
fn parse_json_type_19_has_readable_text() {
    // v6.x: type_19 的 content.text 为 "通话 - 已在其他设备处理"（非空）
    let content = r#"{
        "chatInfo": {"selfUid": "u_self", "selfName": "我", "name": "好友", "type": "private", "peerUid": "u_friend"},
        "messages": [
            {"id":"1","timestamp":1704067200000,"type":"type_19","recalled":false,"system":false,"content":{"text":"通话 - 已在其他设备处理","elements":[{"type":"av_record","data":{"summary":"通话 - 已在其他设备处理"}}]},"sender":{"uid":"u_other","name":"好友"}}
        ]
    }"#;
    let path = create_temp_json("parse_type19_text", content);

    let result = parser::parse_qq_export(Path::new(&path), 10);
    cleanup(&path);

    assert!(result.is_ok());
    let (_sessions, report) = result.unwrap();

    assert_eq!(report.degraded_qce_unsupported, 1);
    assert_eq!(report.skipped_empty, 0);
}
