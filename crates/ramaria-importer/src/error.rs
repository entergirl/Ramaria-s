//! crates/ramaria-importer/src/error.rs - 导入器专用错误类型
//!
//! 设计特点:
//! - 所有导入相关错误统一转换为 `RamariaError`，保持与 core 层一致
//! - 提供便捷构造器，减少上层重复的错误包装样板代码
//! - 导入器内部不使用独立的错误枚举，避免在 crate 边界引入额外错误类型

use ramaria_core::error::RamariaError;

/// 创建文件不存在错误。
pub fn file_not_found(path: &str) -> RamariaError {
    RamariaError::io(
        format!("文件不存在: {path}"),
        Some(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("文件不存在: {path}"),
        )),
    )
}

/// 创建文件格式不匹配错误。
pub fn format_mismatch(expected: &str, detail: &str) -> RamariaError {
    RamariaError::validation(format!("文件格式不匹配：期望 {expected}。{detail}"))
}

/// 创建文件读取错误。
pub fn read_error(path: &str, source: std::io::Error) -> RamariaError {
    RamariaError::io(format!("读取文件失败: {path}"), Some(source))
}

/// 创建 JSON 解析错误。
pub fn json_parse_error(path: &str, detail: &str) -> RamariaError {
    RamariaError::serialization(format!("解析 JSON 文件失败: {path} - {detail}"))
}

/// 创建编码检测错误。
pub fn encoding_error(path: &str, encodings: &[&str], last_error: &str) -> RamariaError {
    RamariaError::io(
        format!("无法以任何编码读取文件 {path}，已尝试: {encodings:?}，最后错误: {last_error}"),
        None,
    )
}

/// 创建导入写入错误。
pub fn import_write_error(detail: &str) -> RamariaError {
    RamariaError::storage(format!("导入写入失败: {detail}"))
}

/// 创建 persona 找不到的错误。
pub fn persona_not_found(uid: &str) -> RamariaError {
    RamariaError::validation(format!("Persona 不存在: {uid}，请先创建该角色"))
}
