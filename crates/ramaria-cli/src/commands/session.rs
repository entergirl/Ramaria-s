//! rust/crates/ramaria-cli/src/commands/session.rs - 会话管理命令
//!
//! 设计特点:
//! - list: 列出所有会话（含状态、消息数、时间）
//! - show: 显示指定会话的完整消息历史
//! - delete: 删除指定会话及其关联消息
//! - 表格化展示，支持截断长消息

use anyhow::Context;
use std::sync::Arc;

/// session 命令的子命令。
pub enum SessionCmd {
    /// 列出所有会话
    List,
    /// 查看指定会话详情
    Show { session_id: String },
    /// 删除指定会话
    Delete { session_id: String },
}

/// 执行 session 命令。
pub async fn run(app: &Arc<ramaria_app::App>, cmd: SessionCmd) -> anyhow::Result<()> {
    match cmd {
        SessionCmd::List => list_sessions(app).await,
        SessionCmd::Show { session_id } => show_session(app, &session_id).await,
        SessionCmd::Delete { session_id } => delete_session(app, &session_id).await,
    }
}

/// 列出所有会话。
async fn list_sessions(app: &Arc<ramaria_app::App>) -> anyhow::Result<()> {
    let sessions = app
        .storage()
        .list_sessions()
        .await
        .context("查询会话列表失败")?;

    if sessions.is_empty() {
        crate::ui::info("暂无会话记录");
        return Ok(());
    }

    println!();
    crate::ui::separator();
    println!("  会话列表（{} 条）", sessions.len());
    crate::ui::separator();
    println!();
    println!("  {:<38}  {:<12}  创建时间", "Session ID", "状态");
    println!("  {:-<38}  {:-<12}  {:-<20}", "", "", "");

    for s in &sessions {
        let status = if s.ended_at.is_some() {
            "已结束"
        } else {
            "进行中"
        };
        let time =
            crate::util::format_timestamp(s.started_at).unwrap_or_else(|| "未知".to_string());
        println!("  {}  {:<12}  {}", s.id, status, time);
    }

    Ok(())
}

/// 查看指定会话的消息历史。
async fn show_session(app: &Arc<ramaria_app::App>, session_id: &str) -> anyhow::Result<()> {
    let sid = uuid::Uuid::parse_str(session_id).context("无效的 session UUID")?;

    let session = app
        .storage()
        .get_session(sid)
        .await
        .context("查询会话失败")?
        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;

    let messages = app
        .storage()
        .list_messages(sid)
        .await
        .context("查询消息失败")?;

    println!();
    crate::ui::separator();
    println!("  会话: {}", session.id);
    crate::ui::labeled(
        "状态",
        if session.ended_at.is_some() {
            "已结束"
        } else {
            "进行中"
        },
    );
    if let Some(ts) = crate::util::format_timestamp(session.started_at) {
        crate::ui::labeled("创建时间", &ts);
    }
    crate::ui::labeled("消息数", &messages.len().to_string());
    crate::ui::separator();

    if messages.is_empty() {
        crate::ui::info("该会话暂无消息");
        return Ok(());
    }

    for msg in &messages {
        let role_icon = match msg.role {
            ramaria_core::types::MessageRole::User => "\x1b[36m👤 用户\x1b[0m",
            ramaria_core::types::MessageRole::Assistant => "\x1b[32m🤖 AI\x1b[0m",
            ramaria_core::types::MessageRole::System => "\x1b[33m⚙ 系统\x1b[0m",
            _ => "\x1b[37m❓ 未知\x1b[0m",
        };

        println!();
        println!("  {role_icon}");

        // 截断长消息
        let content = crate::util::truncate(&msg.content, 200);
        for line in content.lines() {
            println!("    {line}");
        }
    }

    Ok(())
}

/// 删除指定会话。
async fn delete_session(app: &Arc<ramaria_app::App>, session_id: &str) -> anyhow::Result<()> {
    let sid = uuid::Uuid::parse_str(session_id).context("无效的 session UUID")?;

    // 确认删除
    let confirmed = crate::ui::confirm(&format!("确认删除会话 {sid}？此操作不可撤销"));
    if !confirmed? {
        crate::ui::info("已取消");
        return Ok(());
    }

    app.storage()
        .delete_session(sid)
        .await
        .context("删除会话失败")?;

    crate::ui::success(&format!("会话 {sid} 已删除"));
    Ok(())
}

// 辅助函数已提取至 crate::util 模块：
//   - crate::util::format_timestamp()
//   - crate::util::truncate()
