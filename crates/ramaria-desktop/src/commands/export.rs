//! rust/crates/ramaria-desktop/src/commands/export.rs - 数据导出 Tauri Commands
//!
//! 设计特点:
//! - export_sessions_json / export_sessions_markdown: 导出对话数据为文件
//! - 使用 Tauri dialog 选择保存路径（前端调用 open/save dialog 后传入路径）
//! - JSON 格式：结构化 sessions → messages → L1 记忆
//! - Markdown 格式：人类可读的对话记录
//! - 导出路径安全校验：canonicalize + 白名单 + 符号链接拒绝，复用 path_guard 模块

use crate::DesktopState;
use serde::Serialize;
use tauri::State;

// =========================================================
// 导出数据结构
// =========================================================

/// 导出用会话结构。
#[derive(Debug, Clone, Serialize)]
struct ExportSession {
    id: String,
    started_at: i64,
    ended_at: Option<i64>,
    messages: Vec<ExportMessage>,
}

/// 导出用消息结构。
#[derive(Debug, Clone, Serialize)]
struct ExportMessage {
    role: String,
    content: String,
    persona_uid: Option<String>,
    created_at: i64,
}

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
/// - 导出结构：sessions[] 含 messages[]，每消息含 role/content/persona_uid/created_at
/// - 路径安全检查：三层防御（canonicalize + 白名单 + 符号链接拒绝），复用 path_guard 模块
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn export_sessions_json(
    state: State<'_, DesktopState>,
    output_path: String,
) -> Result<String, String> {
    // 路径安全校验（文件可能尚不存在，校验父目录）
    let canonical = crate::path_guard::validate_export_path(&output_path)?;

    let sessions = state
        .app
        .storage()
        .list_sessions()
        .await
        .map_err(|e| format!("查询会话列表失败: {}", e))?;

    let mut export_sessions: Vec<ExportSession> = Vec::new();

    for session in &sessions {
        let messages = state
            .app
            .storage()
            .list_messages(session.id)
            .await
            .map_err(|e| format!("查询消息失败 ({}): {}", session.id, e))?;

        let export_msgs: Vec<ExportMessage> = messages
            .into_iter()
            .map(|m| ExportMessage {
                role: m.role.as_str().to_string(),
                content: m.content,
                persona_uid: m.persona_uid,
                created_at: m.created_at,
            })
            .collect();

        export_sessions.push(ExportSession {
            id: session.id.to_string(),
            started_at: session.started_at,
            ended_at: session.ended_at,
            messages: export_msgs,
        });
    }

    let json = serde_json::to_string_pretty(&export_sessions)
        .map_err(|e| format!("序列化 JSON 失败: {}", e))?;

    std::fs::write(&canonical, &json).map_err(|e| format!("写入文件失败: {}", e))?;

    let output = canonical.to_string_lossy().to_string();
    let count = export_sessions.len();
    tracing::info!(path = %output, session_count = count, "JSON 导出完成");
    Ok(output)
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
/// - 按会话分组，消息按角色标注（👤 用户 / 🤖 助手 / 🔧 系统）
/// - 路径安全检查：三层防御（canonicalize + 白名单 + 符号链接拒绝），复用 path_guard 模块
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn export_sessions_markdown(
    state: State<'_, DesktopState>,
    output_path: String,
) -> Result<String, String> {
    // 路径安全校验（文件可能尚不存在，校验父目录）
    let canonical = crate::path_guard::validate_export_path(&output_path)?;

    let sessions = state
        .app
        .storage()
        .list_sessions()
        .await
        .map_err(|e| format!("查询会话列表失败: {}", e))?;

    let mut md = String::new();
    md.push_str("# Ramaria 对话导出\n\n");
    md.push_str(&format!(
        "导出时间: {}\n\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    ));
    md.push_str("---\n\n");

    for (i, session) in sessions.iter().enumerate() {
        let start_time = chrono::DateTime::from_timestamp_millis(session.started_at)
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "未知时间".to_string());

        md.push_str(&format!("## 会话 {} — {}\n\n", i + 1, start_time));

        let messages = state
            .app
            .storage()
            .list_messages(session.id)
            .await
            .map_err(|e| format!("查询消息失败 ({}): {}", session.id, e))?;

        for msg in &messages {
            let role_icon = match msg.role {
                ramaria_core::types::MessageRole::User => "👤 **用户**",
                ramaria_core::types::MessageRole::Assistant => "🤖 **助手**",
                ramaria_core::types::MessageRole::System => "🔧 **系统**",
                ramaria_core::types::MessageRole::Tool => "🛠 **工具**",
                _ => "❓ **未知**",
            };
            md.push_str(&format!("{}\n\n", role_icon));
            md.push_str(&msg.content);
            md.push_str("\n\n---\n\n");
        }

        md.push('\n');
    }

    std::fs::write(&canonical, &md).map_err(|e| format!("写入文件失败: {}", e))?;

    let output = canonical.to_string_lossy().to_string();
    let count = sessions.len();
    tracing::info!(path = %output, session_count = count, "Markdown 导出完成");
    Ok(output)
}
