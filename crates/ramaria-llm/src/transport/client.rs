//! crates/ramaria-llm/src/transport/client.rs - OpenAI-compatible HTTP 传输客户端
//!
//! 设计特点:
//! - 封装 base_url + API key，构造 `/chat/completions` 请求
//! - 提供 `chat` 非流式和 `chat_stream` 流式两种调用模式
//! - 管理 reqwest HTTP 客户端（连接池、超时）
//! - 手动 `Clone` / `Debug`：API key 遮蔽输出，仅显示是否存在
//! - 请求体和响应体不自动记录（由上层决定是否记录 prompt）

use futures::Stream;
use futures::channel::mpsc;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::lock::{read_recover, write_recover};
use ramaria_core::traits::StreamDelta;
use std::pin::Pin;

use super::error::http_error;
use super::sse::sse_read_loop;

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
                read_recover(&self.api_key, "llm_transport.api_key").clone(),
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
        *write_recover(&self.api_key, "llm_transport.api_key") = api_key;
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

        let api_key = read_recover(&self.api_key, "llm_transport.api_key").clone();
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
    /// - `sse_read_loop` 内含 600s 整体超时保护（首事件 60s 快速失败）。
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
            let response_text = match response.text().await {
                Ok(text) => text,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "读取 HTTP 错误响应体失败，降级为空 body（保留状态码）"
                    );
                    String::new()
                }
            };
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

        let api_key = read_recover(&self.api_key, "llm_transport.api_key").clone();
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
