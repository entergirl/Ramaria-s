//! crates/ramaria-eval/src/sandbox/root.rs - Ramaria 评测沙盒目录准备
//!
//! 设计特点:
//! - 每次运行先清空再重建：旧库、旧配置与旧人格文件都不会跨次残留
//! - 只删除调用方给定的根目录，不向上推断、不触碰用户目录
//! - 失败返回带路径的 Io 错误，便于定位权限或占用问题

use std::path::Path;

use ramaria_core::error::{RamariaError, RamariaResult};

/// 准备沙盒根目录：已存在则清空，随后重建为空目录。
///
/// 参数:
/// - `root`: 沙盒根目录。
///
/// 返回:
/// - 成功时目录存在且为空；
/// - 删除或创建失败返回带路径的 `Io` 错误。
pub fn prepare(root: &Path) -> RamariaResult<()> {
    if root.exists() {
        std::fs::remove_dir_all(root).map_err(|error| {
            RamariaError::io(format!("清理沙盒目录失败: {}", root.display()), Some(error))
        })?;
    }
    std::fs::create_dir_all(root).map_err(|error| {
        RamariaError::io(format!("创建沙盒目录失败: {}", root.display()), Some(error))
    })?;
    Ok(())
}
