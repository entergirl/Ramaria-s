//! crates/ramaria-desktop/src/commands/dialog.rs - 系统文件对话框 Tauri Commands
//!
//! 设计特点:
//! - `save_file_dialog`: 调系统保存对话框选取导出路径（设置页导出流程调用）
//! - `open_file_dialog`: 调系统打开对话框选取待发送文件（对话页发图键调用）
//! - 取消选择返回 None（前端按"用户取消"处理，回退自带输入方式）
//! - 复用已注册的 tauri-plugin-dialog，不新增依赖；前端调用形态保持不变
//! - 选择结果登记为会话级授权票据（选择动作即授权），供导入 / 导出校验放行
//! - 日志只记布尔结果与脱敏标签，不记所选路径（隐私口径：路径不进日志）

use std::path::Path;

use ramaria_core::lock::lock_recover;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::DesktopState;

/// 登记一条文件票据（打开文件对话框的选择结果）。
fn grant_file(state: &DesktopState, picked: &str) {
    let Ok(real) = std::fs::canonicalize(picked) else {
        return;
    };
    lock_recover(&state.dialog_grants, "desktop.dialog_grants").grant_file(real);
}

/// 登记一条目录票据（保存目标所在目录或目录对话框的选择结果）。
fn grant_parent_dir(state: &DesktopState, picked: &str) {
    let Some(parent) = Path::new(picked).parent() else {
        return;
    };
    let Ok(real) = parent.canonicalize() else {
        return;
    };
    lock_recover(&state.dialog_grants, "desktop.dialog_grants").grant_dir(real);
}

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
    state: State<'_, DesktopState>,
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
    if let Some(picked_str) = result.as_ref() {
        grant_parent_dir(&state, picked_str);
    }

    tracing::info!(picked = result.is_some(), "保存对话框已关闭");
    Ok(result)
}

// =========================================================
// open_file_dialog — 系统打开对话框
// =========================================================

/// 弹出系统打开对话框，返回用户选择的文件路径。
///
/// 参数:
/// - `initial_dir`: 初始目录（可选；仅作对话框定位提示，不构成路径授权）。
/// - `title`: 对话框标题（可选）。
/// - `filter_name`: 文件类型过滤器显示名（可选）。
/// - `extensions`: 允许的扩展名（可选；不含点号，空列表不加过滤器）。
///
/// 返回:
/// - `Ok(Some(path))`: 用户确认后的文件路径；
/// - `Ok(None)`: 用户取消选择。
///
/// 说明:
/// - 仅负责选取路径，不读取文件；发送前的目录校验由服务层承担；
/// - 初始目录不存在时对话框回退到系统默认位置；
/// - 与 `save_file_dialog` 一致，参数键为 snake_case（前端传 camelCase，Tauri 自动转换）。
#[tauri::command]
pub async fn open_file_dialog(
    app_handle: AppHandle,
    state: State<'_, DesktopState>,
    initial_dir: Option<String>,
    title: Option<String>,
    filter_name: Option<String>,
    extensions: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    let mut dialog = app_handle.dialog().file();

    if let Some(dir) = initial_dir
        .as_deref()
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
    {
        dialog = dialog.set_directory(std::path::PathBuf::from(dir));
    }

    if let Some(text) = title
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        dialog = dialog.set_title(text);
    }

    // 扩展名归一：去空白与点号，空项丢弃
    let exts: Vec<String> = extensions
        .unwrap_or_default()
        .into_iter()
        .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
        .collect();
    if !exts.is_empty() {
        let refs: Vec<&str> = exts.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(filter_name.as_deref().unwrap_or("文件"), &refs);
    }

    let picked = dialog.blocking_pick_file();
    let result = picked.map(|path| path.to_string());
    if let Some(picked_str) = result.as_ref() {
        grant_file(&state, picked_str);
    }

    tracing::info!(picked = result.is_some(), "打开对话框已关闭");
    Ok(result)
}

// =========================================================
// open_directory_dialog — 系统目录选择对话框
// =========================================================

/// 弹出系统目录选择对话框，返回用户选择的目录路径。
///
/// 参数:
/// - `initial_dir`: 初始目录（可选；仅作对话框定位提示，不构成路径授权）。
/// - `title`: 对话框标题（可选）。
///
/// 返回:
/// - `Ok(Some(path))`: 用户确认后的目录路径；
/// - `Ok(None)`: 用户取消选择。
///
/// 说明:
/// - 仅选取目录，不做目录特征或白名单校验；调用方（如"继续处理"）按业务口径校验；
/// - 日志只记布尔结果，不记所选路径（隐私口径：路径不进日志）。
#[tauri::command]
pub async fn open_directory_dialog(
    app_handle: AppHandle,
    state: State<'_, DesktopState>,
    initial_dir: Option<String>,
    title: Option<String>,
) -> Result<Option<String>, String> {
    let mut dialog = app_handle.dialog().file();

    if let Some(dir) = initial_dir
        .as_deref()
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
    {
        dialog = dialog.set_directory(std::path::PathBuf::from(dir));
    }

    if let Some(text) = title
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        dialog = dialog.set_title(text);
    }

    let picked = dialog.blocking_pick_folder();
    let result = picked.map(|path| path.to_string());
    if let Some(picked_str) = result.as_ref() {
        let Ok(real) = std::fs::canonicalize(picked_str) else {
            tracing::warn!("目录无法规范化，未登记授权");
            return Ok(result);
        };
        lock_recover(&state.dialog_grants, "desktop.dialog_grants").grant_dir(real);
    }

    tracing::info!(picked = result.is_some(), "目录对话框已关闭");
    Ok(result)
}
