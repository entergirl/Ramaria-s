//! crates/ramaria-service/src/chat/generate.rs - Ramaria 生成用例入口与生成侧实现
//!
//! 设计特点:
//! - 用例入口：非流式 `run`（通道入口）与流式 `stream`（交互入口）共用 `prepare_request` 前置编排
//! - 失败不留半条：非流式仅在 LLM 成功后落库；流式先落用户消息，
//!   助手回复仅在无错且非空时落库
//! - 流打不开：返回只含一个 `Error` 事件的事件流句柄（不落库、不启动转发任务）
//! - 事件桥接：后台任务逐增量转发 `Delta`，结束时仅当无错才发 `Done`
//!   （错误经 `Error` 事件表达，不再发 `Done`）
//! - 隐私：日志只记会话 id、人格与长度，不记消息与回复内容

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use futures::StreamExt;
use futures::channel::mpsc;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, StorageBackend, StreamDelta};
use ramaria_core::types::{Message, MessageRole, MessageSource, now_ms};
use uuid::Uuid;

use crate::engine::Engine;
use crate::stream_event::{ChatStreamHandle, StreamEvent};
use crate::types::{ChatSendOutcome, ChatSendRequest, ChatStreamRequest};

use super::steps::{ChatInput, ChatMode, prepare_request};

// =========================================================
// 用例入口
// =========================================================

/// 执行非流式生成用例（通道入口）：以指定人格回复一条消息（记忆检索 + Prompt 装配 + LLM）。
///
/// 流程:
/// 1. 参数与策略校验（空消息 / 人格白名单）；
/// 2. 会话定位（显式 `session_id` > `channel` + `conversation_id`，含惰性封存体检）；
/// 3. 载入会话历史（配置驱动的分页窗口）；
/// 4. 共用召回装配记忆上下文（与在线管线同一份实现）；
/// 5. 脉络素材与行为 / 知识 / 示例素材（失败或关闭均静默降级）；
/// 6. 系统 Prompt 装配（普通 / 协调预算两条路径）+ Token 预算裁剪；
/// 7. LLM 生成（非流式）→ 成功后落库用户消息与助手回复。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 生成请求（消息 / 人格 / 会话定位 / 通道）。
///
/// 返回:
/// - 成功时返回 `reply` / `session_id` / `chars`。
/// - 校验失败返回 `Validation`；人格越权返回 `Privacy`；LLM 不可用返回 `Llm`。
pub(crate) async fn run(engine: &Engine, req: ChatSendRequest) -> RamariaResult<ChatSendOutcome> {
    let config = engine.config();
    let input = ChatInput {
        message: req.message,
        persona: req.persona,
        session_id: req.session_id,
        mode: ChatMode::Channel,
        channel: req.channel,
        conversation_id: req.conversation_id,
        seed_history: Vec::new(),
    };

    let prepared = prepare_request(engine, &input, config.as_ref()).await?;
    tracing::info!(
        request_id = %prepared.request_id,
        session_id = %prepared.session_id,
        persona = %prepared.persona,
        input_chars = prepared.message.chars().count(),
        "生成用例开始（记忆检索与 Prompt 装配完成）"
    );

    // ---- 非流式：LLM 生成（失败不落库，避免库内留下孤立用户消息） ----
    let reply = step_call_llm(engine, &prepared.chat_request, prepared.session_id).await?;

    // ---- 落库（用户消息 + 助手回复；均带人格归属，供桌面按来源展示） ----
    let storage = engine.storage_ref().as_ref();
    let reply_chars = step_persist_reply(
        storage,
        prepared.session_id,
        &prepared.persona,
        &prepared.message,
        &reply,
    )
    .await?;

    tracing::info!(
        session_id = %prepared.session_id,
        persona = %prepared.persona,
        reply_chars,
        "生成用例完成（消息已落库）"
    );

    Ok(ChatSendOutcome {
        reply,
        session_id: prepared.session_id,
        chars: reply_chars,
    })
}

/// 执行流式生成用例（交互入口）：装配完成后返回增量事件流句柄。
///
/// 流程:
/// - 前置编排与非流式完全同源（参数校验 / 门禁 / 会话 / 历史 / 召回 / Prompt / 预算）；
/// - 打开 LLM 增量流失败：不落库、不启动转发任务，返回只含一个 `Error` 事件的流；
/// - 打开成功：后台任务先落库用户消息，再逐增量转发 `Delta`，结束时落库助手回复
///   （仅当无错且非空）并发 `Done`（错误经 `Error` 事件表达，不再发 `Done`）。
///
/// 参数:
/// - `engine`: 服务层引擎（后台转发任务持有存储句柄）。
/// - `req`: 流式生成请求（消息 / 人格 / 会话 / 预置上文 / 配置覆盖）。
///
/// 返回:
/// - 成功时返回 `ChatStreamHandle`（会话定位 + 事件流）；前置编排失败返回对应错误。
pub(crate) async fn stream(
    engine: &Arc<Engine>,
    req: ChatStreamRequest,
) -> RamariaResult<ChatStreamHandle> {
    let config = req
        .config_override
        .clone()
        .unwrap_or_else(|| engine.config());
    let input = ChatInput {
        message: req.message,
        persona: req.persona,
        session_id: req.session_id,
        mode: ChatMode::Interactive,
        channel: String::new(),
        conversation_id: None,
        seed_history: req.seed_history,
    };

    let prepared = prepare_request(engine.as_ref(), &input, config.as_ref()).await?;
    let session_id = prepared.session_id;
    let request_id = prepared.request_id;
    tracing::info!(
        request_id = %request_id,
        session_id = %session_id,
        persona = %prepared.persona,
        input_chars = prepared.message.chars().count(),
        "流式生成开始（记忆检索与 Prompt 装配完成）"
    );

    // ---- 流式：打开 LLM 增量流（失败返回只含一个 Error 事件的事件流，不落库） ----
    let raw_stream =
        match step_open_stream(engine, &prepared.chat_request, request_id, session_id).await {
            Ok(raw_stream) => raw_stream,
            Err(handle) => return Ok(handle),
        };

    // ---- 后台任务：转发增量事件并落库（用户消息先落，助手回复按无错且非空落） ----
    let storage = Arc::clone(engine.storage_ref());
    let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();
    tokio::spawn(forward_and_persist(
        storage,
        raw_stream,
        tx,
        session_id,
        prepared.message,
        request_id,
        Some(prepared.persona),
    ));

    Ok(ChatStreamHandle {
        request_id,
        session_id,
        events: Box::pin(rx),
    })
}

// =========================================================
// 生成侧（非流式 / 流式）
// =========================================================

/// 非流式生成：调用 LLM 并返回回复全文；失败记 warn 并上抛（不落库）。
async fn step_call_llm(
    engine: &Engine,
    request: &ChatRequest,
    session_id: Uuid,
) -> RamariaResult<String> {
    // LLM provider 取快照后在锁外调用（热更新期间取到旧快照或新快照均自洽）
    let llm = engine.llm_ref();
    match llm.chat(request).await {
        Ok(reply) => Ok(reply),
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "LLM 生成失败，本次不写入任何消息"
            );
            Err(e)
        }
    }
}

/// 非流式落库：写入用户消息与助手回复（均带人格归属，来源为本地）。
async fn step_persist_reply(
    storage: &dyn StorageBackend,
    session_id: Uuid,
    persona: &str,
    message: &str,
    reply: &str,
) -> RamariaResult<usize> {
    let user_message = Message::new(
        session_id,
        MessageRole::User,
        message.to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(persona.to_string()));
    storage.save_message(&user_message).await?;

    let reply_chars = reply.chars().count();
    let assistant_message = Message::new(
        session_id,
        MessageRole::Assistant,
        reply.to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(persona.to_string()));
    storage.save_message(&assistant_message).await?;

    Ok(reply_chars)
}

/// LLM 原始增量流类型（provider 层协议）。
type RawDeltaStream = Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>;

/// 步骤 15a（流式）：打开 LLM 增量流。
///
/// 返回:
/// - `Ok(stream)`: 增量流已打开，交由转发任务消费；
/// - `Err(handle)`: 流未打开——返回只含一个 `Error` 事件的事件流句柄（不落库）。
async fn step_open_stream(
    engine: &Engine,
    request: &ChatRequest,
    request_id: Uuid,
    session_id: Uuid,
) -> std::result::Result<RawDeltaStream, ChatStreamHandle> {
    let llm = engine.llm_ref();
    match llm.chat_stream(request).await {
        Ok(stream) => Ok(stream),
        Err(e) => {
            tracing::error!(
                error = %e,
                request_id = %request_id,
                session_id = %session_id,
                "LLM chat_stream 调用失败，构造 Error 事件流"
            );
            let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();
            let _ = tx.unbounded_send(Ok(StreamEvent::error(request_id, e.to_string())));
            Err(ChatStreamHandle {
                request_id,
                session_id,
                events: Box::pin(rx),
            })
        }
    }
}

/// 流式转发任务：消费原始增量流，转发事件并落库消息。
///
/// 语义（与非流式路径的失败纪律对齐）:
/// 1. 先落库用户消息（来源本地）；落库失败 → 事件流发送错误项并终止；
/// 2. 逐增量转发 `Delta`；`done=true` 的增量记录后端标识并结束读取；
/// 3. 流中错误 → 发送 `Error` 事件并标记错误（不再发 `Done`）；
/// 4. 结束时仅当无错且回复非空才落库助手消息（来源线上）；
/// 5. 仅在无错时发送 `Done`（含会话、后端标识与总字符数）。
async fn forward_and_persist(
    storage: Arc<dyn StorageBackend>,
    raw_stream: RawDeltaStream,
    tx: mpsc::UnboundedSender<RamariaResult<StreamEvent>>,
    session_id: Uuid,
    user_message: String,
    request_id: Uuid,
    persona_uid: Option<String>,
) {
    futures::pin_mut!(raw_stream);

    let mut full_reply = String::new();
    let mut backend_id: Option<String> = None;
    let mut has_error = false;
    let started_at = now_ms();

    // 1. 保存用户消息（携带人格归属，表示"在此 persona 的对话上下文中"）
    let user_msg = Message::new(
        session_id,
        MessageRole::User,
        user_message,
        MessageSource::Local,
    )
    .with_persona_uid(persona_uid.clone());
    if let Err(e) = storage.save_message(&user_msg).await {
        tracing::error!(session_id = %session_id, error = %e, "保存用户消息失败");
        let _ = tx.unbounded_send(Err(e));
        return;
    }

    // 2. 消费 LLM 流
    while let Some(delta_result) = raw_stream.next().await {
        match delta_result {
            Ok(delta) => {
                full_reply.push_str(&delta.content);

                // 转发 Delta 事件
                let event = StreamEvent::delta(request_id, delta.content);
                if tx.unbounded_send(Ok(event)).is_err() {
                    return; // 接收端已断开
                }

                if delta.done {
                    backend_id = delta.metadata;
                    break;
                }
            }
            Err(e) => {
                has_error = true;
                tracing::error!(session_id = %session_id, error = %e, "LLM 流错误");
                let event = StreamEvent::error(request_id, e.to_string());
                let _ = tx.unbounded_send(Ok(event));
                break;
            }
        }
    }

    // 3. 保存助手消息（仅在非错误且非空时；来源线上，携带人格归属）
    if !has_error && !full_reply.is_empty() {
        let assistant_msg = Message::new(
            session_id,
            MessageRole::Assistant,
            full_reply.clone(),
            MessageSource::Online,
        )
        .with_persona_uid(persona_uid.clone());
        if let Err(e) = storage.save_message(&assistant_msg).await {
            tracing::error!(session_id = %session_id, error = %e, "保存 assistant 消息失败");
        }
    }

    // 4. 发送 Done 事件（仅在无错误时——错误已通过 Error 事件发送，无需再发 Done）
    if !has_error {
        let done_event = StreamEvent::done(
            request_id,
            Some(session_id),
            backend_id,
            full_reply.chars().count(),
        );
        let _ = tx.unbounded_send(Ok(done_event));
    }

    tracing::info!(
        request_id = %request_id,
        session_id = %session_id,
        reply_chars = full_reply.chars().count(),
        has_error,
        duration_ms = now_ms() - started_at,
        "流式生成完成"
    );
}
