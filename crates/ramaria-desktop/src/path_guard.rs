//! crates/ramaria-desktop/src/path_guard.rs - Ramaria 路径安全校验与日志脱敏模块
//!
//! 设计特点:
//! - 统一导入/导出/只读读取路径的安全校验入口
//! - `canonicalize()` 解析真实路径 + 白名单前缀校验 + 符号链接拒绝，三层防御
//! - 写路径（导入/导出）白名单限定在用户主目录下的安全区域（Documents/Downloads/Desktop 等）
//! - 只读路径（评估产物等）改按"允许根目录集合"校验：用户主目录白名单 ∪ 数据目录 ∪ 用户显式授权目录
//! - 专为 Windows 平台设计，使用 `%USERPROFILE%` 定位用户目录
//! - 校验失败的错误信息面向界面展示（可含被拒路径）；**日志侧一律改记
//!   "文件名 + 短哈希"标签**（`redact_path_label`），避免绝对路径随诊断包外发

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

    // 日志仅记脱敏标签：导入文件路径位于用户主目录下，绝对路径不得入日志
    tracing::debug!(
        file = %redact_path_label(&real_path),
        "导入路径校验通过"
    );
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
    // 日志仅记脱敏标签：导出目标路径位于用户主目录下，绝对路径不得入日志
    tracing::debug!(
        file = %redact_path_label(&canonical),
        "导出路径校验通过"
    );
    Ok(canonical)
}

// =========================================================
// 只读读取路径校验（评估产物等）
// =========================================================
//
// 与写路径的区别:
// - 读取目标不限于"用户主目录白名单"：评估产物默认落在应用数据目录下，
//   也允许用户通过原生对话框显式选择任意目录（选择动作本身即授权）。
// - 因此改为"允许根目录集合"校验：集合 = 用户主目录白名单 ∪ 数据目录 ∪ 显式授权目录。
// - 目标路径必须 `canonicalize()` 后落在集合内（前缀匹配），防 `..`/符号链接越权。

/// 计算"只读读取"允许的根目录集合。
///
/// 参数:
/// - `extra`: 调用方追加的根目录（应用数据目录、用户在原生对话框中显式选择的目录）。
///
/// 返回:
/// - 去重排序后的真实路径集合（全部经 `canonicalize()`，不存在的项已跳过）。
///
/// 说明:
/// - 用户主目录不可用时仅返回 `extra`，不阻断调用（记录 warn，日志不含路径）；
/// - 返回空集合表示"无任何允许目录"，调用方应拒绝读取。
pub fn read_allowed_roots(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();

    match get_user_home_dir().and_then(|home| {
        home.canonicalize()
            .map_err(|_| "无法解析用户主目录路径，请检查系统配置".to_string())
    }) {
        Ok(home_real) => {
            for rel in ALLOWED_RELATIVE_DIRS {
                if let Ok(real) = home_real.join(rel).canonicalize() {
                    roots.push(real);
                }
            }
        }
        Err(reason) => {
            tracing::warn!(
                reason = %reason,
                "用户主目录不可用，只读白名单仅包含显式授权目录"
            );
        }
    }

    for dir in extra {
        match dir.canonicalize() {
            Ok(real) => roots.push(real),
            Err(_) => {
                // 仅记录脱敏标签：授权目录可能位于用户主目录下（如模型/产物目录）
                tracing::debug!(
                    dir = %redact_path_label(dir),
                    "授权目录不存在或不可解析，已跳过"
                );
            }
        }
    }

    roots.sort();
    roots.dedup();
    roots
}

/// 校验只读目录路径（列出目录内容场景）。
///
/// 参数:
/// - `dir_path`: 待校验目录路径（可来自前端传入或原生对话框选择结果）。
/// - `allowed_roots`: 允许的根目录集合（见 `read_allowed_roots`）。
///
/// 返回:
/// - `Ok(PathBuf)`: 规范化后的真实目录路径。
/// - `Err(String)`: 拒绝原因（面向界面展示，可安全写日志——其中的路径已被脱敏）。
///
/// 安全约束:
/// - 目录必须存在且为目录；
/// - 拒绝符号链接（防链接指向允许范围外的真实目录）；
/// - `canonicalize()` 后必须位于 `allowed_roots` 之一内。
pub fn validate_read_dir_path(
    dir_path: &str,
    allowed_roots: &[PathBuf],
) -> Result<PathBuf, String> {
    let path = Path::new(dir_path);

    if !path.exists() {
        return Err(format!("目录不存在，拒绝读取: {}", redact_path_label(path)));
    }
    if !path.is_dir() {
        return Err(format!(
            "路径不是目录，拒绝读取: {}",
            redact_path_label(path)
        ));
    }

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| format!("无法读取目录元信息，拒绝读取: {}", redact_path_label(path)))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "目录是符号链接，拒绝读取: {}",
            redact_path_label(path)
        ));
    }

    let real_path = path
        .canonicalize()
        .map_err(|_| format!("目录无法解析，拒绝读取: {}", redact_path_label(path)))?;

    ensure_in_allowed_roots(&real_path, allowed_roots, "读取目录")?;
    tracing::debug!(
        dir = %redact_path_label(&real_path),
        "只读目录校验通过"
    );
    Ok(real_path)
}

/// 校验只读文件路径（读取文件内容场景）。
///
/// 参数:
/// - `file_path`: 待校验文件路径。
/// - `allowed_roots`: 允许的根目录集合（见 `read_allowed_roots`）。
///
/// 返回:
/// - `Ok(PathBuf)`: 规范化后的真实文件路径。
/// - `Err(String)`: 拒绝原因（面向界面展示，其中的路径已被脱敏）。
///
/// 安全约束:
/// - 文件必须存在且为普通文件（非目录）；
/// - 拒绝符号链接（防链接指向允许范围外的真实文件）；
/// - `canonicalize()` 后必须位于 `allowed_roots` 之一内（同时覆盖 `..` 穿越场景）。
pub fn validate_read_file_path(
    file_path: &str,
    allowed_roots: &[PathBuf],
) -> Result<PathBuf, String> {
    let path = Path::new(file_path);

    if !path.exists() {
        return Err(format!("文件不存在，拒绝读取: {}", redact_path_label(path)));
    }
    if !path.is_file() {
        return Err(format!(
            "路径不是普通文件，拒绝读取: {}",
            redact_path_label(path)
        ));
    }

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| format!("无法读取文件元信息，拒绝读取: {}", redact_path_label(path)))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "文件是符号链接，拒绝读取: {}",
            redact_path_label(path)
        ));
    }

    let real_path = path
        .canonicalize()
        .map_err(|_| format!("文件无法解析，拒绝读取: {}", redact_path_label(path)))?;

    ensure_in_allowed_roots(&real_path, allowed_roots, "读取文件")?;
    tracing::debug!(
        file = %redact_path_label(&real_path),
        "只读文件校验通过"
    );
    Ok(real_path)
}

/// 断言真实路径位于任一允许根目录内。
///
/// 参数:
/// - `real_path`: 已 `canonicalize()` 的真实路径。
/// - `allowed_roots`: 允许的根目录集合（应同样为真实路径）。
/// - `operation`: 操作名（用于错误消息），如"读取文件"。
fn ensure_in_allowed_roots(
    real_path: &Path,
    allowed_roots: &[PathBuf],
    operation: &str,
) -> Result<(), String> {
    if allowed_roots.is_empty() {
        return Err(format!(
            "没有可用的允许目录，拒绝{operation}: {}",
            redact_path_label(real_path)
        ));
    }

    if allowed_roots.iter().any(|root| real_path.starts_with(root)) {
        return Ok(());
    }

    tracing::debug!(
        path = %redact_path_label(real_path),
        "路径不在允许读取范围内"
    );
    Err(format!(
        "路径不在允许读取范围内，拒绝{operation}: {}",
        redact_path_label(real_path)
    ))
}

// =========================================================
// 日志脱敏
// =========================================================
//
// 背景: 桌面日志（`{data_dir}/logs/ramaria.log`）会随诊断包外发，
// 绝对路径会暴露用户名与目录结构，用户文本（关键词/别名等）属个人内容。
// 因此日志侧只保留"文件名/长度 + 短哈希"标签，正文一律不入日志。

/// 生成路径脱敏标签：`<文件名>#<8 位十六进制哈希>`。
///
/// 说明:
/// - 哈希取 64 位 FNV-1a 的低 32 位（稳定、零依赖），输入为原始路径字符串；
/// - 保留可区分性：同一路径在日志中始终同一标签，便于跨日志关联排查；
/// - 路径无文件名时（如盘根）回退 `<unknown>`。
pub fn redact_path_label(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("<unknown>");
    format!("{name}#{:08x}", fnv1a32(path.to_string_lossy().as_bytes()))
}

/// 生成文本脱敏标签：`<N chars>#<8 位十六进制哈希>`。
///
/// 用途:
/// - 记录关键词/别名等用户文本时仅保留长度与哈希，正文不出现在日志中。
pub fn redact_text_label(text: &str) -> String {
    format!(
        "<{} chars>#{:08x}",
        text.chars().count(),
        fnv1a32(text.as_bytes())
    )
}

/// 64 位 FNV-1a 哈希取低 32 位（仅用于日志标签，不用于安全用途）。
fn fnv1a32(bytes: &[u8]) -> u32 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    (hash & 0xffff_ffff) as u32
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
mod tests;

#[cfg(test)]
mod privacy_audit_tests;
