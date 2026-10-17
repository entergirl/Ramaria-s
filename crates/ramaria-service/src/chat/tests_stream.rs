//! crates/ramaria-service/src/chat/tests_stream.rs - Ramaria 流式生成事件序列单元测试
//!
//! 设计特点:
//! - 覆盖流式路径的事件序列与落库纪律：成功（Delta… → Done）/ 流打不开（仅 Error）/
//!   流中错误（Delta… → Error，无 Done）/ 空回复（Error，无 Done）
//! - 与 `tests` 共用请求构造与事件收集辅助（`stream_request` / `collect_events` / `kinds`）
//! - 使用 mock LLM 与真实 SQLite 临时库，断言以事件序列与落库状态为准
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实 API key / 网络调用 / 用户数据。

use super::tests::{REPLY, collect_events, kinds, stream_request};
use crate::stream_event::StreamEvent;
use crate::test_support::{MockLlm, engine_with_failing_llm, engine_with_shared_llm, seed_persona};
use crate::types::DEFAULT_PERSONA_UID;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{AppState, MessageRole, MessageSource};
use std::sync::Arc;

/// 流式成功：事件序列 Delta… → Done；消息落库两条（user 本地 / assistant 线上，均带人格）。
#[tokio::test]
async fn stream_success_emits_deltas_done_and_persists_messages() {
    // 多片段流式（拼接后等于回复全文）
    let llm = Arc::new(MockLlm::with_stream_chunks(&["嗯，", "我在", "听。"]));
    let (engine, storage, dir) =
        engine_with_shared_llm("chat-stream-ok", llm, RamariaConfig::default(), None).await;
    let engine = Arc::new(engine);
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let handle = engine
        .chat_stream(stream_request("你好", None))
        .await
        .expect("流式生成应成功");
    let session_id = handle.session_id;
    let events = collect_events(handle.events).await;

    // 事件序列：Delta… → Done（无 Error）
    assert_eq!(
        kinds(&events),
        vec!["delta", "delta", "delta", "done"],
        "三个增量片段后跟一个完成事件"
    );
    assert_eq!(events.last().map(StreamEvent::kind), Some("done"));
    let delta_text: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Delta { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(delta_text, REPLY, "增量文本拼接应等于回复全文");

    let done = events.iter().find_map(|event| match event {
        StreamEvent::Done {
            session_id,
            backend_id,
            total_chars,
            ..
        } => Some((*session_id, backend_id.clone(), *total_chars)),
        _ => None,
    });
    let (done_session, backend_id, total_chars) = done.expect("应包含 Done 事件");
    assert_eq!(done_session, Some(session_id));
    assert_eq!(backend_id.as_deref(), Some("stop"));
    assert_eq!(total_chars, REPLY.chars().count());

    // 落库：用户消息（本地来源）+ 助手回复（线上来源），均带人格
    let messages = storage
        .list_messages(session_id)
        .await
        .expect("读取消息成功");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[0].content, "你好");
    assert_eq!(messages[0].source, MessageSource::Local);
    assert_eq!(
        messages[0].persona_uid.as_deref(),
        Some(DEFAULT_PERSONA_UID)
    );
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[1].content, REPLY);
    assert_eq!(messages[1].source, MessageSource::Online);
    assert_eq!(
        messages[1].persona_uid.as_deref(),
        Some(DEFAULT_PERSONA_UID)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 流打不开：返回 Ok，首个事件为 Error、无 Done；库内无消息。
#[tokio::test]
async fn stream_open_failure_returns_error_event_without_persistence() {
    let (engine, storage, dir) = engine_with_failing_llm("chat-stream-open-fail").await;
    let engine = Arc::new(engine);
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let handle = engine
        .chat_stream(stream_request("你好", None))
        .await
        .expect("流打不开也应返回事件流句柄");
    let session_id = handle.session_id;
    let request_id = handle.request_id;
    let events = collect_events(handle.events).await;

    assert_eq!(
        kinds(&events),
        vec!["error"],
        "流打不开时应只有一个 Error 事件"
    );
    assert_eq!(events[0].request_id(), request_id);

    let messages = storage
        .list_messages(session_id)
        .await
        .expect("读取消息成功");
    assert!(messages.is_empty(), "流打不开不应写入任何消息");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 流中错误：先收到增量、再收到 Error、无 Done；用户消息已落库、助手回复未落库。
#[tokio::test]
async fn stream_error_after_deltas_keeps_user_message_only() {
    let llm = Arc::new(MockLlm::stream_fails_after(&["好的", "，继续"]));
    let (engine, storage, dir) =
        engine_with_shared_llm("chat-stream-mid-fail", llm, RamariaConfig::default(), None).await;
    let engine = Arc::new(engine);
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let handle = engine
        .chat_stream(stream_request("你好", None))
        .await
        .expect("应返回事件流句柄");
    let session_id = handle.session_id;
    let events = collect_events(handle.events).await;

    let event_kinds = kinds(&events);
    assert!(event_kinds.len() >= 2, "应先收到增量再收到错误");
    assert_eq!(
        event_kinds[event_kinds.len() - 1],
        "error",
        "流中错误应转为 Error 事件"
    );
    assert!(!event_kinds.contains(&"done"), "出错后不应再发 Done");
    assert!(
        event_kinds[..event_kinds.len() - 1]
            .iter()
            .all(|kind| *kind == "delta"),
        "错误之前的序列应为增量事件"
    );

    let messages = storage
        .list_messages(session_id)
        .await
        .expect("读取消息成功");
    assert_eq!(messages.len(), 1, "用户消息已落库、助手回复不落库");
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[0].content, "你好");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空回复：模型 HTTP 200 但正文为空 → 转 Error（可重试）、不发 Done、助手消息不落库。
///
/// 回归背景: 思考模式耗尽输出预算时进度条会一直转到超时且库内无记录，
/// 前端还会留下一个空气泡；本用例锁定"空正文按错误处理"的口径。
#[tokio::test]
async fn stream_empty_reply_emits_error_without_persistence() {
    // 单个空片段（末片 done=true）→ 拼接后的回复为空
    let llm = Arc::new(MockLlm::with_stream_chunks(&[""]));
    let (engine, storage, dir) =
        engine_with_shared_llm("chat-stream-empty", llm, RamariaConfig::default(), None).await;
    let engine = Arc::new(engine);
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let handle = engine
        .chat_stream(stream_request("你好", None))
        .await
        .expect("应返回事件流句柄");
    let session_id = handle.session_id;
    let events = collect_events(handle.events).await;

    let event_kinds = kinds(&events);
    assert_eq!(
        event_kinds.last(),
        Some(&"error"),
        "空回复应转 Error 事件: {event_kinds:?}"
    );
    assert!(
        !event_kinds.contains(&"done"),
        "空回复不应发 Done: {event_kinds:?}"
    );

    let messages = storage
        .list_messages(session_id)
        .await
        .expect("读取消息成功");
    assert_eq!(messages.len(), 1, "用户消息已落库、助手空回复不落库");
    assert_eq!(messages[0].role, MessageRole::User);

    let _ = std::fs::remove_dir_all(&dir);
}
