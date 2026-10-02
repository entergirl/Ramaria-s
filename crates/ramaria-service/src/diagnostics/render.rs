//! crates/ramaria-service/src/diagnostics/render.rs - 诊断包渲染与落盘（临时文件 + 原子替换）
//!
//! 设计特点:
//! - 先写与目标同目录的临时文件（`{文件名}.part`），全部写入并 `finish` 成功后
//!   用 `std::fs::rename` 原子替换目标（Windows 下可原子覆盖已存在文件）
//! - 任一步失败返回 Err 并清理残留临时文件，不留半成品覆盖旧文件
//! - 使用 Deflated 压缩（平衡速度与体积）；逐文件流式写入，不在内存中构建完整 zip
//! - `system.txt` 为键值对格式（每行一个属性），便于机器解析和人类阅读

use std::io::Write;
use std::path::Path;

use super::collect::SystemInfo;

// =========================================================
// 内部实现: zip 打包
// =========================================================

/// 临时文件名后缀：写入完成后通过原子重命名替换正式文件。
const TEMP_SUFFIX: &str = ".part";

/// 将收集到的诊断数据打包为 .zip 文件。
///
/// 打包策略:
/// - 先将内容写入与目标同目录的临时文件（`{文件名}.part`），全部写入并 `finish`
///   成功后再用 `std::fs::rename` 原子替换目标路径（Windows 下可原子覆盖已存在文件）。
/// - 任一步失败返回 Err，并清理残留临时文件，不留半成品覆盖旧文件。
/// - 使用 Deflated 压缩（平衡速度与体积）。
/// - 每个文件一行写入，不在内存中构建完整 zip。
///
/// 返回:
/// - 写入的字节数（文件大小）。
pub(super) fn build_zip(
    output_path: &Path,
    system_info: &SystemInfo,
    logs: &str,
    config_content: &str,
) -> Result<u64, String> {
    // 确保父目录存在（仅当父目录为非空路径）
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("无法创建输出目录 '{}': {e}", parent.display()))?;
    }

    // 临时文件与目标同目录，保证 rename 在同一文件系统内、可原子覆盖旧文件
    let file_name = output_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("输出路径缺少文件名: '{}'", output_path.display()))?;
    let temp_path = output_path.with_file_name(format!("{file_name}{TEMP_SUFFIX}"));

    // 主体闭包：先写临时文件，成功后再原子替换目标；任何 Err 由外层清理临时文件
    let result = (|| -> Result<u64, String> {
        let bytes = write_zip(&temp_path, system_info, logs, config_content)?;
        std::fs::rename(&temp_path, output_path).map_err(|e| {
            format!(
                "原子替换 zip 失败 '{}' → '{}': {e}",
                temp_path.display(),
                output_path.display()
            )
        })?;
        Ok(bytes)
    })();

    if result.is_err() {
        // 写入或 rename 中途失败：清理可能残留的临时文件，不留盘
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// 将诊断数据写入指定路径的 .zip 文件。
///
/// 参数:
/// - `zip_path`: 目标 .zip 文件路径（由调用方决定为临时或正式路径）。
///
/// 返回:
/// - 写入的字节数（文件大小）。
fn write_zip(
    zip_path: &Path,
    system_info: &SystemInfo,
    logs: &str,
    config_content: &str,
) -> Result<u64, String> {
    let file = std::fs::File::create(zip_path)
        .map_err(|e| format!("无法创建临时 zip 文件 '{}': {e}", zip_path.display()))?;

    let mut zip_writer = zip::ZipWriter::new(file);

    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    // 1. 写入 system.txt
    let system_content = build_system_txt(system_info);
    zip_writer
        .start_file("system.txt", options)
        .map_err(|e| format!("zip 写入 system.txt 失败: {e}"))?;
    zip_writer
        .write_all(system_content.as_bytes())
        .map_err(|e| format!("zip 写入 system.txt 内容失败: {e}"))?;

    // 2. 写入 ramaria.log
    zip_writer
        .start_file("ramaria.log", options)
        .map_err(|e| format!("zip 写入 ramaria.log 失败: {e}"))?;
    zip_writer
        .write_all(logs.as_bytes())
        .map_err(|e| format!("zip 写入 ramaria.log 内容失败: {e}"))?;

    // 3. 写入 config.toml
    zip_writer
        .start_file("config.toml", options)
        .map_err(|e| format!("zip 写入 config.toml 失败: {e}"))?;
    zip_writer
        .write_all(config_content.as_bytes())
        .map_err(|e| format!("zip 写入 config.toml 内容失败: {e}"))?;

    // 完成写入，获取文件大小
    let finished = zip_writer
        .finish()
        .map_err(|e| format!("zip 完成写入失败: {e}"))?;

    let file_size = finished.metadata().map(|m| m.len()).unwrap_or(0);

    Ok(file_size)
}

/// 构建 system.txt 内容。
///
/// 格式: 键值对，每行一个属性，便于机器解析和人类阅读。
pub(super) fn build_system_txt(info: &SystemInfo) -> String {
    format!(
        "# Ramaria 诊断报告 - 系统信息\n\
         # 采集时间: {collected_at}\n\
         \n\
         os = {os}\n\
         arch = {arch}\n\
         family = {family}\n\
         app_version = {app_version}\n\
         schema_version = {schema_version}\n",
        collected_at = info.collected_at,
        os = info.os,
        arch = info.arch,
        family = info.family,
        app_version = info.app_version,
        schema_version = info.schema_version,
    )
}
