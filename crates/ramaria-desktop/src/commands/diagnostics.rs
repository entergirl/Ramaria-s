//! rust/crates/ramaria-desktop/src/commands/diagnostics.rs - 诊断与更新 Tauri Commands
//!
//! 设计特点:
//! - `check_update`: 调用 ramaria_app::check_update，返回版本比较结果。
//! - `export_diagnostics`: 弹出保存对话框 → 调用 ramaria_app::export_diagnostics → 打包 zip。
//! - 所有命令返回 `Result<T, String>`，便于前端显示中文错误消息。
//! - 使用 Tauri AppHandle 弹出原生保存对话框（tauri-plugin-dialog）。
//!
//! 安全约束:
//! - 导出路径由用户通过原生对话框指定，不信任前端传入的路径。
//! - API key 脱敏在 export_diagnostics 内部完成，不可逆。

use ramaria_app::DiagnosticsReport;
use ramaria_app::update::UpdateStatus;
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::DesktopState;

// =========================================================
// 前端展示结构体（camelCase 序列化）
// =========================================================

/// 前端"检查更新"结果视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatusView {
    /// 当前版本号
    pub current_version: String,
    /// 远程最新版本标签（如 ""），null 表示无法获取
    pub latest_version: Option<String>,
    /// 是否有新版本可用
    pub update_available: bool,
    /// GitHub Release 页面 URL
    pub release_url: Option<String>,
    /// 版本发布说明（纯文本，前 200 字符截断供 UI 预览）
    pub release_notes_preview: Option<String>,
    /// 检查失败时的错误信息
    pub error: Option<String>,
}

impl From<UpdateStatus> for UpdateStatusView {
    fn from(s: UpdateStatus) -> Self {
        Self {
            current_version: s.current_version,
            latest_version: s.latest_version,
            update_available: s.update_available,
            release_url: s.release_url,
            release_notes_preview: s.release_notes.map(|notes| truncate_preview(&notes, 200)),
            error: s.error,
        }
    }
}

/// 前端"诊断导出"结果视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsExportView {
    /// 导出的文件路径
    pub output_path: String,
    /// 文件大小（字节）
    pub file_size_bytes: u64,
    /// 人类可读的文件大小（如 "45.2 KB"）
    pub file_size_display: String,
    /// 各收集步骤的状态（供前端展示警告）
    pub collection_status: std::collections::HashMap<String, String>,
    /// 人类可读的警告信息列表（空数组表示全部成功）
    pub warnings: Vec<String>,
}

impl From<DiagnosticsReport> for DiagnosticsExportView {
    fn from(r: DiagnosticsReport) -> Self {
        let warnings = generate_warnings(&r.collection_status);
        Self {
            output_path: r.output_path.display().to_string(),
            file_size_bytes: r.file_size_bytes,
            file_size_display: format_file_size(r.file_size_bytes),
            collection_status: r.collection_status,
            warnings,
        }
    }
}

// =========================================================
// Tauri Commands
// =========================================================

/// 检查是否有新版本可用。
///
/// 调用 ramaria_app::check_update，将结果转换为前端友好的视图结构。
///
/// 返回:
/// - `UpdateStatusView`: 含当前版本、最新版本、是否可更新、Release URL 和错误信息。
#[tauri::command]
#[tracing::instrument]
pub async fn check_update() -> Result<UpdateStatusView, String> {
    tracing::info!("用户手动检查更新");

    let status = ramaria_app::update::check_update().await;

    if let Some(ref err) = status.error {
        tracing::warn!(error = %err, "版本检查遇到问题");
    }

    Ok(UpdateStatusView::from(status))
}

/// 获取当前应用版本号（纯本地，无网络请求）。
///
/// 用途:
/// - 设置页展示当前版本号，无需消耗 GitHub API 配额。
/// - 与 `check_update` 不同，此命令不访问网络。
///
/// 返回:
/// - 当前版本号字符串，如 "1.4.0"。
#[tauri::command]
pub fn get_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 导出诊断信息为 .zip 文件。
///
/// 流程:
/// 1. 弹出原生保存对话框，默认文件名为 `ramaria-diagnostics-{日期}.zip`。
/// 2. 用户确认后，调用 `ramaria_app::export_diagnostics` 收集并打包。
/// 3. 返回导出结果视图（文件路径 + 大小）。
///
/// 参数:
/// - `app_handle`: Tauri AppHandle，用于弹出原生对话框。
/// - `state`: 桌面状态（含 App 实例和数据库连接池）。
#[tauri::command]
#[tracing::instrument(skip(app_handle, state))]
pub async fn export_diagnostics(
    app_handle: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<DiagnosticsExportView, String> {
    tracing::info!("用户触发诊断导出");

    // 1. 弹出保存对话框，默认文件名含日期
    let default_name = format!(
        "ramaria-diagnostics-{}.zip",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );

    let file_path = app_handle
        .dialog()
        .file()
        .add_filter("ZIP 文件", &["zip"])
        .set_file_name(&default_name)
        .blocking_save_file();

    let Some(file_path) = file_path else {
        tracing::info!("用户取消了诊断导出");
        return Err("用户取消了导出操作".to_string());
    };

    // `FilePath` 转换为 `PathBuf`（FilePath 实现了 Display trait，通过字符串转换）
    let output_path: PathBuf = PathBuf::from(file_path.to_string());

    // 2. 读取 schema 版本（从 schema_meta 表）
    let schema_version = match sqlx::query_scalar::<_, String>(
        "SELECT value FROM schema_meta WHERE key = 'schema_version'",
    )
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(v)) => v,
        Ok(None) => {
            // schema_meta 表可能没有这个 key，使用默认值
            tracing::debug!("schema_meta 表无 schema_version 记录，使用默认值 '1'");
            "1".to_string()
        }
        Err(e) => {
            tracing::warn!(error = %e, "读取 schema_meta 失败");
            "unknown".to_string()
        }
    };

    // 3. 执行诊断导出
    let config = state.app.config();
    let report = ramaria_app::diagnostics::export_diagnostics(config, schema_version, &output_path)
        .await
        .map_err(|e| {
            let msg = format!("诊断导出失败: {e}");
            tracing::error!(error = %e, "诊断导出失败");
            msg
        })?;

    tracing::info!(
        path = %report.output_path.display(),
        size = report.file_size_bytes,
        "诊断导出成功"
    );

    Ok(DiagnosticsExportView::from(report))
}

// =========================================================
// 辅助函数
// =========================================================

/// 将发布说明截断到指定字符数，保留完整句子（在最近的换行处截断）。
///
/// 实现:
/// - 若内容长度 ≤ max_len，直接返回。
/// - 否则在 `max_len` 位置向前查找最近的 `\n`，在此处截断。
/// - 若未找到换行符，则硬截断并在末尾加 `...`。
fn truncate_preview(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }

    // 在 max_len 位置向前找最近的换行
    let boundary = text[..max_len].rfind('\n').unwrap_or(max_len);

    let truncated = &text[..boundary];
    format!("{truncated}\n...")
}

/// 将字节数格式化为人类可读的文件大小。
///
/// 格式:
/// - < 1 KB: "N B"
/// - < 1 MB: "N.N KB"
/// - ≥ 1 MB: "N.N MB"
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

/// 根据收集状态生成人类可读的警告信息。
///
/// 规则:
/// - 只有 `ok` 和空字符串视为成功。
/// - `skipped` 和 `error` 生成中文警告。
/// - 全部成功时返回空数组。
fn generate_warnings(status: &std::collections::HashMap<String, String>) -> Vec<String> {
    let mut warnings = Vec::new();

    for (key, value) in status {
        if value.starts_with("skipped:") {
            let reason = value.strip_prefix("skipped:").unwrap_or(value).trim();
            warnings.push(match key.as_str() {
                "logs" => format!("日志未收集: {reason}"),
                "config" => format!("配置未收集: {reason}"),
                _ => format!("{key} 未收集: {reason}"),
            });
        } else if value.starts_with("error:") {
            let reason = value.strip_prefix("error:").unwrap_or(value).trim();
            warnings.push(match key.as_str() {
                "logs" => format!("日志收集失败: {reason}"),
                "config" => format!("配置收集失败: {reason}"),
                _ => format!("{key} 收集失败: {reason}"),
            });
        }
    }

    warnings
}
