//! rust/crates/ramaria-cli/src/commands/chat.rs - 交互式对话 REPL
//!
//! 设计特点:
//! - 简单 REPL 循环（不做 ratatui）
//! - 支持 /exit、/quit 退出，/clear 清屏，/save 手动保存对话
//! - 启动后台任务（空闲检测 + L2/L3 定时检查），对齐 Python Thread A/B
//! - 退出时自动调用 save_and_close_session（shutdown hook）
//! - 流式输出 AI 回复
//! - Ctrl+C 优雅退出
//! - 错误不中断 REPL

use anyhow::Context;
use futures::StreamExt;
use std::sync::Arc;

/// 启动交互式对话 REPL。
///
/// v1.1 增强:
/// - 启动后台空闲检测 + L2/L3 定时检查
/// - 支持 `/save` 手动保存并关闭当前 session
/// - 退出时自动关闭活跃 session
pub async fn run(app: &Arc<ramaria_app::App>, yes: bool) -> anyhow::Result<()> {
    // 隐私确认
    crate::privacy::ensure_privacy(app, yes).await?;

    // v1.1: 启动后台任务（空闲检测 + L2/L3 定时检查）
    // 对齐 Python `SessionManager.start()` 启动 Thread A + Thread B
    app.start_background_tasks();

    println!();
    crate::ui::separator();
    println!("  Ramaria 对话模式 (v1.1)");
    println!("  输入消息开始对话，/help 查看命令");
    println!(
        "  空闲 {} 分钟后自动保存对话",
        ramaria_core::config::RamariaConfig::default()
            .session
            .l1_idle_minutes
    );
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
            match handle_command(trimmed, app).await {
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
                // 如果是 session 已关闭错误，退出 REPL
                let err_str = e.to_string();
                if err_str.contains("已关闭") || err_str.contains("closed") {
                    crate::ui::warn("当前会话已关闭，输入任意内容将开始新对话。");
                }
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

    // v1.1: 退出时自动保存并关闭活跃 session
    // 对齐 Python `SessionManager.stop()` 的 shutdown hook
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

/// REPL 内置命令的处理结果。
enum CommandAction {
    Continue,
    Exit,
}

/// 处理 REPL 内置命令。
///
/// v1.1 新增 `/save` 命令：手动保存并关闭当前对话。
async fn handle_command(input: &str, app: &Arc<ramaria_app::App>) -> CommandAction {
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
            // v1.1: 手动保存对话（不清屏，next 消息自动创建新 session）
            match app.save_and_close_session(None).await {
                Ok(()) => {
                    println!("── 对话已保存 ──");
                    crate::ui::info("当前对话已保存，下次消息将自动开始新对话。");
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
            println!("    /save             手动保存当前对话（不清屏）");
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
