//! crates/ramaria-desktop/src/commands/chat/privacy.rs - Ramaria 云端 provider 隐私确认命令
//!
//! 设计特点:
//! - `check_privacy`: 返回当前后端的隐私确认状态（三类状态 + 未知兜底）
//! - `confirm_privacy`: 记录用户确认结果（可选跨重启持久化）
//! - 状态映射在本模块完成，服务层只提供枚举

use crate::DesktopState;
use tauri::State;

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
        .map_err(|e| crate::commands::service_error_message(&e, "检查隐私状态失败"))?;

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
        .map_err(|e| crate::commands::service_error_message(&e, "记录隐私确认失败"))?;

    tracing::info!(persistent = persistent, "隐私确认已记录");
    Ok("ok".to_string())
}
