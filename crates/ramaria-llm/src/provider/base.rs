//! crates/ramaria-llm/src/provider/base.rs - Provider 共享基础设施
//!
//! 设计特点:
//! - `ProviderBase`: 封装 HTTP 传输、消息组装、重试/超时策略与可选 LLM 响应缓存
//! - 手动 `Debug`：缓存为 trait object，仅输出是否启用；API key 不进入 Debug 输出
//! - `with_retry`: 指数退避执行器（网络错误 + 5xx + 429 重试，4xx 与配置错误不重试）
//! - 非流式 `chat` 接入精确缓存：命中直接复用、写失败静默降级；流式不缓存
//! - `resolve_constructor_key`: 构造期 keychain 读取降级（失败不阻断 provider 构造）

use futures::Stream;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatRequest, LlmResponseCache, StreamDelta};
use ramaria_core::types::{BackendConfig, ModelCapability};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::transport::OpenAiTransport;

use super::request::{build_messages, cache_key};
use super::retry::RetryConfig;

// =========================================================
// ProviderBase
// =========================================================

/// Provider 共享基础设施。
///
/// 职责:
/// - 持有 `BackendConfig`（非敏感配置）和 `ModelCapability`（能力描述）
/// - 通过 `OpenAiTransport` 发送 HTTP 请求
/// - 实现重试逻辑（`with_retry`）
/// - 将 `ChatRequest`（trait 格式）组装为 OpenAI 消息数组
/// - 可选接入 LLM 响应精确缓存（三层生成缓存）：
///   `chat()` 先按 `sha256(model_id + template_version + 采样参数 + prompt)` 查缓存，
///   命中直接复用（不重复花费 API 账单）；查询/写入失败静默降级走 LLM。
///
/// 安全约束:
/// - 不持有 API key（由 keychain 在调用时实时获取）
/// - 缓存只存响应不存原文输入（key 为哈希，见 `cache_key`）
#[derive(Clone)]
pub(crate) struct ProviderBase {
    /// 非敏感后端配置
    pub config: BackendConfig,
    /// HTTP 传输层
    transport: Arc<OpenAiTransport>,
    /// 重试配置
    retry_config: RetryConfig,
    /// LLM 响应精确缓存（v1.5 新增；None = 未启用缓存，行为同 v1.4）
    cache: Option<Arc<dyn LlmResponseCache>>,
}

impl std::fmt::Debug for ProviderBase {
    /// 手动 Debug：缓存为 trait object 不实现 Debug，仅输出是否启用。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderBase")
            .field("config", &self.config)
            .field("retry_config", &self.retry_config)
            .field("cache_enabled", &self.cache.is_some())
            .finish_non_exhaustive()
    }
}

impl ProviderBase {
    /// 创建新的 ProviderBase。
    ///
    /// 参数:
    /// - `config`: 后端配置（含 capability）。
    /// - `api_key`: 可选 API key（LM Studio 为 None）。
    ///
    /// 超时在函数体内固定为 600 秒（见 `OpenAiTransport::new`）。
    /// 说明: v1.6 固定 120s 的 client 级超时会截断长生成（双重 120s 超时之一）；
    /// 提升到 600s 与 SSE 整体超时对齐，长回复不再被 client 层提前掐断。
    ///
    /// 返回:
    /// - 成功时返回 ProviderBase 实例。
    pub fn new(config: BackendConfig, api_key: Option<String>) -> RamariaResult<Self> {
        let transport = Arc::new(OpenAiTransport::new(config.base_url.clone(), api_key, 600)?);
        Ok(Self {
            config,
            transport,
            retry_config: RetryConfig::default(),
            cache: None,
        })
    }

    /// 创建带自定义超时和重试配置的 ProviderBase。
    ///
    /// 说明:
    /// - 当前仅测试路径调用（注入短超时、关闭重试以加速单测），保留备用。
    #[allow(dead_code)]
    pub fn with_retry_config(
        config: BackendConfig,
        api_key: Option<String>,
        timeout_secs: u64,
        retry_config: RetryConfig,
    ) -> RamariaResult<Self> {
        let transport = Arc::new(OpenAiTransport::new(
            config.base_url.clone(),
            api_key,
            timeout_secs,
        )?);
        Ok(Self {
            config,
            transport,
            retry_config,
            cache: None,
        })
    }

    /// 接入 LLM 响应精确缓存（v1.5 C 三层生成缓存）。
    ///
    /// 参数:
    /// - `cache`: 缓存实现（通常为 `ramaria_storage::SqliteLlmCache`）。
    ///
    /// 说明:
    /// - 幂等：重复调用以最后一次为准。
    /// - 缓存查询/写入失败均不影响主流程（ProviderBase 内部降级）。
    pub fn with_cache(mut self, cache: Arc<dyn LlmResponseCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// 返回 ModelCapability 引用（供 `LlmProvider::capability` 使用）。
    pub fn capability(&self) -> &ModelCapability {
        &self.config.capability
    }

    /// 返回 BackendConfig 引用（供 `LlmProvider::config` 使用）。
    pub fn backend_config(&self) -> &BackendConfig {
        &self.config
    }

    /// 返回 provider 名称。
    pub fn provider_name(&self) -> &'static str {
        match self.config.provider {
            ramaria_core::types::LlmProvider::LmStudio => "LM Studio",
            ramaria_core::types::LlmProvider::DeepSeek => "DeepSeek",
            ramaria_core::types::LlmProvider::OpenAI => "OpenAI",
            _ => "Unknown",
        }
    }

    /// 返回 HTTP 传输引用（供 validate 使用）。
    pub fn transport(&self) -> &OpenAiTransport {
        &self.transport
    }

    /// 更新传输层 API key（运行时热更新）。
    ///
    /// 说明:
    /// - 线上 provider 每次请求前从 keychain 重新读取并同步到此，
    ///   用户修改 keychain 后无需重建 provider 即生效。
    pub fn set_api_key(&self, api_key: Option<String>) {
        self.transport.set_api_key(api_key);
    }

    // =========================================================
    // 非流式聊天
    // =========================================================

    /// 执行非流式聊天（带重试 + 可选精确缓存）。
    ///
    /// 参数:
    /// - `request`: 组装好的聊天请求。
    ///
    /// 返回:
    /// - 完整 assistant 回复文本。
    ///
    /// 缓存行为（三层生成缓存）:
    /// - 已注入缓存（`with_cache`）且 `request.template_version` 非空时：
    ///   1. 构造 key = sha256(model_id + template_version + 采样参数 + messages JSON)；
    ///   2. 查询缓存：命中 → 记 `cache_hit=true` 日志并直接返回（不发 HTTP）；
    ///   3. 未命中 → 正常调 LLM，成功后写入缓存（写失败记 warn 继续）；
    ///   4. 查询失败 → 记 warn 后直接走 LLM（降级不阻塞）。
    /// - 未注入缓存或 template_version 为空：行为与 v1.4 完全一致。
    ///
    /// 说明:
    /// - 流式 `chat_stream` 不缓存（交互式场景无重跑语义）。
    pub async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        let messages = build_messages(request);
        let model = &self.config.capability.model_id;
        // 优先使用 ChatRequest 显式参数（允许不同调用路径使用不同的
        // temperature/max_tokens），而非统一使用 BackendConfig 的默认值。
        let temperature = request.temperature;
        let max_tokens = request.max_tokens;

        // ---- 精确缓存查询（v1.5）----
        if let Some(cache) = &self.cache
            && !request.template_version.trim().is_empty()
        {
            let key = cache_key(
                model,
                &request.template_version,
                temperature,
                max_tokens,
                &messages,
            );
            match cache.get(&key).await {
                Ok(Some(cached)) => {
                    tracing::info!(
                        provider = self.provider_name(),
                        model = %model,
                        template_version = %request.template_version,
                        cache_key = %key,
                        cache_hit = true,
                        "LLM 精确缓存命中，直接复用响应"
                    );
                    return Ok(cached);
                }
                Ok(None) => {
                    tracing::debug!(
                        provider = self.provider_name(),
                        cache_key = %key,
                        cache_hit = false,
                        "LLM 精确缓存未命中，走真实 LLM 调用"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        provider = self.provider_name(),
                        cache_key = %key,
                        error = %e,
                        "LLM 精确缓存查询失败，降级直接走 LLM"
                    );
                }
            }

            let response = self
                .with_retry(|| async {
                    self.transport
                        .chat(&messages, model, temperature, max_tokens)
                        .await
                })
                .await;

            // ---- 成功后写入缓存（写失败不阻断，记 warn 继续）----
            match &response {
                Ok(text) => {
                    if let Err(e) = cache
                        .put(&key, text, model, &request.template_version)
                        .await
                    {
                        tracing::warn!(
                            provider = self.provider_name(),
                            cache_key = %key,
                            error = %e,
                            "LLM 精确缓存写入失败（非致命，继续返回响应）"
                        );
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        provider = self.provider_name(),
                        error = %e,
                        "LLM 调用失败，不写入缓存"
                    );
                }
            }
            return response;
        }

        // ---- 未启用缓存路径（与 v1.4 行为一致）----
        self.with_retry(|| async {
            self.transport
                .chat(&messages, model, temperature, max_tokens)
                .await
        })
        .await
    }

    // =========================================================
    // 流式聊天
    // =========================================================

    /// 执行流式聊天（带重试，仅对连接建立阶段重试）。
    ///
    /// 说明:
    /// - 连接建立成功后，流内错误通过流本身传播，不再触发外层重试。
    /// - 重试仅针对 `chat_stream` 返回的外层 `Err`（即连接/HTTP 状态码错误）。
    ///
    /// 参数:
    /// - `request`: 组装好的聊天请求。
    ///
    /// 返回:
    /// - 成功时返回异步流。
    pub async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        let messages = build_messages(request);
        let model = &self.config.capability.model_id;
        let temperature = request.temperature;
        let max_tokens = request.max_tokens;

        self.with_retry(|| async {
            self.transport
                .chat_stream(&messages, model, temperature, max_tokens)
                .await
        })
        .await
    }

    // =========================================================
    // 验证
    // =========================================================

    /// 轻量级健康检查——仅检查 base_url 是否可达。
    ///
    /// 与 `validate` 的区别:
    /// - `health_check` 只发送简单 GET 请求到 base_url，超时 5 秒。
    /// - 不检查模型列表、API key 有效性或流式能力。
    ///
    /// 说明:
    /// - 用于 `run_setup` 末尾的启动探测。
    /// - 线上 provider 应覆写此方法实现真正的 HTTP 探测。
    pub async fn health_check(&self) -> RamariaResult<()> {
        let check_url = self.transport.base_url().trim_end_matches('/').to_string();
        let timeout = std::time::Duration::from_secs(5);

        tokio::time::timeout(timeout, async {
            self.transport.send_authenticated_get(&check_url).await
        })
        .await
        .map_err(|_elapsed| {
            tracing::warn!(
                provider = self.provider_name(),
                base_url = %check_url,
                "健康检查超时（5s）— 后端可能未启动"
            );
            RamariaError::llm(format!(
                "{} 健康检查超时（5s）：请确认服务已启动 ({})",
                self.provider_name(),
                check_url,
            ))
        })?
        .map(|_response| {
            tracing::info!(
                provider = self.provider_name(),
                base_url = %check_url,
                "健康检查通过"
            );
        })?;

        Ok(())
    }

    /// 验证 provider 可用性。
    ///
    /// 检查内容:
    /// - base_url 是否可连接（发送带 Authorization 的 GET 到 `/models` 端点）。
    /// - 模型 ID 是否非空（LM Studio 场景允许空字符串，用户后续选择）。
    ///
    /// 注意:
    /// - 修复：使用 `send_authenticated_get` 携带 API key header，
    ///   避免线上 provider（DeepSeek/OpenAI）的 /models 端点返回 401。
    pub async fn validate(&self) -> RamariaResult<()> {
        // 1. 检查 base_url 可连接（带 Authorization header）
        let models_url = format!("{}/models", self.transport.base_url());
        let response = self.transport.send_authenticated_get(&models_url).await?;

        let status = response.status();
        if !status.is_success() {
            return Err(RamariaError::llm(format!(
                "{} 模型列表查询失败 (HTTP {}): 请检查 base_url 和 API key 是否正确",
                self.provider_name(),
                status.as_u16(),
            )));
        }

        // 2. 线上 provider 需检查模型 ID 非空
        if self.config.provider.is_online() && self.config.capability.model_id.is_empty() {
            return Err(RamariaError::validation(format!(
                "{} 的模型 ID 未配置，请在设置中指定模型",
                self.provider_name(),
            )));
        }

        tracing::info!(
            provider = self.provider_name(),
            base_url = %self.transport.base_url(),
            model = %self.config.capability.model_id,
            "Provider 验证通过"
        );

        Ok(())
    }

    // =========================================================
    // 重试执行器
    // =========================================================

    /// 带指数退避重试执行异步操作。
    ///
    /// 参数:
    /// - `f`: 返回 `RamariaResult<T>` 的异步闭包。
    ///
    /// 行为:
    /// - 首次调用 `f`。
    /// - 若返回 `Err` 且 `RetryConfig::should_retry_error` 为 true，等待后退避后重试。
    /// - 最多重试 `max_retries` 次。
    /// - 非可重试错误立即返回。
    async fn with_retry<F, Fut, T>(&self, mut f: F) -> RamariaResult<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = RamariaResult<T>>,
    {
        let mut last_err: Option<RamariaError> = None;

        for attempt in 0..=self.retry_config.max_retries {
            if attempt > 0 {
                let backoff = self.retry_config.backoff_ms(attempt - 1);
                tracing::warn!(
                    attempt,
                    backoff_ms = backoff,
                    provider = self.provider_name(),
                    "LLM 请求重试"
                );
                tokio::time::sleep(Duration::from_millis(backoff)).await;
            }

            match f().await {
                Ok(value) => {
                    if attempt > 0 {
                        tracing::info!(
                            attempt,
                            provider = self.provider_name(),
                            "LLM 请求重试成功"
                        );
                    }
                    return Ok(value);
                }
                Err(err) => {
                    if !self.retry_config.should_retry_error(&err) {
                        tracing::debug!(
                            %err,
                            provider = self.provider_name(),
                            "不可重试错误，立即返回"
                        );
                        return Err(err);
                    }
                    tracing::warn!(
                        attempt,
                        %err,
                        provider = self.provider_name(),
                        "LLM 请求失败，准备重试"
                    );
                    last_err = Some(err);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            RamariaError::llm(format!(
                "{} 请求失败：已达最大重试次数 ({})",
                self.provider_name(),
                self.retry_config.max_retries,
            ))
        }))
    }
}

// =========================================================
// 构造期 keychain 读取
// =========================================================

/// 将 keychain 读取结果解析为构造期可用的 API key 与状态文案。
///
/// 语义:
/// - `Ok(Some(key))` → `(Some(key), "已配置")`。
/// - `Ok(None)` → `(None, "未配置")`（凭据不存在属正常状态）。
/// - `Err(e)` → 记录 warn 后降级为 `(None, "读取失败(降级)")`：
///   keychain 暂时不可用不阻断 provider 构造，调用 chat/validate 时会提示配置。
///
/// 参数:
/// - `result`: `keychain.get_api_key(service)` 的返回值。
/// - `service`: keychain service 名（仅用于日志定位，不记录 key 内容）。
///
/// 返回:
/// - `(api_key, key_status)`: 构造期使用的 key 与状态文案（供日志记录）。
pub fn resolve_constructor_key(
    result: RamariaResult<Option<String>>,
    service: &str,
) -> (Option<String>, &'static str) {
    match result {
        Ok(Some(key)) => (Some(key), "已配置"),
        Ok(None) => (None, "未配置"),
        Err(e) => {
            tracing::warn!(
                service,
                error = %e,
                "keychain 读取 API key 失败，降级为未配置（调用时将提示配置）"
            );
            (None, "读取失败(降级)")
        }
    }
}
