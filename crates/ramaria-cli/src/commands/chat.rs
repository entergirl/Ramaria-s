//! crates/ramaria-cli/src/commands/chat.rs - 交互式对话 REPL
//!
//! 设计特点:
//! - 简单 REPL 循环（不做 ratatui）
//! - 支持 /exit、/quit 退出，/clear 清屏，/save 手动保存对话
//! - 拉起会话生命周期容器（活跃指针 / 空闲检测 / L2/L3 调度 / 手动关闭）
//! - 退出时自动关闭活跃会话（生命周期容器的关停语义）
//! - 流式输出 AI 回复
//! - Ctrl+C 优雅退出
//! - session 被后台空闲检测关闭后，下次发消息自动创建新 session
//! - 错误不中断 REPL

use anyhow::Context;
use futures::StreamExt;
use ramaria_core::types::Session;
use ramaria_service::{
    ChatStreamHandle, ChatStreamRequest, Engine, Lifecycle, LifecycleOptions, StreamEvent,
};
use std::sync::Arc;

/// 启动交互式对话 REPL。
///
/// 增强:
/// - 启动生命周期容器（空闲检测 + L2/L3 定时检查）
/// - 支持 `/save` 手动保存并关闭当前 session
/// - 退出时自动关闭活跃 session
/// - 空闲超时后自动重建 session（无感切换）
pub async fn run(engine: &Arc<Engine>, yes: bool) -> anyhow::Result<()> {
    // 隐私确认
    crate::privacy::ensure_privacy(engine, yes).await?;

    // 加载检索索引（CLI 单命令进程，启动时检索器为空；失败降级不阻塞）
    crate::commands::ask::ensure_retriever_loaded(engine).await;

    // 拉起会话生命周期（活跃指针 / 空闲检查 / L2/L3 调度），与桌面宿主同一套实现
    let lifecycle = engine.start_lifecycle(LifecycleOptions::desktop());

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
    let mut session = engine.create_session(None).await.context("创建会话失败")?;

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
            match handle_command(trimmed, engine, &lifecycle, &mut session).await {
                CommandAction::Continue => {}
                CommandAction::Exit => break,
            }
            continue;
        }

        // 发送消息（含自动重建逻辑）
        let mut handle = match try_send_or_recreate(engine, &lifecycle, trimmed, &mut session).await
        {
            Ok(handle) => handle,
            Err(e) => {
                crate::ui::print_error(&e);
                continue;
            }
        };

        // 流式输出 AI 回复（|| 自动替换为换行）
        print!("\n\x1b[32mAI:\x1b[0m ");
        let mut has_content = false;
        let mut formatter = crate::ui::PersonaFormatter::new();

        while let Some(event_result) = handle.events.next().await {
            match event_result {
                Ok(event) => match event {
                    StreamEvent::Delta { content, .. } => {
                        let formatted = formatter.feed(&content);
                        if !formatted.is_empty() {
                            crate::ui::write_delta(&formatted);
                        }
                        has_content = true;
                    }
                    StreamEvent::Done { .. } => {}
                    StreamEvent::Error { error, .. } => {
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

    // 退出时自动保存并关闭活跃 session（生命周期容器的关停语义）
    if let Err(e) = lifecycle.close_active_session().await {
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
/// - `Ok(handle)`: 消息已发送，返回事件流句柄。
/// - `Err`: 两次尝试均失败（含新 session 创建失败）。
async fn try_send_or_recreate(
    engine: &Arc<Engine>,
    lifecycle: &Arc<Lifecycle>,
    input: &str,
    session: &mut Session,
) -> Result<ChatStreamHandle, ramaria_core::error::RamariaError> {
    // 第一次尝试：使用当前 session
    match send_stream(engine, lifecycle, input, session.id).await {
        Ok(handle) => return Ok(handle),
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
    let new_session = engine.create_session(None).await.map_err(|e| {
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

    // 第二次尝试：使用新 session
    send_stream(engine, lifecycle, input, session.id).await
}

/// 发送一条消息并登记活跃指针（生命周期容器侧）。
///
/// 说明:
/// - 事件流句柄的会话归属由宿主登记为活跃会话并刷新活跃时间
///   （服务层不反向持有生命周期容器）。
async fn send_stream(
    engine: &Arc<Engine>,
    lifecycle: &Arc<Lifecycle>,
    input: &str,
    session_id: uuid::Uuid,
) -> Result<ChatStreamHandle, ramaria_core::error::RamariaError> {
    let handle = engine
        .chat_stream(ChatStreamRequest {
            message: input.to_string(),
            persona: None,
            session_id: Some(session_id),
            seed_history: Vec::new(),
            config_override: None,
        })
        .await?;
    lifecycle.set_active_session_id(Some(handle.session_id));
    lifecycle.touch_session(handle.session_id);
    Ok(handle)
}

/// REPL 内置命令的处理结果。
enum CommandAction {
    Continue,
    Exit,
}

/// 处理 REPL 内置命令。
///
/// `/save` 命令：手动保存并关闭当前对话。
/// `/save` 后 session 被更新为新会话（下次消息直接使用），
/// 新建失败时保持原 session，`try_send_or_recreate` 会在下次发消息时自动重试。
async fn handle_command(
    input: &str,
    engine: &Arc<Engine>,
    lifecycle: &Arc<Lifecycle>,
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
            match lifecycle.close_active_session().await {
                Ok(_) => {
                    println!("── 对话已保存 ──");
                    crate::ui::info("当前对话已保存，下次消息将自动开始新对话。");
                    // 尝试创建新 session 以便下次消息直接使用
                    match engine.create_session(None).await {
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
