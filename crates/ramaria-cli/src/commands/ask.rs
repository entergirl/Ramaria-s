//! crates/ramaria-cli/src/commands/ask.rs - 单次问答命令
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
        if args.no_stream {
            // JSON + 非流式：聚合为单个 done 事件（含 reply/session_id/total_chars）
            consume_json_aggregate(stream).await
        } else {
            // JSON 事件流：每行一个 `{"type":"delta|done|error",...}`
            consume_json(stream).await
        }
    } else if args.no_stream {
        consume_full(stream).await
    } else {
        consume_streaming(stream).await
    }
}

/// 流式输出：逐字打印到 stdout，`||` 自动替换为换行（人格短句渲染）。
async fn consume_streaming(mut stream: SendMessageStream) -> anyhow::Result<()> {
    let mut total_chars = 0usize;
    let mut has_error = false;
    let mut formatter = crate::ui::PersonaFormatter::new();

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => match event {
                ramaria_app::stream_event::StreamEvent::Delta { content, .. } => {
                    // 通过 PersonaFormatter 处理 || → 换行
                    let formatted = formatter.feed(&content);
                    if !formatted.is_empty() {
                        crate::ui::write_delta(&formatted);
                    }
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

    // 刷新 formatter 中可能残留的孤 | 字符
    if let Some(remnant) = formatter.flush() {
        crate::ui::write_delta(&remnant);
    }

    if !has_error {
        crate::ui::finish_delta();
    }

    if total_chars == 0 && !has_error {
        crate::ui::warn("LLM 未返回任何内容");
    }

    Ok(())
}

/// 非流式输出：等待完整回复后一次性打印。`||` 替换为换行。
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
        // 完整回复直接替换 || → 换行
        println!("{}", full_reply.replace("||", "\n"));
    }

    if full_reply.is_empty() && !has_error {
        crate::ui::warn("LLM 未返回任何内容");
    }

    Ok(())
}

/// JSON 事件流输出：每行一个 JSON 对象（`{"type":"delta|done|error",...}`）。
///
/// StreamEvent 已实现 Serialize（统一信封 schema 的流式形态，见 docs/dev-1.5/v1.5-decisions.md §D-V15-011），
/// 输出为合法 JSON（修复 v1.4 用 Debug 格式输出非合法 JSON 的问题）。
async fn consume_json(mut stream: SendMessageStream) -> anyhow::Result<()> {
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => match serialize_event_line(&event) {
                Some(line) => println!("{line}"),
                None => crate::ui::warn("事件序列化失败"),
            },
            Err(e) => {
                // 流错误以 JSON 事件形式输出
                let error_json = serde_json::json!({
                    "type": "error",
                    "error": e.to_string(),
                });
                if let Ok(json) = serde_json::to_string(&error_json) {
                    println!("{json}");
                }
            }
        }
    }
    Ok(())
}

/// 将 StreamEvent 序列化为单行 JSON 事件（`ask --json` 事件流的一行）。
///
/// 返回:
/// - `Some(line)`: 合法 JSON 行（含 `type` 标签）。
/// - `None`: 序列化失败（不应发生，StreamEvent 为简单结构）。
fn serialize_event_line(event: &ramaria_app::stream_event::StreamEvent) -> Option<String> {
    serde_json::to_string(event).ok()
}

/// JSON + 非流式：消费完整流后聚合为单个 done 事件。
///
/// 输出形态: `{"type":"done","reply":"…","session_id":"…","total_chars":N}`。
/// 流中出现错误时输出 error 事件且不输出 done（与事件流语义一致）。
async fn consume_json_aggregate(mut stream: SendMessageStream) -> anyhow::Result<()> {
    use ramaria_app::stream_event::StreamEvent;

    let mut full_reply = String::new();
    let mut session_id: Option<String> = None;
    let mut total_chars = 0usize;
    let mut has_error = false;

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => match event {
                StreamEvent::Delta { content, .. } => {
                    full_reply.push_str(&content);
                }
                StreamEvent::Done {
                    session_id: sid,
                    total_chars: tc,
                    ..
                } => {
                    session_id = sid.map(|s| s.to_string());
                    total_chars = tc;
                }
                StreamEvent::Error { error, .. } => {
                    has_error = true;
                    let error_json = serde_json::json!({
                        "type": "error",
                        "error": error,
                    });
                    if let Ok(json) = serde_json::to_string(&error_json) {
                        println!("{json}");
                    }
                }
                _ => {
                    // StreamEvent 为 #[non_exhaustive]，忽略未知事件类型
                }
            },
            Err(e) => {
                has_error = true;
                let error_json = serde_json::json!({
                    "type": "error",
                    "error": e.to_string(),
                });
                if let Ok(json) = serde_json::to_string(&error_json) {
                    println!("{json}");
                }
            }
        }
    }

    if !has_error {
        let done = serde_json::json!({
            "type": "done",
            "reply": full_reply,
            "session_id": session_id,
            "total_chars": total_chars,
        });
        if let Ok(line) = serde_json::to_string(&done) {
            println!("{line}");
        }
    }

    Ok(())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_app::stream_event::StreamEvent;
    use uuid::Uuid;

    /// `ask --json` 事件流每行必须是合法 JSON 且带 type 标签。
    #[test]
    fn serialize_event_line_is_valid_json() {
        let id = Uuid::new_v4();
        let events = [
            StreamEvent::delta(id, "你好".into()),
            StreamEvent::done(id, Some(Uuid::new_v4()), Some("stop".into()), 2),
            StreamEvent::error(id, "连接超时".into()),
        ];
        for event in &events {
            let line = serialize_event_line(event).expect("事件必须可序列化");
            let parsed: serde_json::Value = serde_json::from_str(&line).expect("必须是合法 JSON");
            let t = parsed["type"].as_str().expect("必须含 type 标签");
            assert!(
                matches!(t, "delta" | "done" | "error"),
                "type 必须是 delta/done/error，实际: {t}"
            );
        }
    }

    /// 聚合输出（json + no_stream）的 done 事件含 reply/session_id/total_chars。
    #[test]
    fn aggregate_done_shape() {
        let done = serde_json::json!({
            "type": "done",
            "reply": "完整回复",
            "session_id": Some("11111111-1111-1111-1111-111111111111"),
            "total_chars": 4,
        });
        let line = serde_json::to_string(&done).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"], "done");
        assert_eq!(parsed["reply"], "完整回复");
        assert_eq!(parsed["session_id"], "11111111-1111-1111-1111-111111111111");
        assert_eq!(parsed["total_chars"], 4);
    }
}
