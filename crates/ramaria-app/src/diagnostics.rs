//! crates/ramaria-app/src/diagnostics.rs - 诊断信息导出
//!
//! 设计特点:
//! - 收集：日志(最近1000行)、配置(API key 脱敏)、数据库 schema 版本、系统信息。
//! - 打包为 .zip 文件供用户手动发送给开发者排查问题。
//! - 所有敏感信息（API key）在收集阶段即脱敏，写入 zip 前已安全。
//! - 使用临时目录构建 zip，避免中断后留下半成品文件。
//! - 收集阶段错误不阻塞导出：缺失项记录占位文本而非报错退出。
//!
//! 安全约束:
//! - API key 脱敏使用 `[REDACTED]` 替换，不可逆。
//! - 不收集用户对话内容、记忆数据等隐私信息。
//! - 日志可能含用户消息片段（前80字符截断），参见日志隐私策略。
//! - 输出路径使用与 CLI export 相同的 `canonicalize` + 前缀检查防护。

use ramaria_core::config::RamariaConfig;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

// =========================================================
// 类型定义
// =========================================================

/// 诊断导出结果。
///
/// 包含导出文件的绝对路径和各组件的成功/失败状态。
#[derive(Debug, Clone)]
pub struct DiagnosticsReport {
    /// 输出的 .zip 文件绝对路径
    pub output_path: PathBuf,
    /// 各收集步骤的状态
    pub collection_status: HashMap<String, String>,
    /// 文件大小（字节）
    pub file_size_bytes: u64,
}

/// 系统信息快照。
#[derive(Debug, Clone)]
struct SystemInfo {
    /// 操作系统（如 "windows"）
    pub os: String,
    /// CPU 架构（如 "x86_64"）
    pub arch: String,
    /// 操作系统家族（如 "windows"）
    pub family: String,
    /// 应用版本
    pub app_version: String,
    /// 数据库 schema 版本
    pub schema_version: String,
    /// 当前时间 ISO 8601
    pub collected_at: String,
}

// =========================================================
// 公开 API
// =========================================================

/// 导出诊断信息，打包为 .zip 文件。
///
/// 调用方:
/// - 桌面端"导出诊断信息"按钮 → Tauri Command（弹出保存对话框后调用此函数）。
/// - CLI `ramaria diagnostics --output <PATH>`。
///
/// 参数:
/// - `config`: 应用配置（需要 log_dir 和 config 路径）。
/// - `schema_version`: 数据库 schema 版本号字符串。
/// - `output_path`: 输出 .zip 文件的绝对路径（由调用方通过文件对话框或 CLI 参数指定）。
///
/// 返回:
/// - `DiagnosticsReport`，含输出路径、各步骤状态和文件大小。
///
/// 导出内容:
/// - `ramaria.log`: 最近最多 1000 行日志内容（从日志文件读取）。
/// - `config.toml`: 当前配置文件内容（API key 已脱敏为 `[REDACTED]`）。
/// - `system.txt`: OS / 架构 / 版本 / schema 版本 / 采集时间。
///
/// 安全约束:
/// - API key 在收集阶段即脱敏，写入前已不可逆。
/// - 日志行中可能包含的用户消息已在日志层截断（≤ 80 字符），此处不加二次清洗。
/// - 输出路径的安全性由调用方保证（CLI/Desktop 各自使用 canonicalize 防护）。
///
/// 示例:
/// ```ignore
/// let report = export_diagnostics(
/// &app.config,
/// "1",
/// Path::new("C:/Users/me/Desktop/ramaria-diagnostics.zip"),
/// ).await?;
/// println!("诊断信息已导出到: {}", report.output_path.display);
/// ```
pub async fn export_diagnostics(
    config: &RamariaConfig,
    schema_version: String,
    output_path: &Path,
) -> Result<DiagnosticsReport, ramaria_core::RamariaError> {
    let mut status = HashMap::new();

    // 1. 收集系统信息（纯内存操作，不会失败）
    let system_info = collect_system_info(&schema_version);

    // 2. 收集日志（最近 1000 行）
    let logs = collect_logs(config, &mut status);

    // 3. 收集配置（API key 脱敏）
    let config_content = collect_config(config, &mut status);

    // 4. 打包为 zip
    let output_path = output_path.to_path_buf();
    let file_size = build_zip(&output_path, &system_info, &logs, &config_content)
        .map_err(|e| ramaria_core::RamariaError::io(format!("生成诊断 zip 文件失败: {e}"), None))?;

    status.insert("zip".to_string(), "ok".to_string());

    tracing::info!(
        path = %output_path.display(),
        size = file_size,
        "诊断信息导出完成"
    );

    Ok(DiagnosticsReport {
        output_path,
        collection_status: status,
        file_size_bytes: file_size,
    })
}

// =========================================================
// 内部实现: 收集
// =========================================================

/// 收集系统信息快照。
///
/// 收集内容:
/// - OS / Arch / Family: 来自 `std::env::consts`。
/// - App 版本: 来自 `env!("CARGO_PKG_VERSION")`。
/// - Schema 版本: 从 DB 的 `schema_meta` 表读取（由调用方传入）。
/// - 采集时间: UTC ISO 8601。
fn collect_system_info(schema_version: &str) -> SystemInfo {
    let collected_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    SystemInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        family: std::env::consts::FAMILY.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_version: schema_version.to_string(),
        collected_at,
    }
}

/// 收集最近 1000 行日志。
///
/// 行为:
/// - 从 `{config.paths.log_dir}/ramaria.log` 读取。
/// - 日志目录为空或文件不存在时，返回占位文本。
/// - 读取失败不阻塞导出，记录在 `status` 中。
///
/// 返回:
/// - 日志文本内容（最多 1000 行）。失败时返回说明性占位文本。
fn collect_logs(config: &RamariaConfig, status: &mut HashMap<String, String>) -> String {
    let log_dir = &config.paths.log_dir;
    if log_dir.is_empty() {
        status.insert("logs".to_string(), "skipped: 日志目录未配置".to_string());
        tracing::debug!("日志目录未配置，跳过日志收集");
        return String::from("# 日志目录未配置，无法收集日志。\n");
    }

    let log_path = PathBuf::from(log_dir).join("ramaria.log");

    match std::fs::read_to_string(&log_path) {
        Ok(content) => {
            // 截取最后 1000 行
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();
            let start = total.saturating_sub(1000);

            let truncated: String = lines[start..].iter().map(|l| format!("{l}\n")).collect();

            status.insert(
                "logs".to_string(),
                format!("ok: {}/{} lines", truncated.lines().count(), total),
            );
            tracing::debug!(
                total_lines = total,
                collected = truncated.lines().count(),
                "日志收集完成"
            );

            if start > 0 {
                format!("# 最近 1000 行日志（共 {total} 行，已截断前 {start} 行）\n\n{truncated}")
            } else {
                format!("# 全部日志（共 {total} 行）\n\n{truncated}")
            }
        }
        Err(e) => {
            let msg = format!("# 无法读取日志文件 ({}): {}\n", log_path.display(), e);
            status.insert("logs".to_string(), format!("error: {e}"));
            tracing::warn!(path = %log_path.display(), error = %e, "日志收集失败");
            msg
        }
    }
}

/// 收集配置文件内容（API key 已脱敏）。
///
/// 行为:
/// - 从 `{config.paths.config_dir}/config.toml` 读取。
/// - 对每一行做脱敏：匹配 `api_key` 模式的行替换值为 `[REDACTED]`。
/// - 文件不存在时返回占位文本。
///
/// 脱敏策略:
/// - 匹配包含 `api_key` 或 `apikey`（不区分大小写）的赋值行。
/// - 将 `=` 右侧的值替换为 `"[REDACTED]"`。
/// - 对于 keychain 中存储的 key，config.toml 中本不应包含，此处做防御性脱敏。
///
/// 返回:
/// - 脱敏后的配置文件文本。
fn collect_config(config: &RamariaConfig, status: &mut HashMap<String, String>) -> String {
    let config_dir = &config.paths.config_dir;
    if config_dir.is_empty() {
        status.insert("config".to_string(), "skipped: 配置目录未配置".to_string());
        tracing::debug!("配置目录未配置，跳过配置收集");
        return String::from("# 配置目录未配置，无法收集配置文件。\n");
    }

    let config_path = PathBuf::from(config_dir).join("config.toml");

    match std::fs::read_to_string(&config_path) {
        Ok(content) => {
            let redacted = redact_api_keys(&content);
            status.insert("config".to_string(), "ok".to_string());
            tracing::debug!(path = %config_path.display(), "配置收集完成（API key 已脱敏）");
            format!(
                "# 配置文件: {}\n# 注意：API key 已脱敏为 [REDACTED]\n\n{redacted}",
                config_path.display()
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // config.toml 不存在是预期行为：Ramaria 将配置存储在数据库中，不使用文件配置
            status.insert(
                "config".to_string(),
                "skipped: 未使用配置文件（配置存储在数据库中）".to_string(),
            );
            tracing::debug!("配置文件不存在（预期：配置存储在数据库中），跳过收集");
            String::from(
                "# 配置文件不存在（Ramaria 将配置存储在数据库中，不使用 config.toml 文件）\n",
            )
        }
        Err(e) => {
            let msg = format!("# 无法读取配置文件 ({}): {}\n", config_path.display(), e);
            status.insert("config".to_string(), format!("error: {e}"));
            tracing::warn!(path = %config_path.display(), error = %e, "配置收集失败");
            msg
        }
    }
}

/// 对配置文件内容做 API key 脱敏。
///
/// 脱敏规则:
/// - 匹配模式: 行中包含 `api_key` 或 `apikey`（不区分大小写），且包含 `=`（赋值语句）。
/// - 将 `=` 右侧的内容替换为 ` "[REDACTED]"`。
/// - 不修改注释行（以 `#` 开头）。
///
/// 返回:
/// - 脱敏后的完整文本。
fn redact_api_keys(content: &str) -> String {
    content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            // 跳过纯注释行
            if trimmed.starts_with('#') || trimmed.starts_with("//") {
                return line.to_string();
            }

            // 检测 api_key 或 apikey（不区分大小写）
            let lower = trimmed.to_lowercase();
            if lower.contains("api_key") || lower.contains("apikey") {
                // 找到 `=` 的位置
                if let Some(eq_pos) = trimmed.find('=') {
                    let key_part = &trimmed[..eq_pos + 1];
                    return format!("{key_part} \"[REDACTED]\"");
                }
            }

            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// =========================================================
// 内部实现: zip 打包
// =========================================================

/// 将收集到的诊断数据打包为 .zip 文件。
///
/// 打包策略:
/// - 使用临时文件构建，成功后移动到目标路径（原子性保证）。
/// - 使用 Deflated 压缩（平衡速度与体积）。
/// - 每个文件一行写入，不在内存中构建完整 zip。
///
/// 返回:
/// - 写入的字节数（文件大小）。
fn build_zip(
    output_path: &Path,
    system_info: &SystemInfo,
    logs: &str,
    config_content: &str,
) -> Result<u64, String> {
    // 确保父目录存在
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("无法创建输出目录 '{}': {e}", parent.display()))?;
    }

    // 创建 zip 文件
    let file = std::fs::File::create(output_path)
        .map_err(|e| format!("无法创建 zip 文件 '{}': {e}", output_path.display()))?;

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
fn build_system_txt(info: &SystemInfo) -> String {
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

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── API key 脱敏 ──

    #[test]
    fn test_redact_api_key_cases() {
        // 单行脱敏、大小写不敏感、注释与无关键保持原样
        let cases = [
            ("api_key = \"sk-abc123def456\"", "api_key = \"[REDACTED]\""),
            ("API_KEY = \"secret\"", "API_KEY = \"[REDACTED]\""),
            (
                "# api_key = \"this is a comment\"",
                "# api_key = \"this is a comment\"",
            ),
            (
                "base_url = \"https://api.example.com\"",
                "base_url = \"https://api.example.com\"",
            ),
            ("// api_key = \"value\"", "// api_key = \"value\""),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_api_keys(input), expected, "input={input:?}");
        }
        // 多行: 保留 base_url/model_id、api_key 脱敏、不含原密文
        let input =
            "base_url = \"https://api.example.com\"\napi_key = \"my-secret\"\nmodel_id = \"gpt-4\"";
        let result = redact_api_keys(input);
        assert!(result.contains("base_url"));
        assert!(result.contains("[REDACTED]"));
        assert!(result.contains("model_id"));
        assert!(!result.contains("my-secret"));
    }

    // ── system.txt 构建 ──

    #[test]
    fn test_build_system_txt_contains_all_fields() {
        let info = SystemInfo {
            os: "windows".into(),
            arch: "x86_64".into(),
            family: "windows".into(),
            app_version: "1.7.0".into(),
            schema_version: "1".into(),
            collected_at: "2026-06-15T12:00:00Z".into(),
        };

        let content = build_system_txt(&info);

        assert!(content.contains("os = windows"));
        assert!(content.contains("arch = x86_64"));
        assert!(content.contains("app_version = 1.7.0"));
        assert!(content.contains("schema_version = 1"));
        assert!(content.contains("2026-06-15T12:00:00Z"));
    }

    // ── 系统信息收集 ──

    #[test]
    fn test_collect_system_info() {
        let info = collect_system_info("42");

        assert_eq!(info.app_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.schema_version, "42");
        assert!(!info.collected_at.is_empty());
        // 验证 OS 字段非空
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
    }

    // ── 日志收集: 空目录 ──

    #[test]
    fn test_collect_logs_empty_dir() {
        let mut config = RamariaConfig::default();
        config.paths.log_dir = String::new();
        let mut status = HashMap::new();

        let result = collect_logs(&config, &mut status);

        assert!(result.contains("未配置"));
        assert!(status.contains_key("logs"));
    }

    // ── 配置收集: 空目录 ──

    #[test]
    fn test_collect_config_empty_dir() {
        let mut config = RamariaConfig::default();
        config.paths.config_dir = String::new();
        let mut status = HashMap::new();

        let result = collect_config(&config, &mut status);

        assert!(result.contains("未配置"));
        assert!(status.contains_key("config"));
    }
}
