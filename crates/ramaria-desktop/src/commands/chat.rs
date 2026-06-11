//! rust/crates/ramaria-desktop/src/commands/chat.rs - 聊天与对话 Tauri Commands
//!
//! 设计特点:
//! - `send_message`: 异步启动 LLM 流式对话，立即返回 request_id，通过 Tauri Event 推送增量内容
//! - `get_app_state`: 返回当前应用状态，前端据此决定显示哪个界面
//! - `check_privacy` / `confirm_privacy`: 委托 ramaria_app 的隐私确认流程
//! - 所有错误通过 ChatErrorPayload 格式返回，包含用户友好的标题和详情
//! - 不写业务逻辑，只做参数校验 + 委托调用 + 事件发射

use crate::DesktopState;
use crate::events::{ChatDeltaPayload, ChatDonePayload, ChatErrorPayload};
use ramaria_app::error_hint;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio_stream::StreamExt;

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
/// - `chat-done`: LLM 回复完成时发射，携带 request_id 和统计信息
/// - `chat-error`: LLM 调用出错时发射，携带 request_id 和错误详情
///
/// 说明:
/// - 此命令不会阻塞等待 LLM 完整回复，而是立即返回
/// - 前端应监听上述三个事件来渲染回复内容
#[tauri::command]
#[tracing::instrument(skip(state, app_handle, message), fields(msg_len))]
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

    // ---- 克隆必要资源以供后台任务使用 ----
    let app = state.app.clone();
    let handle = app_handle.clone();

    let persona_uid_owned = persona_uid.clone();
    let session_id_parsed: Option<uuid::Uuid> = session_id
        .as_ref()
        .and_then(|s| uuid::Uuid::parse_str(s).ok());

    // ---- 启动后台流处理任务 ----
    tokio::spawn(async move {
        process_message_stream(
            app,
            handle,
            rid_for_task,
            trimmed,
            persona_uid_owned,
            session_id_parsed,
        )
        .await;
    });

    Ok(request_id)
}

/// 后台任务：消费 send_message 返回的流，逐个发射 Tauri 事件。
async fn process_message_stream(
    app: Arc<ramaria_app::App>,
    handle: AppHandle,
    request_id: String,
    message: String,
    persona_uid: Option<String>,
    session_id: Option<uuid::Uuid>,
) {
    // 调用 App::send_message 获取流
    let mut stream = match app
        .send_message(&message, persona_uid.as_deref(), session_id)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            // send_message 本身失败（状态检查、隐私确认等）
            emit_chat_error(&handle, &request_id, &e);
            tracing::error!(
                request_id = %request_id,
                error = %e,
                "send_message 调用失败"
            );
            return;
        }
    };

    // 逐事件消费流（send_message 已返回 Pin<Box<dyn Stream>>）
    let mut total_chars: usize = 0;

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(ramaria_app::StreamEvent::Delta { content, .. }) => {
                total_chars += content.chars().count();
                let payload = ChatDeltaPayload::new(request_id.clone(), content);
                if let Err(e) = handle.emit(crate::events::EVENT_CHAT_DELTA, &payload) {
                    tracing::error!(
                        request_id = %request_id,
                        error = %e,
                        "发射 chat-delta 事件失败"
                    );
                }
            }
            Ok(ramaria_app::StreamEvent::Done {
                backend_id,
                total_chars: stream_total,
                ..
            }) => {
                total_chars = stream_total; // 以 StreamEvent 通知的为准
                let payload = ChatDonePayload::new(request_id.clone(), backend_id, total_chars);
                if let Err(e) = handle.emit(crate::events::EVENT_CHAT_DONE, &payload) {
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
                return;
            }
            Ok(ramaria_app::StreamEvent::Error { error, .. }) => {
                let hint = error_hint::ErrorHint::from_error(
                    &ramaria_core::error::RamariaError::llm(error.clone()),
                );
                let payload = ChatErrorPayload::new(
                    request_id.clone(),
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
                emit_chat_error(&handle, &request_id, &e);
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
    let payload = ChatDonePayload::new(request_id.clone(), None, total_chars);
    if let Err(e) = handle.emit(crate::events::EVENT_CHAT_DONE, &payload) {
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
}

/// 辅助函数：将 RamariaError 转换为 ChatErrorPayload 并发射 `chat-error` 事件。
fn emit_chat_error(handle: &AppHandle, request_id: &str, err: &ramaria_core::error::RamariaError) {
    let hint = error_hint::ErrorHint::from_error(err);
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

// =========================================================
// get_app_state — 应用状态查询
// =========================================================

/// 获取当前应用状态。
///
/// 返回:
/// - 状态字符串："NeedsSetup" | "DownloadingModel" | "Indexing" | "Ready" | "Degraded" | "FatalError"
///
/// 说明:
/// - 前端在加载时调用此命令，根据返回值决定显示哪个页面
#[tauri::command]
pub async fn get_app_state(state: State<'_, DesktopState>) -> Result<String, String> {
    let app_state = state.app.current_state();
    tracing::debug!(state = %app_state.as_str(), "get_app_state 查询");
    Ok(app_state.as_str().to_string())
}

// =========================================================
// check_privacy — 隐私确认状态检查
// =========================================================

/// 检查当前后端的隐私确认状态。
///
/// 返回:
/// - JSON 对象，包含 status 字段：
///   - `"NotNeeded"`: 本地服务，无需确认
///   - `"Confirmed"`: 已确认（含 persistent 标志和确认时间）
///   - `"NeedsConfirmation"`: 需要用户确认（含 provider_name 和 base_url）
#[tauri::command]
pub async fn check_privacy(state: State<'_, DesktopState>) -> Result<serde_json::Value, String> {
    let status = state
        .app
        .check_privacy()
        .await
        .map_err(|e| format!("检查隐私状态失败: {}", e))?;

    let json = match &status {
        ramaria_app::PrivacyStatus::NotNeeded => {
            serde_json::json!({ "status": "NotNeeded" })
        }
        ramaria_app::PrivacyStatus::Confirmed {
            persistent,
            confirmed_at,
        } => {
            serde_json::json!({
                "status": "Confirmed",
                "persistent": persistent,
                "confirmed_at": confirmed_at,
            })
        }
        ramaria_app::PrivacyStatus::NeedsConfirmation {
            provider_name,
            base_url,
        } => {
            serde_json::json!({
                "status": "NeedsConfirmation",
                "provider_name": provider_name,
                "base_url": base_url,
            })
        }
        _ => serde_json::json!({ "status": "Unknown" }),
    };

    Ok(json)
}

// =========================================================
// confirm_privacy — 记录隐私确认
// =========================================================

/// 记录用户对当前 provider 的隐私确认。
///
/// 参数:
/// - `persistent`: 是否持久化（跨重启记住）
///
/// 返回:
/// - `"ok"` 表示确认已记录
#[tauri::command]
pub async fn confirm_privacy(
    state: State<'_, DesktopState>,
    persistent: bool,
) -> Result<String, String> {
    state
        .app
        .confirm_privacy(persistent)
        .await
        .map_err(|e| format!("记录隐私确认失败: {}", e))?;

    tracing::info!(persistent = persistent, "隐私确认已记录");
    Ok("ok".to_string())
}
