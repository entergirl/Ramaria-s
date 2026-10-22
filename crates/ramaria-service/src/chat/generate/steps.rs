//! crates/ramaria-service/src/chat/generate/steps.rs - Ramaria 生成侧步骤实现
//!
//! 设计特点:
//! - 非流式仅在 LLM 成功后落库；失败记 warn 并上抛，不留半条
//! - 流式后台任务逐增量转发，结束仅在无错时发完成事件
//! - 空回复与非流式路径同一纪律：转错误事件、不落助手消息
//! - 隐私：日志只记会话与长度，不记消息与回复内容

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

use super::EMPTY_REPLY_MESSAGE;

// =========================================================
// 生成侧（非流式 / 流式）
// =========================================================

/// 非流式生成：调用 LLM 并返回回复全文；失败记 warn 并上抛（不落库）。
pub(super) async fn step_call_llm(
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
pub(super) async fn step_persist_reply(
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

/// 社交通道落库：只写入助手回复行（入站用户消息已由通道写入用例落库）。
pub(super) async fn step_persist_assistant_only(
    storage: &dyn StorageBackend,
    session_id: Uuid,
    persona: &str,
    reply: &str,
) -> RamariaResult<usize> {
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
pub(super) type RawDeltaStream = Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>;

/// 步骤 15a（流式）：打开 LLM 增量流。
///
/// 返回:
/// - `Ok(stream)`: 增量流已打开，交由转发任务消费；
/// - `Err(handle)`: 流未打开——返回只含一个 `Error` 事件的事件流句柄（不落库）。
pub(super) async fn step_open_stream(
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
/// 1. 用户消息已由前置编排落库（携带人格归属），本任务不再写入；
/// 2. 逐增量转发 `Delta`；`done=true` 的增量记录后端标识并结束读取；
/// 3. 流中错误 → 发送 `Error` 事件并标记错误（不再发 `Done`）；
/// 4. 结束时仅当无错且回复非空才落库助手消息（来源线上）；
/// 5. 仅在无错时发送 `Done`（含会话、后端标识与总字符数）。
pub(super) async fn forward_and_persist(
    storage: Arc<dyn StorageBackend>,
    raw_stream: RawDeltaStream,
    tx: mpsc::UnboundedSender<RamariaResult<StreamEvent>>,
    session_id: Uuid,
    request_id: Uuid,
    persona_uid: Option<String>,
) {
    futures::pin_mut!(raw_stream);

    let mut full_reply = String::new();
    let mut backend_id: Option<String> = None;
    let mut has_error = false;
    let started_at = now_ms();

    // 1. 消费 LLM 流
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

    // 2.5 空回复判定：与非流式路径同一纪律——空正文按错误处理。
    // 触发场景: 思考模式耗尽输出预算、后端返回 200 但 choices 为空、内容审核截断。
    // 若按成功收尾，助手消息不落库（下方判空跳过）却已发 `Done(0)`，
    // 前端会留下一个没有任何内容、也无法重试的气泡。
    if !has_error && full_reply.trim().is_empty() {
        has_error = true;
        tracing::warn!(
            request_id = %request_id,
            session_id = %session_id,
            duration_ms = now_ms() - started_at,
            "模型返回空回复（HTTP 200 但无正文），按错误处理"
        );
        let _ = tx.unbounded_send(Ok(StreamEvent::error(
            request_id,
            EMPTY_REPLY_MESSAGE.to_string(),
        )));
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
