//! rust/crates/ramaria-desktop/src/notification.rs - 桌面通知模块
//!
//! 设计特点:
//! - 封装 tauri-plugin-notification 的桌面通知功能
//! - 支持发送标题+正文的简单通知
//! - Phase 5 骨架，通知插件在启用前端通知功能时激活
//!
//! 当前状态: 占位模块。通知能力需在 tauri.conf.json capabilities 中启用
//!           notification:default 权限，并在 Cargo.toml 中添加 tauri-plugin-notification。

use tauri::AppHandle;

/// 发送桌面通知（骨架）。
///
/// 参数:
/// - `title`: 通知标题
/// - `body`: 通知正文
///
/// 说明:
/// - 当前为占位实现，仅记录日志
/// - 后续启用 tauri-plugin-notification 后实现真实通知
#[allow(dead_code)]
pub fn send_notification(app_handle: &AppHandle, title: &str, body: &str) {
    tracing::info!(title = %title, body = %body, "桌面通知（占位）");
    // TODO: Phase 5 后续批次接入 tauri-plugin-notification
    // 示例代码:
    // use tauri_plugin_notification::NotificationExt;
    // app_handle
    //     .notification()
    //     .builder()
    //     .title(title)
    //     .body(body)
    //     .show()
    //     .ok();
    let _ = app_handle;
    let _ = title;
    let _ = body;
}
