//! rust/crates/ramaria-llm/src/transport.rs - OpenAI-compatible HTTP 传输层
//!
//! 设计特点:
//! - 真正的 SSE 流式处理：使用 `reqwest::Response::bytes_stream()` + `futures::channel::mpsc`
//!   逐块读取、逐行解析，不一次性读取响应体
//! - SSE 解析器支持缓冲区拼接跨 chunk 的不完整行
//! - 错误分类：HTTP 4xx → Validation/Llm 错误，5xx → Llm 错误，网络错误 → Llm 错误
//! - 非流式请求（`stream: false`）直接解析完整 JSON 响应
//! - 所有 HTTP 错误保留 status code 和响应体前 500 字符，便于诊断
//! - 不记录 API key 或完整消息内容

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
/// - 提供 `chat()` 非流式和 `chat_stream()` 流式两种调用模式
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
    /// - 使用 `futures::channel::mpsc::unbounded()` 桥接 tokio 后台任务与返回流。
    /// - 后台任务逐块从 `bytes_stream()` 读取、拼接不完整行、逐行解析 SSE。
    /// - 当接收端丢弃 stream 时，后台任务自动退出（`tx.unbounded_send` 返回错误）。
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
        let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamDelta>>();

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
// SSE 读取循环（后台 tokio 任务）
// =========================================================

/// 在后台 tokio 任务中逐块读取 HTTP 响应、逐行解析 SSE、通过 channel 发送。
///
/// 设计:
/// - 使用 `BytesMut` 缓冲区拼接跨 chunk 的不完整行。
/// - 遇到 `\n` 时切割一行，调用 `parse_sse_line` 解析。
/// - `data: [DONE]` 时发送 `done=true` 的 Delta 后退出。
/// - 接收端 drop stream 时 `tx.unbounded_send` 返回错误，此时静默退出。
async fn sse_read_loop(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
    tx: mpsc::UnboundedSender<RamariaResult<StreamDelta>>,
) {
    use futures::StreamExt;

    futures::pin_mut!(byte_stream);
    let mut buffer = BytesMut::new();

    while let Some(chunk_result) = byte_stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                buffer.extend_from_slice(&chunk);

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
                    let line = String::from_utf8_lossy(content);

                    if let Some(delta_result) = parse_sse_line(&line) {
                        match delta_result {
                            Ok(delta) => {
                                let is_done = delta.done;
                                if tx.unbounded_send(Ok(delta)).is_err() {
                                    return; // 接收端已丢弃
                                }
                                if is_done {
                                    // [DONE] 已发送，正常退出
                                    return;
                                }
                            }
                            Err(e) => {
                                if tx.unbounded_send(Err(e)).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    // 空行/注释行 → 跳过，继续下一行
                }
            }
            Err(e) => {
                let _ = tx.unbounded_send(Err(RamariaError::llm_with_source("HTTP 流读取失败", e)));
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
            let _ = tx.unbounded_send(delta_result);
        }
    }
    // 仅当流中未发送 done 时才发送合成 done 信号，避免双重 Done
    if !done_already_sent {
        let _ = tx.unbounded_send(Ok(StreamDelta {
            content: String::new(),
            done: true,
            metadata: Some("stream_ended_without_done".to_string()),
        }));
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
    let data = line
        .strip_prefix("data: ")
        .or_else(|| line.strip_prefix("data:"))?;

    // [DONE] 标记
    if data == "[DONE]" {
        return Some(Ok(StreamDelta {
            content: String::new(),
            done: true,
            metadata: Some("[DONE]".to_string()),
        }));
    }

    // 解析 JSON chunk
    match serde_json::from_str::<serde_json::Value>(data) {
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
            format!("SSE data 解析失败: {}", &data[..data.len().min(200)]),
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

    #[test]
    fn parse_empty_line_returns_none() {
        assert!(parse_sse_line("").is_none());
        assert!(parse_sse_line("   ").is_none());
    }

    #[test]
    fn parse_comment_line_returns_none() {
        assert!(parse_sse_line(": heartbeat").is_none());
        assert!(parse_sse_line(":ok").is_none());
    }

    #[test]
    fn parse_done_marker() {
        let result = parse_sse_line("data: [DONE]").expect("应解析 [DONE]");
        let delta = result.expect("应为 Ok");
        assert!(delta.done);
        assert_eq!(delta.metadata.as_deref(), Some("[DONE]"));
        assert!(delta.content.is_empty());
    }

    #[test]
    fn parse_content_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":"你好"},"finish_reason":null}]}"#;
        let result = parse_sse_line(line).expect("应解析内容增量");
        let delta = result.expect("应为 Ok");
        assert_eq!(delta.content, "你好");
        assert!(!delta.done);
        assert!(delta.metadata.is_none());
    }

    #[test]
    fn parse_finish_reason() {
        let line = r#"data: {"choices":[{"delta":{"content":""},"finish_reason":"stop"}]}"#;
        let result = parse_sse_line(line).expect("应解析结束标记");
        let delta = result.expect("应为 Ok");
        assert!(delta.done);
        assert_eq!(delta.metadata.as_deref(), Some("stop"));
        assert!(delta.content.is_empty());
    }

    #[test]
    fn parse_malformed_json_returns_err() {
        let line = "data: {not valid json}";
        let result = parse_sse_line(line).expect("应尝试解析");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.category(), "llm");
        assert!(err.context().contains("SSE data 解析失败"));
    }

    #[test]
    fn parse_no_data_prefix_returns_none() {
        assert!(parse_sse_line("event: ping").is_none());
    }

    #[test]
    fn parse_data_with_leading_whitespace() {
        let line = "  data: [DONE]  ";
        let result = parse_sse_line(line).expect("应解析");
        let delta = result.expect("应为 Ok");
        assert!(delta.done);
    }

    #[test]
    fn parse_data_prefix_without_space() {
        // W3C SSE 规范允许 "data:" 无空格
        let line = "data:[DONE]";
        let result = parse_sse_line(line).expect("应解析");
        let delta = result.expect("应为 Ok");
        assert!(delta.done);
        assert_eq!(delta.metadata.as_deref(), Some("[DONE]"));
    }

    #[test]
    fn parse_content_delta_without_space_after_data() {
        // 某些 server 发送 "data:" 不带空格
        let line = r#"data:{"choices":[{"delta":{"content":"测试"},"finish_reason":null}]}"#;
        let result = parse_sse_line(line).expect("应解析");
        let delta = result.expect("应为 Ok");
        assert_eq!(delta.content, "测试");
        assert!(!delta.done);
    }

    // ---- http_error ----

    #[test]
    fn http_401_is_auth_error() {
        let err = http_error(401, r#"{"error":"Invalid API key"}"#);
        assert_eq!(err.category(), "llm");
        assert!(err.context().contains("鉴权失败"));
        assert!(err.context().contains("401"));
    }

    #[test]
    fn http_429_is_rate_limit() {
        let err = http_error(429, "Too many requests");
        assert!(err.context().contains("频率超限"));
        assert!(err.context().contains("429"));
    }

    #[test]
    fn http_500_is_server_error() {
        let err = http_error(500, "Internal error");
        assert!(err.context().contains("服务端错误"));
        assert!(err.context().contains("500"));
    }

    #[test]
    fn http_400_is_request_error() {
        let err = http_error(400, r#"{"error":"model not found"}"#);
        assert!(err.context().contains("请求错误"));
        assert!(err.context().contains("400"));
    }

    #[test]
    fn http_error_body_truncated() {
        let long_body = "x".repeat(1000);
        let err = http_error(422, &long_body);
        // 上下文应截断到 500 字符
        assert!(err.context().len() < 700); // 500 + 前缀长度
    }
}
