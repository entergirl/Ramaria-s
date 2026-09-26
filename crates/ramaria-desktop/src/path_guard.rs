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
        if let Err(err) = result {
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

    // ── 日志脱敏标签测试 ──

    /// 创建测试用唯一临时目录（调用方负责删除）。
    fn temp_test_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ramaria-path-guard-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("创建测试临时目录失败");
        dir
    }

    #[test]
    fn redact_path_label_hides_directory_structure() {
        let label = redact_path_label(Path::new(r"C:\Users\Alice\Documents\chat.json"));
        assert!(label.starts_with("chat.json#"), "标签应保留文件名: {label}");
        assert_eq!(label.matches('#').count(), 1, "标签应恰含一个哈希分隔符");
        assert!(!label.contains('\\'), "标签不得含路径分隔符: {label}");
        assert!(!label.contains('/'), "标签不得含路径分隔符: {label}");
        assert!(!label.contains(':'), "标签不得含盘符: {label}");
        assert!(!label.contains("Alice"), "标签不得含用户名: {label}");
    }

    #[test]
    fn redact_path_label_is_stable_and_distinguishable() {
        let a = redact_path_label(Path::new(r"C:\Users\Alice\a.json"));
        let b = redact_path_label(Path::new(r"C:\Users\Alice\a.json"));
        let c = redact_path_label(Path::new(r"C:\Users\Bob\a.json"));
        assert_eq!(a, b, "同一路径应产生稳定标签（便于跨日志关联）");
        assert_ne!(a, c, "不同路径应可区分");

        // 无文件名形态（盘根）回退占位名，不 panic
        assert!(redact_path_label(Path::new(r"C:\")).starts_with("<unknown>#"));
    }

    #[test]
    fn redact_text_label_keeps_length_only() {
        let label = redact_text_label("秘密关键词");
        assert!(
            label.starts_with("<5 chars>#"),
            "文本标签应记录字符数: {label}"
        );
        assert!(!label.contains("秘密"), "文本标签不得含原文: {label}");
        assert_eq!(label.matches('#').count(), 1);
    }

    // ── 只读允许根目录集合测试 ──

    #[test]
    fn read_allowed_roots_includes_extra_and_skips_missing() {
        let base = temp_test_dir("roots");
        let real_base = base.canonicalize().expect("canonicalize base");

        let roots = read_allowed_roots(std::slice::from_ref(&base));
        assert!(roots.contains(&real_base), "追加目录应纳入允许根集合");
        assert!(roots.iter().all(|r| r.is_absolute()), "允许根应为绝对路径");

        let missing = base.join("not-exist-dir");
        let roots_with_missing = read_allowed_roots(std::slice::from_ref(&missing));
        assert!(
            !roots_with_missing.iter().any(|r| r == &missing),
            "不存在的授权目录不应产生无效根"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── 只读路径校验测试 ──

    #[test]
    fn validate_read_dir_path_accepts_dir_within_roots() {
        let base = temp_test_dir("dir-ok");
        let roots = vec![base.canonicalize().expect("canonicalize base")];

        let real = validate_read_dir_path(base.to_str().expect("utf8 路径"), &roots)
            .expect("允许根目录内的目录应通过校验");
        assert_eq!(real, roots[0]);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn validate_read_dir_path_rejects_outside_and_empty_roots() {
        let base = temp_test_dir("dir-deny");
        let outside = temp_test_dir("dir-outside");
        let roots = vec![base.canonicalize().expect("canonicalize base")];

        let err = validate_read_dir_path(outside.to_str().expect("utf8 路径"), &roots)
            .expect_err("允许根目录外的目录应被拒绝");
        assert!(
            err.contains("不在允许读取范围内"),
            "错误消息应说明范围限制: {err}"
        );

        let err_empty = validate_read_dir_path(base.to_str().expect("utf8 路径"), &[])
            .expect_err("无允许目录时应被拒绝");
        assert!(
            err_empty.contains("没有可用的允许目录"),
            "错误消息应说明无可用范围: {err_empty}"
        );

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn validate_read_file_path_accepts_file_and_rejects_escapes() {
        let base = temp_test_dir("file-ok");
        let outside = temp_test_dir("file-outside");
        let roots = vec![base.canonicalize().expect("canonicalize base")];

        let inside_file = base.join("result.json");
        std::fs::write(&inside_file, b"{}").expect("写入测试文件失败");
        assert!(
            validate_read_file_path(inside_file.to_str().expect("utf8 路径"), &roots).is_ok(),
            "允许根目录内的文件应通过校验"
        );

        // 形态 1：直接指向根目录外的文件
        let outside_file = outside.join("result.json");
        std::fs::write(&outside_file, b"{}").expect("写入测试文件失败");
        let err = validate_read_file_path(outside_file.to_str().expect("utf8 路径"), &roots)
            .expect_err("允许根目录外的文件应被拒绝");
        assert!(
            err.contains("不在允许读取范围内"),
            "错误消息应有范围语义: {err}"
        );

        // 形态 2：`..` 穿越（canonicalize 后落到根目录外）
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).expect("创建子目录失败");
        let escape = sub
            .join("..")
            .join("..")
            .join(outside.file_name().expect("临时目录名"))
            .join("result.json");
        let err_escape = validate_read_file_path(escape.to_str().expect("utf8 路径"), &roots)
            .expect_err("`..` 穿越到根目录外的文件应被拒绝");
        assert!(
            err_escape.contains("不在允许读取范围内"),
            "错误消息应有范围语义: {err_escape}"
        );

        // 形态 3：目录与不存在路径不得当作文件读取
        assert!(validate_read_file_path(sub.to_str().expect("utf8 路径"), &roots).is_err());
        assert!(
            validate_read_file_path(
                base.join("not-exist.json").to_str().expect("utf8 路径"),
                &roots
            )
            .is_err()
        );

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&outside);
    }
}

// =========================================================
// 日志隐私审计（静态源码扫描，仅测试构建参与）
// =========================================================

#[cfg(test)]
mod privacy_audit_tests {
    //! 断言桌面 crate 的日志不落绝对路径、裸路径变量与用户原文/密钥。
    //!
    //! 扫描对象: `src/**/*.rs`（递归），以源码文本静态断言，拦截回归；
    //! 不依赖运行期日志内容，故不写库、不起 Tauri 应用。
    //!
    //! 规则:
    //! - 规则 1：`tracing::{trace,debug,info,warn,error}!` 宏体内不得出现
    //!   `display()`（渲染路径）或裸路径变量（`%path` / `%file_path` / `%dir` …），
    //!   路径须经 `redact_path_label` 折叠为"文件名 + 短哈希"。
    //! - 规则 2：`#[tracing::instrument(...)]` 若未 `skip_all`，其函数签名中命中的
    //!   敏感参数（路径/密钥/用户原文语义）必须出现在 `skip(...)` 列表内，
    //!   否则该参数会以 INFO 级被 span 自动记录并随诊断包外发。

    use std::path::{Path, PathBuf};

    /// 日志宏体内禁止出现的裸路径变量（脱敏标签写法不含这些子串）。
    const FORBIDDEN_LOG_VARIABLES: &[&str] = &[
        "%path",
        "%file_path",
        "%real_path",
        "%dir",
        "%output",
        "%db_path",
        "%saved_path",
        "%path_trimmed",
        "%canonical",
        "%real_parent",
        "%config_path",
        "%data_dir",
        "%log_file_path",
    ];

    /// 命中即必须出现在 `instrument` skip 列表中的参数名。
    const MUST_SKIP_PARAMS: &[&str] = &[
        "path",
        "file_path",
        "output_path",
        "api_key",
        "base_url",
        "value",
        "config_json",
        "message",
        "request",
        "reaction",
        "avoid",
        "alias",
    ];

    #[test]
    fn tracing_macros_do_not_render_paths() {
        for (name, src) in audit_sources() {
            for body in log_macro_bodies(&src) {
                assert!(
                    !body.contains("display()"),
                    "{name}: 日志宏内出现 display()（绝对路径会随诊断包外发）: {body}"
                );
                for var in FORBIDDEN_LOG_VARIABLES {
                    assert!(
                        !contains_bare_variable(&body, var),
                        "{name}: 日志宏内出现裸路径变量 {var}（应改记 redact_path_label）: {body}"
                    );
                }
            }
        }
    }

    #[test]
    fn instrument_skips_sensitive_arguments() {
        for (name, src) in audit_sources() {
            for (skip_args, rest) in instrument_attributes(&src) {
                if skip_declares_param(&skip_args, "skip_all") {
                    continue;
                }
                for param in function_params(&rest) {
                    if MUST_SKIP_PARAMS.iter().any(|p| *p == param)
                        && !skip_declares_param(&skip_args, &param)
                    {
                        panic!(
                            "{name}: `#[tracing::instrument]` 未 skip 敏感参数 `{param}`\
                             （会以 INFO 级自动记录入日志，可能含路径/密钥/原文）"
                        );
                    }
                }
            }
        }
    }

    // ── 源码收集 ──

    /// 收集 `src/` 下全部 `.rs` 源文件，返回（相对路径, 内容）；行尾统一为 LF。
    fn audit_sources() -> Vec<(String, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs_files(&root, &mut files);

        assert!(!files.is_empty(), "未扫描到任何源文件: {root:?}");

        files
            .into_iter()
            .map(|path| {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("读取源码失败 {path:?}: {e}"));
                let label = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                (label, text.replace("\r\n", "\n"))
            })
            .collect()
    }

    /// 递归收集目录下全部 `.rs` 文件。
    fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    // ── 规则 1：日志宏体提取 ──

    /// 提取全部日志宏（trace/debug/info/warn/error）的括号体文本。
    fn log_macro_bodies(src: &str) -> Vec<String> {
        const MACROS: &[&str] = &["trace!", "debug!", "info!", "warn!", "error!"];

        let mut bodies = Vec::new();
        let mut cursor = 0usize;

        while let Some(rel) = src[cursor..].find("tracing::") {
            let after_prefix = cursor + rel + "tracing::".len();
            let rest = &src[after_prefix..];
            let Some(macro_len) = MACROS
                .iter()
                .find(|m| rest.starts_with(**m))
                .map(|m| m.len())
            else {
                cursor = after_prefix;
                continue;
            };

            let tail = &rest[macro_len..];
            let Some(open_rel) = tail.find('(') else {
                cursor = after_prefix;
                continue;
            };
            match matching_paren(tail, open_rel) {
                Some(close_rel) => {
                    bodies.push(tail[open_rel + 1..close_rel].to_string());
                    cursor = after_prefix + macro_len + close_rel + 1;
                }
                None => cursor = after_prefix + macro_len,
            }
        }

        bodies
    }

    /// 判断文本中是否出现"裸变量"引用。
    ///
    /// 说明:
    /// - `var` 形如 `%path`；命中后要求其后续字符不是标识符字符，
    ///   以免把 `%path_guard::redact_path_label(...)` 这类模块路径调用误判为裸变量。
    fn contains_bare_variable(text: &str, var: &str) -> bool {
        let mut cursor = 0usize;
        while let Some(rel) = text[cursor..].find(var) {
            let end = cursor + rel + var.len();
            let next_is_ident = text[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if !next_is_ident {
                return true;
            }
            cursor = end;
        }
        false
    }

    /// 返回 `s[open..]` 中与 `s[open]` 配对的右括号下标。
    fn matching_paren(s: &str, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (i, byte) in s.as_bytes().iter().enumerate().skip(open) {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    // ── 规则 2：instrument 属性提取 ──

    /// 提取全部 `#[tracing::instrument(...)]` 属性，返回（参数体, 属性之后的源码）。
    ///
    /// 说明:
    /// - 无参数形式 `#[tracing::instrument]` 返回空参数体（等价"记录所有参数"，
    ///   由后续敏感参数检查兜底）。
    fn instrument_attributes(src: &str) -> Vec<(String, String)> {
        const ATTR: &str = "#[tracing::instrument";

        let mut out = Vec::new();
        let mut cursor = 0usize;

        while let Some(rel) = src[cursor..].find(ATTR) {
            let start = cursor + rel;
            let after = &src[start + ATTR.len()..];
            let next_paren = after.find('(');
            let next_bracket = after.find(']');

            match (next_paren, next_bracket) {
                // 无参数形式：`]` 先出现
                (Some(open), Some(close_bracket)) if open > close_bracket => {
                    out.push((String::new(), after[close_bracket + 1..].to_string()));
                    cursor = start + ATTR.len() + close_bracket + 1;
                }
                // 有参数形式：按括号配对取参数体
                (Some(open), _) => {
                    let Some(close_rel) = matching_paren(after, open) else {
                        cursor = start + ATTR.len();
                        continue;
                    };
                    out.push((
                        after[open + 1..close_rel].to_string(),
                        after[close_rel + 1..].to_string(),
                    ));
                    cursor = start + ATTR.len() + close_rel + 1;
                }
                _ => {
                    cursor = start + ATTR.len();
                }
            }
        }

        out
    }

    /// 判断 skip 参数体中是否声明了指定参数名（按标识符整词比较，避免子串误判）。
    fn skip_declares_param(skip_args: &str, param: &str) -> bool {
        skip_args
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|token| token == param)
    }

    /// 从函数签名文本中提取参数名列表（`fn f(a: T, b: U)` → `["a", "b"]`）。
    fn function_params(rest: &str) -> Vec<String> {
        let Some(fn_rel) = rest.find("fn ") else {
            return Vec::new();
        };
        let after_fn = &rest[fn_rel..];
        let Some(open) = after_fn.find('(') else {
            return Vec::new();
        };
        let Some(close) = matching_paren(after_fn, open) else {
            return Vec::new();
        };
        let params = &after_fn[open + 1..close];

        // 按顶层逗号切分（跳过尖括号/括号/方括号内的逗号）
        let mut parts: Vec<String> = Vec::new();
        let mut depth = 0i32;
        let mut current = String::new();
        for ch in params.chars() {
            match ch {
                '<' | '(' | '[' => {
                    depth += 1;
                    current.push(ch);
                }
                '>' | ')' | ']' => {
                    depth -= 1;
                    current.push(ch);
                }
                ',' if depth == 0 => parts.push(std::mem::take(&mut current)),
                _ => current.push(ch),
            }
        }
        parts.push(current);

        parts
            .iter()
            .filter_map(|part| {
                let (name, _) = part.split_once(':')?;
                let name = name.trim().trim_start_matches("mut ").trim();
                if name.is_empty() {
                    None
                } else {
                    Some(name.to_string())
                }
            })
            .collect()
    }
}
