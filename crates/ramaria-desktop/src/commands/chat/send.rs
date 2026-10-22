//! crates/ramaria-desktop/src/commands/chat/send.rs - Ramaria 对话发送与流式事件桥
//!
//! 设计特点:
//! - `send_message`: 异步启动 LLM 流式对话，立即返回 request_id，通过 Tauri Event 推送增量内容
//! - 流式事件桥：消费服务层的流式事件（增量 / 完成 / 错误），逐个发射 Tauri 事件
//! - 活跃会话指针在拿到流式句柄后由桌面设置（服务层不反向持有生命周期容器）
//! - 会话预检：已关闭或不存在的会话先自动重建再进入生成，避免前端竞态直接收到错误

use crate::DesktopState;
use crate::events::{ChatDeltaPayload, ChatDonePayload, ChatErrorPayload};
use crate::notification;
use futures::StreamExt;
use ramaria_core::error::RamariaError;
use ramaria_service::{ChatStreamRequest, ErrorHint, StreamEvent};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

// =========================================================
// send_message — 核心对话命令
// =========================================================

/// 发送用户消息并启动 LLM 流式对话。
///
/// 参数:
/// - `message`: 用户输入的文本消息（不可为空）
/// - `persona_uid`: 可选，指定对话 persona
/// - `session_id`: 可选，复用已有会话
///
/// 返回:
/// - 立即返回 `request_id`（UUID v4 字符串），前端用此 ID 关联后续事件
///
/// 事件流:
/// - `chat-delta`: 每收到 LLM 增量文本时发射，携带 request_id 和 content
/// - `chat-done`: LLM 回复完成时发射，携带 request_id、统计信息和完整回复文本
/// - `chat-error`: LLM 调用出错时发射，携带 request_id 和错误详情
///
/// 说明:
/// - 此命令不会阻塞等待 LLM 完整回复，而是立即返回
/// - 前端应监听上述三个事件来渲染回复内容
#[tauri::command]
#[tracing::instrument(skip(state, app_handle, message))]
pub async fn send_message(
    state: State<'_, DesktopState>,
    app_handle: AppHandle,
    message: String,
    persona_uid: Option<String>,
    session_id: Option<String>,
) -> Result<String, String> {
    // ---- 参数校验 ----
    let trimmed = message.trim().to_string();
    if trimmed.is_empty() {
        return Err("消息不能为空".to_string());
    }

    // ---- 生成请求 ID ----
    let request_id = uuid::Uuid::new_v4().to_string();
    let rid_for_task = request_id.clone();

    // 仅记录消息长度，不记录完整用户消息内容（隐私安全）
    tracing::info!(
        request_id = %request_id,
        persona_uid = ?persona_uid,
        session_id = ?session_id,
        msg_len = trimmed.chars().count(),
        "收到 send_message 请求"
    );

    // ---- 会话预检：已关闭 / 不存在的会话自动重建 ----
    // 前端竞态窗口可能传入已关闭（手动保存或空闲超时）或已删除的会话，
    // 预检命中时先新建会话再进入生成，避免直接收到"会话已关闭"错误。
    let persona_uid_owned = persona_uid.clone();
    let parsed_session_id: Option<uuid::Uuid> = session_id
        .as_ref()
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let effective_session_id = match parsed_session_id {
        Some(sid) => Some(
            state
                .engine
                .resolve_send_session(persona_uid.as_deref(), Some(sid))
                .await
                .map_err(|e| crate::commands::service_error_message(&e, "无法自动创建新会话"))?,
        ),
        None => None,
    };

    // ---- 启动后台流处理任务 ----
    let engine = Arc::clone(&state.engine);
    let lifecycle = Arc::clone(&state.lifecycle);
    let handle = app_handle.clone();
    tokio::spawn(async move {
        process_message_stream(
            engine,
            lifecycle,
            handle,
            rid_for_task,
            trimmed,
            persona_uid_owned,
            effective_session_id,
        )
        .await;
    });

    Ok(request_id)
}

/// 后台任务：消费服务层流式事件并逐个发射 Tauri 事件。
async fn process_message_stream(
    engine: Arc<ramaria_service::Engine>,
    lifecycle: Arc<ramaria_service::Lifecycle>,
    app_handle: AppHandle,
    request_id: String,
    message: String,
    persona_uid: Option<String>,
    session_id: Option<uuid::Uuid>,
) {
    // 调用服务层流式生成用例获取事件流
    let chat_handle = match engine
        .chat_stream(ChatStreamRequest {
            message,
            persona: persona_uid,
            session_id,
            seed_history: Vec::new(),
            config_override: None,
        })
        .await
    {
        Ok(chat_handle) => chat_handle,
        Err(e) => {
            // 前置编排失败（状态检查、隐私确认等）
            emit_chat_error(&app_handle, &request_id, &e);
            tracing::error!(
                request_id = %request_id,
                error = %e,
                "chat_stream 调用失败"
            );
            return;
        }
    };

    // ---- 活跃会话指针与活跃时间 ----
    // 服务层不反向持有生命周期容器：本轮对话的会话归属由桌面记录，
    // 供"手动保存 / 空闲检测"与后续消息复用同一会话。
    lifecycle.set_active_session_id(Some(chat_handle.session_id));
    lifecycle.touch_session(chat_handle.session_id);

    // 逐事件消费流
    let mut stream = chat_handle.events;
    let mut total_chars: usize = 0;
    // 累积完整回复文本（用于通知预览与 chat-done 事件全文回传）
    let mut accumulated_text = String::new();

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(StreamEvent::Delta { content, .. }) => {
                total_chars += content.chars().count();
                accumulated_text.push_str(&content);
                let payload = ChatDeltaPayload::new(request_id.clone(), content);
                if let Err(e) = app_handle.emit(crate::events::EVENT_CHAT_DELTA, &payload) {
                    tracing::error!(
                        request_id = %request_id,
                        error = %e,
                        "发射 chat-delta 事件失败"
                    );
                }
            }
            Ok(StreamEvent::Done {
                backend_id,
                total_chars: stream_total,
                ..
            }) => {
                total_chars = stream_total; // 以 StreamEvent 通知的为准
                let payload = ChatDonePayload::new(
                    request_id.clone(),
                    backend_id,
                    total_chars,
                    accumulated_text.clone(),
                );
                if let Err(e) = app_handle.emit(crate::events::EVENT_CHAT_DONE, &payload) {
                    tracing::error!(
                        request_id = %request_id,
                        error = %e,
                        "发射 chat-done 事件失败"
                    );
                }
                tracing::info!(
                    request_id = %request_id,
                    total_chars = total_chars,
                    "对话完成"
                );

                // ---- 发送桌面通知（窗口不可见时） ----
                notification::send_chat_notification(&app_handle, &accumulated_text, total_chars);

                return;
            }
            Ok(StreamEvent::Error { error, .. }) => {
                let hint = ErrorHint::from_error(&RamariaError::llm(error.clone()));
                let payload = ChatErrorPayload::new(
                    request_id.clone(),
                    hint.title,
                    hint.detail,
                    hint.retryable,
                );
                if let Err(e) = app_handle.emit(crate::events::EVENT_CHAT_ERROR, &payload) {
                    tracing::error!(
                        request_id = %request_id,
                        error = %e,
                        "发射 chat-error 事件失败"
                    );
                }
                return;
            }
            Ok(_) => {
                // #[non_exhaustive] 兜底：忽略未知变体
                tracing::warn!(
                    request_id = %request_id,
                    "收到未识别的 StreamEvent 变体，已忽略"
                );
            }
            Err(e) => {
                emit_chat_error(&app_handle, &request_id, &e);
                tracing::error!(
                    request_id = %request_id,
                    error = %e,
                    "流式事件接收失败"
                );
                return;
            }
        }
    }

    // 流在未发射 Done 或 Error 的情况下意外结束，发送合成 Done
    let payload = ChatDonePayload::new(
        request_id.clone(),
        None,
        total_chars,
        accumulated_text.clone(),
    );
    if let Err(e) = app_handle.emit(crate::events::EVENT_CHAT_DONE, &payload) {
        tracing::error!(
            request_id = %request_id,
            error = %e,
            "发射 chat-done 事件失败（流意外结束）"
        );
    }
    tracing::warn!(
        request_id = %request_id,
        total_chars = total_chars,
        "流在 Done 事件前意外结束，已发送合成 Done"
    );

    // 流意外结束时也发送通知（如果有累积文本）
    if !accumulated_text.is_empty() {
        notification::send_chat_notification(&app_handle, &accumulated_text, total_chars);
    }
}

/// 辅助函数：将 RamariaError 转换为 ChatErrorPayload 并发射 `chat-error` 事件。
fn emit_chat_error(handle: &AppHandle, request_id: &str, err: &RamariaError) {
    let hint = ErrorHint::from_error(err);
    let payload = ChatErrorPayload::new(
        request_id.to_string(),
        hint.title,
        hint.detail,
        hint.retryable,
    );
    if let Err(e) = handle.emit(crate::events::EVENT_CHAT_ERROR, &payload) {
        tracing::error!(
            request_id = %request_id,
            error = %e,
            "发射 chat-error 事件失败"
        );
    }
}
