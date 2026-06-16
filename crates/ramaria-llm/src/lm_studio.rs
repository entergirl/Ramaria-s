//! rust/crates/ramaria-llm/src/lm_studio.rs - LM Studio Provider 实现
//!
//! 设计特点:
//! - 本地 LLM 后端，不需要 API key 认证
//! - 实现 `ramaria_core::traits::LlmProvider` trait
//! - 通过 `ProviderBase` 组合实现，共享 HTTP 传输和重试逻辑
//! - `validate` 检查 base_url 可连接，不要求模型 ID 非空（用户可后续选择）
//! - LM Studio 默认不支持 JSON mode，context_window 取决于加载的模型
//!
//! 安全约束:
//! - 本地服务不涉及隐私确认
//! - 不记录完整 prompt 或用户消息

use async_trait::async_trait;
use futures::Stream;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider, StreamDelta};
use ramaria_core::types::{BackendConfig, ModelCapability};
use std::pin::Pin;

use crate::provider::{ProviderBase, RetryConfig};

// =========================================================
// LmStudioProvider
// =========================================================

/// LM Studio Provider。
///
/// 职责:
/// - 封装本地 LM Studio OpenAI-compatible API。
/// - 默认 base_url: `http://localhost:1234/v1`。
/// - 无需 API key，HTTP 请求不发送 Authorization header。
///
/// 用法:
/// ```ignore
/// let config = BackendConfig::lm_studio_default();
/// let provider = LmStudioProvider::new(config)?;
/// provider.validate().await?;
/// let reply = provider.chat(&request).await?;
/// ```
pub struct LmStudioProvider {
    base: ProviderBase,
}

impl LmStudioProvider {
    /// 创建 LM Studio Provider。
    ///
    /// 参数:
    /// - `config`: 后端配置。model_id 可为空（用户后续在 LM Studio 中选择模型）。
    ///
    /// 返回:
    /// - 成功时返回 provider 实例。
    pub fn new(config: BackendConfig) -> RamariaResult<Self> {
        // LM Studio 不需要 API key
        let base = ProviderBase::new(config, None)?;
        tracing::info!(
            base_url = %base.transport().base_url(),
            "LmStudioProvider 已创建"
        );
        Ok(Self { base })
    }

    /// 创建带自定义重试配置的 LM Studio Provider。
    pub fn with_retry_config(
        config: BackendConfig,
        timeout_secs: u64,
        retry_config: RetryConfig,
    ) -> RamariaResult<Self> {
        let base = ProviderBase::with_retry_config(config, None, timeout_secs, retry_config)?;
        Ok(Self { base })
    }
}

#[async_trait]
impl LlmProvider for LmStudioProvider {
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        self.base.chat(request).await
    }

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        self.base.chat_stream(request).await
    }

    fn capability(&self) -> &ModelCapability {
        self.base.capability()
    }

    fn config(&self) -> &BackendConfig {
        self.base.backend_config()
    }

    async fn validate(&self) -> RamariaResult<()> {
        self.base.validate().await
    }

    fn name(&self) -> &'static str {
        "LM Studio"
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
    fn lm_studio_construction() {
        let config = BackendConfig::lm_studio_default();
        let provider = LmStudioProvider::new(config).expect("构造应成功");
        assert_eq!(provider.name(), "LM Studio");
        assert_eq!(provider.config().provider, ProviderKind::LmStudio);
        assert_eq!(provider.config().base_url, "http://localhost:1234/v1");
    }

    #[test]
    fn lm_studio_capability() {
        let config = BackendConfig::lm_studio_default();
        let provider = LmStudioProvider::new(config).expect("构造应成功");
        let cap = provider.capability();
        assert_eq!(cap.provider, ProviderKind::LmStudio);
        assert!(cap.supports_streaming);
        assert!(!cap.supports_json_mode);
        assert!(cap.context_window > 0);
    }

    #[test]
    fn lm_studio_no_api_key_required() {
        let config = BackendConfig::lm_studio_default();
        let provider = LmStudioProvider::new(config).expect("构造应成功");
        let _ = provider;
    }
}
