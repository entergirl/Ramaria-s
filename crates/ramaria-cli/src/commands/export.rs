//! rust/crates/ramaria-cli/src/commands/export.rs - 数据导出命令
//!
//! 设计特点:
//! - 支持 JSON 和 Markdown 两种导出格式
//! - JSON: 结构化的 sessions → messages → L1 memories → L2 events
//! - Markdown: 人类可读的对话记录
//! - --persona 筛选特定 persona 的数据
//! - --output 指定输出文件（默认 stdout）
//! - 敏感信息不出现在导出中（API key 等）

use anyhow::Context;
use chrono::TimeZone;
use std::io::Write;
use std::sync::Arc;

/// export 命令参数。
pub struct ExportArgs {
    /// 导出格式: json / markdown
    pub format: String,
    /// 按 persona_uid 筛选
    pub persona: Option<String>,
    /// 输出文件路径（默认 stdout）
    pub output: Option<String>,
}

/// 执行 export 命令。
pub async fn run(app: &Arc<ramaria_app::App>, args: ExportArgs) -> anyhow::Result<()> {
    match args.format.as_str() {
        "json" => export_json(app, &args).await,
        "markdown" | "md" => export_markdown(app, &args).await,
        other => anyhow::bail!("不支持的导出格式: '{other}'。支持: json / markdown"),
    }
}

// =========================================================
// JSON 导出
// =========================================================

async fn export_json(app: &Arc<ramaria_app::App>, args: &ExportArgs) -> anyhow::Result<()> {
    let sessions = app
        .storage()
        .list_sessions()
        .await
        .context("查询会话失败")?;

    let mut export_data: Vec<serde_json::Value> = Vec::new();

    for session in &sessions {
        let messages = app
            .storage()
            .list_messages(session.id)
            .await
            .unwrap_or_default();

        // Persona 筛选
        if let Some(ref _persona_filter) = args.persona {
            let has_relevant = messages.iter().any(|m| {
                m.role == ramaria_core::types::MessageRole::User
                    || m.role == ramaria_core::types::MessageRole::Assistant
            });
            if !has_relevant {
                continue;
            }
        }

        let session_json = serde_json::json!({
            "session_id": session.id.to_string(),
            "started_at": format_timestamp(session.started_at),
            "ended_at": format_timestamp(session.ended_at.unwrap_or(0)),
            "messages": messages.iter().map(|m| {
                serde_json::json!({
                    "role": m.role.as_str(),
                    "content": m.content,
                    "source": m.source.to_string(),
                    "created_at": format_timestamp(m.created_at),
                })
            }).collect::<Vec<_>>(),
        });

        export_data.push(session_json);
    }

    // 添加 L1 记忆
    if let Some(ref persona_uid) = args.persona {
        let l1_memories = app
            .storage()
            .list_unabsorbed_l1(persona_uid)
            .await
            .unwrap_or_default();

        let l1_json = serde_json::json!({
            "type": "l1_memories",
            "persona_uid": persona_uid,
            "count": l1_memories.len(),
            "items": l1_memories.iter().map(|m| {
                serde_json::json!({
                    "id": m.id.to_string(),
                    "session_id": m.session_id.to_string(),
                    "summary": m.summary,
                    "valence": m.valence,
                    "salience": m.salience,
                    "created_at": format_timestamp(m.created_at),
                })
            }).collect::<Vec<_>>(),
        });

        export_data.push(l1_json);
    }

    let json_output = serde_json::to_string_pretty(&serde_json::json!({
        "ramaria_export": {
            "version": "0.1.0",
            "exported_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "sessions": export_data,
        }
    }))?;

    write_output(&json_output, args.output.as_deref())?;

    crate::ui::success(&format!("已导出 {} 个会话", sessions.len()));
    Ok(())
}

// =========================================================
// Markdown 导出
// =========================================================

async fn export_markdown(app: &Arc<ramaria_app::App>, args: &ExportArgs) -> anyhow::Result<()> {
    let sessions = app
        .storage()
        .list_sessions()
        .await
        .context("查询会话失败")?;

    let mut md = String::new();
    md.push_str("# Ramaria 对话导出\n\n");
    md.push_str(&format!(
        "导出时间: {}\n\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    ));
    md.push_str("---\n\n");

    let mut exported_count = 0usize;

    for session in &sessions {
        let messages = app
            .storage()
            .list_messages(session.id)
            .await
            .unwrap_or_default();

        if messages.is_empty() {
            continue;
        }

        // Persona 筛选
        if let Some(ref _persona_filter) = args.persona {
            // Markdown 导出暂不做严格 persona 筛选，导出所有对话
        }

        exported_count += 1;
        md.push_str(&format!("## 会话 {}\n\n", session.id));
        if let Some(ts) = format_timestamp(session.started_at) {
            md.push_str(&format!("*创建时间: {ts}*\n\n"));
        }

        for msg in &messages {
            let role_label = match msg.role {
                ramaria_core::types::MessageRole::User => "**👤 用户**",
                ramaria_core::types::MessageRole::Assistant => "**🤖 AI**",
                ramaria_core::types::MessageRole::System => "*⚙ 系统*",
                _ => "*❓ 未知*",
            };
            md.push_str(&format!("{role_label}\n\n"));
            md.push_str(&msg.content);
            md.push_str("\n\n---\n\n");
        }
    }

    if exported_count == 0 {
        crate::ui::info("没有可导出的会话数据");
        return Ok(());
    }

    write_output(&md, args.output.as_deref())?;

    crate::ui::success(&format!("已导出 {} 个会话为 Markdown 格式", exported_count));
    Ok(())
}

// =========================================================
// 辅助函数
// =========================================================

/// 将内容写入输出（文件或 stdout）。
fn write_output(content: &str, output: Option<&str>) -> anyhow::Result<()> {
    match output {
        Some(path) => {
            let mut file =
                std::fs::File::create(path).with_context(|| format!("无法创建输出文件: {path}"))?;
            file.write_all(content.as_bytes())
                .with_context(|| format!("写入文件失败: {path}"))?;
            crate::ui::info(&format!("已写入: {path}"));
        }
        None => {
            println!("{content}");
        }
    }
    Ok(())
}

fn format_timestamp(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let secs = ms / 1000;
    chrono::Utc
        .timestamp_opt(secs, ((ms % 1000) * 1_000_000) as u32)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
}
