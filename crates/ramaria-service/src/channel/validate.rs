//! crates/ramaria-service/src/channel/validate.rs - Ramaria 社交通道入站与绑定参数校验
//!
//! 设计特点:
//! - 单一校验口径：绑定用例与入站写入共用同一份 trim + 非空判定，错误文案不漂移
//! - 纯函数：不访问存储、不产生日志，返回值已 trim
//! - 错误为业务校验错误（中文文案），由调用方决定是否继续

use ramaria_core::error::{RamariaError, RamariaResult};

/// 校验必填文本（trim 后非空）。
///
/// 参数:
/// - `value`: 原始输入（两侧空白容忍）。
/// - `message`: 校验失败时的业务文案。
///
/// 返回:
/// - 成功返回 trim 后的切片；失败返回校验错误。
pub(crate) fn required<'a>(value: &'a str, message: &str) -> RamariaResult<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RamariaError::validation(message));
    }
    Ok(trimmed)
}

/// 校验通道标识（trim 后非空）。
pub(crate) fn channel(value: &str) -> RamariaResult<&str> {
    required(value, "通道标识不能为空")
}

/// 校验会话外部标识（trim 后非空）。
pub(crate) fn external_ref(value: &str) -> RamariaResult<&str> {
    required(value, "会话外部标识不能为空")
}
