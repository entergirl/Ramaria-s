//! crates/ramaria-service/src/diagnostics/collect.rs - 诊断信息采集
//!
//! 设计特点:
//! - 采集项：系统信息（纯内存）/ 日志（最近 1000 行）/ 配置文件 / 检索索引构建状态
//! - 读取失败不阻塞导出：记录占位文本与状态，路径类信息只写文件名（不暴露目录结构）
//! - 日志与配置在采集阶段即做二次脱敏（绝对路径 → 文件名；消息类字段 → 字符数）
//! - 状态汇总经 `status` 出入参传递（调用方负责汇总进诊断报告）

use std::collections::HashMap;
use std::path::PathBuf;

use ramaria_core::config::RamariaConfig;

use crate::engine::Engine;

use super::redact::{path_log_label, redact_api_keys, redact_for_export};

// =========================================================
// 类型定义
// =========================================================

/// 系统信息快照。
#[derive(Debug, Clone)]
pub(super) struct SystemInfo {
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
// 内部实现: 收集
// =========================================================

/// 收集系统信息快照。
///
/// 收集内容:
/// - OS / Arch / Family: 来自 `std::env::consts`。
/// - 应用版本: 来自 `env!("CARGO_PKG_VERSION")`。
/// - Schema 版本: 从 DB 的 `schema_meta` 表读取（由调用方传入）。
/// - 采集时间: UTC ISO 8601。
pub(super) fn collect_system_info(schema_version: &str) -> SystemInfo {
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
pub(super) fn collect_logs(config: &RamariaConfig, status: &mut HashMap<String, String>) -> String {
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
            // 二次脱敏：绝对路径 → 文件名；消息类字段值 → 字符数（原文不出端）
            let redacted = redact_for_export(&truncated);

            status.insert(
                "logs".to_string(),
                format!("ok: {}/{} lines", redacted.lines().count(), total),
            );
            tracing::debug!(
                total_lines = total,
                collected = redacted.lines().count(),
                "日志收集完成（已二次脱敏）"
            );

            if start > 0 {
                format!("# 最近 1000 行日志（共 {total} 行，已截断前 {start} 行）\n\n{redacted}")
            } else {
                format!("# 全部日志（共 {total} 行）\n\n{redacted}")
            }
        }
        Err(e) => {
            // 占位文本随诊断包外发：路径只写文件名（与成功内容的导出前脱敏口径一致）
            let msg = format!(
                "# 无法读取日志文件 ({}): {}\n",
                path_log_label(&log_path),
                e
            );
            status.insert("logs".to_string(), format!("error: {e}"));
            tracing::warn!(file = %path_log_label(&log_path), error = %e, "日志收集失败");
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
pub(super) fn collect_config(
    config: &RamariaConfig,
    status: &mut HashMap<String, String>,
) -> String {
    let config_dir = &config.paths.config_dir;
    if config_dir.is_empty() {
        status.insert("config".to_string(), "skipped: 配置目录未配置".to_string());
        tracing::debug!("配置目录未配置，跳过配置收集");
        return String::from("# 配置目录未配置，无法收集配置文件。\n");
    }

    let config_path = PathBuf::from(config_dir).join("config.toml");

    match std::fs::read_to_string(&config_path) {
        Ok(content) => {
            // 两道脱敏：API key → [REDACTED]；绝对路径 → 文件名（paths 组可能含本机路径）
            let redacted = redact_for_export(&redact_api_keys(&content));
            // 包头只写文件名，不写绝对路径（诊断包外发时不暴露本机目录结构）
            let file_label = config_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "config.toml".to_string());
            status.insert("config".to_string(), "ok".to_string());
            tracing::debug!(file = %path_log_label(&config_path), "配置收集完成（API key 已脱敏）");
            format!("# 配置文件: {file_label}\n# 注意：API key 已脱敏为 [REDACTED]\n\n{redacted}")
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
            // 占位文本随诊断包外发：路径只写文件名（与成功内容的导出前脱敏口径一致）
            let msg = format!(
                "# 无法读取配置文件 ({}): {}\n",
                path_log_label(&config_path),
                e
            );
            status.insert("config".to_string(), format!("error: {e}"));
            tracing::warn!(file = %path_log_label(&config_path), error = %e, "配置收集失败");
            msg
        }
    }
}

/// 收集检索索引构建状态：`ok` / `failed: <脱敏原因>` / `not_built`。
///
/// 口径:
/// - 最近一次构建失败 → `failed: <原因>`（原因在记录时已脱敏，不含用户原文）；
/// - 无失败且成功构建过（构建完成时间非零）→ `ok`；
/// - 否则 → `not_built`（尚未触发过懒加载 / 自愈构建）。
pub(super) fn collect_index_build_status(engine: &Engine, status: &mut HashMap<String, String>) {
    let value = match engine.index_build_failure() {
        Some(failure) => format!("failed: {}", failure.reason),
        None if engine.last_index_build_time() > 0 => "ok".to_string(),
        None => "not_built".to_string(),
    };
    status.insert("index_build".to_string(), value);
}
