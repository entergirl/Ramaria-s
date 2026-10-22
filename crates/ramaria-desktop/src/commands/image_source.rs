//! crates/ramaria-desktop/src/commands/image_source.rs - 发图目录 Tauri Commands
//!
//! 设计特点:
//! - `get_image_source_dir`: 返回生效发图目录（默认目录首次读取时创建）
//! - `set_image_source_dir`: 设置自定义目录；`None` = 恢复默认并删除设置键
//! - 目录校验与默认目录解析全部委托服务层（桌面只做参数与文案转换）
//! - 日志只记"是否自定义"，不记绝对路径

use crate::DesktopState;
use tauri::State;

// =========================================================
// get_image_source_dir — 读取生效发图目录
// =========================================================

/// 读取生效的发图目录。
///
/// 返回:
/// - 目录绝对路径（默认目录 `{data_dir}/outgoing`，缺失时创建）。
#[tauri::command]
pub async fn get_image_source_dir(state: State<'_, DesktopState>) -> Result<String, String> {
    let dir = state
        .engine
        .image_source_dir()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "读取发图目录失败"))?;
    Ok(dir.to_string_lossy().to_string())
}

// =========================================================
// set_image_source_dir — 设置发图目录
// =========================================================

/// 设置发图目录（`None` / 空白 = 恢复默认）。
///
/// 参数:
/// - `path`: 自定义目录路径；`None` 表示恢复默认并删除设置键。
///
/// 返回:
/// - 生效目录的绝对路径。
///
/// 错误:
/// - 目录不存在 / 不是目录 / 是符号链接 / 位于系统目录或盘符根时返回校验错误文案。
#[tauri::command]
pub async fn set_image_source_dir(
    state: State<'_, DesktopState>,
    path: Option<String>,
) -> Result<String, String> {
    let custom = path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let dir = state
        .engine
        .set_image_source_dir(custom)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "设置发图目录失败"))?;

    tracing::info!(custom = custom.is_some(), "发图目录设置已更新");
    Ok(dir.to_string_lossy().to_string())
}
