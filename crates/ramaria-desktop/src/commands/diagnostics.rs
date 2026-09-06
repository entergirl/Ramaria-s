//! crates/ramaria-desktop/src/commands/diagnostics.rs - 诊断与更新 Tauri Commands
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
/// - 当前版本号字符串，如 "1.7.0"。
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
/// - 若内容字符数 ≤ max_len，直接返回。
/// - 否则在预算（字符数）内向前查找最近的 `\n`，在此处截断。
/// - 若未找到换行符，则按字符边界硬截断并在末尾加 `\n...`。
///
/// 说明:
/// - 按 Unicode 字符（而非 UTF-8 字节）截断，由 `ramaria_core::text::truncate_char_boundary`
///   保证不切开多字节中文/emoji（避免旧字节切片在中文内容上的 panic）。
fn truncate_preview(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        return text.to_string();
    }

    // 在预算（字符）内向前找最近的换行符（字符位置），截断到其前（不含换行）
    let mut boundary: Option<usize> = None;
    for (i, ch) in text.chars().take(max_len).enumerate() {
        if ch == '\n' {
            boundary = Some(i);
        }
    }

    let cut = boundary.unwrap_or(max_len);
    let truncated = ramaria_core::text::truncate_char_boundary(text, cut);
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

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // 纯 ASCII 长文本（无换行）——字符边界与旧字节切片语义在此一致。
    #[test]
    fn truncate_preview_ascii_no_newline_truncates_with_marker() {
        let text = "a".repeat(250);
        let result = truncate_preview(&text, 200);
        // hard 分支保留前 200 字符并追加 "\n..."（4 字节）
        assert!(result.ends_with("\n..."));
        assert_eq!(result.len(), 200 + 4);
        assert_eq!(&result[..200], "a".repeat(200));
    }

    // 纯 ASCII 含换行——按换行优先截断，切到预算内最后一个换行前。
    #[test]
    fn truncate_preview_ascii_cuts_at_newline_within_budget() {
        // 第 150 个字符处一个换行，预算 200 → 切到该换行前并加标记
        let mut text = "x".repeat(150);
        text.push('\n');
        text.push_str(&"y".repeat(100));
        let result = truncate_preview(&text, 200);
        assert!(result.starts_with(&"x".repeat(150)));
        assert!(!result[..150].contains('\n'));
        assert!(result.ends_with("\n..."));
    }

    // 中文内容字符数 > 200（UTF-8 字节远超 200，release_notes 形态）——
    // 回归旧字节切片 panic；不得 panic 且切在字符边界、不切开多字节字符。
    #[test]
    fn truncate_preview_chinese_no_panic_and_char_boundary() {
        // 单中文字符 3 字节；250 字符 ≈ 750 字节 > 200 字节
        let text = "中".repeat(250);
        let result = truncate_preview(&text, 200);
        // 保留前 200 个中文字符（600 字节）+"\n..."（4 字节）
        assert!(result.ends_with("\n..."));
        assert_eq!(result.len(), 200 * 3 + 4);
        // 截断部分为完整 200 个中文字符（无半个多字节字符残留）
        assert_eq!(result[..200 * 3].chars().count(), 200);
    }

    // 全中文且无换行——预算内找不到换行，走 hard boundary 分支，
    // 保留前 200 字符并追加 \n...
    #[test]
    fn truncate_preview_all_chinese_no_newline_hard_boundary() {
        let text = "汉".repeat(300);
        let result = truncate_preview(&text, 200);
        assert!(result.ends_with("\n..."));
        assert_eq!(result.len(), 200 * 3 + 4);
        assert_eq!(result[..200 * 3].chars().count(), 200);
    }

    // 中文含换行——按换行优先截断，切点不切开多字节字符。
    #[test]
    fn truncate_preview_chinese_cuts_at_newline() {
        let mut text = "啊".repeat(100); // 100 字符 / 300 字节
        text.push('\n');
        text.push_str(&"啊".repeat(100)); // 总字符 201 > 200
        let result = truncate_preview(&text, 200);
        // 预算 200 内最后一个换行在字符位置 100 → 切到其前并加标记
        assert_eq!(result, format!("{}\n...", "啊".repeat(100)));
    }

    // 短内容（≤ max_len）原样返回、不加截断标记。
    #[test]
    fn truncate_preview_short_content_returned_as_is() {
        assert_eq!(truncate_preview("short", 200), "short");
        assert_eq!(
            truncate_preview("a".repeat(200).as_str(), 200),
            "a".repeat(200)
        );
        let zh_short = "中文短内容";
        assert_eq!(truncate_preview(zh_short, 200), zh_short);
        assert_eq!(truncate_preview("", 200), "");
    }

    // 混合中英文且无换行——'a' 与中文均计 1 字符，切在字符边界。
    #[test]
    fn truncate_preview_mixed_ascii_chinese_boundary() {
        // 恰好 200 字符（199 个 'a' + 1 个中文）→ 不截断
        let at_max = format!("{}中", "a".repeat(199));
        assert_eq!(truncate_preview(&at_max, 200), at_max);

        // 201 字符超限 → 保留前 200 字符（199 个 'a' + 第 1 个'中'），
        // 第 201 个'中'被切，追加 "\n..."；切点不切开多字节字符。
        let over = format!("{}中中", "a".repeat(199));
        let result = truncate_preview(&over, 200);
        assert_eq!(result, format!("{}中\n...", "a".repeat(199)));
    }
}
