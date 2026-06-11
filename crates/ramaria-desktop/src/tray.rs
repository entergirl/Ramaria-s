//! rust/crates/ramaria-desktop/src/tray.rs - 系统托盘模块
//!
//! 设计特点:
//! - 注册系统托盘图标，支持常驻后台和快速呼出
//! - 左键点击切换主窗口显示/隐藏
//! - 托盘菜单：显示主窗口 / 退出
//!
//! 当前状态: Phase 5 占位。
//! 托盘图标需要实际的 .ico/.png 资源文件。
//! 后续批次中创建图标资源后启用完整托盘功能。

use tauri::{AppHandle, Manager, Runtime};

/// 初始化系统托盘（当前为占位实现）。
///
/// 说明:
/// - 由于缺少托盘图标资源文件，当前仅记录日志
/// - 完整实现将在图标资源就绪后启用
pub fn setup_tray<R: Runtime>(app_handle: &AppHandle<R>) -> tauri::Result<()> {
    // 隐藏而非关闭主窗口（点击关闭按钮时隐藏到托盘）
    if let Some(window) = app_handle.get_webview_window("main") {
        let window_clone = window.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // 关闭请求时隐藏窗口而非退出
                let _ = window_clone.hide();
                // 阻止默认关闭行为
                // Note: Tauri 2 中阻止关闭需通过 prevent_close API
            }
        });
    }

    tracing::info!("系统托盘功能已预留（图标资源待添加）");
    Ok(())
}
