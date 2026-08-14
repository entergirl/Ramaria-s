//! crates/ramaria-desktop/src/main.rs - Windows 桌面应用入口点
//!
//! 设计特点:
//! - 在 release 模式下隐藏控制台窗口（windows_subsystem = "windows"）
//! - 委托 lib::run 启动 Tauri 应用
//! - 不包含任何初始化逻辑

// Windows 子系统配置：release 构建时不显示控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    ramaria_desktop_lib::run();
}
