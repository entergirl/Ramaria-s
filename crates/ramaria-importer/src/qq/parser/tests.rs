//! crates/ramaria-importer/src/qq/parser/tests.rs - QQ JSON 解析器单元测试
//!
//! 设计特点:
//! - 覆盖图片元素提取与占位符渲染（配对、回退链、幂等）/ 回复正文提取 /
//!   指纹性质 / 会话切割 / 日期换算
//! - 覆盖 JSON 元素描述提取优先级（description > title > None）
//! - 流式解析与整读参考实现在大导出上的会话/报告快照等价对照
//! - 全程使用临时文件与内存数据，不依赖真实 QQ 数据

use std::collections::HashSet;
use std::path::Path;

use ramaria_core::error::RamariaResult;
use ramaria_core::types::MemberRole;

use crate::traits::{ImportReport, ImportedSession, ParsedMessage};

use super::*;

// -- 图片元素提取与占位符渲染 --

/// 构造 image 元素 JSON（filename + md5）。
fn image_json(filename: &str, md5: &str) -> serde_json::Value {
    serde_json::json!({"type": "image", "data": {"filename": filename, "md5": md5}})
}

/// image_element_infos：全字段读取，md5 小写规范化。
#[test]
fn image_element_infos_reads_full_fields() {
    let elements = vec![serde_json::json!({
        "type": "image",
        "data": {
            "filename": "EA12E26D5376DBD64D163300CE6EECE6.jpg",
            "size": 241558,
            "width": 960,
            "height": 1728,
            "md5": "EA12E26D5376DBD64D163300CE6EECE6",
            "url": "resources/images/ea12_EA12.jpg",
            "localPath": "images/ea12_EA12.jpg",
            "subType": "photo"
        }
    })];
    let infos = image_element_infos(&elements);
    assert_eq!(infos.len(), 1);
    assert_eq!(
        infos[0].md5.as_deref(),
        Some("ea12e26d5376dbd64d163300ce6eece6"),
        "md5 应小写规范化"
    );
    assert_eq!(
        infos[0].filename.as_deref(),
        Some("EA12E26D5376DBD64D163300CE6EECE6.jpg")
    );
    assert_eq!(
        infos[0].url.as_deref(),
        Some("resources/images/ea12_EA12.jpg")
    );
    assert_eq!(infos[0].local_path.as_deref(), Some("images/ea12_EA12.jpg"));
    assert_eq!(infos[0].size, Some(241558));
    assert_eq!(infos[0].width, Some(960));
    assert_eq!(infos[0].height, Some(1728));
    assert_eq!(infos[0].sub_type.as_deref(), Some("photo"));
}

/// image_element_infos：空 data 全 None、字段部分缺失、非 image 元素不产出。
#[test]
fn image_element_infos_empty_and_partial_fields() {
    // 空 data {}
    let infos = image_element_infos(&[serde_json::json!({"type": "image", "data": {}})]);
    assert_eq!(infos.len(), 1);
    assert!(infos[0].md5.is_none());
    assert!(infos[0].filename.is_none());
    assert!(infos[0].url.is_none());
    assert!(infos[0].local_path.is_none());
    assert!(infos[0].size.is_none());
    assert!(infos[0].width.is_none());
    assert!(infos[0].height.is_none());
    assert!(infos[0].sub_type.is_none());

    // 字段部分缺失（仅 md5）
    let infos = image_element_infos(&[serde_json::json!({
        "type": "image",
        "data": {"md5": "aabbccdd"}
    })]);
    assert_eq!(infos[0].md5.as_deref(), Some("aabbccdd"));
    assert!(infos[0].filename.is_none());

    // 非 image 元素不产出
    let infos = image_element_infos(&[serde_json::json!({"type": "text", "data": {"text": "hi"}})]);
    assert!(infos.is_empty());
}

/// 占位符渲染：filename 命中优先，不按顺序消费。
#[test]
fn render_matches_filename_first() {
    let infos = image_element_infos(&[
        image_json("AAA.jpg", "aabbccddeeff00112233445566778899"),
        image_json("BBB.jpg", "11223344556677889900aabbccddeeff"),
    ]);
    assert_eq!(
        render_image_placeholders("看图 [图片:BBB.jpg]", &infos),
        "看图 [图片#11223344]",
        "filename 命中第二个元素时应取其 md5 hash"
    );
}

/// 占位符渲染：filename 未命中 → 按序取下一个未使用元素。
#[test]
fn render_falls_back_to_next_unused_element_in_order() {
    let infos = image_element_infos(&[
        image_json("AAA.jpg", "aabbccddeeff00112233445566778899"),
        image_json("BBB.jpg", "11223344556677889900aabbccddeeff"),
    ]);
    assert_eq!(
        render_image_placeholders("[图片:CCC.jpg][图片:DDD.jpg]", &infos),
        "[图片#aabbccdd][图片#11223344]",
        "未命中时按序消费元素"
    );
}

/// 占位符渲染：多占位符多元素按序配对（filename 命中与顺序兜底混合）。
#[test]
fn render_pairs_multiple_placeholders_in_order() {
    let infos = image_element_infos(&[
        image_json("AAA.jpg", "aabbccddeeff00112233445566778899"),
        image_json("BBB.jpg", "11223344556677889900aabbccddeeff"),
    ]);
    assert_eq!(
        render_image_placeholders("前 [图片:BBB.jpg] 后 [图片:AAA.jpg]", &infos),
        "前 [图片#11223344] 后 [图片#aabbccdd]"
    );
}

/// 占位符渲染：无元素时从内容提取 32 位连续 hex（大小写不敏感）。
#[test]
fn render_extracts_32_hex_without_elements() {
    assert_eq!(
        render_image_placeholders("[图片:EA12E26D5376DBD64D163300CE6EECE6.jpg]", &[]),
        "[图片#ea12e26d]"
    );
    assert_eq!(
        render_image_placeholders("[图片:ea12e26d5376dbd64d163300ce6eece6.png]", &[]),
        "[图片#ea12e26d]",
        "hex 段大小写不敏感"
    );
    // 带空格（早期文档形态）同样命中
    assert_eq!(
        render_image_placeholders("[图片: EA12E26D5376DBD64D163300CE6EECE6.jpg]", &[]),
        "[图片#ea12e26d]"
    );
}

/// 占位符渲染：无可用 32 位 hex 段时 sha256 前 8 位兜底。
#[test]
fn render_hashes_non_hex_placeholder() {
    // 内容是 12 位 hex（长度不足 32）→ sha256("abc123def456.jpg") 前 8 位
    assert_eq!(
        render_image_placeholders("[图片:abc123def456.jpg]", &[]),
        "[图片#a7486078]"
    );
}

/// 占位符渲染：畸形 `[图片:` 原样保留；幂等（已渲染文本零变化）。
#[test]
fn render_keeps_malformed_and_is_idempotent() {
    assert_eq!(
        render_image_placeholders("前[图片:abc 后", &[]),
        "前[图片:abc 后",
        "找不到 ] 的畸形占位符应原样保留"
    );

    let rendered = render_image_placeholders("图 [图片:abc123def456.jpg] 完", &[]);
    assert_eq!(
        render_image_placeholders(&rendered, &[]),
        rendered,
        "已渲染文本再次渲染零变化"
    );
    assert_eq!(render_image_placeholders("普通消息", &[]), "普通消息");
}

/// 无文本纯图占位符：首个元素有 md5 → hash 形态；否则纯文字回退。
#[test]
fn fallback_placeholder_uses_first_element_md5() {
    let infos = image_element_infos(&[
        image_json("AAA.jpg", "AABBCCDDEEFF00112233445566778899"),
        image_json("BBB.jpg", "11223344556677889900aabbccddeeff"),
    ]);
    assert_eq!(fallback_image_placeholder(&infos), "[图片#aabbccdd]");

    assert_eq!(fallback_image_placeholder(&[]), "[图片]");
    let no_md5 = image_element_infos(&[serde_json::json!({"type": "image", "data": {}})]);
    assert_eq!(fallback_image_placeholder(&no_md5), "[图片]");
}

/// source_ref 规范化：url 优先 / localPath 补前缀 / 均无空串 / 特殊 url 原样。
#[test]
fn normalize_source_ref_cases() {
    let one = |data: serde_json::Value| {
        let infos = image_element_infos(&[serde_json::json!({"type": "image", "data": data})]);
        normalize_source_ref(&infos[0])
    };

    // url 优先（相对路径直接使用）
    assert_eq!(
        one(serde_json::json!({
            "url": "resources/images/a_b.jpg",
            "localPath": "images/a_b.jpg"
        })),
        "resources/images/a_b.jpg"
    );
    // url 缺失 → localPath 补 resources/ 前缀
    assert_eq!(
        one(serde_json::json!({"localPath": "images/a_b.jpg"})),
        "resources/images/a_b.jpg"
    );
    // localPath 已带前缀 → 不重复补
    assert_eq!(
        one(serde_json::json!({"localPath": "resources/images/a_b.jpg"})),
        "resources/images/a_b.jpg"
    );
    // 均无 → 空串
    assert_eq!(one(serde_json::json!({})), "");
    // 服务器链接与 http 链接原样保留（不可定位判定在写入侧）
    assert_eq!(
        one(serde_json::json!({"url": "/download?appid=1406&fileid=x"})),
        "/download?appid=1406&fileid=x"
    );
    assert_eq!(
        one(serde_json::json!({"url": "https://example.com/a.jpg"})),
        "https://example.com/a.jpg"
    );
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
        group_nickname: None,
        member_role: None,
        attachments: Vec::new(),
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

// =========================================================
// 群聊成员聚合与群名片 / 角色
// =========================================================

/// 构造群聊导出 JSON：self + 2 位他人（含群名片 / 角色）+ 一条空 UID 消息。
fn group_export_json() -> String {
    r#"{
        "chatInfo": {"selfUid":"u_self","selfName":"小明","selfUin":"10001","name":"测试群","type":"group","peerUid":"u_group","peerUin":"30003"},
        "messages": [
            {"id":"m1","timestamp":1704067200000,"type":"text","recalled":false,"system":false,"content":{"text":"大家好","elements":[]},"sender":{"uid":"u_self","name":"小明","uin":"10001"}},
            {"id":"m2","timestamp":1704067260000,"type":"text","recalled":false,"system":false,"content":{"text":"你好","elements":[]},"sender":{"uid":"u_a","name":"昵称A","uin":"20001","groupNickname":"群名片A","role":"owner"}},
            {"id":"m3","timestamp":1704067320000,"type":"text","recalled":false,"system":false,"content":{"text":"早","elements":[]},"sender":{"uid":"u_b","name":"昵称B"}},
            {"id":"m4","timestamp":1704067380000,"type":"text","recalled":false,"system":false,"content":{"text":"再聊","elements":[]},"sender":{"uid":"u_a","name":"昵称A改"}},
            {"id":"m5","timestamp":1704067440000,"type":"text","recalled":false,"system":false,"content":{"text":"好","elements":[]},"sender":{"uid":"u_b","name":"昵称B"}},
            {"id":"m6","timestamp":1704067500000,"type":"text","recalled":false,"system":false,"content":{"text":"回聊","elements":[]},"sender":{"uid":"u_a","name":"昵称A改"}},
            {"id":"m7","timestamp":1704067560000,"type":"text","recalled":false,"system":false,"content":{"text":"无名发言","elements":[]},"sender":{"uid":"","name":"神秘人"}}
        ]
    }"#
    .to_string()
}

/// 写临时文件并运行给定解析闭包（自定义文件名避免并行测试互相覆盖）。
fn run_with_group_file<T>(name: &str, content: &str, f: impl FnOnce(&Path) -> T) -> T {
    let path =
        std::env::temp_dir().join(format!("ramaria_group_{name}_{}.json", std::process::id()));
    std::fs::write(&path, content).expect("写入临时文件失败");
    let result = f(&path);
    let _ = std::fs::remove_file(&path);
    result
}

/// 群聊导出：成员按消息数降序聚合；name 取最后非空；uin 取首个非空；空 UID 不参与。
#[test]
fn parse_group_export_aggregates_members() {
    let content = group_export_json();
    let (_sessions, report) = run_with_group_file("aggregate", &content, |p| {
        parse_qq_export(p, 10).expect("群聊解析失败")
    });

    assert_eq!(report.chat_type, "group");
    assert_eq!(report.members.len(), 3, "空 UID 消息不参与成员统计");
    let summary: Vec<(&str, Option<&str>, usize)> = report
        .members
        .iter()
        .map(|m| (m.name.as_str(), m.uin.as_deref(), m.message_count))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("昵称A改", Some("20001"), 3),
            ("昵称B", None, 2),
            ("小明", Some("10001"), 1),
        ],
        "成员应按消息数降序；name 取最后非空；uin 取首个非空"
    );
}

/// 直接聚合：同条数按名称升序、同名称按 UID 升序；空 UID 跳过。
#[test]
fn aggregate_members_sorts_ties_by_name_and_skips_empty_uid() {
    let make = |uid: &str, name: &str| ParsedMessage {
        role: "assistant".to_string(),
        content: "发言".to_string(),
        created_at: 1,
        fingerprint: format!("fp-{uid}-{name}"),
        sender_uid: uid.to_string(),
        sender_uin: None,
        sender_name: name.to_string(),
        group_nickname: None,
        member_role: None,
        attachments: Vec::new(),
    };
    let messages = vec![
        make("u_b", "bob"),
        make("", "ghost"),
        make("u_a", "alice"),
        make("u_c", "alice"),
    ];

    let members = aggregate_members(&messages);
    let got: Vec<(&str, &str)> = members
        .iter()
        .map(|m| (m.name.as_str(), m.uid.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![("alice", "u_a"), ("alice", "u_c"), ("bob", "u_b")],
        "同条数按名称升序，同名称按 UID 升序；空 UID 不参与"
    );
}

/// 摘要渲染成员分布；掩码版不泄露成员昵称。
#[test]
fn group_export_summary_renders_member_distribution_with_masking() {
    let content = group_export_json();
    let (_sessions, report) = run_with_group_file("summary", &content, |p| {
        parse_qq_export(p, 10).expect("群聊解析失败")
    });

    let plain = report.summary();
    assert!(plain.contains("成员分布: 3 人（消息数降序）:"), "{plain}");
    assert!(plain.contains("  - 昵称A改 3 条"), "{plain}");
    assert!(plain.contains("昵称B"), "{plain}");

    let masked = report.summary_masked();
    assert!(masked.contains("成员分布: 3 人"), "{masked}");
    assert!(
        !masked.contains("昵称A改"),
        "掩码摘要不应包含原昵称: {masked}"
    );
    assert!(!masked.contains("昵称B"), "{masked}");
}

/// 单条解析：sender 带 groupNickname / role 时正确读取。
#[test]
fn parse_json_message_reads_group_nickname_and_role() {
    let raw = serde_json::json!({
        "id": "g1",
        "timestamp": 1704067260000i64,
        "type": "text",
        "recalled": false,
        "system": false,
        "content": {"text": "你好", "elements": []},
        "sender": {"uid": "u_a", "uin": "20001", "name": "昵称A", "groupNickname": "群名片A", "role": "owner"}
    });
    let mut report = ImportReport::default();

    let parsed = parse_json_message(&raw, "u_self", "小明", &mut report).expect("应解析成功");
    assert_eq!(parsed.group_nickname.as_deref(), Some("群名片A"));
    assert_eq!(parsed.member_role, Some(MemberRole::Owner));
}

/// 单条解析：群名片缺失 / 为空或角色非法时一律 None，不阻塞解析。
#[test]
fn parse_json_message_group_fields_absent_or_invalid_are_none() {
    let mut report = ImportReport::default();
    let raw = serde_json::json!({
        "id": "g2",
        "timestamp": 1704067260000i64,
        "type": "text",
        "recalled": false,
        "system": false,
        "content": {"text": "你好", "elements": []},
        "sender": {"uid": "u_a", "name": "昵称A", "groupNickname": "", "role": "super"}
    });
    let parsed = parse_json_message(&raw, "u_self", "小明", &mut report).expect("应解析成功");
    assert!(parsed.group_nickname.is_none(), "空群名片视为缺失");
    assert!(parsed.member_role.is_none(), "非法角色视为未知");

    let raw = serde_json::json!({
        "id": "g3",
        "timestamp": 1704067260001i64,
        "type": "text",
        "recalled": false,
        "system": false,
        "content": {"text": "你好", "elements": []},
        "sender": {"uid": "u_b", "name": "昵称B"}
    });
    let parsed = parse_json_message(&raw, "u_self", "小明", &mut report).expect("应解析成功");
    assert!(parsed.group_nickname.is_none(), "缺少群名片键视为缺失");
    assert!(parsed.member_role.is_none(), "缺少角色键视为未知");
}
