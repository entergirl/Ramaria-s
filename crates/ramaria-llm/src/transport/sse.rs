//! crates/ramaria-llm/src/transport/sse.rs - SSE 流式读取与解析
//!
//! 设计特点:
//! - `bytes_stream` 逐块读取，`BytesMut` 缓冲区拼接跨 chunk 的不完整行
//! - 有界 channel 背压发送：channel 满时等待接收端消费，不丢弃 delta
//! - 分级超时：首事件 60s 快速失败 + 整体 600s 兜底长流
//! - SSE 单行 > 10KB 截断并 warn，防止异常服务器无换行超大 chunk 撑爆缓冲
//! - 流内 error 载荷上抛为错误，不被静默吞成空 delta

use bytes::BytesMut;
use futures::SinkExt;
use futures::Stream;
use futures::channel::mpsc;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StreamDelta;

// =========================================================
// SSE 保护常量
// =========================================================

/// SSE 单行最大字节数（10KB）。
///
/// 说明:
/// - 正常 SSE `data:` 行通常 < 1KB
/// - 超过此限制的行将被截断，记录 warn 日志
/// - 防止异常服务器返回无换行的超大 chunk 导致 `BytesMut` 无限增长
const SSE_MAX_LINE_BYTES: usize = 10 * 1024;

/// 流式读取整体超时秒数（600s = 10 分钟）。
///
/// 说明:
/// - 从首次接收到 HTTP 响应到流结束的总时间上限。
/// - v1.6 固定 120s 会截断长生成（长回复 + 慢服务端可超过 2 分钟）；
///   提升到 600s 覆盖绝大多数长回复（决策 D-V17-013 / 备忘 §二 15）。
/// - 超时后发送错误事件并退出，防止服务端挂起导致资源泄漏。
const SSE_STREAM_TIMEOUT_SECS: u64 = 600;

/// 流式首事件超时秒数（60s）。
///
/// 说明:
/// - 服务端在 60s 内未发送任何 SSE 事件（无首包）→ 视为挂起，快速报错退出。
/// - 与整体超时分级：首包超时快速失败，整体超时（`SSE_STREAM_TIMEOUT_SECS`）
///   兜底长流——长生成只受整体超时约束，首包等待不拖慢正常长流。
const SSE_FIRST_EVENT_TIMEOUT_SECS: u64 = 60;

// =========================================================
// SSE 读取循环（后台 tokio 任务）
// =========================================================

/// 使用 `mpsc::Sender`（有界 channel）替代 `UnboundedSender`。
///
/// 背压语义（决策 D-V17-013 / 备忘 §二 15）:
/// - channel 满时 `await send()` 阻塞等待接收端消费（背压），**不丢弃 delta**——
///   消费慢时暂停 SSE 读取，由 TCP 窗口把压力回传服务端，杜绝流内容静默缺失。
/// - 接收端 drop stream 时 `send` 返回 Disconnected 错误，停止读取（静默退出）。
///
/// 设计:
/// - 使用 `BytesMut` 缓冲区拼接跨 chunk 的不完整行。
/// - 遇到 `\n` 时切割一行，调用 `parse_sse_line` 解析。
/// - `data: [DONE]` 时发送 `done=true` 的 Delta 后退出。
/// - 单行 > `SSE_MAX_LINE_BYTES` 时截断并记 warn。
/// - 分级超时：首事件 60s（服务端无首包即挂起，快速失败）+ 整体 600s（兜底长流）。
///
/// 参数:
/// - `byte_stream`: HTTP 响应体字节流。
/// - `tx`: 有界事件发送通道（容量 64）。
pub(crate) async fn sse_read_loop(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
    tx: mpsc::Sender<RamariaResult<StreamDelta>>,
) {
    sse_read_loop_inner(
        byte_stream,
        tx,
        None,
        std::time::Duration::from_secs(SSE_FIRST_EVENT_TIMEOUT_SECS),
        std::time::Duration::from_secs(SSE_STREAM_TIMEOUT_SECS),
    )
    .await;
}

/// SSE 读取循环内部实现——支持可选的 request_id 与分级超时参数（测试可注入短超时）。
pub(crate) async fn sse_read_loop_inner(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
    mut tx: mpsc::Sender<RamariaResult<StreamDelta>>,
    request_id: Option<String>,
    first_event_timeout: std::time::Duration,
    stream_timeout: std::time::Duration,
) {
    // 整体超时保护：覆盖从首包到流结束的总时长（长生成兜底）
    let timeout_result = tokio::time::timeout(
        stream_timeout,
        sse_read_core(
            byte_stream,
            &mut tx,
            request_id.as_deref(),
            first_event_timeout,
        ),
    )
    .await;

    match timeout_result {
        Ok(_) => {
            // 正常完成（首事件超时/流错误已在 sse_read_core 内部发送错误事件）
        }
        Err(_elapsed) => {
            // 整体超时：发送错误事件后退出
            let rid = request_id.as_deref().unwrap_or("unknown");
            tracing::warn!(
                request_id = %rid,
                timeout_secs = stream_timeout.as_secs(),
                "SSE 流式读取整体超时"
            );
            let _ = send_event(
                &mut tx,
                Err(RamariaError::llm(format!(
                    "SSE 流式读取超时（{}s），服务端可能已挂起",
                    stream_timeout.as_secs()
                ))),
            )
            .await;
        }
    }
}

/// SSE 核心读取逻辑——逐 chunk 读取、逐行解析、通过 channel 发送。
///
/// 职责:
/// - 从 `byte_stream` 逐块读取 HTTP 响应体。
/// - 使用 `BytesMut` 缓冲区拼接跨 chunk 的不完整行。
/// - 逐行调用 `parse_sse_line` 解析 SSE 格式。
/// - P-7: 单行 > `SSE_MAX_LINE_BYTES` 时截断并 warn。
/// - 首事件超时（`first_event_timeout`）：服务端无首包 → 快速失败（挂起防护）。
/// - 背压发送（`send_event`）：channel 满时等待接收端消费，**不丢弃 delta**。
///
/// `send_event` 需要 `&mut self`，因此 `tx` 声明为 `&mut mpsc::Sender`。
async fn sse_read_core(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
    tx: &mut mpsc::Sender<RamariaResult<StreamDelta>>,
    request_id: Option<&str>,
    first_event_timeout: std::time::Duration,
) {
    use futures::StreamExt;

    futures::pin_mut!(byte_stream);
    let mut buffer = BytesMut::new();

    // 首事件超时（分级超时之一）：服务端在 first_event_timeout 内未发送任何数据视为挂起。
    // 整体超时（长流兜底）由 sse_read_loop_inner 包裹。
    match tokio::time::timeout(first_event_timeout, byte_stream.next()).await {
        Ok(Some(chunk_result)) => {
            if process_chunk(chunk_result, &mut buffer, tx, request_id).await {
                return;
            }
        }
        Ok(None) => {
            // 流立即结束（无任何数据）→ 跳过循环，由尾部逻辑发合成 done
        }
        Err(_elapsed) => {
            let rid = request_id.unwrap_or("unknown");
            tracing::warn!(
                request_id = %rid,
                first_event_timeout_secs = first_event_timeout.as_secs(),
                "SSE 首事件超时（服务端未发送任何数据），视为挂起"
            );
            let _ = send_event(
                tx,
                Err(RamariaError::llm(format!(
                    "SSE 首事件超时（{}s），服务端可能已挂起",
                    first_event_timeout.as_secs()
                ))),
            )
            .await;
            return;
        }
    }

    // 后续块：正常逐块读取（整体超时由外层 sse_read_loop_inner 兜底）
    while let Some(chunk_result) = byte_stream.next().await {
        if process_chunk(chunk_result, &mut buffer, tx, request_id).await {
            return;
        }
    }

    // 流意外结束（未收到 [DONE]）：发送剩余内容
    let mut done_already_sent = false;
    if !buffer.is_empty() {
        let leftover = String::from_utf8_lossy(&buffer);
        if let Some(delta_result) = parse_sse_line(&leftover) {
            // 检查残余缓冲区解析结果是否已包含 done 信号
            // 例如最后一个 chunk 恰好包含 finish_reason，则无需再发合成 Done
            if let Ok(ref delta) = delta_result {
                done_already_sent = delta.done;
            }
            if send_event(tx, delta_result).await {
                return;
            }
        }
    }
    // 仅当流中未发送 done 时才发送合成 done 信号，避免双重 Done
    // （函数即将结束，无需处理断开返回值）
    if !done_already_sent {
        let _ = send_event(
            tx,
            Ok(StreamDelta {
                content: String::new(),
                done: true,
                metadata: Some("stream_ended_without_done".to_string()),
            }),
        )
        .await;
    }
}

/// 背压式发送事件到有界 channel。
///
/// 策略（决策 D-V17-013 / 备忘 §二 15）:
/// - channel 满时 `await send()` 阻塞等待接收端消费（背压），**不丢弃 delta**——
///   消费慢时暂停 SSE 读取，由 TCP 窗口把压力回传服务端，杜绝流内容静默缺失。
/// - 接收端已 drop（send 返回 Disconnected）→ 停止读取。
///
/// 返回:
/// - `true`: 接收端已断开/发送失败，应停止读取。
/// - `false`: 发送成功，继续读取。
async fn send_event(
    tx: &mut mpsc::Sender<RamariaResult<StreamDelta>>,
    item: RamariaResult<StreamDelta>,
) -> bool {
    match tx.send(item).await {
        Ok(()) => false,
        Err(e) if e.is_disconnected() => {
            tracing::debug!("SSE 接收端已断开，停止读取");
            true
        }
        Err(e) => {
            // 其余 send 错误（极少见）按断开处理，避免死循环
            tracing::warn!("SSE channel send 失败（{e}），停止读取");
            true
        }
    }
}

/// 处理单个 HTTP chunk：追加到缓冲区并逐行解析 SSE。
///
/// 返回:
/// - `true`: 应停止读取（done 已发送 / 接收端断开 / 流错误）。
/// - `false`: 继续读取。
async fn process_chunk(
    chunk_result: Result<bytes::Bytes, reqwest::Error>,
    buffer: &mut BytesMut,
    tx: &mut mpsc::Sender<RamariaResult<StreamDelta>>,
    request_id: Option<&str>,
) -> bool {
    match chunk_result {
        Ok(chunk) => {
            buffer.extend_from_slice(&chunk);

            // P-7: 检查缓冲区是否超过 SSE_MAX_LINE_BYTES 且无换行符
            // 异常服务器可能持续发送无换行的超大单行数据
            if buffer.len() > SSE_MAX_LINE_BYTES && !buffer.contains(&b'\n') {
                let rid = request_id.unwrap_or("unknown");
                tracing::warn!(
                    request_id = %rid,
                    buffer_len = buffer.len(),
                    max_line_bytes = SSE_MAX_LINE_BYTES,
                    "SSE 缓冲区超过行长度上限且无换行符，截断缓冲区以防止内存无限增长"
                );
                // 截断缓冲区到安全大小，丢弃溢出数据
                buffer.truncate(SSE_MAX_LINE_BYTES);
                // 在截断处插入换行符，强制触发行解析
                buffer.extend_from_slice(b"\n");
            }

            // 逐行解析
            while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                let line_bytes = buffer.split_to(pos + 1);
                // 去除尾部 \r\n → 保留纯内容
                let len = line_bytes.len();
                let content = if len >= 2 && line_bytes[len - 2] == b'\r' {
                    &line_bytes[..len - 2]
                } else {
                    &line_bytes[..len - 1]
                };

                // P-7: 单行长度保护——正常 SSE data 行 < 1KB，超大行视为异常
                if content.len() > SSE_MAX_LINE_BYTES {
                    let rid = request_id.unwrap_or("unknown");
                    tracing::warn!(
                        request_id = %rid,
                        line_len = content.len(),
                        max_line_bytes = SSE_MAX_LINE_BYTES,
                        "SSE 单行超过长度上限，截断处理"
                    );
                    // 截取前 SSE_MAX_LINE_BYTES 字节尝试解析
                    let truncated = &content[..SSE_MAX_LINE_BYTES];
                    let line = String::from_utf8_lossy(truncated);
                    if let Some(delta_result) = parse_sse_line(&line)
                        && send_event(tx, delta_result).await
                    {
                        return true;
                    }
                    continue;
                }

                let line = String::from_utf8_lossy(content);

                if let Some(delta_result) = parse_sse_line(&line) {
                    match delta_result {
                        Ok(delta) => {
                            let is_done = delta.done;
                            if send_event(tx, Ok(delta)).await {
                                return true;
                            }
                            if is_done {
                                // [DONE] 已发送，正常退出
                                return true;
                            }
                        }
                        Err(e) => {
                            if send_event(tx, Err(e)).await {
                                return true;
                            }
                        }
                    }
                }
                // 空行/注释行 → 跳过，继续下一行
            }
            false
        }
        Err(e) => send_event(tx, Err(RamariaError::llm_with_source("HTTP 流读取失败", e))).await,
    }
}

// =========================================================
// SSE 行解析
// =========================================================

/// 解析一行 SSE 数据。
///
/// 格式:
/// - `data: {"choices": [{"delta": {"content": "..."}, "finish_reason": null}]}`: 增量文本
/// - `data: {"error": {...}}`: 流内错误载荷，直接上抛错误
/// - `data: [DONE]`: 流结束标记
/// - `: ...` 或空行: 注释/心跳，返回 None 跳过
///
/// 返回:
/// - `Some(Ok(StreamDelta))`: 成功解析的增量
/// - `Some(Err(...))`: JSON 解析失败，或流内 `error` 载荷上抛
/// - `None`: 注释行/空行/非 data 行/未知结构（无 choices 数组），应跳过
pub(crate) fn parse_sse_line(line: &str) -> Option<RamariaResult<StreamDelta>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return None;
    }

    // 提取 "data:" 或 "data: " 前缀后的内容（兼容 W3C SSE 规范）
    let payload = line
        .strip_prefix("data: ")
        .or_else(|| line.strip_prefix("data:"))?;

    // [DONE] 标记
    if payload == "[DONE]" {
        return Some(Ok(StreamDelta {
            content: String::new(),
            done: true,
            metadata: Some("[DONE]".to_string()),
        }));
    }

    // 解析 JSON chunk
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(chunk) => {
            // 流内错误载荷（如顶层 {"error": {...}}）必须上抛为错误：
            // 上报错误而非空 delta，避免限流/鉴权失败被上层误认为空回复。
            if let Some(error_obj) = chunk.get("error") {
                return Some(Err(RamariaError::llm(format!(
                    "SSE 流内错误: {}",
                    summarize_stream_error(error_obj)
                ))));
            }

            // choices 缺失或类型不符 = 未知结构；只记长度不落正文。
            let choices = match chunk.get("choices").and_then(|c| c.as_array()) {
                Some(choices) => choices,
                None => {
                    tracing::warn!(
                        payload_len = payload.len(),
                        "SSE 数据块 choices 缺失或非数组（且无 error 字段），已跳过（未知结构）"
                    );
                    return None;
                }
            };
            if choices.is_empty() {
                return None;
            }

            // 提取 delta.content
            let content = choices[0]["delta"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();

            // 检查 finish_reason
            let finish_reason = choices[0]["finish_reason"].as_str().map(|s| s.to_string());

            let done = finish_reason.is_some();

            Some(Ok(StreamDelta {
                content,
                done,
                metadata: finish_reason,
            }))
        }
        Err(e) => Some(Err(RamariaError::llm_with_source(
            format!("SSE data 解析失败: {}", &payload[..payload.len().min(200)]),
            e,
        ))),
    }
}

/// 将 SSE 流内 `error` 载荷压缩为可读的错误摘要。
///
/// 兼容形态:
/// - object: 依次读取 `message` / `type` / `code` 字段，按序拼接（跳过缺失或空字段）。
/// - string: 直接作为摘要文本。
///
/// 参数:
/// - `error_obj`: `chunk["error"]` 对应的 JSON 值。
///
/// 返回:
/// - 截断到 200 字符以内的摘要；无任何可读字段时返回 `"[未知错误形态]"`。
///
/// 说明:
/// - 摘要仅用于错误链与日志诊断，不包含请求原文与 API key。
pub(crate) fn summarize_stream_error(error_obj: &serde_json::Value) -> String {
    let mut parts: Vec<String> = Vec::new();

    match error_obj {
        serde_json::Value::String(text) => {
            if !text.trim().is_empty() {
                parts.push(text.clone());
            }
        }
        serde_json::Value::Object(map) => {
            for key in ["message", "type", "code"] {
                if let Some(value) = map.get(key) {
                    let text = match value {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        _ => String::new(),
                    };
                    if !text.trim().is_empty() {
                        parts.push(text);
                    }
                }
            }
        }
        _ => {}
    }

    let summary = ramaria_core::text::truncate_chars_bare(&parts.join(" | "), 200);
    if summary.trim().is_empty() {
        "[未知错误形态]".to_string()
    } else {
        summary
    }
}
