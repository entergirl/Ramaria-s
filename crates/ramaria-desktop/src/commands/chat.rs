//! crates/ramaria-desktop/src/commands/chat.rs - 聊天与对话 Tauri Commands
//!
//! 设计特点:
//! - `send_message`: 异步启动 LLM 流式对话，立即返回 request_id，通过 Tauri Event 推送增量内容
//! - 流式事件桥：消费服务层的流式事件（增量 / 完成 / 错误），逐个发射 Tauri 事件
//! - 活跃会话指针在拿到流式句柄后由桌面设置（服务层不反向持有生命周期容器）
//! - `get_app_state`: 返回当前应用状态，前端据此决定显示哪个界面
//! - `check_privacy` / `confirm_privacy`: 委托服务层隐私确认流程
//! - 所有错误通过 ChatErrorPayload 格式返回，包含用户友好的标题和详情
//! - 不写业务逻辑，只做参数校验 + 委托调用 + 事件发射

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
                .map_err(|e| format!("无法自动创建新会话: {e}"))?,
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

// =========================================================
// get_app_state — 应用状态查询
// =========================================================

/// 获取当前应用状态。
///
/// 返回:
/// - 状态字符串（来自 AppState::as_str，snake_case）：
/// "needs_setup" | "downloading_model" | "indexing" | "ready" | "degraded" | "fatal_error"
///
/// 说明:
/// - 前端在加载时调用此命令，根据返回值决定显示哪个页面
#[tauri::command]
pub async fn get_app_state(state: State<'_, DesktopState>) -> Result<String, String> {
    let app_state = state.engine.current_state();
    tracing::debug!(state = %app_state.as_str(), "get_app_state 查询");
    Ok(app_state.as_str().to_string())
}

// =========================================================
// save_current_session — 手动保存并关闭当前活跃 session
// =========================================================

/// 手动保存当前对话：关闭活跃 session → 生成 L1 摘要 → 不清屏，下次消息自动创建新 session。
///
/// 参数:
/// - `persona_uid`: 当前对话人格 UID（仅用于诊断日志；L1 摘要归属以会话记录为准）。
///
/// 对齐 Python `POST /save` 路由和 `SessionManager.force_close_current_session`。
///
/// 返回:
/// - `{ status: "ok" | "no_active_session", l1_generated: bool }` JSON 字符串。
#[tauri::command]
pub async fn save_current_session(
    state: State<'_, DesktopState>,
    persona_uid: Option<String>,
) -> Result<String, String> {
    let Some(sid) = state.lifecycle.active_session_id() else {
        tracing::info!("save_current_session: 无活跃 session");
        return Ok(serde_json::json!({
            "status": "no_active_session",
            "l1_generated": false
        })
        .to_string());
    };

    // 诊断：仅取条数，避免全量加载会话消息（计数失败按 0 处理）
    let msg_count = state.engine.count_session_messages(sid).await;
    tracing::info!(%sid, msg_count, ?persona_uid, "save_current_session 开始");

    // 手动保存：抢占式关闭活跃会话并触发封存链路
    state
        .lifecycle
        .close_active_session()
        .await
        .map_err(|e| format!("保存对话失败: {}", e))?;

    // 验证 L1 是否生成
    let l1_entries = state
        .engine
        .memory_l1_by_session(sid)
        .await
        .map_err(|e| format!("查询 L1 状态失败: {}", e))?;

    let l1_generated = !l1_entries.is_empty();

    if l1_generated {
        let l1 = &l1_entries[0];
        // 隐私红线：不记 L1 摘要全文，仅记 id 与字符长度供诊断
        tracing::info!(
            %sid,
            l1_id = %l1.id,
            summary_len = l1.summary.chars().count(),
            persona_uid = ?l1.persona_uid,
            l1_count = l1_entries.len(),
            "L1 摘要已确认存在"
        );
    } else {
        tracing::error!(
            %sid,
            msg_count,
            "❌ L1 摘要生成失败！session 有 {msg_count} 条消息但 memory_l1 表为空。LLM 调用可能失败。"
        );
    }

    Ok(serde_json::json!({
        "status": "ok",
        "l1_generated": l1_generated,
        "session_id": sid.to_string(),
        "l1_count": l1_entries.len(),
        "msg_count": msg_count
    })
    .to_string())
}

// =========================================================
// generate_l1 — 手动重试 L1 摘要生成
// =========================================================

/// 为指定已关闭 session 重新生成 L1 摘要（手动重试）。
///
/// 使用场景:
/// - save_current_session 中 L1 生成失败后，用户可手动重试。
/// - LLM 服务恢复后补救之前失败保存的会话。
///
/// 参数:
/// - `session_id`: 目标 session UUID 字符串。
/// - `persona_uid`: 可选的人格标识。
///
/// 返回:
/// - `{ l1_generated: bool, summary?: string }` JSON。
#[tauri::command]
pub async fn generate_l1(
    state: State<'_, DesktopState>,
    session_id: String,
    persona_uid: Option<String>,
) -> Result<String, String> {
    let sid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("session_id 格式无效: {e}"))?;

    let result = state
        .engine
        .regenerate_l1(sid, persona_uid.as_deref(), None, None)
        .await
        .map_err(|e| format!("L1 生成失败: {e}"))?;

    match result {
        Some(l1) => {
            tracing::info!(%sid, l1_id = %l1.id, "L1 手动重试成功");
            Ok(serde_json::json!({
                "l1_generated": true,
                "summary": l1.summary,
                "session_id": sid.to_string()
            })
            .to_string())
        }
        None => {
            tracing::warn!(%sid, "L1 手动重试：session 无消息");
            Ok(serde_json::json!({
                "l1_generated": false,
                "reason": "no_messages",
                "session_id": sid.to_string()
            })
            .to_string())
        }
    }
}

// =========================================================
// check_privacy — 隐私确认状态检查
// =========================================================

/// 检查当前后端的隐私确认状态。
///
/// 返回:
/// - JSON 对象，包含 status 字段：
/// - `"NotNeeded"`: 本地服务，无需确认
/// - `"Confirmed"`: 已确认（含 persistent 标志和确认时间）
/// - `"NeedsConfirmation"`: 需要用户确认（含 provider_name 和 base_url）
#[tauri::command]
pub async fn check_privacy(state: State<'_, DesktopState>) -> Result<serde_json::Value, String> {
    let status = state
        .engine
        .check_privacy()
        .await
        .map_err(|e| format!("检查隐私状态失败: {}", e))?;

    let json = match &status {
        ramaria_service::PrivacyStatus::NotNeeded => {
            serde_json::json!({ "status": "NotNeeded" })
        }
        ramaria_service::PrivacyStatus::Confirmed {
            persistent,
            confirmed_at,
        } => {
            serde_json::json!({
                "status": "Confirmed",
                "persistent": persistent,
                "confirmed_at": confirmed_at,
            })
        }
        ramaria_service::PrivacyStatus::NeedsConfirmation {
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
        .engine
        .confirm_privacy(persistent)
        .await
        .map_err(|e| format!("记录隐私确认失败: {}", e))?;

    tracing::info!(persistent = persistent, "隐私确认已记录");
    Ok("ok".to_string())
}
