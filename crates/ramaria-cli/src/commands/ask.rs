//! rust/crates/ramaria-cli/src/commands/ask.rs - 单次问答命令
//!
//! 设计特点:
//! - 默认流式输出（逐字打印），--no-stream 切换为完整回复
//! - --json 以 JSON 事件流输出原始 StreamEvent（供脚本消费）
//! - --persona 指定目标 persona
//! - --session 复用已有会话
//! - 自动处理隐私确认（线上 provider）
//! - 错误通过 ui 模块格式化输出

use anyhow::Context;
use futures::StreamExt;
use ramaria_app::SendMessageStream;
use std::sync::Arc;

/// ask 命令参数。
pub struct AskArgs {
    /// 用户输入的消息
    pub message: String,
    /// 指定 persona_uid
    pub persona: Option<String>,
    /// 指定 session_id
    pub session: Option<String>,
    /// 非流式输出（等待完整回复）
    pub no_stream: bool,
    /// JSON 事件流输出
    pub json: bool,
    /// 跳过隐私确认（仅线上 provider 生效）
    pub yes: bool,
}

/// 执行 ask 命令。
pub async fn run(app: &Arc<ramaria_app::App>, args: AskArgs) -> anyhow::Result<()> {
    // Step 1: 隐私确认
    crate::privacy::ensure_privacy(app, args.yes).await?;

    // Step 2: 解析可选参数
    let persona_uid = args.persona.as_deref();
    let session_id = args
        .session
        .as_deref()
        .map(uuid::Uuid::parse_str)
        .transpose()
        .context("无效的 session UUID")?;

    // Step 3: 发送消息
    let stream: SendMessageStream = app
        .send_message(&args.message, persona_uid, session_id)
        .await
        .context("发送消息失败")?;

    // Step 4: 消费流
    if args.json {
        consume_json(stream).await
    } else if args.no_stream {
        consume_full(stream).await
    } else {
        consume_streaming(stream).await
    }
}

/// 流式输出：逐字打印到 stdout。
async fn consume_streaming(mut stream: SendMessageStream) -> anyhow::Result<()> {
    let mut total_chars = 0usize;
    let mut has_error = false;

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => match event {
                ramaria_app::stream_event::StreamEvent::Delta { content, .. } => {
                    crate::ui::write_delta(&content);
                    total_chars += content.chars().count();
                }
                ramaria_app::stream_event::StreamEvent::Done {
                    total_chars: tc, ..
                } => {
                    total_chars = tc;
                }
                ramaria_app::stream_event::StreamEvent::Error { error, .. } => {
                    has_error = true;
                    eprintln!();
                    crate::ui::warn(&format!("LLM 返回错误: {error}"));
                }
                _ => {
                    // StreamEvent 为 #[non_exhaustive]，忽略未知事件类型
                }
            },
            Err(e) => {
                has_error = true;
                crate::ui::print_error(&e);
            }
        }
    }

    if !has_error {
        crate::ui::finish_delta();
    }

    if total_chars == 0 && !has_error {
        crate::ui::warn("LLM 未返回任何内容");
    }

    Ok(())
}

/// 非流式输出：等待完整回复后一次性打印。
async fn consume_full(mut stream: SendMessageStream) -> anyhow::Result<()> {
    let mut full_reply = String::new();
    let mut has_error = false;

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => match event {
                ramaria_app::stream_event::StreamEvent::Delta { content, .. } => {
                    full_reply.push_str(&content);
                }
                ramaria_app::stream_event::StreamEvent::Done { .. } => {}
                ramaria_app::stream_event::StreamEvent::Error { error, .. } => {
                    has_error = true;
                    eprintln!();
                    crate::ui::warn(&format!("LLM 返回错误: {error}"));
                }
                _ => {}
            },
            Err(e) => {
                has_error = true;
                crate::ui::print_error(&e);
            }
        }
    }

    if !has_error {
        println!("{full_reply}");
    }

    if full_reply.is_empty() && !has_error {
        crate::ui::warn("LLM 未返回任何内容");
    }

    Ok(())
}

/// JSON 事件流输出：每行一个 JSON 对象。
async fn consume_json(mut stream: SendMessageStream) -> anyhow::Result<()> {
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                // StreamEvent 未实现 Serialize，使用 Debug 格式输出 JSON-like 表示
                let json = format!("{:?}", event);
                println!("{json}");
            }
            Err(e) => {
                // 流错误也以 JSON 格式输出
                let error_json = serde_json::json!({
                    "type": "StreamError",
                    "error": e.to_string()
                });
                if let Ok(json) = serde_json::to_string(&error_json) {
                    println!("{json}");
                }
            }
        }
    }
    Ok(())
}
