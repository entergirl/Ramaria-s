//! rust/crates/ramaria-llm/src/openai.rs - OpenAI Provider 实现
//!
//! 设计特点:
//! - 线上 LLM 后端，API key 从 OS keychain 实时读取
//! - 实现 `ramaria_core::traits::LlmProvider` trait
//! - 通过 `ProviderBase` 组合实现，共享 HTTP 传输和重试逻辑
//! - `validate()` 检查 keychain 中 API key 存在、base_url 可连接、模型 ID 非空
//! - 支持 streaming + JSON mode，context_window 128000
//!
//! 安全约束:
//! - API key 仅在调用 keychain 时出现内存中，不缓存、不记录日志
//! - 线上 provider 需要隐私确认后方可调用（由 app 层在调用前检查）
//! - 鉴权错误 (401/403) 不触发重试

use async_trait::async_trait;
use futures::Stream;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatRequest, LlmProvider, StreamDelta};
use ramaria_core::types::{BackendConfig, ModelCapability};
use std::pin::Pin;
use std::sync::Arc;

use crate::keychain::Keychain;
use crate::provider::{ProviderBase, RetryConfig};

// =========================================================
// OpenAIProvider
// =========================================================

/// OpenAI Provider。
///
/// 职责:
/// - 封装 OpenAI API（base_url: `https://api.openai.com/v1`）。
/// - API key 从 OS keychain 读取（service name: `"openai"`）。
///
/// 用法:
/// ```ignore
/// let keychain = Arc::new(Keychain::new());
/// let config = BackendConfig::openai_default();
/// let provider = OpenAIProvider::new(config, keychain)?;
/// provider.validate().await?;
/// let reply = provider.chat(&request).await?;
/// ```
pub struct OpenAIProvider {
    base: ProviderBase,
    keychain: Arc<Keychain>,
}

impl OpenAIProvider {
    /// 创建 OpenAI Provider。
    ///
    /// 参数:
    /// - `config`: 后端配置（含默认 capability，model_id = "gpt-4o"）。
    /// - `keychain`: OS keychain 实例，用于读取 API key。
    ///
    /// 返回:
    /// - 成功时返回 provider 实例。
    /// - API key 不存在不在此处报错（延迟到 `chat`/`validate` 时检查）。
    pub fn new(config: BackendConfig, keychain: Arc<Keychain>) -> RamariaResult<Self> {
        // 单次 keychain 调用，消除 TOCTOU 窗口
        let result = keychain.get_api_key("openai");
        let api_key = result.unwrap_or(None);
        let key_status = match &api_key {
            Some(_) => "已配置",
            None => "未配置",
        };

        let base = ProviderBase::new(config, api_key)?;

        tracing::info!(
            key_status,
            base_url = %base.transport().base_url(),
            "OpenAIProvider 已创建"
        );

        Ok(Self { base, keychain })
    }

    /// 创建带自定义重试配置的 OpenAI Provider。
    pub fn with_retry_config(
        config: BackendConfig,
        keychain: Arc<Keychain>,
        timeout_secs: u64,
        retry_config: RetryConfig,
    ) -> RamariaResult<Self> {
        let api_key = keychain.get_api_key("openai").unwrap_or(None);

        let base = ProviderBase::with_retry_config(config, api_key, timeout_secs, retry_config)?;
        Ok(Self { base, keychain })
    }

    /// 从 keychain 获取 API key。
    fn resolve_api_key(&self) -> RamariaResult<Option<String>> {
        self.keychain.get_api_key("openai")
    }
}

#[async_trait]
impl LlmProvider for OpenAIProvider {
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        let api_key = self.resolve_api_key()?;
        if api_key.is_none() {
            return Err(RamariaError::privacy(
                "OpenAI API key 未配置。请在设置中配置 OpenAI API key 后再试。",
            ));
        }
        self.base.chat(request).await
    }

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        let api_key = self.resolve_api_key()?;
        if api_key.is_none() {
            return Err(RamariaError::privacy(
                "OpenAI API key 未配置。请在设置中配置 OpenAI API key 后再试。",
            ));
        }
        self.base.chat_stream(request).await
    }

    fn capability(&self) -> &ModelCapability {
        self.base.capability()
    }

    fn config(&self) -> &BackendConfig {
        self.base.backend_config()
    }

    async fn validate(&self) -> RamariaResult<()> {
        // 1. 检查 API key
        let api_key = self.resolve_api_key()?;
        if api_key.is_none() {
            return Err(RamariaError::privacy(
                "OpenAI API key 未配置。请先在 keychain 中设置 OpenAI API key。",
            ));
        }

        // 2. 检查连接和模型
        self.base.validate().await
    }

    fn name(&self) -> &'static str {
        "OpenAI"
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::LlmProvider as ProviderKind;

    #[test]
    fn openai_construction() {
        let config = BackendConfig::openai_default();
        let keychain = Arc::new(Keychain::new());
        let provider = OpenAIProvider::new(config, keychain).expect("构造应成功");
        assert_eq!(provider.name(), "OpenAI");
        assert_eq!(provider.config().provider, ProviderKind::OpenAI);
        assert_eq!(provider.config().base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn openai_capability() {
        let config = BackendConfig::openai_default();
        let keychain = Arc::new(Keychain::new());
        let provider = OpenAIProvider::new(config, keychain).expect("构造应成功");
        let cap = provider.capability();
        assert_eq!(cap.provider, ProviderKind::OpenAI);
        assert_eq!(cap.model_id, "gpt-4o");
        assert!(cap.supports_streaming);
        assert!(cap.supports_json_mode);
        assert_eq!(cap.context_window, 128000);
    }
}
