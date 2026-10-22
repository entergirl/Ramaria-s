//! crates/ramaria-desktop/src/commands/chat/session.rs - Ramaria 会话关闭与摘要命令
//!
//! 设计特点:
//! - `save_current_session`: 手动保存 = 关闭活跃会话 → 触发封存链路 → 不清屏
//! - `generate_l1`: 已关闭会话的 L1 摘要手动重试，供 LLM 恢复后补救
//! - 摘要确认只取条数与字符长度做诊断，不记摘要全文

use crate::DesktopState;
use tauri::State;

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
        .map_err(|e| crate::commands::service_error_message(&e, "保存对话失败"))?;

    // 验证 L1 是否生成
    let l1_entries = state
        .engine
        .memory_l1_by_session(sid)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询 L1 状态失败"))?;

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
        .map_err(|e| crate::commands::service_error_message(&e, "L1 生成失败"))?;

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
