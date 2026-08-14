//! crates/ramaria-cli/src/commands/session.rs - 会话管理命令
//!
//! 设计特点:
//! - list: 列出所有会话（含状态、消息数、时间）
//! - show: 显示指定会话的完整消息历史
//! - delete: 删除指定会话及其关联消息（需确认；非 TTY 无 --yes 直接失败不挂起）
//! - summarize: 为指定会话重新生成 L1 摘要
//! - --json 输出信封（时间戳 ISO-8601 UTC），文本模式表格化展示

use anyhow::Context;
use ramaria_core::error::RamariaError;
use std::sync::Arc;

/// session 命令的子命令。
pub enum SessionCmd {
    /// 列出所有会话
    List {
        /// 输出条数上限（None = 全部）
        limit: Option<usize>,
        /// 跳过前 N 条（分页）
        offset: usize,
    },
    /// 查看指定会话详情
    Show { session_id: String },
    /// 删除指定会话
    Delete {
        session_id: String,
        /// 强制删除（等同 --yes 双保险）
        force: bool,
    },
    /// 为指定 session 重新生成 L1 摘要
    Summarize {
        session_id: String,
        /// 可选的人格标识
        persona_uid: Option<String>,
    },
}

/// 执行 session 命令。
pub async fn run(
    app: &Arc<ramaria_app::App>,
    cmd: SessionCmd,
    json: bool,
    auto_yes: bool,
) -> anyhow::Result<()> {
    match cmd {
        SessionCmd::List { limit, offset } => list_sessions(app, json, limit, offset).await,
        SessionCmd::Show { session_id } => show_session(app, &session_id, json).await,
        SessionCmd::Delete { session_id, force } => {
            delete_session(app, &session_id, auto_yes || force, json).await
        }
        SessionCmd::Summarize {
            session_id,
            persona_uid,
        } => summarize_session(app, &session_id, persona_uid.as_deref(), json).await,
    }
}

/// 列出所有会话（支持 --limit/--offset 分页）。
async fn list_sessions(
    app: &Arc<ramaria_app::App>,
    json: bool,
    limit: Option<usize>,
    offset: usize,
) -> anyhow::Result<()> {
    let sessions = app
        .storage()
        .list_sessions()
        .await
        .context("查询会话列表失败")?;

    // 分页：先跳过 offset 条，再取 limit 条（limit=None 表示全部）
    let page_limit = limit.unwrap_or(usize::MAX);
    let paged: Vec<_> = sessions.iter().skip(offset).take(page_limit).collect();

    if json {
        let items: Vec<serde_json::Value> = paged
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id.to_string(),
                    "started_at": crate::util::format_timestamp_iso(s.started_at),
                    "ended_at": s.ended_at.and_then(crate::util::format_timestamp_iso),
                    "persona_uid": s.persona_uid,
                    "status": if s.ended_at.is_some() { "ended" } else { "active" },
                })
            })
            .collect();
        let data = serde_json::json!({"sessions": items});
        return crate::json::emit_ok(&data);
    }

    if paged.is_empty() {
        crate::ui::info("暂无会话记录");
        return Ok(());
    }

    println!();
    crate::ui::separator();
    println!("  会话列表（{} 条）", paged.len());
    crate::ui::separator();
    println!();
    println!("  {:<38}  {:<12}  创建时间", "Session ID", "状态");
    println!("  {:-<38}  {:-<12}  {:-<20}", "", "", "");

    for s in &paged {
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
async fn show_session(
    app: &Arc<ramaria_app::App>,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let sid = parse_session_uuid(session_id)?;

    let session = app
        .storage()
        .get_session(sid)
        .await
        .context("查询会话失败")?
        .ok_or_else(|| {
            // 业务校验失败（会话不存在，exit code 4）
            anyhow::anyhow!(RamariaError::validation(format!(
                "会话不存在: {session_id}"
            )))
        })?;

    let messages = app
        .storage()
        .list_messages(sid)
        .await
        .context("查询消息失败")?;

    if json {
        let msg_items: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id.to_string(),
                    "role": format!("{:?}", m.role).to_lowercase(),
                    "content": m.content,
                    "created_at": crate::util::format_timestamp_iso(m.created_at),
                    "source": format!("{:?}", m.source).to_lowercase(),
                })
            })
            .collect();
        let data = serde_json::json!({
            "session": {
                "id": session.id.to_string(),
                "started_at": crate::util::format_timestamp_iso(session.started_at),
                "ended_at": session.ended_at.and_then(crate::util::format_timestamp_iso),
                "persona_uid": session.persona_uid,
            },
            "messages": msg_items,
        });
        return crate::json::emit_ok(&data);
    }

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

/// 解析 session UUID；非法输入为业务校验失败（exit code 4）。
fn parse_session_uuid(session_id: &str) -> anyhow::Result<uuid::Uuid> {
    uuid::Uuid::parse_str(session_id).map_err(|_| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "无效的 session UUID: {session_id}"
        )))
    })
}

/// 删除指定会话。
///
/// 确认规则（M1 B 项）:
/// - `--yes` 自动确认；
/// - 非 TTY 且无 `--yes` 不挂起，直接失败（业务校验失败，exit code 4）。
async fn delete_session(
    app: &Arc<ramaria_app::App>,
    session_id: &str,
    auto_yes: bool,
    json: bool,
) -> anyhow::Result<()> {
    let sid = parse_session_uuid(session_id)?;

    // 确认删除（非 TTY 无 --yes 时 confirm 返回错误 → 映射为业务校验失败）
    let confirmed = crate::ui::confirm(&format!("确认删除会话 {sid}？此操作不可撤销"), auto_yes)
        .map_err(|e| RamariaError::validation(e.to_string()))?;
    if !confirmed {
        if json {
            // 用户主动取消：非错误（ok:true + cancelled 标志，exit 0）
            let data = serde_json::json!({ "session_id": sid.to_string(), "cancelled": true });
            return crate::json::emit_ok(&data);
        }
        crate::ui::info("已取消");
        return Ok(());
    }

    app.storage()
        .delete_session(sid)
        .await
        .context("删除会话失败")?;

    if json {
        let data = serde_json::json!({ "session_id": sid.to_string(), "deleted": true });
        return crate::json::emit_ok(&data);
    }
    crate::ui::success(&format!("会话 {sid} 已删除"));
    Ok(())
}

/// 为指定 session 重新生成 L1 摘要（手动重试）。
///
/// 使用场景:
/// - save_and_close 中 L1 生成失败后的补救。
/// - LLM 服务恢复后的批量补救。
async fn summarize_session(
    app: &Arc<ramaria_app::App>,
    session_id: &str,
    persona_uid: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let sid = parse_session_uuid(session_id)?;

    // 检查 session 存在
    let _session = app
        .storage()
        .get_session(sid)
        .await
        .context("查询会话失败")?
        .ok_or_else(|| {
            anyhow::anyhow!(RamariaError::validation(format!(
                "会话不存在: {session_id}"
            )))
        })?;

    let messages = app
        .storage()
        .list_messages(sid)
        .await
        .context("查询消息失败")?;

    if messages.is_empty() {
        // --json 模式：输出空数据信封（agent 可区分“成功但无数据”与异常，stdout 纯净性不破坏）
        if json {
            let data = serde_json::json!({
                "session_id": sid.to_string(),
                "generated": false,
                "reason": "no_messages",
            });
            return crate::json::emit_ok(&data);
        }
        crate::ui::info("该会话无消息，无法生成摘要");
        return Ok(());
    }

    crate::ui::info(&format!(
        "正在为会话 {} 生成 L1 摘要（{} 条消息）...",
        session_id,
        messages.len()
    ));

    match app.regenerate_l1(sid, persona_uid, None, None).await {
        Ok(Some(l1)) => {
            if json {
                let data = serde_json::json!({
                    "session_id": sid.to_string(),
                    "summary": l1.summary,
                    "keywords": l1.keywords,
                    "atmosphere": l1.atmosphere,
                    "valence": l1.valence,
                    "salience": l1.salience,
                    "created_at": crate::util::format_timestamp_iso(l1.created_at),
                });
                return crate::json::emit_ok(&data);
            }
            crate::ui::success("L1 摘要生成成功");
            println!();
            crate::ui::labeled("摘要", &l1.summary);
            if let Some(ref kw) = l1.keywords {
                crate::ui::labeled("关键词", kw);
            }
            if let Some(ref atm) = l1.atmosphere {
                crate::ui::labeled("氛围", atm);
            }
            crate::ui::labeled("效价", &format!("{:.2}", l1.valence));
            crate::ui::labeled("显著性", &format!("{:.2}", l1.salience));
        }
        Ok(None) => {
            // --json 模式：输出空数据信封（与 messages.is_empty 分支语义一致）
            if json {
                let data = serde_json::json!({
                    "session_id": sid.to_string(),
                    "generated": false,
                    "reason": "no_messages",
                });
                return crate::json::emit_ok(&data);
            }
            crate::ui::warn("该会话无消息，无法生成摘要");
        }
        Err(e) => {
            crate::ui::print_error(&e);
            anyhow::bail!("L1 摘要生成失败: {e}");
        }
    }

    Ok(())
}

// 辅助函数已提取至 crate::util 模块：
// - crate::util::format_timestamp
// - crate::util::truncate
