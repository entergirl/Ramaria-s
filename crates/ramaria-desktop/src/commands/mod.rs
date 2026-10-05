//! crates/ramaria-desktop/src/commands/mod.rs - Tauri Commands 模块入口
//!
//! 设计特点:
//! - 聚合所有 Tauri Command 子模块
//! - 每个子模块只做参数转换 + 委托服务层用例，不写业务逻辑
//! - 提供服务层错误到前端文案的统一转换（委托服务层入口映射：业务校验原文直出，技术错误附场景前缀）

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
pub mod memory_view;
pub mod persona;
pub mod proactive_cmd;
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
/// - 文案口径由服务层 [`ramaria_service::entry_error_message`] 唯一提供：
///   业务校验 / 隐私类错误原文直出；其余类别 `{场景}: {类别标题}: {原因}`。
pub(crate) fn service_error_message(
    err: &ramaria_core::error::RamariaError,
    context: &str,
) -> String {
    ramaria_service::entry_error_message(err, context)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::error::RamariaError;

    /// 业务校验错误：原文直出（不叠加场景前缀）。
    #[test]
    fn validation_error_message_is_verbatim() {
        let err = RamariaError::validation("会话不存在: abc");
        assert_eq!(
            service_error_message(&err, "查询会话失败"),
            "会话不存在: abc"
        );
    }

    /// 技术错误：场景 + 中文类别标题 + 原因（不复现英文类别串）。
    #[test]
    fn technical_error_message_has_scene_and_title() {
        let err = RamariaError::storage("磁盘只读");
        let message = service_error_message(&err, "查询记忆失败");
        assert_eq!(message, "查询记忆失败: 数据库错误: 磁盘只读");
        assert!(!message.contains("storage error"), "{message}");
    }
}
