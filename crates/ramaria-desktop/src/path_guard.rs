//! crates/ramaria-desktop/src/path_guard.rs - Ramaria 路径安全校验模块
//!
//! 设计特点:
//! - 统一导入/导出路径的安全校验入口，两个函数覆盖所有文件操作场景
//! - `canonicalize()` 解析真实路径 + 白名单前缀校验 + 符号链接拒绝，三层防御
//! - 白名单限定在用户主目录下的安全区域（Documents/Downloads/Desktop 等）
//! - 专为 Windows 平台设计，使用 `%USERPROFILE%` 定位用户目录
//! - 错误信息不暴露目录结构细节，防止信息泄露

use std::path::{Path, PathBuf};

// =========================================================
// 白名单常量
// =========================================================

/// 用户主目录下的授权相对子目录列表（相对于 `%USERPROFILE%`）。
///
/// 原则:
/// - 只允许用户数据目录（文档、下载、桌面、OneDrive 同步目录）
/// - 拒绝系统目录（Windows/Program Files/ProgramData 等）
/// - 拒绝其他用户的目录（Users/OtherUserName）
const ALLOWED_RELATIVE_DIRS: &[&str] = &[
    "Documents",
    "Downloads",
    "Desktop",
    "OneDrive\\Documents",
    "OneDrive\\Desktop",
    "OneDrive",
];

// =========================================================
// 导入路径校验
// =========================================================

/// 校验导入文件路径的安全性。
///
/// 用法:
/// - `file_path`: 用户选择的导入文件路径（来自 Tauri dialog 或 CLI 参数）
///
/// 返回:
/// - `Ok(PathBuf)`: 规范化后的安全绝对路径
/// - `Err(String)`: 拒绝原因（路径不存在、越权、符号链接等）
///
/// 安全约束:
/// - 文件必须存在且为普通文件（非目录）
/// - 路径经 `canonicalize()` 解析真实路径
/// - 解析后的路径必须在授权白名单目录内
/// - 拒绝符号链接（防止链接指向白名单外路径）
pub fn validate_import_file_path(file_path: &str) -> Result<PathBuf, String> {
    let path = Path::new(file_path);

    // Step 1: 基本存在性校验
    if !path.exists() {
        return Err(format!("文件不存在，拒绝访问: {}", file_path));
    }
    if !path.is_file() {
        return Err(format!("路径不是普通文件，拒绝访问: {}", file_path));
    }

    // Step 2: 拒绝符号链接（防止链接指向白名单外路径）
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| format!("无法读取文件元信息，拒绝访问: {}", file_path))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("路径是符号链接，拒绝访问: {}", file_path));
    }

    // Step 3: canonicalize 解析真实路径（消除 ../ 和符号链接解析后的路径）
    let real_path = path
        .canonicalize()
        .map_err(|_| format!("路径无法解析，拒绝访问: {}", file_path))?;

    // Step 4: 白名单前缀校验
    validate_path_in_allowed_zone(&real_path, "导入")?;

    tracing::debug!(input = %file_path, real = %real_path.display(), "导入路径校验通过");
    Ok(real_path)
}

// =========================================================
// 导出路径校验
// =========================================================

/// 校验导出目标路径的安全性。
///
/// 用法:
/// - `output_path`: 用户选择的导出目标路径（文件可能尚不存在）
///
/// 返回:
/// - `Ok(PathBuf)`: 规范化后的安全目标路径（含文件名）
/// - `Err(String)`: 拒绝原因（父目录不存在、越权等）
///
/// 安全约束:
/// - 文件可以尚不存在（Tauri dialog 新建文件场景）
/// - 但父目录必须存在且经过 `canonicalize()` 验证
/// - 父目录必须在授权白名单目录内
/// - 拒绝写入系统目录
pub fn validate_export_path(output_path: &str) -> Result<PathBuf, String> {
    let path = Path::new(output_path);

    // Step 1: 获取父目录（文件可能尚不存在，不能 canonicalize 文件本身）
    let parent = path
        .parent()
        .ok_or_else(|| format!("路径无效（无父目录），拒绝导出: {}", output_path))?;

    // Step 2: 父目录必须存在
    if !parent.exists() {
        return Err(format!("导出目录不存在，拒绝导出: {}", parent.display()));
    }
    if !parent.is_dir() {
        return Err(format!(
            "导出路径的父目录不是有效目录，拒绝导出: {}",
            parent.display()
        ));
    }

    // Step 3: 拒绝符号链接（防父目录被链接到系统目录）
    let parent_metadata = std::fs::symlink_metadata(parent)
        .map_err(|_| format!("无法读取目录元信息，拒绝导出: {}", parent.display()))?;
    if parent_metadata.file_type().is_symlink() {
        return Err(format!(
            "导出目录是符号链接，拒绝导出: {}",
            parent.display()
        ));
    }

    // Step 4: canonicalize 父目录为真实路径
    let real_parent = parent
        .canonicalize()
        .map_err(|_| format!("导出目录无法解析，拒绝导出: {}", parent.display()))?;

    // Step 5: 白名单前缀校验
    validate_path_in_allowed_zone(&real_parent, "导出")?;

    // Step 6: 提取文件名，构造规范化的完整路径
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("路径缺少文件名，拒绝导出: {}", output_path))?;

    let canonical = real_parent.join(file_name);
    tracing::debug!(
        input = %output_path,
        canonical = %canonical.display(),
        "导出路径校验通过"
    );
    Ok(canonical)
}

// =========================================================
// 内部校验函数
// =========================================================

/// 校验路径是否在用户主目录下的授权白名单内。
///
/// 参数:
/// - `real_path`: 已经 `canonicalize()` 解析的真实绝对路径
/// - `operation`: 操作名称（"导入"或"导出"），用于错误消息
///
/// 返回:
/// - `Ok(())`: 路径在白名单内
/// - `Err(String)`: 路径不在白名单内
///
/// 说明:
/// - 使用 `%USERPROFILE%` 定位 Windows 用户主目录
/// - 路径必须位于 `%USERPROFILE%` 下的白名单子目录中
/// - 拒绝系统目录（Windows, Program Files, ProgramData 等）
fn validate_path_in_allowed_zone(real_path: &Path, operation: &str) -> Result<(), String> {
    // 获取用户主目录
    let home_dir = get_user_home_dir()?;
    let home_canonical = home_dir
        .canonicalize()
        .map_err(|_| "无法解析用户主目录路径，请检查系统配置".to_string())?;

    // 检查路径是否在用户主目录下
    if !real_path.starts_with(&home_canonical) {
        return Err(format!(
            "路径不在用户主目录内，拒绝{}: {}",
            operation,
            real_path.display()
        ));
    }

    // 检查路径是否在白名单子目录中
    // 允许路径直接等于主目录（便于选择整个 Documents 等场景）
    if real_path == home_canonical {
        return Ok(());
    }

    // 获取相对于主目录的路径前缀
    // 例如: real_path = C:\Users\Alice\Documents\chat.json
    //       relative = Documents\chat.json
    let relative = real_path
        .strip_prefix(&home_canonical)
        .map_err(|_| "内部错误: 路径前缀解析失败".to_string())?;

    // 取路径的第一级子目录名
    // 例如 Documents\chat.json → "Documents"
    let top_dir = relative
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_default();

    // 检查第一级子目录是否在白名单中
    let is_allowed = ALLOWED_RELATIVE_DIRS
        .iter()
        .any(|allowed| top_dir.eq_ignore_ascii_case(allowed));

    if !is_allowed {
        return Err(format!(
            "路径不在授权目录内，拒绝{}（允许: Documents/Downloads/Desktop）: {}",
            operation,
            real_path.display()
        ));
    }

    Ok(())
}

/// 获取用户主目录路径。
///
/// 返回:
/// - `Ok(PathBuf)`: 用户主目录的绝对路径
/// - `Err(String)`: 无法确定用户主目录
///
/// 说明:
/// - Windows 上优先使用 `%USERPROFILE%` 环境变量
/// - 回退使用 `dirs` crate 逻辑（通过 std::env 通用探测）
fn get_user_home_dir() -> Result<PathBuf, String> {
    // Windows: 使用 USERPROFILE 环境变量
    #[cfg(target_os = "windows")]
    {
        if let Ok(home) = std::env::var("USERPROFILE") {
            return Ok(PathBuf::from(home));
        }
        if let Ok(home) = std::env::var("HOMEDRIVE")
            .and_then(|d| std::env::var("HOMEPATH").map(|p| format!("{}{}", d, p)))
        {
            return Ok(PathBuf::from(home));
        }
    }

    // Unix/macOS: 使用 HOME 环境变量
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(home) = std::env::var("HOME") {
            return Ok(PathBuf::from(home));
        }
    }

    // 最终回退
    Err("无法确定用户主目录（未设置 USERPROFILE/HOME 环境变量）".to_string())
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    // ── 导入路径校验测试 ──

    #[test]
    fn test_validate_import_file_path_file_not_exists() {
        let result = validate_import_file_path(r"C:\This\Path\Does\Not\Exist.json");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("不存在") || err.contains("拒绝访问"));
    }

    #[test]
    fn test_validate_import_file_path_is_directory() {
        // 使用临时目录（但目录不是文件）
        let tmp = std::env::temp_dir();
        let result = validate_import_file_path(tmp.to_str().unwrap());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("不是普通文件") || err.contains("拒绝访问"));
    }

    #[test]
    fn test_validate_import_file_path_valid_temp_file() {
        // 在临时目录中创建有效文件（临时目录应白名单通过或失败清晰）
        let tmp = std::env::temp_dir();
        let test_file = tmp.join("__ramaria_test_import_valid.tmp");
        let mut f = File::create(&test_file).expect("创建临时文件失败");
        writeln!(f, "test content").ok();

        let result = validate_import_file_path(test_file.to_str().unwrap());

        // 清理
        let _ = std::fs::remove_file(&test_file);

        // temp 目录通常不在白名单内，预期失败但错误消息应清晰
        // 如果 temp 目录恰好是用户主目录下的子目录（极少），可能成功
        if result.is_err() {
            let err = result.unwrap_err();
            assert!(
                err.contains("授权") || err.contains("拒绝"),
                "错误消息应说明白名单限制: {}",
                err
            );
        }
    }

    #[test]
    fn test_validate_import_file_path_empty_path() {
        let result = validate_import_file_path("");
        assert!(result.is_err());
    }

    // ── 导出路径校验测试 ──

    #[test]
    fn test_validate_export_path_parent_not_exists() {
        let result = validate_export_path(r"C:\NonExistentDir\output.json");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("不存在") || err.contains("拒绝"),
            "错误消息应说明目录不存在: {}",
            err
        );
    }

    #[test]
    fn test_validate_export_path_no_filename() {
        // 纯目录路径无文件名
        let tmp = std::env::temp_dir();
        let result = validate_export_path(tmp.to_str().unwrap());
        // 纯目录在导出场景应被拒绝（缺少文件名）
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_export_path_system_dir() {
        // 系统目录应被拒绝
        let result = validate_export_path(r"C:\Windows\output.json");
        assert!(result.is_err());
    }

    // ── 用户主目录探测测试 ──

    #[test]
    fn test_get_user_home_dir_success() {
        let home = get_user_home_dir();
        assert!(home.is_ok(), "应能获取用户主目录: {:?}", home.err());
        let path = home.unwrap();
        assert!(path.is_absolute(), "主目录应为绝对路径");
        assert!(path.exists(), "主目录应存在");
    }
}
