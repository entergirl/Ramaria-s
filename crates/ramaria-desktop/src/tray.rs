//! rust/crates/ramaria-desktop/src/tray.rs - 系统托盘模块
//!
//! 设计特点:
//! - 基于 Tauri 2 tray-icon feature，注册系统托盘图标和右键菜单
//! - 托盘图标资源嵌入编译产物（icons/icon.ico），零外部依赖
//! - 右键菜单：显示主窗口 / 退出应用
//! - 左键点击托盘图标：切换主窗口显示/隐藏
//! - 窗口关闭按钮行为：拦截 CloseRequested → 通过事件通知前端弹窗确认
//!   （前端提供「最小化到托盘」和「退出 Ramaria」两个选项）
//! - 托盘图标 tooltip 显示 "Ramaria - 个人AI陪伴记忆系统"
//! - 错误处理：托盘创建失败记录错误日志但不阻断应用启动
//!
//! Tauri 2 API 参考:
//! - tray::TrayIconBuilder: 构建托盘图标
//! - menu::MenuBuilder / MenuItemBuilder: 构建右键菜单
//! - TrayIconEvent: 左键点击事件（toggle 窗口）

use tauri::{
    AppHandle, Emitter, Manager, Runtime,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

// =========================================================
// 菜单项 ID 常量
// =========================================================

/// 托盘菜单项：显示/隐藏主窗口
const MENU_ID_SHOW: &str = "tray_show";
/// 托盘菜单项：退出应用
const MENU_ID_QUIT: &str = "tray_quit";

// =========================================================
// 托盘初始化
// =========================================================

/// 初始化系统托盘。
///
/// 流程:
/// 1. 嵌入图标资源（icons/icon.ico → bytes）
/// 2. 构建右键菜单（显示窗口 + 退出）
/// 3. 创建 TrayIconBuilder 并注册事件
/// 4. 拦截主窗口关闭事件（CloseRequested → hide 而非 close）
///
/// 参数:
/// - `app_handle`: Tauri AppHandle，用于获取窗口和注册事件
///
/// 返回:
/// - `Ok(())` 初始化成功
/// - `Err(String)` 初始化失败（图标加载、菜单构建等错误）
pub fn setup_tray<R: Runtime>(app_handle: &AppHandle<R>) -> Result<(), Box<dyn std::error::Error>> {
    // ---- 创建托盘图标（32×32 RGBA 粉色 Brand 色方块） ----
    // 说明:
    // - Tauri 2 Image::from_bytes 需要 image-ico/image-png feature（可能不可用）
    // - 改为程序化生成 RGBA 像素：一个 32×32 的纯色方块
    // - 颜色使用 Ramaria 品牌粉色 oklch(0.53 0.19 10) → sRGB #c44d5a
    // - 后续可以用 PNG/ICO 资源文件替换（icons/icon.ico 已存在）
    let width: u32 = 32;
    let height: u32 = 32;
    let rgba: [u8; 4] = [0xc4, 0x4d, 0x5a, 0xff]; // #c44d5a 完全不透明
    let pixel_count = (width * height) as usize;
    let mut pixels = Vec::with_capacity(pixel_count * 4);
    for _ in 0..pixel_count {
        pixels.extend_from_slice(&rgba);
    }
    let icon = tauri::image::Image::new(&pixels, width, height);

    // ---- 构建右键菜单 ----
    let show_item = MenuItemBuilder::with_id(MENU_ID_SHOW, "显示主窗口")
        .accelerator("Ctrl+Shift+R")
        .build(app_handle)?;

    let quit_item = MenuItemBuilder::with_id(MENU_ID_QUIT, "退出 Ramaria").build(app_handle)?;

    let menu = MenuBuilder::new(app_handle)
        .item(&show_item)
        .separator()
        .item(&quit_item)
        .build()?;

    // ---- 创建托盘图标 ----
    let _tray = TrayIconBuilder::new()
        .icon(icon)
        .tooltip("Ramaria - 个人AI陪伴记忆系统")
        .menu(&menu)
        .show_menu_on_left_click(false) // 左键不显示菜单，用事件处理 toggle
        .on_menu_event(move |app, event| {
            handle_menu_event(app, event.id().as_ref());
        })
        .on_tray_icon_event(|tray, event| {
            handle_tray_icon_event(tray, &event);
        })
        .build(app_handle)?;

    // ---- 拦截窗口关闭行为 ----
    // 点击关闭按钮 → 隐藏窗口到托盘，而非退出应用
    intercept_close_event(app_handle);

    tracing::info!("系统托盘初始化成功");
    Ok(())
}

// =========================================================
// 事件处理
// =========================================================

/// 处理托盘右键菜单事件。
///
/// - `MENU_ID_SHOW`: 切换主窗口显示/隐藏
/// - `MENU_ID_QUIT`: 退出整个应用（调用 app_handle.exit(0)）
fn handle_menu_event<R: Runtime>(app_handle: &AppHandle<R>, menu_id: &str) {
    match menu_id {
        MENU_ID_SHOW => {
            toggle_main_window(app_handle);
        }
        MENU_ID_QUIT => {
            tracing::info!("用户从托盘菜单选择退出");
            // 确保主窗口可见（某些平台退出前需要先显示窗口才能正确清理）
            if let Some(window) = app_handle.get_webview_window("main") {
                let _ = window.show();
            }
            app_handle.exit(0);
        }
        other => {
            tracing::warn!(menu_id = %other, "未识别的托盘菜单项");
        }
    }
}

/// 处理托盘图标左键点击事件。
///
/// 说明:
/// - 左键单击：切换主窗口显示/隐藏
/// - 不响应右键和双击事件（右键由菜单处理）
fn handle_tray_icon_event<R: Runtime>(tray: &tauri::tray::TrayIcon<R>, event: &TrayIconEvent) {
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => {
            // 左键松开时切换窗口
            let app_handle = tray.app_handle();
            toggle_main_window(app_handle);
        }
        _ => {
            // 忽略其他事件（右键保持菜单行为）
        }
    }
}

// =========================================================
// 窗口操作
// =========================================================

/// 切换主窗口显示/隐藏。
///
/// 说明:
/// - 如果窗口当前可见 → 隐藏到托盘
/// - 如果窗口当前不可见 → 显示并聚焦
/// - 窗口不存在时静默忽略（极少发生，但安全处理）
fn toggle_main_window<R: Runtime>(app_handle: &AppHandle<R>) {
    if let Some(window) = app_handle.get_webview_window("main") {
        match window.is_visible() {
            Ok(true) => {
                tracing::debug!("隐藏主窗口到托盘");
                if let Err(e) = window.hide() {
                    tracing::error!(error = %e, "隐藏窗口失败");
                }
            }
            Ok(false) => {
                tracing::debug!("从托盘恢复主窗口");
                if let Err(e) = window.show() {
                    tracing::error!(error = %e, "显示窗口失败");
                }
                if let Err(e) = window.set_focus() {
                    tracing::error!(error = %e, "聚焦窗口失败");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "查询窗口可见状态失败");
            }
        }
    } else {
        tracing::warn!("未找到 main 窗口，无法切换显示状态");
    }
}

/// 拦截主窗口关闭事件。
///
/// 说明:
/// - 用户点击窗口关闭按钮（×）时，默认行为是关闭窗口
/// - 我们拦截 CloseRequested 事件，改为发送 `close-requested` 事件给前端
/// - 前端弹窗让用户选择「最小化到托盘」或「退出 Ramaria」
/// - 前端选择后调用 `confirm_close_action` 命令执行对应操作
fn intercept_close_event<R: Runtime>(app_handle: &AppHandle<R>) {
    if let Some(window) = app_handle.get_webview_window("main") {
        let window_clone = window.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // 阻止默认关闭行为（窗口真正销毁）
                api.prevent_close();

                // ★ 发送事件给前端，触发确认弹窗
                // 不在此处直接 hide()——将选择权交给用户
                tracing::debug!("拦截关闭请求 → 通知前端弹窗确认");
                let payload = serde_json::json!({
                    "action": "close-requested"
                });
                if let Err(e) = window_clone.emit(crate::events::EVENT_CLOSE_REQUESTED, payload) {
                    tracing::error!(error = %e, "发送 close-requested 事件失败，回退为隐藏到托盘");
                    // 事件发送失败时，回退为旧行为：直接隐藏到托盘
                    if let Err(e2) = window_clone.hide() {
                        tracing::error!(error = %e2, "隐藏窗口失败（CloseRequested 回退）");
                    }
                }
            }
        });
    }
}

/// 执行关闭确认操作（由前端 `confirm_close_action` 命令调用）。
///
/// 参数:
/// - `action`: "minimize" → 最小化到托盘；"exit" → 退出应用
///
/// 说明:
/// - 前端弹窗后，根据用户选择调用此函数
/// - "minimize" 将窗口隐藏到托盘
/// - "exit" 确保窗口可见后退出整个应用（对齐托盘菜单退出行为）
pub fn handle_close_action<R: Runtime>(app_handle: &AppHandle<R>, action: &str) {
    match action {
        "minimize" => {
            tracing::info!("用户选择最小化到托盘");
            if let Some(window) = app_handle.get_webview_window("main") {
                if let Err(e) = window.hide() {
                    tracing::error!(error = %e, "隐藏窗口失败");
                }
            }
        }
        "exit" => {
            tracing::info!("用户选择退出 Ramaria");
            // 确保主窗口可见（某些平台退出前需要先显示窗口才能正确清理）
            if let Some(window) = app_handle.get_webview_window("main") {
                let _ = window.show();
            }
            app_handle.exit(0);
        }
        other => {
            tracing::warn!(action = %other, "未识别的关闭操作，回退为最小化到托盘");
            handle_close_action(app_handle, "minimize");
        }
    }
}

/// Tauri Command：前端确认关闭操作后调用。
///
/// 参数:
/// - `app_handle`: Tauri AppHandle
/// - `action`: "minimize" | "exit"
///
/// 用法（前端）:
///   TauriBridge.invoke('confirm_close_action', { action: 'minimize' });
///   TauriBridge.invoke('confirm_close_action', { action: 'exit' });
#[tauri::command]
#[tracing::instrument(skip(app_handle))]
pub fn confirm_close_action(app_handle: AppHandle, action: String) -> Result<String, String> {
    tracing::info!(%action, "前端确认关闭操作");
    handle_close_action(&app_handle, &action);
    Ok(action)
}

// =========================================================
// 公开 API（供其他模块调用）
// =========================================================

/// 判断主窗口当前是否可见。
///
/// 用途：notification.rs 据此决定是否发送桌面通知。
///       窗口可见时无需通知，窗口隐藏时才弹 toast。
///
/// 返回:
/// - `true`: 窗口当前可见（含最小化状态）
/// - `false`: 窗口隐藏或不存在
pub fn is_main_window_visible<R: Runtime>(app_handle: &AppHandle<R>) -> bool {
    app_handle
        .get_webview_window("main")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_id_constants_are_unique() {
        assert_ne!(MENU_ID_SHOW, MENU_ID_QUIT);
    }

    #[test]
    fn menu_id_show_is_non_empty() {
        assert!(!MENU_ID_SHOW.is_empty());
    }

    #[test]
    fn menu_id_quit_is_non_empty() {
        assert!(!MENU_ID_QUIT.is_empty());
    }
}
