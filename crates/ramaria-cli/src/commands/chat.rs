//! rust/crates/ramaria-cli/src/commands/chat.rs - 交互式对话 REPL
//!
//! 设计特点:
//! - 简单 REPL 循环（不做 ratatui）
//! - 支持 /exit、/quit 退出，/clear 清屏，/save 手动保存对话
//! - 启动后台任务（空闲检测 + L2/L3 定时检查），对齐 Python Thread A/B
//! - 退出时自动调用 save_and_close_session（shutdown hook）
//! - 流式输出 AI 回复
//! - Ctrl+C 优雅退出
//! - session 被后台空闲检测关闭后，下次发消息自动创建新 session
//! - 错误不中断 REPL

use anyhow::Context;
use futures::StreamExt;
use ramaria_core::types::Session;
use std::sync::Arc;

/// 启动交互式对话 REPL。
///
/// 增强:
/// - 启动后台空闲检测 + L2/L3 定时检查
/// - 支持 `/save` 手动保存并关闭当前 session
/// - 退出时自动关闭活跃 session
/// - 空闲超时后自动重建 session（无感切换）
pub async fn run(app: &Arc<ramaria_app::App>, yes: bool) -> anyhow::Result<()> {
    // 隐私确认
    crate::privacy::ensure_privacy(app, yes).await?;

    // 启动后台任务（空闲检测 + L2/L3 定时检查）
    // 对齐 Python `SessionManager.start` 启动 Thread A + Thread B
    app.start_background_tasks();

    println!();
    crate::ui::separator();
    println!("  Ramaria 对话模式 ");
    println!("  输入消息开始对话，/help 查看命令");
    println!(
        "  空闲 {} 分钟后自动保存对话",
        ramaria_core::config::RamariaConfig::default()
            .session
            .l1_idle_minutes
    );
    crate::ui::separator();
    println!();

    // 创建新 session（mutable：空闲关闭后自动重建）
    let mut session = app
        .storage()
        .create_session(None)
        .await
        .context("创建会话失败")?;

    tracing::info!(session_id = %session.id, "REPL 会话已创建");

    loop {
        // 显示提示符
        let input = match crate::ui::read_line("\n\x1b[36m你:\x1b[0m") {
            Ok(line) => line,
            Err(e) => {
                crate::ui::warn(&format!("读取输入失败: {e}"));
                break;
            }
        };

        let trimmed = input.trim();

        // 空输入跳过
        if trimmed.is_empty() {
            continue;
        }

        // 处理内置命令
        if trimmed.starts_with('/') {
            match handle_command(trimmed, app, &mut session).await {
                CommandAction::Continue => {}
                CommandAction::Exit => break,
            }
            continue;
        }

        // 发送消息（含自动重建逻辑）
        let mut stream = match try_send_or_recreate(app, trimmed, &mut session).await {
            Ok(s) => s,
            Err(e) => {
                crate::ui::print_error(&e);
                continue;
            }
        };

        // 流式输出 AI 回复（|| 自动替换为换行）
        print!("\n\x1b[32mAI:\x1b[0m ");
        let mut has_content = false;
        let mut formatter = crate::ui::PersonaFormatter::new();

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(event) => match event {
                    ramaria_app::stream_event::StreamEvent::Delta { content, .. } => {
                        let formatted = formatter.feed(&content);
                        if !formatted.is_empty() {
                            crate::ui::write_delta(&formatted);
                        }
                        has_content = true;
                    }
                    ramaria_app::stream_event::StreamEvent::Done { .. } => {}
                    ramaria_app::stream_event::StreamEvent::Error { error, .. } => {
                        eprintln!();
                        crate::ui::warn(&format!("LLM 错误: {error}"));
                    }
                    _ => {}
                },
                Err(e) => {
                    eprintln!();
                    crate::ui::print_error(&e);
                }
            }
        }

        // 刷新残留字符
        if let Some(remnant) = formatter.flush() {
            crate::ui::write_delta(&remnant);
        }

        if has_content {
            println!();
        }
    }

    println!();

    // 退出时自动保存并关闭活跃 session
    // 对齐 Python `SessionManager.stop` 的 shutdown hook
    if let Err(e) = app.save_and_close_session(None).await {
        crate::ui::warn(&format!("退出时保存对话失败: {e}"));
    } else {
        crate::ui::info("对话已保存。");
    }

    crate::ui::info(&format!(
        "使用 `ramaria session show {}` 查看记录。",
        session.id
    ));
    Ok(())
}

/// 尝试发送消息到当前 session。
///
/// 若 session 已被后台空闲检测关闭，自动创建新 session 并重试一次。
/// 对齐 Python REPL 中 session 关闭后自动重建的行为。
///
/// 返回:
/// - `Ok(stream)`: 消息已发送，返回流式响应。
/// - `Err`: 两次尝试均失败（含新 session 创建失败）。
async fn try_send_or_recreate(
    app: &Arc<ramaria_app::App>,
    input: &str,
    session: &mut Session,
) -> Result<ramaria_app::SendMessageStream, ramaria_core::error::RamariaError> {
    // 第一次尝试：使用当前 session
    match app.send_message(input, None, Some(session.id)).await {
        Ok(stream) => return Ok(stream),
        Err(e) => {
            let err_str = e.to_string();
            // 仅当 session 已关闭时才自动重建（其他错误直接返回）
            if !err_str.contains("已关闭") && !err_str.contains("closed") {
                return Err(e);
            }
            // Session 被空闲检测关闭 → 自动创建新 session 并重试
            tracing::info!(
                old_session_id = %session.id,
                "REPL 检测到 session 已关闭（空闲超时），自动创建新 session"
            );
        }
    }

    // 重建 session
    let new_session = app.storage().create_session(None).await.map_err(|e| {
        ramaria_core::error::RamariaError::storage(format!(
            "创建新会话失败（原 session {} 已关闭）: {e}",
            session.id
        ))
    })?;

    let old_id = session.id;
    *session = new_session;

    crate::ui::info(&format!(
        "会话 {} 已自动保存，新会话 {} 已创建。",
        &old_id.to_string()[..8],
        &session.id.to_string()[..8]
    ));

    tracing::info!(
        old_session_id = %old_id,
        new_session_id = %session.id,
        "REPL 自动重建 session 完成，重试发送消息"
    );

    // 第二次尝试：使用新 session 重试
    app.send_message(input, None, Some(session.id)).await
}

/// REPL 内置命令的处理结果。
enum CommandAction {
    Continue,
    Exit,
}

/// 处理 REPL 内置命令。
///
/// `/save` 命令：手动保存并关闭当前对话。
/// `/save` 后 session 被更新为待重建状态（id 不变但已关闭），
/// 下次发消息时 `try_send_or_recreate` 自动创建新 session。
async fn handle_command(
    input: &str,
    app: &Arc<ramaria_app::App>,
    session: &mut Session,
) -> CommandAction {
    match input {
        "/exit" | "/quit" | "/q" => {
            println!("再见！");
            CommandAction::Exit
        }
        "/clear" => {
            // 清屏
            print!("\x1b[2J\x1b[H");
            CommandAction::Continue
        }
        "/save" => {
            // 手动保存对话（不清屏，next 消息自动创建新 session）
            let old_sid = session.id;
            match app.save_and_close_session(None).await {
                Ok(()) => {
                    println!("── 对话已保存 ──");
                    crate::ui::info("当前对话已保存，下次消息将自动开始新对话。");
                    // 尝试创建新 session 以便下次消息直接使用
                    match app.storage().create_session(None).await {
                        Ok(new_s) => {
                            *session = new_s;
                            tracing::info!(
                                old_session_id = %old_sid,
                                new_session_id = %session.id,
                                "/save 后自动创建新 session"
                            );
                        }
                        Err(e) => {
                            crate::ui::warn(&format!("创建新会话失败: {e}，下次消息时将自动重试"));
                        }
                    }
                }
                Err(e) => {
                    crate::ui::warn(&format!("保存对话失败: {e}"));
                }
            }
            CommandAction::Continue
        }
        "/help" | "/?" => {
            println!("  可用命令：");
            println!("    /exit, /quit, /q  退出对话");
            println!("    /save             手动保存当前对话（自动创建新对话）");
            println!("    /clear            清屏");
            println!("    /help, /?          显示帮助");
            println!("  直接输入文本即可与 AI 对话。");
            CommandAction::Continue
        }
        other => {
            crate::ui::warn(&format!("未知命令: {other}。输入 /help 查看帮助。"));
            CommandAction::Continue
        }
    }
}
