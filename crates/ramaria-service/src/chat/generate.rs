//! crates/ramaria-service/src/chat/generate.rs - Ramaria 生成用例入口与生成侧实现
//!
//! 设计特点:
//! - 用例入口：非流式 `run`（通道入口）与流式 `stream`（交互入口）共用 `prepare_request` 前置编排
//! - 失败不留半条：非流式仅在 LLM 成功后落库；流式在前置编排即落用户消息，
//!   助手回复仅在无错且非空时落库
//! - 流打不开：返回只含一个 `Error` 事件的事件流句柄（用户消息已由前置编排落库，
//!   不启动转发任务）
//! - 事件桥接：后台任务逐增量转发 `Delta`，结束时仅当无错才发 `Done`
//!   （错误经 `Error` 事件表达，不再发 `Done`）
//! - 空回复：HTTP 200 但正文为空（思考模式耗尽预算 / 后端异常 / choices 为空）
//!   与非流式路径同一纪律——转 `Error`、不发 `Done`、不落助手消息，
//!   避免前端留下无法判断的空气泡
//! - 隐私：日志只记会话 id、人格与长度，不记消息与回复内容

mod reply;
mod steps;

pub(crate) use reply::reply_to_inbound;

use std::sync::Arc;

use futures::channel::mpsc;
use ramaria_core::error::RamariaResult;
use ramaria_memory::chat::SegmentStyle;

use crate::engine::Engine;
use crate::stream_event::{ChatStreamHandle, StreamEvent};
use crate::types::{ChatSendOutcome, ChatSendRequest, ChatStreamRequest};

use super::steps::{ChatInput, ChatMode, prepare_request};
use steps::{forward_and_persist, step_call_llm, step_open_stream, step_persist_reply};

/// 空回复的错误文案（流式路径）。
///
/// 说明:
/// - 前端经 `chat-error` 提示并允许重试；
/// - 文案不含模型 / provider 细节（用户只需要"没生成出来，可以再试一次"）。
const EMPTY_REPLY_MESSAGE: &str = "模型本轮没有返回内容，请再试一次。";
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
        message_in_history: false,
        segment_style: SegmentStyle::Pipe,
        proactive: None,
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
///   用户消息在前置编排中落库（历史窗口加载之后，首条消息不阻塞）；
/// - 打开 LLM 增量流失败：不启动转发任务，返回只含一个 `Error` 事件的流
///   （用户消息保持已落库，供重试与历史可见）；
/// - 打开成功：后台任务逐增量转发 `Delta`，结束时落库助手回复
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
        message_in_history: false,
        segment_style: SegmentStyle::Pipe,
        proactive: None,
    };

    let prepared = prepare_request(engine.as_ref(), &input, config.as_ref()).await?;
    let session_id = prepared.session_id;
    let request_id = prepared.request_id;
    tracing::info!(
        request_id = %request_id,
        session_id = %session_id,
        persona = %prepared.persona,
        input_chars = prepared.message.chars().count(),
        user_persisted = prepared.user_persisted,
        "流式生成开始（记忆检索与 Prompt 装配完成）"
    );

    // ---- 流式：打开 LLM 增量流（失败返回只含一个 Error 事件的事件流，不落库） ----
    let raw_stream =
        match step_open_stream(engine, &prepared.chat_request, request_id, session_id).await {
            Ok(raw_stream) => raw_stream,
            Err(handle) => return Ok(handle),
        };

    // ---- 后台任务：转发增量事件并落库（用户消息已由前置编排落库，助手回复按无错且非空落） ----
    let storage = Arc::clone(engine.storage_ref());
    let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();
    tokio::spawn(forward_and_persist(
        storage,
        raw_stream,
        tx,
        session_id,
        request_id,
        Some(prepared.persona),
    ));

    Ok(ChatStreamHandle {
        request_id,
        session_id,
        events: Box::pin(rx),
    })
}
