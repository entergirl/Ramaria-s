//! crates/ramaria-llm/src/transport.rs - OpenAI-compatible HTTP 传输层
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
use futures::SinkExt;
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
pub struct OpenAiTransport {
    /// 不含尾随 `/` 的 base URL，例如 `https://api.deepseek.com/v1`
    base_url: String,
    /// 可选 API key（LM Studio 不需要；运行时可通过 `set_api_key` 热更新）
    api_key: std::sync::RwLock<Option<String>>,
    /// HTTP 客户端
    http: reqwest::Client,
}

impl Clone for OpenAiTransport {
    fn clone(&self) -> Self {
        Self {
            base_url: self.base_url.clone(),
            api_key: std::sync::RwLock::new(
                self.api_key
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            ),
            http: self.http.clone(),
        }
    }
}

// 手动实现 Debug：遮蔽 API key，仅显示 base_url 和 key 是否存在
impl std::fmt::Debug for OpenAiTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiTransport")
            .field("base_url", &self.base_url)
            .field(
                "api_key",
                &self
                    .api_key
                    .read()
                    .map(|g| if g.is_some() { "***" } else { "None" })
                    .unwrap_or("poisoned"),
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
            api_key: std::sync::RwLock::new(api_key),
            http,
        })
    }

    /// 更新 API key（运行时热更新；LM Studio 场景传 None 清除）。
    ///
    /// 说明:
    /// - 用户修改 keychain 后，下一次请求即使用新 key，无需重建 provider。
    pub fn set_api_key(&self, api_key: Option<String>) {
        *self.api_key.write().unwrap_or_else(|e| e.into_inner()) = api_key;
    }

    /// 返回 base_url 引用（供 validate 使用）。
    pub fn base_url(&self) -> &str {
        &self.base_url
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

        let api_key = self
            .api_key
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(key) = api_key.as_ref() {
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
        // 关闭思考模式（thinking disabled）：
        // - deepseek-v4-flash 默认开启思考（官方文档 thinking_mode：默认 enabled，
        //   effort=high），思考内容（reasoning_content）消耗输出预算；
        //   在 max_tokens 较小（如 L1 摘要 512）时思考即可耗尽预算，
        //   导致 content 为空或截断（2026-08-08 实测 reasoning_len=30556 后 content 空）。
        // - 本函数服务全部结构化提取任务（L1 摘要/L2 事件提取/L3 推断/冷启动），
        //   不需要链式思考；关闭后 temperature 参数也恢复生效（思考模式下
        //   temperature/top_p 等参数无效，官方文档 Input and Output Parameters）。
        // - 对话路径（chat_stream）已同步关闭思考（2026-08-25），
        //   保证 temperature 在对话链路同样生效、输出可复现。
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
            "thinking": {"type": "disabled"},
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
            // 推理模型（如 DeepSeek Reasoner）可能将输出全部消耗在思考过程，
            // 导致 content 为空——此时继续以空串解析 JSON 只会得到误导性的
            // "JSON 解析失败"；改为明确错误，供上层重试与诊断。
            let reasoning_len = parsed["choices"][0]["message"]["reasoning_content"]
                .as_str()
                .map(|s| s.len())
                .unwrap_or(0);
            tracing::warn!(
                model,
                reasoning_len,
                "LLM 返回空内容（HTTP 200），可能模型不可用、请求被拒绝或 max_tokens 被思考过程耗尽"
            );
            return Err(RamariaError::llm(format!(
                "LLM 返回空内容（HTTP 200），可能模型不可用或请求被拒绝: {model}"
            )));
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
        // 关闭思考模式（thinking disabled），与 chat 非流式方法（已修复）保持一致：
        // - deepseek-v4-flash 默认开启思考（官方文档 thinking_mode：默认 enabled，
        //   effort=high）；思考模式下 temperature/top_p 等采样参数不生效
        //   （官方文档 Input and Output Parameters："设置不报错但不生效"），
        //   输出由思考主导、同参数不可复现（2026-08-25 探针复跑一致性验证失败：
        //   同 seed 同命令两次运行，全部回复不同）。
        // - 本函数服务对话路径；关闭后 temperature 恢复生效、输出显著更确定。
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": true,
            "thinking": {"type": "disabled"},
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

        let api_key = self
            .api_key
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(key) = api_key.as_ref() {
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
async fn sse_read_loop(
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
async fn sse_read_loop_inner(
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

    // ---- chat_stream 请求 body 构造（思考模式禁用）----

    /// 启动本地 mock SSE server：捕获 POST `/chat/completions` 的请求 body，
    /// 返回一段固定 SSE 流。返回 `(base_url, 捕获的请求 body)`。
    async fn spawn_mock_sse_server() -> (
        String,
        std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定 mock 端口应成功");
        let addr = listener.local_addr().expect("获取 mock 地址应成功");
        let captured: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = std::sync::Arc::clone(&captured);

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let (mut socket, _) = listener.accept().await.expect("mock 应收到连接");

            // 读取完整请求：先读到头部结束（\r\n\r\n），再按 Content-Length 补齐 body
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            let header_end = loop {
                let n = socket.read(&mut tmp).await.expect("mock 读请求头应成功");
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(pos + 4);
                }
            };
            let header_end = header_end.expect("mock 应收到请求头");
            let head = String::from_utf8_lossy(&buf[..header_end]);
            let content_len: usize = head
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().parse().ok()))
                .flatten()
                .unwrap_or(0);
            while buf.len() < header_end + content_len {
                let n = socket.read(&mut tmp).await.expect("mock 读 body 应成功");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }

            let body_str = String::from_utf8_lossy(&buf[header_end..header_end + content_len]);
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body_str) {
                *captured_clone.lock().expect("mock 捕获锁应可用") = Some(v);
            }

            // 返回固定 SSE 流（一个内容增量 + [DONE]）
            let sse = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":null}]}\n\n",
                "data: [DONE]\n\n"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse.len(),
                sse
            );
            socket
                .write_all(resp.as_bytes())
                .await
                .expect("mock 写响应应成功");
            let _ = socket.shutdown().await;
        });

        (format!("http://{}", addr), captured)
    }

    /// chat_stream 请求 body 必须含 `thinking.type == "disabled"`。
    ///
    /// 背景:
    /// - 思考模式下 temperature/top_p 等采样参数不生效（官方文档 Input and
    ///   Output Parameters："设置不报错但不生效"），输出由思考主导、不可复现；
    ///   chat 非流式已修复，chat_stream 未同步导致对话链路同参数不可复现。
    /// - 通过 mock server 捕获真实发送的请求 body，断言字段存在且取值正确。
    #[tokio::test]
    async fn chat_stream_body_disables_thinking() {
        use futures::StreamExt;

        let (base_url, captured) = spawn_mock_sse_server().await;
        let transport = OpenAiTransport::new(base_url, Some("test-key".into()), 5)
            .expect("构造 transport 应成功");

        let messages = vec![serde_json::json!({"role": "user", "content": "你好"})];
        let mut stream = transport
            .chat_stream(&messages, "deepseek-chat", 0.0, 512)
            .await
            .expect("chat_stream 应成功");

        // 消费流直到 [DONE]，验证链路完整可用
        let mut received = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(d) if d.done => break,
                Ok(d) => received.push_str(&d.content),
                Err(e) => panic!("流内错误: {e}"),
            }
        }
        assert_eq!(received, "你好", "应收到 mock 返回的流内容");

        // 断言请求 body：思考模式必须禁用，其余字段正确透传
        let body = captured
            .lock()
            .expect("捕获锁应可用")
            .clone()
            .expect("应捕获到请求 body");
        assert_eq!(
            body["thinking"]["type"], "disabled",
            "流式请求必须禁用思考模式（temperature 才能生效）"
        );
        assert_eq!(body["stream"], true, "stream 应为 true");
        assert_eq!(body["model"], "deepseek-chat", "model 应正确");
        assert_eq!(body["temperature"], 0.0, "temperature 应透传");
        assert_eq!(body["max_tokens"], 512, "max_tokens 应透传");
        assert_eq!(body["messages"][0]["role"], "user", "messages 应透传");
    }

    // =========================================================
    // 背压与分级超时（决策 D-V17-013 / 备忘 §二 15）
    // =========================================================

    /// channel 满时不丢 delta：70 个事件（> 默认容量 64）经小容量 channel 全部送达。
    ///
    /// 回归红线 4：流式修复后长回复不截断、无静默丢 delta。
    #[tokio::test]
    async fn channel_full_backpressure_no_delta_lost() {
        use futures::StreamExt;
        use futures::stream;

        // 构造 70 个增量事件（超过默认 channel 容量 64，触发满/背压）
        let events: Vec<String> = (0..70)
            .map(|i| {
                format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"块{i}\"}},\"finish_reason\":null}}]}}\n\n"
                )
            })
            .collect();
        let byte_stream = stream::iter(events.into_iter().map(|e| Ok(bytes::Bytes::from(e))));

        // 小容量 channel（2）强制触发满 → 背压等待消费
        let (tx, mut rx) = mpsc::channel::<RamariaResult<StreamDelta>>(2);

        // 消费者并行运行，消费完内容后记录
        let consumer = tokio::spawn(async move {
            let mut received = String::new();
            let mut count = 0usize;
            while let Some(item) = rx.next().await {
                match item {
                    Ok(d) if d.done => break,
                    Ok(d) => {
                        received.push_str(&d.content);
                        count += 1;
                    }
                    Err(e) => panic!("流内错误: {e}"),
                }
                // 模拟慢消费（让出执行权），确保背压路径被触发
                tokio::task::yield_now().await;
            }
            (count, received)
        });

        sse_read_loop_inner(
            byte_stream,
            tx,
            Some("test-rid".to_string()),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(30),
        )
        .await;

        let (count, received) = consumer.await.expect("消费者应完成");
        assert_eq!(
            count, 70,
            "所有 delta 都应送达（背压不丢内容），实际 {count}"
        );
        assert!(received.contains("块0"), "应收到首个增量");
        assert!(received.contains("块69"), "应收到末个增量");
    }

    /// 首事件超时：服务端 60s（测试注入 50ms）内无任何数据 → 报错退出。
    ///
    /// 分级超时之首包快速失败：避免服务端挂起时长时间无反馈。
    #[tokio::test]
    async fn first_event_timeout_sends_error() {
        use futures::StreamExt;
        use futures::stream;

        // 永远 pending 的 stream：模拟服务端接受连接后不发送任何数据（挂起）
        let byte_stream = stream::pending::<Result<bytes::Bytes, reqwest::Error>>();
        let (tx, mut rx) = mpsc::channel::<RamariaResult<StreamDelta>>(4);

        sse_read_loop_inner(
            byte_stream,
            tx,
            Some("test-rid".to_string()),
            std::time::Duration::from_millis(50),
            std::time::Duration::from_secs(10),
        )
        .await;

        let item = rx.next().await.expect("应收到错误事件");
        let err = item.expect_err("应为错误事件");
        assert!(
            err.context().contains("首事件超时"),
            "错误信息应含首事件超时，got: {}",
            err.context()
        );
    }

    /// 整体超时：流永不结束（服务端持续发送但无 [DONE]）→ 整体超时兜底报错。
    ///
    /// 分级超时之整体兜底：长流只受整体超时约束，不因首包已到而无限等待。
    #[tokio::test]
    async fn stream_overall_timeout_sends_error() {
        use futures::StreamExt;

        // 无限发送 SSE 注释行（`: 心跳`）的 stream：服务端持续有数据但不产生事件、
        // 也永不 [DONE]——验证整体超时兜底。unfold 每 1ms 生成一行，流永不结束；
        // boxed() 使 !Unpin 的 unfold 流满足 sse_read_loop_inner 的 Unpin 约束。
        let byte_stream = futures::stream::unfold(0u64, |i| async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            Some((Ok(bytes::Bytes::from(": 心跳\n\n")), i + 1))
        })
        .boxed();
        let (tx, mut rx) = mpsc::channel::<RamariaResult<StreamDelta>>(8);

        // 消费者并行消费，记录是否收到整体超时错误
        let consumer = tokio::spawn(async move {
            let mut saw_overall_timeout = false;
            while let Some(item) = rx.next().await {
                match item {
                    Ok(d) if d.done => break,
                    Ok(_) => {}
                    Err(e) => {
                        saw_overall_timeout = e.context().contains("流式读取超时");
                        break;
                    }
                }
            }
            saw_overall_timeout
        });

        sse_read_loop_inner(
            byte_stream,
            tx,
            Some("test-rid".to_string()),
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(100),
        )
        .await;

        let saw_overall_timeout = consumer.await.expect("消费者应完成");
        assert!(
            saw_overall_timeout,
            "整体超时应发送错误事件（服务端持续发送但无 [DONE]）"
        );
    }
}
