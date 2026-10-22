//! crates/ramaria-desktop/src/commands/chat.rs - 聊天与对话 Tauri Commands 模块入口
//!
//! 设计特点:
//! - 按职责分部：对话发送与流式事件桥 / 会话与摘要 / 隐私确认 / 本地会话消息
//! - 子模块命令在此重导出，命令注册路径与前端契约保持不变
//! - `get_app_state`: 返回当前应用状态，前端据此决定显示哪个界面
//! - 所有错误通过 ChatErrorPayload 格式返回，包含用户友好的标题和详情
//! - 不写业务逻辑，只做参数校验 + 委托调用 + 事件发射

mod local;
mod privacy;
mod send;
mod session;

pub use local::{recall_message, send_local_image};
pub use privacy::{check_privacy, confirm_privacy};
pub use send::send_message;
pub use session::{generate_l1, save_current_session};

// 命令宏与命令同路径导出：命令注册按「模块路径::命令名」解析命令与其宏
#[doc(hidden)]
pub use local::{
    __cmd__recall_message, __cmd__send_local_image, __tauri_command_name_recall_message,
    __tauri_command_name_send_local_image,
};
#[doc(hidden)]
pub use privacy::{
    __cmd__check_privacy, __cmd__confirm_privacy, __tauri_command_name_check_privacy,
    __tauri_command_name_confirm_privacy,
};
#[doc(hidden)]
pub use send::{__cmd__send_message, __tauri_command_name_send_message};
#[doc(hidden)]
pub use session::{
    __cmd__generate_l1, __cmd__save_current_session, __tauri_command_name_generate_l1,
    __tauri_command_name_save_current_session,
};

use crate::DesktopState;
use tauri::State;

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
