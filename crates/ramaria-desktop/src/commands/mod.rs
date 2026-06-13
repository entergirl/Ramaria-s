//! rust/crates/ramaria-desktop/src/commands/mod.rs - Tauri Commands 模块入口
//!
//! 设计特点:
//! - 聚合所有 Tauri Command 子模块
//! - 提供统一的命令注册函数 `all_commands()`
//! - 每个子模块只做参数转换 + 调用 ramaria_app，不写业务逻辑

pub mod chat;
pub mod config;
pub mod export;
pub mod index_cmd;
pub mod memory;
pub mod session;
pub mod setup;

/// 返回所有 Tauri 命令处理函数的列表，用于 `tauri::generate_handler![]`。
///
/// 用法:
/// ```ignore
/// .invoke_handler(tauri::generate_handler![
///     commands::chat::send_message,
///     commands::chat::get_app_state,
///     // ...
/// ])
/// ```
/// 注意：由于各个命令函数分布在不同的子模块中，注册时需要在 lib.rs 中显式列出。
/// 此函数的存在是为了文档化和代码审查便利性，实际注册使用宏。
#[allow(dead_code)]
pub fn command_names() -> Vec<&'static str> {
    vec![
        // chat
        "send_message",
        "save_current_session",
        "get_app_state",
        "check_privacy",
        "confirm_privacy",
        // setup
        "run_setup",
        "get_setup_status",
        "refresh_setup_state",
        // session
        "list_sessions",
        "get_session",
        "delete_session",
        "create_session",
        // memory
        "get_personas",
        "get_l1_memories",
        "get_l2_events",
        "get_l3_traits",
        // config
        "get_backend_config",
        "update_backend_config",
        "get_settings",
        "update_setting",
        // export
        "export_sessions_json",
        "export_sessions_markdown",
        // index
        "rebuild_index",
    ]
}
