//! crates/ramaria-service/src/chat/tests.rs - Ramaria 生成用例单元测试
//!
//! 设计特点:
//! - 由 chat 模块以 `#[cfg(test)] mod tests;` 收纳：覆盖非流式 / 流式 / 历史窗口 / 交互入口语义
//! - 使用 mock LLM 与真实 SQLite 临时库，断言以落库状态与记录到的 LLM 请求为准
//! - 流式路径断言事件序列（Delta… → Done / Error）与消息落库纪律
//! - 历史窗口用例经 `step_load_history` 直接验证配置驱动的加载条数与字符预算
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实 API key / 网络调用 / 用户数据。

use super::steps::step_load_history;
use crate::recall::RecallPolicy;
use crate::stream_event::{ChatEventStream, StreamEvent};
use crate::test_support::{
    MockLlm, engine_with_db, engine_with_failing_llm, engine_with_l1_reply, engine_with_shared_llm,
    seed_closed_session_with_messages, seed_persona, seed_session_with_messages,
};
use crate::types::{CHANNEL_MCP, ChatSendRequest, ChatStreamRequest, DEFAULT_PERSONA_UID};
use futures::StreamExt;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{ChatMessage, StoreCrud, StoreInfrastructure};
use ramaria_core::types::{
    AppState, BackendConfig, Message, MessageRole, MessageSource, PrivacyConsent, now_ms,
};
use std::sync::Arc;
use uuid::Uuid;

/// 固定回复（供"生成成功"路径断言）。
const REPLY: &str = "嗯，我在听。";

/// 构造生成请求（默认人格 rama-0001、通道 mcp）。
fn request(message: &str, conversation_id: Option<&str>) -> ChatSendRequest {
    ChatSendRequest {
        message: message.to_string(),
        persona: Some(DEFAULT_PERSONA_UID.to_string()),
        session_id: None,
        conversation_id: conversation_id.map(str::to_string),
        channel: CHANNEL_MCP.to_string(),
    }
}

/// 构造流式生成请求（默认人格、无预置上文）。
fn stream_request(message: &str, session_id: Option<Uuid>) -> ChatStreamRequest {
    ChatStreamRequest {
        message: message.to_string(),
        persona: Some(DEFAULT_PERSONA_UID.to_string()),
        session_id,
        seed_history: Vec::new(),
        config_override: None,
    }
}

/// 收集事件流全部事件（出现流内错误项即失败，用于"成功路径"断言）。
async fn collect_events(events: ChatEventStream) -> Vec<StreamEvent> {
    let mut stream = events;
    let mut collected = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => collected.push(event),
            Err(e) => panic!("事件流不应返回错误项: {e}"),
        }
    }
    collected
}

/// 提取事件类型序列。
fn kinds(events: &[StreamEvent]) -> Vec<&'static str> {
    events.iter().map(StreamEvent::kind).collect()
}

// =========================================================
// 非流式（通道入口）——既有断言保持
// =========================================================

/// 生成成功：返回回复、落库两条消息、会话带通道标识。
#[tokio::test]
async fn generates_reply_and_persists_both_messages() {
    let (engine, storage, dir) = engine_with_l1_reply("chat-ok", REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let outcome = engine
        .chat_send(request("你好", Some("client-A")))
        .await
        .expect("生成应成功");
    assert_eq!(outcome.reply, REPLY);
    assert_eq!(outcome.chars, REPLY.chars().count());

    let session = storage
        .get_session(outcome.session_id)
        .await
        .expect("读取会话成功")
        .expect("会话应存在");
    assert_eq!(session.channel, CHANNEL_MCP, "外部生成会话应带通道标识");
    assert_eq!(session.external_ref.as_deref(), Some("client-A"));

    let messages = storage
        .list_messages(outcome.session_id)
        .await
        .expect("读取消息成功");
    assert_eq!(messages.len(), 2, "应写入用户消息与助手回复");
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[0].content, "你好");
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[1].content, REPLY);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 同一外部对话标识：续写同一会话（多轮累计消息）。
#[tokio::test]
async fn reuses_same_session_for_same_conversation_id() {
    let (engine, storage, dir) = engine_with_l1_reply("chat-reuse", REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let first = engine
        .chat_send(request("第一轮", Some("client-A")))
        .await
        .expect("首轮应成功");
    let second = engine
        .chat_send(request("第二轮", Some("client-A")))
        .await
        .expect("二轮应成功");
    assert_eq!(
        first.session_id, second.session_id,
        "同一标识应续写同一会话"
    );

    let messages = storage
        .list_messages(first.session_id)
        .await
        .expect("读取消息成功");
    assert_eq!(messages.len(), 4, "两轮对话共 4 条消息");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空消息：显式校验错误（不产生任何写入）。
#[tokio::test]
async fn rejects_empty_message() {
    let (engine, storage, dir) = engine_with_db("chat-empty").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let err = engine
        .chat_send(request("   ", None))
        .await
        .expect_err("空消息应报错");
    assert_eq!(err.category(), "validation");

    let _ = std::fs::remove_dir_all(&dir);
}

/// LLM 失败：错误上抛且不写半条（库内无孤立用户消息）。
#[tokio::test]
async fn llm_failure_leaves_no_partial_write() {
    let (engine, storage, dir) = engine_with_failing_llm("chat-fail").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let err = engine
        .chat_send(request("你好", Some("client-A")))
        .await
        .expect_err("LLM 失败应上抛");
    assert_eq!(err.category(), "llm");

    let session = storage
        .find_active_session_by_channel(CHANNEL_MCP, Some("client-A"))
        .await
        .expect("按通道查询成功")
        .expect("会话应已创建（便于重试）");
    let messages = storage
        .list_messages(session.id)
        .await
        .expect("读取消息成功");
    assert!(messages.is_empty(), "LLM 失败不应写入孤立用户消息");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 人格白名单：越权人格直接拒绝（Privacy）。
#[tokio::test]
async fn persona_whitelist_rejects_generation() {
    let (engine, storage, dir) = engine_with_l1_reply("chat-whitelist", REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_recall_policy(
        RecallPolicy::default().with_allowed_personas(vec!["char-0001".to_string()]),
    );

    let err = engine
        .chat_send(request("你好", None))
        .await
        .expect_err("越权人格应被拒绝");
    assert_eq!(err.category(), "privacy");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 显式 session_id：串人格拒绝，归属一致则复用。
#[tokio::test]
async fn explicit_session_id_checks_persona_binding() {
    let (engine, storage, dir) = engine_with_l1_reply("chat-session", REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_persona(&storage, "char-0001").await;

    let session = storage
        .create_session_in_channel(Some("char-0001"), CHANNEL_MCP, Some("client-B"))
        .await
        .expect("建会话成功");

    let mut mismatch = request("你好", None);
    mismatch.session_id = Some(session.id);
    let err = engine
        .chat_send(mismatch)
        .await
        .expect_err("会话归属人格不一致应报错");
    assert_eq!(err.category(), "validation");

    let mut same_persona = request("你好", None);
    same_persona.persona = Some("char-0001".to_string());
    same_persona.session_id = Some(session.id);
    let outcome = engine
        .chat_send(same_persona)
        .await
        .expect("归属一致应可复用会话");
    assert_eq!(outcome.session_id, session.id);

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 流式（交互入口）
// =========================================================

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

/// 状态门禁：交互入口在非就绪状态被拒、就绪后放行；通道入口不受门禁（初值 NeedsSetup）。
#[tokio::test]
async fn interactive_state_gate_blocks_until_ready_but_channel_passes() {
    let (engine, storage, dir) = engine_with_l1_reply("chat-state-gate", REPLY).await;
    let engine = Arc::new(engine);
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    // 通道入口（MCP）：状态机初值 NeedsSetup 下仍可生成
    assert_eq!(engine.current_state(), AppState::NeedsSetup);
    let outcome = engine
        .chat_send(request("通道你好", Some("client-state")))
        .await
        .expect("通道入口不应受状态门禁约束");
    assert_eq!(outcome.reply, REPLY);

    // 交互入口：非就绪状态拒绝
    engine.set_state(AppState::Indexing);
    let err = engine
        .chat_stream(stream_request("你好", None))
        .await
        .expect_err("未就绪状态应拒绝流式生成");
    assert_eq!(err.category(), "validation");
    assert!(
        err.context().contains("尚未就绪"),
        "错误应含未就绪提示: {err}"
    );

    // 交互入口：就绪状态放行
    engine.set_state(AppState::Ready);
    let handle = engine
        .chat_stream(stream_request("你好", None))
        .await
        .expect("就绪状态应放行");
    let events = collect_events(handle.events).await;
    assert_eq!(events.last().map(StreamEvent::kind), Some("done"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 隐私门禁：线上 provider 未确认时拒绝（Privacy）；写入确认后放行。
#[tokio::test]
async fn interactive_privacy_gate_requires_consent_for_online_provider() {
    let (engine, storage, dir) = engine_with_shared_llm(
        "chat-privacy",
        Arc::new(MockLlm::online()),
        RamariaConfig::default(),
        None,
    )
    .await;
    let engine = Arc::new(engine);
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let err = engine
        .chat_stream(stream_request("你好", None))
        .await
        .expect_err("线上 provider 未确认隐私应被拒绝");
    assert_eq!(err.category(), "privacy");

    // 写入确认后放行
    let backend = BackendConfig::deepseek_default();
    storage
        .save_privacy_consent(&PrivacyConsent::new(
            backend.provider,
            backend.base_url.clone(),
            true,
        ))
        .await
        .expect("写入隐私确认应成功");

    let handle = engine
        .chat_stream(stream_request("你好", None))
        .await
        .expect("确认后应放行");
    let events = collect_events(handle.events).await;
    assert_eq!(events.last().map(StreamEvent::kind), Some("done"));

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 历史窗口（配置驱动）
// =========================================================

/// 历史窗口：默认条数上限 200——250 条消息只加载最近 200 条进入 Prompt。
#[tokio::test]
async fn history_window_caps_at_default_messages_limit() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "chat-history-cap",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let session_id =
        seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 250, now_ms() - 250_000).await;

    let mut req = request("你好", None);
    req.session_id = Some(session_id);
    let outcome = engine.chat_send(req).await.expect("生成应成功");
    assert_eq!(outcome.reply, REPLY);

    let recorded = llm.requests();
    let last_request = recorded.last().expect("应记录一次 LLM 请求");
    assert_eq!(
        last_request.history.len(),
        200,
        "进入 Prompt 的历史应为默认上限（200）"
    );
    assert_eq!(
        last_request.history[0].content, "消息内容 50",
        "窗口应从第 50 条开始（最近 200 条）"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 历史窗口：字符预算越界即停（40 条 × 200 字符在第 2 页后越界 → 加载 40 条）。
#[tokio::test]
async fn history_window_stops_at_char_budget() {
    let (engine, storage, dir) = engine_with_db("chat-history-budget").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let session = storage
        .create_session(Some(DEFAULT_PERSONA_UID))
        .await
        .expect("创建会话应成功");

    // 40 条 × 200 字符：每条约 216 字符（含 role 标记开销），第 2 页后累计 8640 > 预算 6000
    let base_ts = now_ms() - 100_000;
    for i in 0..40 {
        let mut message = Message::new(
            session.id,
            MessageRole::User,
            "字".repeat(200),
            MessageSource::Local,
        )
        .with_persona_uid(Some(DEFAULT_PERSONA_UID.to_string()));
        message.created_at = base_ts + i as i64;
        storage
            .save_message(&message)
            .await
            .expect("写入消息应成功");
    }

    let config = engine.config();
    let history = step_load_history(storage.as_ref(), config.as_ref(), session.id, &[]).await;
    assert_eq!(history.len(), 40, "字符预算在第 2 页后越界即停");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 历史窗口：条数上限按配置覆盖（设 40 时仅加载 40 条）。
#[tokio::test]
async fn history_window_follows_config_override() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let mut config = RamariaConfig::default();
    config.session.max_history_messages = 40;
    let (engine, storage, dir) =
        engine_with_shared_llm("chat-history-config", Arc::clone(&llm), config, None).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let session_id =
        seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 120, now_ms() - 120_000).await;

    let mut req = request("你好", None);
    req.session_id = Some(session_id);
    engine.chat_send(req).await.expect("生成应成功");

    let recorded = llm.requests();
    let last_request = recorded.last().expect("应记录一次 LLM 请求");
    assert_eq!(last_request.history.len(), 40, "条数上限应按配置截断");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 交互入口语义（桥接 / 弱反馈 / 预置上文 / 人格覆盖）
// =========================================================

/// 新会话桥接：system_prompt 含上一已关闭会话的尾部文本；注入闸门关闭时不加载。
#[tokio::test]
async fn bridge_context_injected_into_new_interactive_session() {
    // 正例：开桥接
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "chat-bridge-on",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    let engine = Arc::new(engine);
    seed_persona(&storage, "char-0001").await;
    seed_closed_session_with_messages(&storage, "char-0001", 10, now_ms() - 60_000).await;
    engine.set_state(AppState::Ready);

    let mut req = stream_request("你好", None);
    req.persona = Some("char-0001".to_string());
    let handle = engine.chat_stream(req).await.expect("流式生成应成功");
    let events = collect_events(handle.events).await;
    assert_eq!(events.last().map(StreamEvent::kind), Some("done"));

    let recorded = llm.requests();
    let last_request = recorded.last().expect("应记录一次 LLM 请求");
    assert!(
        last_request.system_prompt.contains("消息内容 9"),
        "新会话应注入上一会话尾部文本"
    );

    let _ = std::fs::remove_dir_all(&dir);

    // 反例：关桥接
    let llm_off = Arc::new(MockLlm::with_reply(REPLY));
    let mut config = RamariaConfig::default();
    config.injection.bridge = false;
    let (engine_off, storage_off, dir_off) =
        engine_with_shared_llm("chat-bridge-off", Arc::clone(&llm_off), config, None).await;
    let engine_off = Arc::new(engine_off);
    seed_persona(&storage_off, "char-0001").await;
    seed_closed_session_with_messages(&storage_off, "char-0001", 10, now_ms() - 60_000).await;
    engine_off.set_state(AppState::Ready);

    let mut req_off = stream_request("你好", None);
    req_off.persona = Some("char-0001".to_string());
    let handle_off = engine_off
        .chat_stream(req_off)
        .await
        .expect("流式生成应成功");
    let _ = collect_events(handle_off.events).await;

    let recorded_off = llm_off.requests();
    let last_request_off = recorded_off.last().expect("应记录一次 LLM 请求");
    assert!(
        !last_request_off.system_prompt.contains("消息内容 9"),
        "桥接闸门关闭不应注入上一会话文本"
    );

    let _ = std::fs::remove_dir_all(&dir_off);
}

/// 弱反馈：窗口内的纠正前缀写入 feedback_log；`[feedback].enabled=false` 不产生记录。
#[tokio::test]
async fn weak_feedback_detected_unless_disabled() {
    let (engine, storage, dir) = engine_with_l1_reply("chat-feedback", REPLY).await;
    let engine = Arc::new(engine);
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let session = storage
        .create_session(Some(DEFAULT_PERSONA_UID))
        .await
        .expect("创建会话应成功");
    // 会话已有上一条助手回复（窗口内），当前消息为纠正前缀
    let mut assistant = Message::new(
        session.id,
        MessageRole::Assistant,
        "我认为应该是甲方案".to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(DEFAULT_PERSONA_UID.to_string()));
    assistant.created_at = now_ms() - 10_000;
    storage
        .save_message(&assistant)
        .await
        .expect("写入助手消息应成功");
    engine.set_state(AppState::Ready);

    let handle = engine
        .chat_stream(stream_request("不对，你说错了", Some(session.id)))
        .await
        .expect("流式生成应成功");
    let _ = collect_events(handle.events).await;

    let logs = storage
        .list_feedback_logs_by_persona(DEFAULT_PERSONA_UID)
        .await
        .expect("读取反馈日志应成功");
    assert_eq!(logs.len(), 1, "应写入一条纠正反馈");
    assert_eq!(
        logs[0].signal_type,
        ramaria_core::behavior::SignalType::Correction
    );

    // 关闭开关后不再产生记录（配置覆盖路径）
    let mut config = RamariaConfig::default();
    config.feedback.enabled = false;
    let mut req = stream_request("不对，你又错了", None);
    req.config_override = Some(Arc::new(config));
    let handle_off = engine.chat_stream(req).await.expect("流式生成应成功");
    let _ = collect_events(handle_off.events).await;

    let logs_after = storage
        .list_feedback_logs_by_persona(DEFAULT_PERSONA_UID)
        .await
        .expect("读取反馈日志应成功");
    assert_eq!(logs_after.len(), 1, "关闭弱反馈后不应新增记录");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 预置上文：非空 seed 前置进入本轮 Prompt 历史段（顺序保持），且不落库。
#[tokio::test]
async fn seed_history_prepended_into_prompt() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "chat-seed",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    let engine = Arc::new(engine);
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let mut req = stream_request("继续聊", None);
    req.seed_history = vec![
        ChatMessage {
            role: MessageRole::User,
            content: "昨晚在写 Rust".to_string(),
        },
        ChatMessage {
            role: MessageRole::Assistant,
            content: "写到哪一步了".to_string(),
        },
    ];
    let handle = engine.chat_stream(req).await.expect("流式生成应成功");
    let session_id = handle.session_id;
    let _ = collect_events(handle.events).await;

    let recorded = llm.requests();
    let last_request = recorded.last().expect("应记录一次 LLM 请求");
    assert_eq!(last_request.history.len(), 2, "新会话无库内历史");
    assert_eq!(last_request.history[0].content, "昨晚在写 Rust");
    assert_eq!(last_request.history[1].content, "写到哪一步了");

    // 预置上文不落库：会话内仅有本轮两条消息
    let messages = storage
        .list_messages(session_id)
        .await
        .expect("读取消息成功");
    assert_eq!(messages.len(), 2, "预置上文不应落库");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 交互式显式会话：会话已绑定人格覆盖请求人格（不报错，按会话归属生成与落库）。
#[tokio::test]
async fn interactive_session_persona_overrides_request_persona() {
    let (engine, storage, dir) = engine_with_l1_reply("chat-persona-override", REPLY).await;
    let engine = Arc::new(engine);
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_persona(&storage, "char-0001").await;
    engine.set_state(AppState::Ready);
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("建会话成功");

    let handle = engine
        .chat_stream(stream_request("你好", Some(session.id)))
        .await
        .expect("会话归属覆盖应放行");
    assert_eq!(handle.session_id, session.id);
    let _ = collect_events(handle.events).await;

    let messages = storage
        .list_messages(session.id)
        .await
        .expect("读取消息成功");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].persona_uid.as_deref(), Some("char-0001"));
    assert_eq!(messages[1].persona_uid.as_deref(), Some("char-0001"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 交互式显式会话：存量 NULL 归属会话按调用方显式人格回写绑定。
#[tokio::test]
async fn interactive_null_persona_session_is_bound() {
    let (engine, storage, dir) = engine_with_l1_reply("chat-null-bind", REPLY).await;
    let engine = Arc::new(engine);
    seed_persona(&storage, "char-0001").await;
    engine.set_state(AppState::Ready);
    let session = storage.create_session(None).await.expect("建会话成功");

    let mut req = stream_request("你好", Some(session.id));
    req.persona = Some("char-0001".to_string());
    let handle = engine.chat_stream(req).await.expect("回写绑定应放行");
    let _ = collect_events(handle.events).await;

    let stored = storage
        .get_session(session.id)
        .await
        .expect("读取会话成功")
        .expect("会话应存在");
    assert_eq!(stored.persona_uid.as_deref(), Some("char-0001"));

    let _ = std::fs::remove_dir_all(&dir);
}
