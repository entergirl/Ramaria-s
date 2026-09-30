//! crates/ramaria-desktop/src/commands/mod.rs - Tauri Commands 模块入口
//!
//! 设计特点:
//! - 聚合所有 Tauri Command 子模块
//! - 每个子模块只做参数转换 + 委托服务层用例，不写业务逻辑
//! - 提供服务层错误到前端文案的统一转换（业务校验原文直出，技术错误附场景前缀）

pub mod chat;
pub mod config;
pub mod diagnostics;
pub mod dialog;
pub mod evaluation;
pub mod export;
pub mod import_cmd;
pub mod index_cmd;
pub mod keywords;
pub mod mcp;
pub mod memory;
pub mod persona;
pub mod rules;
pub mod session;
pub mod setup;
pub mod style;

// =========================================================
// 服务层错误文案转换
// =========================================================

/// 将服务层错误转换为面向前端的用户可读文案。
///
/// 用法:
/// - 各命令的 `map_err` 中调用，替换直接对错误做 `format!` 的写法。
///
/// 参数:
/// - `err`: 服务层返回的统一错误。
/// - `context`: 调用场景描述（如 `"查询记忆失败"`），仅用于技术错误的场景前缀。
///
/// 返回:
/// - 用户可读的中文文案。
///
/// 说明:
/// - 业务校验错误（`validation`）取上下文原文：其文案已是面向用户的完整描述，
///   不再叠加英文分类前缀；
/// - 其他错误保留结构化展示并附场景前缀（如 `查询记忆失败: storage error: …`）。
pub(crate) fn service_error_message(
    err: &ramaria_core::error::RamariaError,
    context: &str,
) -> String {
    if err.category() == "validation" {
        err.context().to_string()
    } else {
        format!("{context}: {err}")
    }
}
