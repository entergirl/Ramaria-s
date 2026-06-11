//! rust/crates/ramaria-cli/src/commands/chat.rs - 交互式对话 REPL
//!
//! 设计特点:
//! - 简单 REPL 循环（不做 ratatui）
//! - 支持 /exit、/quit 退出，/clear 清屏
//! - 自动维护 session 上下文
//! - 流式输出 AI 回复
//! - Ctrl+C 优雅退出
//! - 错误不中断 REPL

use anyhow::Context;
use futures::StreamExt;
use std::sync::Arc;

/// 启动交互式对话 REPL。
pub async fn run(app: &Arc<ramaria_app::App>, yes: bool) -> anyhow::Result<()> {
    // 隐私确认
    crate::privacy::ensure_privacy(app, yes).await?;

    println!();
    crate::ui::separator();
    println!("  Ramaria 对话模式");
    println!("  输入消息开始对话，/help 查看命令");
    crate::ui::separator();
    println!();

    // 创建新 session
    let session = app
        .storage()
        .create_session()
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
            match handle_command(trimmed) {
                CommandAction::Continue => {}
                CommandAction::Exit => break,
            }
            continue;
        }

        // 发送消息
        let mut stream = match app.send_message(trimmed, None, Some(session.id)).await {
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
    crate::ui::info(&format!(
        "会话 {} 已结束。使用 `ramaria session show {}` 查看记录。",
        session.id, session.id
    ));
    Ok(())
}

/// REPL 内置命令的处理结果。
enum CommandAction {
    Continue,
    Exit,
}

/// 处理 REPL 内置命令。
fn handle_command(input: &str) -> CommandAction {
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
        "/help" | "/?" => {
            println!("  可用命令：");
            println!("    /exit, /quit, /q  退出对话");
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
