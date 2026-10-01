//! crates/ramaria-desktop/src/commands/export.rs - 数据导出 Tauri Commands
//!
//! 设计特点:
//! - export_sessions_json / export_sessions_markdown: 导出对话数据为文件
//! - 数据装配与载荷渲染均委托服务层（与 CLI 同源，JSON / Markdown 结构逐字段一致）
//! - 使用 Tauri dialog 选择保存路径（前端调用 open/save dialog 后传入路径）
//! - Markdown 无可导出会话时返回错误且不写文件
//! - 导出路径安全校验：canonicalize + 白名单 + 符号链接拒绝，复用 path_guard 模块

use crate::DesktopState;
use ramaria_service::{ExportData, ExportDataRequest};
use std::path::Path;
use tauri::State;

// =========================================================
// export_sessions_json — 导出 JSON 格式
// =========================================================

/// 导出全部会话数据为 JSON 文件。
///
/// 参数:
/// - `output_path`: 输出文件路径（前端通过 Tauri dialog 获取）
///
/// 返回:
/// - 导出文件的绝对路径
///
/// 说明:
/// - 文件内容由服务层渲染：`ramaria_export` 信封 + 版本 + 会话 / 消息字段（与 CLI 同结构）
/// - 路径安全检查：三层防御（canonicalize + 白名单 + 符号链接拒绝），复用 path_guard 模块
#[tauri::command]
#[tracing::instrument(skip(state, output_path))]
pub async fn export_sessions_json(
    state: State<'_, DesktopState>,
    output_path: String,
) -> Result<String, String> {
    // 路径安全校验（文件可能尚不存在，校验父目录）
    let canonical = crate::path_guard::validate_export_path(&output_path)?;

    let data = state
        .engine
        .export_sessions(ExportDataRequest::default())
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询导出数据失败"))?;

    write_export_json_file(&canonical, &data)?;

    let count = data.sessions.len();
    tracing::info!(
        file = %crate::path_guard::redact_path_label(&canonical),
        session_count = count,
        "JSON 导出完成"
    );
    Ok(canonical.to_string_lossy().to_string())
}

// =========================================================
// export_sessions_markdown — 导出 Markdown 格式
// =========================================================

/// 导出全部对话数据为 Markdown 文件。
///
/// 参数:
/// - `output_path`: 输出文件路径（前端通过 Tauri dialog 获取）
///
/// 返回:
/// - 导出文件的绝对路径
///
/// 说明:
/// - 文件内容由服务层渲染（按会话分组，角色标签与 CLI 一致；跳过无消息会话）
/// - 全部会话无消息时不写文件，返回"没有可导出的会话数据"
/// - 路径安全检查：三层防御（canonicalize + 白名单 + 符号链接拒绝），复用 path_guard 模块
#[tauri::command]
#[tracing::instrument(skip(state, output_path))]
pub async fn export_sessions_markdown(
    state: State<'_, DesktopState>,
    output_path: String,
) -> Result<String, String> {
    // 路径安全校验（文件可能尚不存在，校验父目录）
    let canonical = crate::path_guard::validate_export_path(&output_path)?;

    let data = state
        .engine
        .export_sessions(ExportDataRequest::default())
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询导出数据失败"))?;

    write_export_markdown_file(&canonical, &data)?;

    let count = data.sessions.len();
    tracing::info!(
        file = %crate::path_guard::redact_path_label(&canonical),
        session_count = count,
        "Markdown 导出完成"
    );
    Ok(canonical.to_string_lossy().to_string())
}

// =========================================================
// 文件写出（服务层渲染 → 落盘）
// =========================================================

/// 渲染并写出 JSON 导出文件（桌面不脱敏；载荷与 CLI 同源）。
fn write_export_json_file(canonical: &Path, data: &ExportData) -> Result<(), String> {
    let json = ramaria_service::render_sessions_json(data, false);
    std::fs::write(canonical, &json).map_err(|e| format!("写入文件失败: {}", e))
}

/// 渲染并写出 Markdown 导出文件；无可导出会话时返回错误且不写文件。
fn write_export_markdown_file(canonical: &Path, data: &ExportData) -> Result<(), String> {
    match ramaria_service::render_sessions_markdown(data, false) {
        Some(markdown) => {
            std::fs::write(canonical, markdown).map_err(|e| format!("写入文件失败: {}", e))
        }
        None => Err("没有可导出的会话数据".to_string()),
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{Message, MessageRole, MessageSource, Session};
    use ramaria_service::ExportSessionData;
    use uuid::Uuid;

    /// 固定时间样例：2024-06-10 08:00 UTC。
    const T0: i64 = 1_718_006_400_000;

    /// 系统临时目录下的唯一测试文件路径。
    fn temp_export_file(tag: &str, ext: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "ramaria_export_{tag}_{}_{stamp}.{ext}",
            std::process::id()
        ))
    }

    /// 构造一个含单条用户消息的导出样例。
    fn sample_data() -> ExportData {
        let session_id =
            Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("固定 UUID");
        let mut message = Message::new(
            session_id,
            MessageRole::User,
            "你好".to_string(),
            MessageSource::Local,
        );
        message.created_at = T0;
        ExportData {
            total_sessions: 1,
            sessions: vec![ExportSessionData {
                session: Session {
                    id: session_id,
                    started_at: T0,
                    ended_at: None,
                    persona_uid: None,
                    channel: "local".to_string(),
                    external_ref: None,
                },
                messages: vec![message],
            }],
            l1_persona: None,
            l1_memories: None,
        }
    }

    /// JSON 写出内容与服务层渲染逐字段一致（仅 exported_at 为渲染时刻需归一）。
    #[test]
    fn json_export_file_matches_service_render() {
        let path = temp_export_file("json", "json");
        let data = sample_data();
        write_export_json_file(&path, &data).expect("写出应成功");

        let mut written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("读取应成功"))
                .expect("写出内容应为合法 JSON");
        let mut expected: serde_json::Value =
            serde_json::from_str(&ramaria_service::render_sessions_json(&data, false))
                .expect("渲染结果应为合法 JSON");
        written["ramaria_export"]["exported_at"] = serde_json::json!("<normalized>");
        expected["ramaria_export"]["exported_at"] = serde_json::json!("<normalized>");

        assert_eq!(written, expected, "桌面输出应与服务层渲染逐字段一致");
        assert_eq!(
            written["ramaria_export"]["version"],
            ramaria_service::EXPORT_FORMAT_VERSION
        );
        assert_eq!(
            written["ramaria_export"]["sessions"][0]["messages"][0]["content"],
            "你好"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Markdown 写出内容与服务层渲染一致（导出时间行取渲染时刻，比对时剔除）。
    #[test]
    fn markdown_export_file_matches_service_render() {
        let path = temp_export_file("markdown", "md");
        let data = sample_data();
        write_export_markdown_file(&path, &data).expect("写出应成功");

        let written = std::fs::read_to_string(&path).expect("读取应成功");
        let expected = ramaria_service::render_sessions_markdown(&data, false).expect("应可导出");
        let strip_export_time = |text: &str| {
            text.lines()
                .filter(|line| !line.starts_with("导出时间: "))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip_export_time(&written), strip_export_time(&expected));

        let _ = std::fs::remove_file(&path);
    }

    /// 全部会话无消息：Markdown 返回错误且不写文件。
    #[test]
    fn markdown_export_empty_returns_err_without_file() {
        let path = temp_export_file("markdown-empty", "md");
        let mut data = sample_data();
        for entry in &mut data.sessions {
            entry.messages.clear();
        }

        let error = write_export_markdown_file(&path, &data).expect_err("无可导出会话应报错");
        assert_eq!(error, "没有可导出的会话数据");
        assert!(!path.exists(), "失败时不应写文件");
    }
}
