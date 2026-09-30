//! crates/ramaria-desktop/src/commands/dialog.rs - 系统文件对话框 Tauri Commands
//!
//! 设计特点:
//! - `save_file_dialog`: 调系统保存对话框选取导出路径（设置页导出流程调用）
//! - 取消选择返回 None（前端按"用户取消"处理，回退自带输入方式）
//! - 复用已注册的 tauri-plugin-dialog，不新增依赖；前端调用形态保持不变
//! - 日志只记布尔结果，不记所选路径（隐私口径：路径不进日志）

use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

// =========================================================
// save_file_dialog — 系统保存对话框
// =========================================================

/// 弹出系统保存对话框，返回用户选择的保存路径。
///
/// 参数:
/// - `default_name`: 默认文件名（含扩展名；用于预填文件名与推断过滤器扩展名）。
/// - `filter_name`: 文件类型过滤器显示名（可选）。
///
/// 返回:
/// - `Ok(Some(path))`: 用户确认后的保存路径；
/// - `Ok(None)`: 用户取消选择。
///
/// 说明:
/// - 仅负责选取路径，不写文件；写出由调用方（前端经导出命令）完成；
/// - 默认文件名可推断出扩展名时附带类型过滤器（如 `.json`），否则不加过滤器；
/// - 参数键与前端既有调用形态一致（snake_case），故显式声明 rename 规则。
#[tauri::command(rename_all = "snake_case")]
pub async fn save_file_dialog(
    app_handle: AppHandle,
    default_name: Option<String>,
    filter_name: Option<String>,
) -> Result<Option<String>, String> {
    let dialog = app_handle.dialog().file();

    // 预填默认文件名（空值视为未提供）
    let dialog = match default_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        Some(name) => dialog.set_file_name(name),
        None => dialog,
    };

    // 依据默认文件名推断扩展名，附带类型过滤器
    let extension: Option<String> = default_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .and_then(|n| {
            std::path::Path::new(n)
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
        });
    let dialog = match (
        filter_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty()),
        &extension,
    ) {
        (Some(name), Some(ext)) => dialog.add_filter(name, &[ext.as_str()]),
        _ => dialog,
    };

    let picked = dialog.blocking_save_file();
    let result = picked.map(|path| path.to_string());

    tracing::info!(picked = result.is_some(), "保存对话框已关闭");
    Ok(result)
}
