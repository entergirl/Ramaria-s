//! crates/ramaria-service/src/export/tests.rs - Ramaria 会话导出模块单元测试
//!
//! 设计特点:
//! - 由 export.rs 以 `#[cfg(test)] mod tests;` 收纳：覆盖装配用例与载荷渲染两条路径
//! - 渲染断言锁定入口共用结构：信封 / 会话与消息字段 / 时间格式化 / L1 段 / 脱敏对照
//! - 装配用例使用 crate::test_support 的真实 SQLite 临时库夹具
//!
//! 安全约束:
//! - 全部数据为合成样例，不使用真实 LLM / embedding / 用户数据。

use super::*;
use crate::test_support::{engine_with_db, seed_l1, seed_persona, seed_session_with_messages};
use ramaria_core::types::MessageSource;
use uuid::Uuid;

// =========================================================
// 装配用例
// =========================================================

/// 空库：装配返回空集合（非错误），无人格过滤时不带 L1 段。
#[tokio::test]
async fn collect_empty_db_returns_empty_result() {
    let (engine, _storage, dir) = engine_with_db("export-empty").await;

    let data = engine
        .export_sessions(ExportDataRequest::default())
        .await
        .expect("空库装配应成功");
    assert_eq!(data.total_sessions, 0);
    assert!(data.sessions.is_empty());
    assert!(data.l1_persona.is_none());
    assert!(data.l1_memories.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 无过滤：全部会话与全量消息逐会话装配（总数与消息数口径）。
#[tokio::test]
async fn collect_without_filter_exports_all_sessions() {
    let (engine, storage, dir) = engine_with_db("export-all").await;
    seed_persona(&storage, "char-0001").await;
    seed_persona(&storage, "char-0002").await;
    seed_session_with_messages(&storage, "char-0001", 3, 1_000).await;
    seed_session_with_messages(&storage, "char-0002", 2, 2_000).await;

    let data = engine
        .export_sessions(ExportDataRequest::default())
        .await
        .expect("装配应成功");
    assert_eq!(data.total_sessions, 2);
    assert_eq!(data.sessions.len(), 2);
    assert!(
        data.sessions.iter().all(|s| !s.messages.is_empty()),
        "每个会话应装配全量消息"
    );
    let message_total: usize = data.sessions.iter().map(|s| s.messages.len()).sum();
    assert_eq!(message_total, 5, "消息数应为两会话之和");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 人格过滤：仅保留含目标 persona_uid 消息的会话；指定人格时装配 L1 段。
#[tokio::test]
async fn collect_filters_by_persona_and_includes_l1() {
    let (engine, storage, dir) = engine_with_db("export-filter").await;
    seed_persona(&storage, "char-0001").await;
    seed_persona(&storage, "char-0002").await;
    seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;
    seed_session_with_messages(&storage, "char-0002", 2, 2_000).await;
    // seed_l1 会为承载摘要自动建一个无消息会话（外键依赖）：总数计 3 个会话
    seed_l1(
        &storage,
        "char-0001",
        "工作压力摘要",
        Some("工作压力"),
        3_000,
    )
    .await;

    let data = engine
        .export_sessions(ExportDataRequest {
            persona: Some("char-0001".to_string()),
            limit: None,
            offset: None,
        })
        .await
        .expect("装配应成功");
    assert_eq!(data.total_sessions, 3, "总数口径为过滤前全部会话");
    assert_eq!(
        data.sessions.len(),
        1,
        "仅保留含匹配人格消息的会话（无消息会话不计入）"
    );
    assert!(
        data.sessions[0]
            .messages
            .iter()
            .all(|m| m.persona_uid.as_deref() == Some("char-0001")),
        "过滤后的会话应只含目标人格相关数据"
    );
    let l1 = data.l1_memories.expect("指定人格时应装配 L1 段");
    assert_eq!(l1.len(), 1);
    assert_eq!(l1[0].summary, "工作压力摘要");
    assert_eq!(data.l1_persona.as_deref(), Some("char-0001"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 分页：offset / limit 作用于过滤后的会话集合；总数仍为过滤前口径。
#[tokio::test]
async fn collect_pages_filtered_sessions() {
    let (engine, storage, dir) = engine_with_db("export-page").await;
    seed_persona(&storage, "char-0001").await;
    seed_session_with_messages(&storage, "char-0001", 1, 1_000).await;
    seed_session_with_messages(&storage, "char-0001", 1, 2_000).await;
    seed_session_with_messages(&storage, "char-0001", 1, 3_000).await;

    let data = engine
        .export_sessions(ExportDataRequest {
            persona: None,
            limit: Some(1),
            offset: Some(1),
        })
        .await
        .expect("装配应成功");
    assert_eq!(data.total_sessions, 3, "总数不随分页变化");
    assert_eq!(data.sessions.len(), 1, "offset 1 + limit 1 应只余一条");

    // 无人格指定：不带 L1 段，也无请求 persona
    assert!(data.l1_persona.is_none());
    assert!(data.l1_memories.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 载荷渲染
// =========================================================

/// 固定时间样例：2024-06-10 08:00 UTC。
const T0: i64 = 1_718_006_400_000;
/// 固定时间样例：2024-06-10 09:00 UTC。
const T1: i64 = 1_718_010_000_000;

/// 构造渲染样例：一个含 4 条消息（覆盖全部角色）的会话 + 一个无消息会话。
fn render_sample_data() -> ExportData {
    let session_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("固定 UUID");
    let mut messages = vec![
        Message::new(
            session_id,
            MessageRole::User,
            "你好".to_string(),
            MessageSource::Local,
        ),
        Message::new(
            session_id,
            MessageRole::Assistant,
            "你好呀".to_string(),
            MessageSource::Online,
        ),
        Message::new(
            session_id,
            MessageRole::System,
            "系统提示".to_string(),
            MessageSource::Local,
        ),
        Message::new(
            session_id,
            MessageRole::Tool,
            "工具输出".to_string(),
            MessageSource::Local,
        ),
    ];
    for (index, message) in messages.iter_mut().enumerate() {
        message.created_at = if index % 2 == 0 { T0 } else { T1 };
    }

    let empty_session_id =
        Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("固定 UUID");
    ExportData {
        total_sessions: 2,
        sessions: vec![
            ExportSessionData {
                session: Session {
                    id: session_id,
                    started_at: T0,
                    ended_at: Some(T1),
                    persona_uid: None,
                    channel: "local".to_string(),
                    external_ref: None,
                },
                messages,
            },
            ExportSessionData {
                session: Session {
                    id: empty_session_id,
                    started_at: 0,
                    ended_at: None,
                    persona_uid: None,
                    channel: "local".to_string(),
                    external_ref: None,
                },
                messages: Vec::new(),
            },
        ],
        l1_persona: None,
        l1_memories: None,
    }
}

/// 构造带人格归属的 L1 样例（摘要 6 字符，供脱敏对照）。
fn render_sample_l1(session_id: Uuid) -> MemoryL1 {
    let mut l1 = MemoryL1::new(session_id, "工作压力摘要".to_string(), None);
    l1.persona_uid = Some("char-0001".to_string());
    l1.created_at = T0;
    l1.valence = 0.25;
    l1.salience = 0.75;
    l1
}

/// 时间格式化：有效值 → `%Y-%m-%d %H:%M`（UTC）；≤0 → None。
#[test]
fn format_timestamp_cases() {
    assert_eq!(format_timestamp(T0).as_deref(), Some("2024-06-10 08:00"));
    assert_eq!(
        format_timestamp(T0 + 60_000).as_deref(),
        Some("2024-06-10 08:01")
    );
    assert_eq!(format_timestamp(0), None);
    assert_eq!(format_timestamp(-1), None);
}

/// JSON 结构逐字段：信封 / 会话 / 消息 / 时间格式化（含空消息会话与缺省 ended_at）。
#[test]
fn render_json_matches_contract_structure() {
    let text = render_sessions_json(&render_sample_data(), false);
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("渲染结果应为合法 JSON");

    let exported_at = value["ramaria_export"]["exported_at"]
        .as_str()
        .expect("exported_at 应为字符串")
        .to_string();
    assert_eq!(exported_at.len(), 20, "ISO-8601 秒级: {exported_at}");
    assert!(exported_at.ends_with('Z'), "UTC 后缀: {exported_at}");
    value["ramaria_export"]["exported_at"] = serde_json::json!("<normalized>");

    let expected = serde_json::json!({
        "ramaria_export": {
            "version": EXPORT_FORMAT_VERSION,
            "exported_at": "<normalized>",
            "sessions": [
                {
                    "session_id": "11111111-1111-1111-1111-111111111111",
                    "started_at": "2024-06-10 08:00",
                    "ended_at": "2024-06-10 09:00",
                    "messages": [
                        {"role": "user", "content": "你好", "source": "local", "created_at": "2024-06-10 08:00"},
                        {"role": "assistant", "content": "你好呀", "source": "online", "created_at": "2024-06-10 09:00"},
                        {"role": "system", "content": "系统提示", "source": "local", "created_at": "2024-06-10 08:00"},
                        {"role": "tool", "content": "工具输出", "source": "local", "created_at": "2024-06-10 09:00"},
                    ],
                },
                {
                    "session_id": "22222222-2222-2222-2222-222222222222",
                    "started_at": null,
                    "ended_at": null,
                    "messages": [],
                },
            ],
        }
    });
    assert_eq!(value, expected, "JSON 载荷应逐字段一致");
}

/// JSON 脱敏对照：正文与 L1 摘要替换为 `<N chars>`，其余字段不变。
#[test]
fn render_json_redact_replaces_bodies_only() {
    let session_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("固定 UUID");
    let mut data = render_sample_data();
    data.l1_persona = Some("char-0001".to_string());
    data.l1_memories = Some(vec![render_sample_l1(session_id)]);

    let plain = render_sessions_json(&data, false);
    let redacted = render_sessions_json(&data, true);
    assert!(plain.contains("工具输出") && plain.contains("工作压力摘要"));
    assert!(
        redacted.contains("<2 chars>") && redacted.contains("<6 chars>"),
        "正文与摘要应按字符数占位: {redacted}"
    );
    assert!(
        !redacted.contains("你好")
            && !redacted.contains("系统提示")
            && !redacted.contains("工作压力"),
        "脱敏后不得含原文: {redacted}"
    );

    // 除被脱敏字段（消息 content / L1 summary）外，两侧逐字段一致
    let mut plain_value: serde_json::Value = serde_json::from_str(&plain).expect("合法 JSON");
    let mut redacted_value: serde_json::Value = serde_json::from_str(&redacted).expect("合法 JSON");
    for value in [&mut plain_value, &mut redacted_value] {
        value["ramaria_export"]["exported_at"] = serde_json::json!("<normalized>");
        for message in value["ramaria_export"]["sessions"][0]["messages"]
            .as_array_mut()
            .expect("消息数组")
        {
            message.as_object_mut().expect("消息对象").remove("content");
        }
        for item in value["ramaria_export"]["sessions"][2]["items"]
            .as_array_mut()
            .expect("L1 条目数组")
        {
            item.as_object_mut().expect("L1 条目对象").remove("summary");
        }
    }
    assert_eq!(plain_value, redacted_value, "除正文 / 摘要外其余字段应一致");
}

/// L1 段：仅 `Some` 时附加；条目逐字段一致；空列表保持段结构（count=0）。
#[test]
fn render_json_l1_segment_follows_assembly() {
    let session_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("固定 UUID");
    let mut data = render_sample_data();
    assert!(
        !render_sessions_json(&data, false).contains("l1_memories"),
        "未指定人格（None）时不应出现 L1 段"
    );

    let l1 = render_sample_l1(session_id);
    let l1_id = l1.id.to_string();
    data.l1_persona = Some("char-0001".to_string());
    data.l1_memories = Some(vec![l1]);
    let value: serde_json::Value =
        serde_json::from_str(&render_sessions_json(&data, false)).expect("合法 JSON");
    let segment = &value["ramaria_export"]["sessions"][2];
    assert_eq!(segment["type"], "l1_memories");
    assert_eq!(segment["persona_uid"], "char-0001");
    assert_eq!(segment["count"], 1);
    assert_eq!(segment["items"][0]["id"], l1_id);
    assert_eq!(
        segment["items"][0]["session_id"],
        "11111111-1111-1111-1111-111111111111"
    );
    assert_eq!(segment["items"][0]["summary"], "工作压力摘要");
    assert_eq!(segment["items"][0]["valence"], 0.25);
    assert_eq!(segment["items"][0]["salience"], 0.75);
    assert_eq!(segment["items"][0]["created_at"], "2024-06-10 08:00");

    // 空列表：段结构保持；persona_uid 保留请求人格、count=0、items 空
    data.l1_memories = Some(Vec::new());
    let value: serde_json::Value =
        serde_json::from_str(&render_sessions_json(&data, false)).expect("合法 JSON");
    let segment = &value["ramaria_export"]["sessions"][2];
    assert_eq!(segment["count"], 0);
    assert_eq!(segment["items"], serde_json::json!([]));
    assert_eq!(
        segment["persona_uid"], "char-0001",
        "空列表仍保留请求 persona: {segment}"
    );
}

/// 指定人格无未吸收摘要：装配空列表 + 渲染 → persona_uid 为请求值。
#[tokio::test]
async fn collect_l1_empty_keeps_request_persona() {
    let (engine, storage, dir) = engine_with_db("export-l1-empty").await;
    seed_persona(&storage, "char-0007").await;

    let data = engine
        .export_sessions(ExportDataRequest {
            persona: Some("char-0007".to_string()),
            limit: None,
            offset: None,
        })
        .await
        .expect("装配应成功");
    assert_eq!(data.l1_persona.as_deref(), Some("char-0007"));
    assert!(
        data.l1_memories.as_deref().is_some_and(|l1| l1.is_empty()),
        "该人格无未吸收摘要应为空列表"
    );

    let value: serde_json::Value =
        serde_json::from_str(&render_sessions_json(&data, false)).expect("合法 JSON");
    let segment = &value["ramaria_export"]["sessions"][0];
    assert_eq!(segment["type"], "l1_memories");
    assert_eq!(segment["persona_uid"], "char-0007", "保留请求 persona");
    assert_eq!(segment["count"], 0);
    assert_eq!(segment["items"], serde_json::json!([]));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Markdown：头部 / 会话标题 / 角色标签 / 创建时间逐字一致；无消息会话跳过。
#[test]
fn render_markdown_follows_contract_rules() {
    let markdown =
        render_sessions_markdown(&render_sample_data(), false).expect("有消息会话时应返回文本");

    let mut lines = markdown.lines();
    assert_eq!(lines.next(), Some("# Ramaria 对话导出"));
    assert_eq!(lines.next(), Some(""));
    let export_time = lines.next().unwrap_or_default().to_string();
    assert!(
        export_time.starts_with("导出时间: ") && export_time.ends_with(" UTC"),
        "导出时间应标注 UTC: {export_time}"
    );

    assert!(markdown.contains("## 会话 11111111-1111-1111-1111-111111111111"));
    assert!(!markdown.contains("22222222"), "无消息会话应跳过");
    assert!(markdown.contains("*创建时间: 2024-06-10 08:00*"));
    assert!(markdown.contains("**👤 用户**\n\n你好\n\n---\n\n"));
    assert!(markdown.contains("**🤖 AI**\n\n你好呀\n\n---\n\n"));
    assert!(markdown.contains("*⚙ 系统*\n\n系统提示\n\n---\n\n"));
    assert!(markdown.contains("*❓ 未知*\n\n工具输出\n\n---\n\n"));
}

/// Markdown 脱敏：正文替换为 `<N chars>` 且不含原文。
#[test]
fn render_markdown_redact_replaces_bodies() {
    let markdown =
        render_sessions_markdown(&render_sample_data(), true).expect("有消息会话时应返回文本");
    assert!(
        markdown.contains("<2 chars>") && markdown.contains("<4 chars>"),
        "正文应按字符数占位: {markdown}"
    );
    assert!(
        !markdown.contains("你好")
            && !markdown.contains("系统提示")
            && !markdown.contains("工具输出"),
        "脱敏后不得含原文: {markdown}"
    );
}

/// Markdown 空数据：空集合或全部会话无消息时返回 None（调用方不写文件）。
#[test]
fn render_markdown_returns_none_without_exportable_sessions() {
    let mut data = render_sample_data();
    data.sessions.clear();
    assert!(
        render_sessions_markdown(&data, false).is_none(),
        "空集合应为 None"
    );

    data.sessions = render_sample_data().sessions;
    for entry in &mut data.sessions {
        entry.messages.clear();
    }
    assert!(
        render_sessions_markdown(&data, false).is_none(),
        "全部会话无消息应为 None"
    );
}
