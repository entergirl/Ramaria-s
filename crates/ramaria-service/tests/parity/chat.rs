//! crates/ramaria-service/tests/parity/chat.rs - 对照路径：生成（chat_send）
//!
//! 设计特点:
//! - fixture 固定：persona + 空会话空间 + 固定脚本回复；外部对话标识只作"是否提供"布尔落盘，
//!   便于同一实现重复执行时的快照可比
//! - 快照只含稳定字段：回复与字符数 / 会话通道与消息数 / 消息序列（角色 + 内容）/
//!   Prompt 结构指标（各段长度、是否含记忆上下文、模板版本）；**不落 Prompt 全文**（隐私红线）
//! - 覆盖链路：基线一致（golden 冻结）、跨隔离环境等价（`assert_parity` / `assert_stable`）、
//!   会话续写（同一外部标识落在同一会话）、LLM 不可用时"不落半条"（库内不产生孤立用户消息）、
//!   流式事件序列（delta → done）与长会话历史窗口上限
//! - 输出入口：`snapshot_of` 是"某一实现在该 fixture 上的规范化输出"的唯一入口，
//!   同形状快照可直接送入 `assert_parity` 比对

use std::sync::Arc;

use futures::StreamExt;
use ramaria_core::traits::{ChatRequest, LlmProvider, StoreCrud};
use ramaria_core::types::{AppState, CHANNEL_LOCAL};
use ramaria_service::types::{ChatSendRequest, ChatStreamRequest};
use ramaria_service::{CHANNEL_MCP, DEFAULT_PERSONA_UID, StreamEvent};
use serde_json::json;

use crate::support::{
    AppEnv, GoldenStore, ParityEnv, ParityError, ParityResult, ScriptedLlm, Snapshot,
    assert_parity, assert_stable, fixtures,
};

/// 场景名（同时作为 golden 基线文件名）。
const SCENARIO: &str = "chat_send_reply_and_persist";

/// 流式场景名（同时作为 golden 基线文件名）。
const STREAM_SCENARIO: &str = "chat_stream_delta_done";

/// 历史窗口场景名（同时作为 golden 基线文件名）。
const HISTORY_SCENARIO: &str = "chat_history_window_limit";

/// 首轮用户消息（固定文本，便于历史与落库断言）。
const USER_MESSAGE: &str = "今天有点累，随便聊聊吧";

/// 脚本回复（短句，字符数断言简洁）。
const REPLY: &str = "嗯，我在听。";

/// 历史窗口场景的消息条数（超过默认加载上限 200，验证窗口截断）。
const HISTORY_SEED_MESSAGES: usize = 250;

/// 逐字对照场景名（快照标签，不写基线）。
const CROSS_SCENARIO: &str = "chat/app-vs-service";

/// 逐字对照的既有会话消息条数（落在默认加载窗口内，避开窗口截断口径）。
const CROSS_HISTORY_MESSAGES: usize = 4;

// =========================================================
// 场景执行
// =========================================================

/// 造 fixture：persona（无会话；会话由生成用例按外部标识创建）。
async fn fixture(env: &ParityEnv) -> ParityResult<()> {
    fixtures::seed_persona(env.storage(), DEFAULT_PERSONA_UID).await
}

/// 构造生成请求（指定外部对话标识）。
fn request(conversation_id: &str) -> ChatSendRequest {
    ChatSendRequest {
        message: USER_MESSAGE.to_string(),
        persona: Some(DEFAULT_PERSONA_UID.to_string()),
        session_id: None,
        conversation_id: Some(conversation_id.to_string()),
        channel: CHANNEL_MCP.to_string(),
    }
}

/// 在给定环境上执行一轮生成，产出规范化快照。
///
/// 参数:
/// - `env`: 对照环境（脚本 LLM 由调用方构造并保留引用）。
/// - `llm`: 脚本 LLM 引用（用于读取本轮请求的 Prompt 结构指标）。
/// - `conversation_id`: 外部对话标识（决定会话续写或新建；不进入快照）。
async fn snapshot_of(
    env: &ParityEnv,
    llm: &ScriptedLlm,
    conversation_id: &str,
) -> ParityResult<Snapshot> {
    fixture(env).await?;

    let outcome = env
        .engine()
        .chat_send(request(conversation_id))
        .await
        .map_err(|e| ParityError::env("执行生成用例", e))?;

    let session = env
        .storage()
        .get_session(outcome.session_id)
        .await
        .map_err(|e| ParityError::env("读取生成会话", e))?
        .ok_or_else(|| ParityError::env("读取生成会话", "会话应存在"))?;
    let messages = env
        .storage()
        .list_messages(outcome.session_id)
        .await
        .map_err(|e| ParityError::env("读取生成会话消息", e))?;

    let requests = llm.requests();
    let last_request = requests
        .last()
        .ok_or_else(|| ParityError::env("读取 LLM 请求", "应至少记录一次请求"))?;

    Ok(Snapshot::new(
        SCENARIO,
        json!({
            "reply": outcome.reply,
            "chars": outcome.chars,
            "session": {
                "channel": session.channel,
                "has_external_ref": session.external_ref.is_some(),
                "message_count": messages.len(),
            },
            "messages": messages
                .iter()
                .map(|message| json!({
                    "role": message.role,
                    "content": message.content.clone(),
                }))
                .collect::<Vec<_>>(),
            "prompt": {
                "system_prompt_chars": last_request.system_prompt.chars().count(),
                "memory_context_present": last_request.memory_context.is_some(),
                "memory_context_chars": last_request.memory_context.as_ref().map(|text| text.chars().count()).unwrap_or(0),
                "history_len": last_request.history.len(),
                "user_message": last_request.user_message.clone(),
                "template_version": last_request.template_version.clone(),
            },
        }),
    ))
}

/// 构造"脚本回复"环境并返回 LLM 引用（供 Prompt 结构断言）。
async fn env_with_script(tag: &str) -> ParityResult<(ParityEnv, Arc<ScriptedLlm>)> {
    let llm = Arc::new(ScriptedLlm::reply(REPLY));
    let llm_dyn: Arc<dyn LlmProvider> = Arc::clone(&llm) as Arc<dyn LlmProvider>;
    let env = ParityEnv::with_llm(tag, llm_dyn).await?;
    Ok((env, llm))
}

/// 在给定环境上执行一轮流式生成，产出规范化快照。
///
/// 说明:
/// - 交互入口需要就绪状态（状态门禁），fixture 后显式置 `Ready`；
/// - 快照只含稳定字段：事件类型序列 / 增量文本 / Done 字段 / 落库消息（角色 + 来源 + 内容）。
async fn stream_snapshot(env: &ParityEnv) -> ParityResult<Snapshot> {
    fixture(env).await?;
    env.engine().set_state(AppState::Ready);

    let handle = env
        .engine()
        .chat_stream(ChatStreamRequest {
            message: USER_MESSAGE.to_string(),
            persona: Some(DEFAULT_PERSONA_UID.to_string()),
            session_id: None,
            seed_history: Vec::new(),
            config_override: None,
        })
        .await
        .map_err(|e| ParityError::env("执行流式生成", e))?;

    let session_id = handle.session_id;
    let mut event_kinds: Vec<String> = Vec::new();
    let mut delta_text = String::new();
    let mut done: Option<(bool, Option<String>, usize)> = None;
    let mut stream = handle.events;
    while let Some(item) = stream.next().await {
        let event = item.map_err(|e| ParityError::env("消费流式事件", e))?;
        event_kinds.push(event.kind().to_string());
        match &event {
            StreamEvent::Delta { content, .. } => delta_text.push_str(content),
            StreamEvent::Done {
                session_id,
                backend_id,
                total_chars,
                ..
            } => done = Some((session_id.is_some(), backend_id.clone(), *total_chars)),
            _ => {}
        }
    }
    let (has_session_id, backend_id, total_chars) =
        done.ok_or_else(|| ParityError::env("执行流式生成", "事件流应包含 Done 事件"))?;

    let session = env
        .storage()
        .get_session(session_id)
        .await
        .map_err(|e| ParityError::env("读取流式生成会话", e))?
        .ok_or_else(|| ParityError::env("读取流式生成会话", "会话应存在"))?;
    let messages = env
        .storage()
        .list_messages(session_id)
        .await
        .map_err(|e| ParityError::env("读取流式生成会话消息", e))?;

    Ok(Snapshot::new(
        STREAM_SCENARIO,
        json!({
            "event_kinds": event_kinds,
            "delta_text": delta_text,
            "done": {
                "has_session_id": has_session_id,
                "backend_id": backend_id,
                "total_chars": total_chars,
            },
            "session": {
                "channel": session.channel,
                "message_count": messages.len(),
            },
            "messages": messages
                .iter()
                .map(|message| json!({
                    "role": message.role,
                    "source": message.source,
                    "content": message.content.clone(),
                }))
                .collect::<Vec<_>>(),
        }),
    ))
}

/// 在给定环境上执行"长会话历史窗口"场景，产出规范化快照。
///
/// 说明:
/// - 会话预置 `HISTORY_SEED_MESSAGES` 条消息（超过默认加载上限 200）；
/// - 快照只取结构指标：进入 Prompt 的历史条数与会话消息总数（不落历史全文）。
async fn history_window_snapshot(env: &ParityEnv, llm: &ScriptedLlm) -> ParityResult<Snapshot> {
    fixture(env).await?;
    let session_id = fixtures::seed_active_session(
        env.storage(),
        DEFAULT_PERSONA_UID,
        HISTORY_SEED_MESSAGES,
        fixtures::fixture_ts(0),
    )
    .await?;

    let outcome = env
        .engine()
        .chat_send(ChatSendRequest {
            message: USER_MESSAGE.to_string(),
            persona: Some(DEFAULT_PERSONA_UID.to_string()),
            session_id: Some(session_id),
            conversation_id: None,
            channel: CHANNEL_MCP.to_string(),
        })
        .await
        .map_err(|e| ParityError::env("执行生成用例", e))?;

    let recorded = llm.requests();
    let last_request = recorded
        .last()
        .ok_or_else(|| ParityError::env("读取 LLM 请求", "应至少记录一次请求"))?;
    let message_count = env
        .storage()
        .list_messages(session_id)
        .await
        .map_err(|e| ParityError::env("读取生成会话消息", e))?
        .len();

    Ok(Snapshot::new(
        HISTORY_SCENARIO,
        json!({
            "reply": outcome.reply,
            "chars": outcome.chars,
            "history_len": last_request.history.len(),
            "user_message": last_request.user_message.clone(),
            "session_message_count": message_count,
        }),
    ))
}

// =========================================================
// 测试
// =========================================================

/// 基线一致：生成输出与冻结基线逐字段一致，且落库与字符数断言成立。
#[tokio::test]
async fn chat_snapshot_matches_golden_baseline() {
    let (env, llm) = env_with_script("chat-golden")
        .await
        .expect("生成对照环境应可构建");
    let snapshot = snapshot_of(&env, &llm, "parity-client")
        .await
        .expect("生成场景应执行成功");

    // 关键行为断言：回复与字符数一致、两条消息落库、Prompt 携带本轮输入
    assert_eq!(
        snapshot.value()["chars"].as_u64(),
        Some(REPLY.chars().count() as u64),
        "字符数应为回复字符数"
    );
    assert_eq!(
        snapshot.value()["session"]["message_count"].as_u64(),
        Some(2),
        "应落库用户消息与助手回复两条"
    );
    assert_eq!(
        snapshot.value()["prompt"]["user_message"].as_str(),
        Some(USER_MESSAGE),
        "Prompt 应携带本轮用户输入"
    );
    assert_eq!(llm.call_count(), 1, "一轮生成应只调用一次 LLM");

    let outcome = GoldenStore::new()
        .expect("基线仓库应可定位")
        .assert_or_record(&snapshot)
        .expect("基线比对或首次生成应成功");
    assert!(
        !outcome.is_updated(),
        "未开启更新模式时不应覆盖基线（{outcome:?}）"
    );
    tracing::info!(
        path = %outcome.path().display(),
        ?outcome,
        "生成基线比对完成"
    );

    env.cleanup().await;
}

/// 会话续写：同一外部对话标识的两轮生成应落在同一会话（历史累计 4 条消息）。
#[tokio::test]
async fn chat_reuses_session_for_same_conversation_id() {
    let (env, _llm) = env_with_script("chat-reuse")
        .await
        .expect("生成对照环境应可构建");
    fixture(&env).await.expect("fixture 应造数成功");

    let first = env
        .engine()
        .chat_send(request("parity-same-client"))
        .await
        .expect("首轮生成应成功");
    let second = env
        .engine()
        .chat_send(request("parity-same-client"))
        .await
        .expect("第二轮生成应成功");
    assert_eq!(
        first.session_id, second.session_id,
        "同一外部标识应续写同一会话"
    );

    let message_count = env
        .storage()
        .list_messages(first.session_id)
        .await
        .expect("读取会话消息应成功")
        .len();
    assert_eq!(message_count, 4, "两轮生成应累计 4 条消息");

    env.cleanup().await;
}

/// 独立产出等价：两个隔离环境各自执行一轮生成，输出应完全一致。
#[tokio::test]
async fn chat_isolated_envs_produce_equivalent_snapshots() {
    let (first_env, first_llm) = env_with_script("chat-parity-a")
        .await
        .expect("首个对照环境应可构建");
    let first = snapshot_of(&first_env, &first_llm, "parity-client")
        .await
        .expect("首轮生成应成功");
    first_env.cleanup().await;

    let (second_env, second_llm) = env_with_script("chat-parity-b")
        .await
        .expect("第二个对照环境应可构建");
    let second = snapshot_of(&second_env, &second_llm, "parity-client")
        .await
        .expect("第二轮生成应成功");
    second_env.cleanup().await;

    assert_parity("chat/send-reply-and-persist", &first, &second);
}

/// LLM 不可用：错误上抛且库内不写半条（不产生孤立用户消息）。
#[tokio::test]
async fn chat_llm_failure_leaves_no_partial_write() {
    let llm: Arc<dyn LlmProvider> = Arc::new(ScriptedLlm::failing("模拟 LLM 后端不可用"));
    let env = ParityEnv::with_llm("chat-failure", llm)
        .await
        .expect("生成对照环境应可构建");
    fixture(&env).await.expect("fixture 应造数成功");

    let error = env
        .engine()
        .chat_send(request("parity-client"))
        .await
        .expect_err("LLM 失败应上抛");
    assert_eq!(error.category(), "llm", "应返回 LLM 类错误: {error}");

    // 会话可能已创建（会话解析先于 LLM 调用），但任何会话内都不得出现消息
    let active_sessions = env
        .storage()
        .list_active_sessions()
        .await
        .expect("读取活跃会话应成功");
    for session in &active_sessions {
        let messages = env
            .storage()
            .list_messages(session.id)
            .await
            .expect("读取会话消息应成功");
        assert!(
            messages.is_empty(),
            "LLM 失败不应写入孤立用户消息（会话 {}）: {messages:?}",
            session.id
        );
    }

    env.cleanup().await;
}

/// 流式事件序列稳定：Delta… → Done 的事件序列、Done 字段与落库结果
/// 在两个隔离环境上一致，并与冻结基线逐字段一致。
#[tokio::test]
async fn chat_stream_events_are_stable_and_match_golden() {
    let (first_env, first_llm) = env_with_script("chat-stream-a")
        .await
        .expect("首个对照环境应可构建");
    let first = stream_snapshot(&first_env)
        .await
        .expect("流式场景应执行成功");
    assert_eq!(first_llm.call_count(), 1, "一轮生成应只调用一次 LLM");
    first_env.cleanup().await;

    let (replay_env, _replay_llm) = env_with_script("chat-stream-b")
        .await
        .expect("第二个对照环境应可构建");
    let replay = stream_snapshot(&replay_env)
        .await
        .expect("流式场景应执行成功");
    replay_env.cleanup().await;

    assert_stable("chat/stream-delta-done", &first, &replay);

    // 关键行为断言：delta… → done、增量拼接为回复全文、落库两条消息
    let kinds: Vec<&str> = first.value()["event_kinds"]
        .as_array()
        .expect("事件序列应为数组")
        .iter()
        .filter_map(|item| item.as_str())
        .collect();
    assert!(kinds.len() >= 2, "应至少包含一个增量与一个完成事件");
    assert!(
        kinds[..kinds.len() - 1].iter().all(|kind| *kind == "delta"),
        "完成事件之前应全为增量事件: {kinds:?}"
    );
    assert_eq!(kinds[kinds.len() - 1], "done");
    assert_eq!(first.value()["delta_text"].as_str(), Some(REPLY));
    assert_eq!(first.value()["done"]["has_session_id"], json!(true));
    assert_eq!(first.value()["done"]["backend_id"].as_str(), Some("stop"));
    assert_eq!(
        first.value()["done"]["total_chars"].as_u64(),
        Some(REPLY.chars().count() as u64)
    );
    assert_eq!(
        first.value()["session"]["message_count"].as_u64(),
        Some(2),
        "流式成功应落库用户消息与助手回复两条"
    );

    let outcome = GoldenStore::new()
        .expect("基线仓库应可定位")
        .assert_or_record(&first)
        .expect("基线比对或首次生成应成功");
    assert!(
        !outcome.is_updated(),
        "未开启更新模式时不应覆盖基线（{outcome:?}）"
    );
}

/// 历史窗口上限：长会话（250 条消息）进入 Prompt 的历史为默认上限 200 条，
/// 两个隔离环境产出等价快照，并与冻结基线逐字段一致。
#[tokio::test]
async fn chat_history_window_is_stable_and_match_golden() {
    let (first_env, first_llm) = env_with_script("chat-history-a")
        .await
        .expect("首个对照环境应可构建");
    let first = history_window_snapshot(&first_env, &first_llm)
        .await
        .expect("历史窗口场景应执行成功");
    assert_eq!(
        first.value()["history_len"].as_u64(),
        Some(200),
        "默认加载上限 200 应生效"
    );
    first_env.cleanup().await;

    let (replay_env, replay_llm) = env_with_script("chat-history-b")
        .await
        .expect("第二个对照环境应可构建");
    let replay = history_window_snapshot(&replay_env, &replay_llm)
        .await
        .expect("历史窗口场景应执行成功");
    replay_env.cleanup().await;

    assert_stable("chat/history-window-limit", &first, &replay);

    let outcome = GoldenStore::new()
        .expect("基线仓库应可定位")
        .assert_or_record(&first)
        .expect("基线比对或首次生成应成功");
    assert!(
        !outcome.is_updated(),
        "未开启更新模式时不应覆盖基线（{outcome:?}）"
    );
}

// =========================================================
// 逐字对照（应用装配 vs 服务装配）
// =========================================================

/// 取最后一个生成请求（每轮恰有一次生成调用）。
fn last_request(requests: Vec<ChatRequest>) -> ParityResult<ChatRequest> {
    requests
        .into_iter()
        .last()
        .ok_or_else(|| ParityError::env("读取生成请求", "应至少记录一次请求"))
}

/// 生成结果快照（两侧同形；渠道与外部标识两侧口径不同，不进入快照）。
fn chat_value(
    reply: &str,
    request: &ChatRequest,
    messages: &[ramaria_core::types::Message],
) -> serde_json::Value {
    json!({
        "reply": reply,
        "chars": reply.chars().count(),
        "messages": messages
            .iter()
            .map(|message| json!({
                "role": message.role,
                "content": message.content.clone(),
            }))
            .collect::<Vec<_>>(),
        "session": {
            "message_count": messages.len(),
        },
        "prompt": {
            "system_prompt_chars": request.system_prompt.chars().count(),
            "memory_context_present": request.memory_context.is_some(),
            "memory_context_chars": request
                .memory_context
                .as_ref()
                .map(|text| text.chars().count())
                .unwrap_or(0),
            "history_len": request.history.len(),
            "user_message": request.user_message.clone(),
            "template_version": request.template_version.clone(),
        },
    })
}

/// 应用装配：向既有会话发送一条消息（消费事件流），产出回复 / 落库 / 生成请求结构快照。
async fn cross_snapshot_app(env: &AppEnv) -> ParityResult<Snapshot> {
    fixtures::seed_persona(env.storage(), DEFAULT_PERSONA_UID).await?;
    let session_id = fixtures::seed_active_session(
        env.storage(),
        DEFAULT_PERSONA_UID,
        CROSS_HISTORY_MESSAGES,
        fixtures::fixture_ts(0),
    )
    .await?;
    env.setup_ready().await?;

    let mut stream = env
        .app()
        .send_message(USER_MESSAGE, Some(DEFAULT_PERSONA_UID), Some(session_id))
        .await
        .map_err(|e| ParityError::env("应用装配发送对照消息", e))?;

    let mut reply = String::new();
    while let Some(item) = stream.next().await {
        let event = item.map_err(|e| ParityError::env("应用装配消费对照事件流", e))?;
        if let ramaria_app::StreamEvent::Delta { content, .. } = event {
            reply.push_str(&content);
        }
    }

    let request = last_request(env.llm().requests())?;
    let messages = env
        .storage()
        .list_messages(session_id)
        .await
        .map_err(|e| ParityError::env("应用装配读取对照会话消息", e))?;
    Ok(Snapshot::new(
        CROSS_SCENARIO,
        chat_value(&reply, &request, &messages),
    ))
}

/// 服务装配：向既有会话发送一条消息，产出回复 / 落库 / 生成请求结构快照。
async fn cross_snapshot_service(env: &ParityEnv, llm: &ScriptedLlm) -> ParityResult<Snapshot> {
    fixtures::seed_persona(env.storage(), DEFAULT_PERSONA_UID).await?;
    let session_id = fixtures::seed_active_session(
        env.storage(),
        DEFAULT_PERSONA_UID,
        CROSS_HISTORY_MESSAGES,
        fixtures::fixture_ts(0),
    )
    .await?;

    let outcome = env
        .engine()
        .chat_send(ChatSendRequest {
            message: USER_MESSAGE.to_string(),
            persona: Some(DEFAULT_PERSONA_UID.to_string()),
            session_id: Some(session_id),
            conversation_id: None,
            channel: CHANNEL_LOCAL.to_string(),
        })
        .await
        .map_err(|e| ParityError::env("服务装配发送对照消息", e))?;

    let request = last_request(llm.requests())?;
    let messages = env
        .storage()
        .list_messages(session_id)
        .await
        .map_err(|e| ParityError::env("服务装配读取对照会话消息", e))?;
    Ok(Snapshot::new(
        CROSS_SCENARIO,
        chat_value(&outcome.reply, &request, &messages),
    ))
}

/// 逐字对照：回复内容 / 落库消息序列 / 生成请求结构在两侧等价。
///
/// 口径说明:
/// - 两侧为同一 fixture（同一人格 + 同一会话的 4 条历史消息），各发同一条消息到该会话；
/// - 渠道与外部标识两侧口径不同（本地会话无外部标识），不进入快照；
/// - 生成请求结构只取稳定指标（长度 / 是否携带记忆上下文 / 历史条数 / 模板版本），不落请求全文。
#[tokio::test]
async fn chat_reply_and_request_are_equivalent_between_app_and_service() {
    let app_env = AppEnv::with_llm("chat-cross-app", Arc::new(ScriptedLlm::reply(REPLY)))
        .await
        .expect("应用装配对照环境应可构建");
    let left = cross_snapshot_app(&app_env)
        .await
        .expect("应用装配生成场景应执行成功");
    app_env.cleanup().await;

    let service_llm = Arc::new(ScriptedLlm::reply(REPLY));
    let service_env = ParityEnv::with_llm(
        "chat-cross-service",
        Arc::clone(&service_llm) as Arc<dyn LlmProvider>,
    )
    .await
    .expect("服务装配对照环境应可构建");
    let right = cross_snapshot_service(&service_env, &service_llm)
        .await
        .expect("服务装配生成场景应执行成功");
    service_env.cleanup().await;

    // 关键行为锚点：回复落库、历史进入请求（对照面成立）
    assert_eq!(left.value()["reply"].as_str(), Some(REPLY));
    assert_eq!(
        left.value()["session"]["message_count"].as_u64(),
        Some((CROSS_HISTORY_MESSAGES + 2) as u64),
        "历史 4 条 + 本轮用户消息 + 助手回复"
    );
    assert_eq!(
        left.value()["prompt"]["history_len"].as_u64(),
        Some(CROSS_HISTORY_MESSAGES as u64),
        "既有会话历史应进入本轮生成请求"
    );

    assert_parity(CROSS_SCENARIO, &left, &right);
}
