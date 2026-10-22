//! crates/ramaria-desktop/src/commands/import_cmd/detect.rs - Ramaria 导入文件格式检测命令
//!
//! 设计特点:
//! - 先做路径安全校验，再委托服务层按文件内容判断格式（不看扩展名）
//! - 仅回答是否匹配 qq-chat-exporter v6.x JSON

use crate::DesktopState;
use tauri::State;

// =========================================================
// detect_qq_format — 检测文件是否为 QQ 聊天记录格式
// =========================================================

/// 检测文件是否为 QQ 聊天记录支持的格式。
///
/// 参数:
/// - `file_path`: 待检测的文件绝对路径。
///
/// 返回:
/// - `true`: 文件格式匹配 qq-chat-exporter v6.x JSON
/// - `false`: 格式不匹配，应提示用户选择正确的文件
///
/// 说明:
/// - 先检查文件存在性，再委托服务层做格式检测。
/// - 格式检测基于文件内容（首字节判断 JSON vs 文本）而非扩展名。
#[tauri::command]
#[tracing::instrument(skip(state, file_path))]
pub async fn detect_qq_format(
    state: State<'_, DesktopState>,
    file_path: String,
) -> Result<bool, String> {
    // 路径安全校验：三层防御（canonicalize + 白名单 + 符号链接拒绝）
    let real_path = crate::path_guard::validate_import_file_path(&file_path)?;

    state
        .engine
        .detect_qq_format(&real_path)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "格式检测失败"))
}
