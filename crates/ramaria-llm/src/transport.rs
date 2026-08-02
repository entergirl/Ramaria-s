//! rust/crates/ramaria-llm/src/transport.rs - OpenAI-compatible HTTP 传输层
//!
//! 设计特点:
//! - 真正的 SSE 流式处理：使用 `reqwest::Response::bytes_stream` + `futures::channel::mpsc`
//!
//! 逐块读取、逐行解析，不一次性读取响应体
//! - SSE 解析器支持缓冲区拼接跨 chunk 的不完整行
//! - 错误分类：HTTP 4xx → Validation/Llm 错误，5xx → Llm 错误，网络错误 → Llm 错误
//! - 非流式请求（`stream: false`）直接解析完整 JSON 响应
//! - 所有 HTTP 错误保留 status code 和响应体前 500 字符，便于诊断
//! - 不记录 API key 或完整消息内容
//! - SSE 单行 > 10KB 截断并 warn；流式整体 120s 超时保护

use bytes::BytesMut;
use futures::Stream;
use futures::channel::mpsc;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StreamDelta;
use std::pin::Pin;

// =========================================================
// OpenAI-compatible HTTP 传输
// =========================================================

/// OpenAI-compatible API 的 HTTP 传输层。
///
/// 职责:
/// - 封装 base_url + API key，构造 `/chat/completions` 请求
/// - 提供 `chat` 非流式和 `chat_stream` 流式两种调用模式
/// - 管理 reqwest HTTP 客户端（连接池、超时）
///
/// 安全约束:
/// - `api_key` 仅在 `Authorization: Bearer` header 中使用，不进入日志
/// - 请求体和响应体不自动记录（由上层决定是否记录 prompt）
#[derive(Clone)]
pub struct OpenAiTransport {
    /// 不含尾随 `/` 的 base URL，例如 `https://api.deepseek.com/v1`
    base_url: String,
    /// 可选 API key（LM Studio 不需要）
    api_key: Option<String>,
    /// HTTP 客户端
    http: reqwest::Client,
}

// 手动实现 Debug：遮蔽 API key，仅显示 base_url 和 key 是否存在
impl std::fmt::Debug for OpenAiTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiTransport")
            .field("base_url", &self.base_url)
            .field(
                "api_key",
                &if self.api_key.is_some() {
                    "***"
                } else {
                    "None"
                },
            )
            .field("http", &self.http)
            .finish()
    }
}

impl OpenAiTransport {
    /// 创建新的传输实例。
    ///
    /// 参数:
    /// - `base_url`: OpenAI-compatible API 基础地址。
    /// - `api_key`: 可选 API key，为 None 时（LM Studio 场景）不发送 Authorization header。
    /// - `timeout_secs`: 单次 HTTP 请求超时秒数（不含流式读取）。
    pub fn new(
        base_url: String,
        api_key: Option<String>,
        timeout_secs: u64,
    ) -> RamariaResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| RamariaError::llm_with_source("创建 HTTP 客户端失败", e))?;

        let base_url = base_url.trim_end_matches('/').to_string();
        tracing::debug!(%base_url, has_key = api_key.is_some(), "OpenAiTransport 已初始化");

        Ok(Self {
            base_url,
            api_key,
            http,
        })
    }

    /// 返回 base_url 引用（供 validate 使用）。
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 返回 HTTP 客户端引用（供 validate 发送测试请求）。
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    /// 发送带认证的 GET 请求。
    ///
    /// 用于 validate 中测试 `/models` 端点可达性。
    /// 与 `send_request` 不同：使用 GET 而非 POST，无 JSON body。
    ///
    /// 参数:
    /// - `url`: 完整请求 URL（如 `https://api.deepseek.com/v1/models`）。
    ///
    /// 返回:
    /// - `Ok(Response)`: 请求成功（含 HTTP 状态码）。
    /// - `Err`: 连接/超时等网络错误。
    pub async fn send_authenticated_get(&self, url: &str) -> RamariaResult<reqwest::Response> {
        let mut req = self.http.get(url);

        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        req.send().await.map_err(|e| {
            if e.is_timeout() {
                RamariaError::llm(format!("验证请求超时: {url}"))
            } else if e.is_connect() {
                RamariaError::llm(format!(
                    "无法连接到服务: {url} — 请检查 base_url 和网络连接"
                ))
            } else {
                RamariaError::llm_with_source(format!("验证请求失败: {url}"), e)
            }
        })
    }

    // =========================================================
    // 非流式请求
    // =========================================================

    /// 发送非流式聊天请求，返回完整 assistant 回复文本。
    ///
    /// 参数:
    /// - `messages`: OpenAI 格式消息数组（已包含 system/user/assistant 角色）。
    /// - `model`: 模型标识。
    /// - `temperature`: 生成温度 0.0..2.0。
    /// - `max_tokens`: 最大输出 token 数。
    ///
    /// 返回:
    /// - 成功时返回 assistant 完整文本。
    /// - HTTP 4xx → `RamariaError::Llm`（含 status 和响应体摘要）。
    /// - HTTP 5xx / 网络错误 → `RamariaError::Llm`（含 source）。
    pub async fn chat(
        &self,
        messages: &[serde_json::Value],
        model: &str,
        temperature: f64,
        max_tokens: u32,
    ) -> RamariaResult<String> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
        });

        let response = self.send_request(&url, &body).await?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| RamariaError::llm_with_source("读取非流式响应体失败", e))?;

        if !status.is_success() {
            return Err(http_error(status.as_u16(), &response_text));
        }

        // 解析 OpenAI chat completion 响应
        let parsed: serde_json::Value = serde_json::from_str(&response_text).map_err(|e| {
            RamariaError::llm_with_source(
                format!(
                    "解析 LLM 响应 JSON 失败: {}",
                    &response_text[..response_text.len().min(200)]
                ),
                e,
            )
        })?;

        let content = parsed["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        if content.is_empty() {
            tracing::warn!(model, "LLM 返回空内容，可能模型不可用或请求被拒绝");
        }

        Ok(content)
    }

    // =========================================================
    // 流式请求
    // =========================================================

    /// 发送流式聊天请求，返回异步流。
    ///
    /// 参数:
    /// - `messages`: OpenAI 格式消息数组。
    /// - `model`: 模型标识。
    /// - `temperature`: 生成温度。
    /// - `max_tokens`: 最大输出 token 数。
    ///
    /// 返回:
    /// - 成功时返回 `Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>>>>`。
    /// - HTTP 连接/状态码错误 → 外层 `RamariaResult::Err`。
    /// - 流中解析错误 → 流内的 `RamariaResult::Err`（不中断流）。
    ///
    /// 实现:
    /// - 使用 `mpsc::channel(64)` 有界通道替代 unbounded，背压保护。
    /// - `sse_read_loop` 内含 120s 整体超时保护。
    /// - 后台任务逐块从 `bytes_stream` 读取、拼接不完整行、逐行解析 SSE。
    /// - 当接收端丢弃 stream 时，后台任务自动退出（`tx.send` 返回错误）。
    pub async fn chat_stream(
        &self,
        messages: &[serde_json::Value],
        model: &str,
        temperature: f64,
        max_tokens: u32,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": true,
        });

        let response = self.send_request(&url, &body).await?;

        let status = response.status();
        if !status.is_success() {
            let response_text = response.text().await.unwrap_or_default();
            return Err(http_error(status.as_u16(), &response_text));
        }

        // 真正流式：使用 bytes_stream 逐块读取
        let byte_stream = response.bytes_stream();
        // 有界 channel，容量 64，满时 send 返回错误自然降级
        let (tx, rx) = mpsc::channel::<RamariaResult<StreamDelta>>(64);

        tokio::spawn(async move {
            sse_read_loop(byte_stream, tx).await;
        });

        Ok(Box::pin(rx))
    }

    // =========================================================
    // 内部辅助
    // =========================================================

    /// 构造并发送 HTTP POST 请求。
    async fn send_request(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> RamariaResult<reqwest::Response> {
        let mut req = self
            .http
            .post(url)
            .json(body)
            .header("Content-Type", "application/json");

        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        req.send().await.map_err(|e| {
            // 区分超时与其他网络错误
            if e.is_timeout() {
                RamariaError::llm(format!("LLM 请求超时: {url}"))
            } else if e.is_connect() {
                RamariaError::llm(format!(
                    "无法连接到 LLM 服务: {url} — 请检查 base_url 和服务是否启动"
                ))
            } else {
                RamariaError::llm_with_source(format!("LLM 请求失败: {url}"), e)
            }
        })
    }
}

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

/// 流式读取整体超时秒数（120s）。
///
/// 说明:
/// - 从首次接收到 HTTP 响应到流结束的总时间上限
/// - 超时后发送错误事件并退出，防止服务端挂起导致资源泄漏
const SSE_STREAM_TIMEOUT_SECS: u64 = 120;

// =========================================================
// SSE 读取循环（后台 tokio 任务）
// =========================================================

/// 使用 `mpsc::Sender`（有界 channel）替代 `UnboundedSender`。
/// 使用 `try_send` 非阻塞发送，满时丢弃并记 warn（避免阻塞 SSE 读取线程）。
///
/// 设计:
/// - 使用 `BytesMut` 缓冲区拼接跨 chunk 的不完整行。
/// - 遇到 `\n` 时切割一行，调用 `parse_sse_line` 解析。
/// - `data: [DONE]` 时发送 `done=true` 的 Delta 后退出。
/// - 接收端 drop stream 时 `tx.try_send` 返回 Disconnected 错误，此时静默退出。
/// - 单行 > `SSE_MAX_LINE_BYTES` 时截断并记 warn。
/// - 整体 120s 超时保护，超时发送错误事件。
///
/// 参数:
/// - `byte_stream`: HTTP 响应体字节流。
/// - `tx`: 有界事件发送通道（容量 64）。
async fn sse_read_loop(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
    tx: mpsc::Sender<RamariaResult<StreamDelta>>,
) {
    sse_read_loop_inner(byte_stream, tx, None).await;
}

/// SSE 读取循环内部实现——支持可选的 request_id 用于超时日志。
async fn sse_read_loop_inner(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
    mut tx: mpsc::Sender<RamariaResult<StreamDelta>>,
    request_id: Option<String>,
) {
    // P-8: 流式整体 120s 超时保护
    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_secs(SSE_STREAM_TIMEOUT_SECS),
        sse_read_core(byte_stream, &mut tx, request_id.as_deref()),
    )
    .await;

    match timeout_result {
        Ok(_) => {
            // 正常完成
        }
        Err(_elapsed) => {
            // 超时：发送错误事件后退出
            let rid = request_id.as_deref().unwrap_or("unknown");
            tracing::warn!(
                request_id = %rid,
                timeout_secs = SSE_STREAM_TIMEOUT_SECS,
                "SSE 流式读取整体超时"
            );
            let _ = tx.try_send(Err(RamariaError::llm(format!(
                "SSE 流式读取超时（{SSE_STREAM_TIMEOUT_SECS}s），服务端可能已挂起"
            ))));
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
/// - 使用 `try_send` 非阻塞发送，满时丢弃 delta 并记 warn。
///
/// `try_send` 需要 `&mut self`，因此 `tx` 声明为 `&mut mpsc::Sender`。
async fn sse_read_core(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
    tx: &mut mpsc::Sender<RamariaResult<StreamDelta>>,
    request_id: Option<&str>,
) {
    use futures::StreamExt;

    // 辅助闭包：尝试发送事件到有界 channel
    // - 成功 → 返回 false（继续读取）
    // - Full (is_disconnected=false) → 记 warn + 返回 false
    // - Disconnected → 返回 true（停止读取）
    let try_send = |tx: &mut mpsc::Sender<_>, item: RamariaResult<StreamDelta>| -> bool {
        match tx.try_send(item) {
            Ok(()) => false,
            Err(e) if e.is_disconnected() => {
                tracing::debug!("SSE 接收端已断开，停止读取");
                true
            }
            Err(e) => {
                // Full: 有界 channel 容量满，丢弃事件不阻塞
                let item = e.into_inner();
                if let Ok(ref delta) = item
                    && !delta.content.is_empty()
                {
                    let rid = request_id.unwrap_or("unknown");
                    tracing::warn!(
                        request_id = %rid,
                        "SSE 有界 channel 已满（容量 64），丢弃 delta 事件（前端消费慢或卡顿）"
                    );
                }
                false
            }
        }
    };

    futures::pin_mut!(byte_stream);
    let mut buffer = BytesMut::new();

    while let Some(chunk_result) = byte_stream.next().await {
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
                            && try_send(tx, delta_result)
                        {
                            return;
                        }
                        continue;
                    }

                    let line = String::from_utf8_lossy(content);

                    if let Some(delta_result) = parse_sse_line(&line) {
                        match delta_result {
                            Ok(delta) => {
                                let is_done = delta.done;
                                if try_send(tx, Ok(delta)) {
                                    return;
                                }
                                if is_done {
                                    // [DONE] 已发送，正常退出
                                    return;
                                }
                            }
                            Err(e) => {
                                if try_send(tx, Err(e)) {
                                    return;
                                }
                            }
                        }
                    }
                    // 空行/注释行 → 跳过，继续下一行
                }
            }
            Err(e) => {
                let _ = try_send(tx, Err(RamariaError::llm_with_source("HTTP 流读取失败", e)));
                return;
            }
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
            let _ = try_send(tx, delta_result);
        }
    }
    // 仅当流中未发送 done 时才发送合成 done 信号，避免双重 Done
    if !done_already_sent {
        let _ = try_send(
            tx,
            Ok(StreamDelta {
                content: String::new(),
                done: true,
                metadata: Some("stream_ended_without_done".to_string()),
            }),
        );
    }
}

// =========================================================
// SSE 行解析
// =========================================================

/// 解析一行 SSE 数据。
///
/// 格式:
/// - `data: {"choices": [{"delta": {"content": "..."}, "finish_reason": null}]}`: 增量文本
/// - `data: [DONE]`: 流结束标记
/// - `: ...` 或空行: 注释/心跳，返回 None 跳过
///
/// 返回:
/// - `Some(Ok(StreamDelta))`: 成功解析的增量
/// - `Some(Err(...))`: JSON 解析失败
/// - `None`: 注释行/空行/非 data 行，应跳过
fn parse_sse_line(line: &str) -> Option<RamariaResult<StreamDelta>> {
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
            // 提取 delta.content
            let content = chunk["choices"][0]["delta"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();

            // 检查 finish_reason
            let finish_reason = chunk["choices"][0]["finish_reason"]
                .as_str()
                .map(|s| s.to_string());

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

// =========================================================
// HTTP 错误分类
// =========================================================

/// 将 HTTP 错误状态码映射为 `RamariaError::Llm`。
///
/// 分类:
/// - 401 / 403: 鉴权错误（API key 无效或过期）
/// - 429: 速率限制
/// - 4xx: 请求错误（模型名、参数等）
/// - 5xx: 服务端错误
fn http_error(status: u16, body: &str) -> RamariaError {
    let summary: String = body.chars().take(500).collect();
    let context = match status {
        401 => "LLM 鉴权失败 (HTTP 401): API key 无效或过期。请检查 keychain 中的密钥是否正确"
            .to_string(),
        403 => "LLM 访问被拒绝 (HTTP 403): 请检查 API key 权限或账户状态".to_string(),
        429 => "LLM 请求频率超限 (HTTP 429): 请稍后重试".to_string(),
        400..=499 => format!("LLM 请求错误 (HTTP {status}): {summary}"),
        500..=599 => format!("LLM 服务端错误 (HTTP {status}): {summary}"),
        _ => format!("LLM 未知 HTTP 错误 ({status}): {summary}"),
    };
    RamariaError::llm(context)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_sse_line ----

    /// parse_sse_line 各分支参数化验证：空行/注释/事件行 → None；
    /// [DONE]/内容增量/finish_reason → Some(Ok(delta))；非法 JSON → Some(Err)。
    #[test]
    fn parse_sse_line_cases() {
        enum Expect {
            None,                  // None
            Done(&'static str),    // Some(Ok(done=true, metadata=Some(x)))
            Content(&'static str), // Some(Ok(content=x, done=false))
            Err,                   // Some(Err)
        }
        let cases = [
            ("", Expect::None),
            ("   ", Expect::None),
            (": heartbeat", Expect::None),
            (":ok", Expect::None),
            ("event: ping", Expect::None),
            ("data: [DONE]", Expect::Done("[DONE]")),
            (
                r#"data: {"choices":[{"delta":{"content":"你好"},"finish_reason":null}]}"#,
                Expect::Content("你好"),
            ),
            (
                r#"data: {"choices":[{"delta":{"content":""},"finish_reason":"stop"}]}"#,
                Expect::Done("stop"),
            ),
            ("data: {not valid json}", Expect::Err),
            ("  data: [DONE]  ", Expect::Done("[DONE]")),
            ("data:[DONE]", Expect::Done("[DONE]")),
            (
                r#"data:{"choices":[{"delta":{"content":"测试"},"finish_reason":null}]}"#,
                Expect::Content("测试"),
            ),
        ];
        for (line, expect) in cases {
            let parsed = parse_sse_line(line);
            match (parsed, expect) {
                (None, Expect::None) => {}
                (None, _) => panic!("line={line:?} 应返回 Some"),
                (Some(_), Expect::None) => panic!("line={line:?} 应返回 None"),
                (Some(result), Expect::Err) => {
                    let err = result.unwrap_err();
                    assert_eq!(err.category(), "llm", "line={line:?}");
                    assert!(err.context().contains("SSE data 解析失败"), "line={line:?}");
                }
                (Some(result), Expect::Done(meta)) => {
                    let d = result.expect("应为 Ok");
                    assert!(d.done, "line={line:?} 应 done");
                    assert_eq!(d.metadata.as_deref(), Some(meta), "line={line:?}");
                    assert!(d.content.is_empty(), "line={line:?}");
                }
                (Some(result), Expect::Content(c)) => {
                    let d = result.expect("应为 Ok");
                    assert_eq!(d.content, c, "line={line:?}");
                    assert!(!d.done, "line={line:?}");
                    assert!(d.metadata.is_none(), "line={line:?}");
                }
            }
        }
    }

    // ---- http_error ----

    /// http_error 各状态码分支参数化验证。
    #[test]
    fn http_error_cases() {
        let cases = [
            (401, r#"{"error":"Invalid API key"}"#, "鉴权失败"),
            (429, "Too many requests", "频率超限"),
            (500, "Internal error", "服务端错误"),
            (400, r#"{"error":"model not found"}"#, "请求错误"),
        ];
        for (status, body, keyword) in cases {
            let err = http_error(status, body);
            assert_eq!(err.category(), "llm");
            assert!(
                err.context().contains(keyword),
                "status={status} 应包含 {keyword}"
            );
            assert!(
                err.context().contains(&status.to_string()),
                "status={status}"
            );
        }
        // 长 body 应截断
        let long_body = "x".repeat(1000);
        let err = http_error(422, &long_body);
        assert!(err.context().len() < 700); // 500 + 前缀长度
    }
}
