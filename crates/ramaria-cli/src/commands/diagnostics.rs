//! crates/ramaria-cli/src/commands/diagnostics.rs - CLI 诊断导出命令
//!
//! 设计特点:
//! - `ramaria diagnostics --output <PATH>`: 收集诊断信息并打包为 .zip。
//! - 使用 canonicalize + 前缀检查防护路径穿越。
//! - 输出路径默认为当前目录下的 `ramaria-diagnostics-{timestamp}.zip`。
//! - `--json` 输出信封（导出路径/文件大小/收集状态）；文本模式打印收集状态摘要。
//! - 所有错误使用 anyhow::Result，由 main.rs 统一处理。
//!
//! 安全约束:
//! - 导出路径通过 canonicalize 防护路径穿越。
//! - API key 脱敏在诊断导出用例内部完成。

use anyhow::Context;
use ramaria_service::{DiagnosticsRequest, Engine};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// diagnostics 命令参数。
pub struct DiagnosticsArgs {
    /// 输出文件路径（可选，默认自动生成）
    pub output: Option<String>,
    /// 统一脱敏开关（诊断导出始终脱敏，此处为脚本统一传参兼容，幂等）
    pub redact: bool,
}

/// 执行 diagnostics 命令。
///
/// 流程:
/// 1. 确定输出路径（用户指定 → 默认 `ramaria-diagnostics-{timestamp}.zip`）。
/// 2. 读取数据库 schema 版本（读取失败降级为 "unknown"，不阻塞导出）。
/// 3. 收集系统信息、日志、配置并打包为 .zip 文件。
///
/// 参数:
/// - `engine`: 服务层引擎（配置快照提供日志与配置目录）。
/// - `args`: 命令参数。
/// - `json`: `--json` 信封输出（导出路径/文件大小/收集状态）。
pub async fn run(engine: &Arc<Engine>, args: DiagnosticsArgs, json: bool) -> anyhow::Result<()> {
    // 诊断导出链路始终执行脱敏；--redact 为统一传参兼容（幂等，不改变行为）
    tracing::debug!(redact = args.redact, "诊断导出脱敏开关（始终生效）");

    let output_path = match args.output {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(default_diagnostics_path()),
    };

    // 路径安全校验：使用 canonicalize 防护路径穿越
    let output_path = canonicalize_diagnostics_path(&output_path)?;

    crate::ui::info(&format!(
        "正在收集诊断信息...\n  输出路径: {}",
        output_path.display()
    ));

    // 读取 schema 版本（读取失败降级为 "unknown"，不阻塞导出）
    let schema_version = match engine.schema_version().await {
        Ok(version) => version.to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "读取 schema 版本失败，诊断包按 unknown 记录");
            "unknown".to_string()
        }
    };

    // 执行诊断导出
    let report = engine
        .export_diagnostics(DiagnosticsRequest {
            output_path,
            schema_version,
        })
        .await
        .context("诊断信息导出失败")?;

    if json {
        return crate::json::emit_ok(&serde_json::json!({
            "file": report.output_path.display().to_string(),
            "file_size_bytes": report.file_size_bytes,
            "collection_status": report.collection_status,
        }));
    }

    crate::ui::success(&format!(
        "诊断信息已导出到: {}\n文件大小: {}",
        report.output_path.display(),
        format_file_size(report.file_size_bytes),
    ));

    // 打印收集状态摘要
    crate::ui::info("收集状态:");
    for (key, value) in &report.collection_status {
        crate::ui::info(&format!("  [{key}]: {value}"));
    }

    Ok(())
}

// =========================================================
// 辅助函数
// =========================================================

/// 生成默认诊断文件路径: `ramaria-diagnostics-{timestamp}.zip`
fn default_diagnostics_path() -> String {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    format!("ramaria-diagnostics-{ts}.zip")
}

/// 规范化导出路径，防护路径穿越。
///
/// 安全措施:
/// - 对父目录调用 canonicalize，解析符号链接和相对路径。
/// - 拒绝包含 RootDir 或 Prefix 组件的裸路径。
fn canonicalize_diagnostics_path(path: &Path) -> anyhow::Result<PathBuf> {
    // 拒绝裸根目录和 Windows 盘符前缀路径
    let has_root_or_prefix = path
        .components()
        .any(|c| matches!(c, Component::RootDir | Component::Prefix(_)));
    if has_root_or_prefix && path.components().count() <= 1 {
        return Err(anyhow::anyhow!(
            "不安全的输出路径: '{}'。不能直接导出到根目录。",
            path.display()
        ));
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));

    // 跳过空父路径
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("无法创建输出目录: {}", parent.display()))?;
    }

    let canonical_parent = parent
        .canonicalize()
        .with_context(|| format!("无法访问目录: {}", parent.display()))?;

    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("无效的路径: 缺少文件名 ({})", path.display()))?;

    Ok(canonical_parent.join(file_name))
}

/// 格式化文件大小为人类可读格式。
fn format_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;

    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < (1024 * 1024) {
        format!("{:.1} KB", bytes as f64 / KB)
    } else {
        format!("{:.1} MB", bytes as f64 / MB)
    }
}
