//! rust/crates/ramaria-llm/src/deepseek.rs - DeepSeek Provider 实现
//!
//! 设计特点:
//! - 线上 LLM 后端，API key 从 OS keychain 实时读取
//! - 通过 `ProviderBase` 组合实现，共享 HTTP 传输和重试逻辑
//! - `LlmProvider` trait 实现由 `impl_online_provider!` 宏生成，消除与 OpenAI 的 97% 重复
//! - 支持 streaming + JSON mode，context_window 65536
//!
//! 安全约束:
//! - API key 仅在调用 keychain 时出现内存中，不缓存、不记录日志
//! - 线上 provider 需要隐私确认后方可调用（由 app 层在调用前检查）
//! - 鉴权错误 (401/403) 不触发重试

use ramaria_core::error::RamariaResult;
use ramaria_core::types::BackendConfig;
use std::sync::Arc;

use crate::impl_online_provider;
use crate::keychain::Keychain;
use crate::provider::{ProviderBase, RetryConfig};

// =========================================================
// DeepSeekProvider
// =========================================================

/// DeepSeek Provider。
///
/// 职责:
/// - 封装 DeepSeek OpenAI-compatible API（base_url: `https://api.deepseek.com/v1`）。
/// - API key 从 OS keychain 读取（service name: `"deepseek"`）。
/// - `LlmProvider` trait 由 `impl_online_provider!` 宏自动生成。
///
/// 用法:
/// ```ignore
/// let keychain = Arc::new(Keychain::new());
/// let config = BackendConfig::deepseek_default();
/// let provider = DeepSeekProvider::new(config, keychain)?;
/// provider.validate().await?;
/// let reply = provider.chat(&request).await?;
/// ```
pub struct DeepSeekProvider {
    base: ProviderBase,
    keychain: Arc<Keychain>,
}

impl DeepSeekProvider {
    /// 创建 DeepSeek Provider。
    ///
    /// 参数:
    /// - `config`: 后端配置（含默认 capability，model_id = "deepseek-chat"）。
    /// - `keychain`: OS keychain 实例，用于读取 API key。
    ///
    /// 返回:
    /// - 成功时返回 provider 实例。
    /// - API key 不存在不在此处报错（延迟到 `chat`/`validate` 时检查）。
    pub fn new(config: BackendConfig, keychain: Arc<Keychain>) -> RamariaResult<Self> {
        let result = keychain.get_api_key("deepseek");
        let api_key = result.unwrap_or(None);
        let key_status = match &api_key {
            Some(_) => "已配置",
            None => "未配置",
        };

        let base = ProviderBase::new(config, api_key)?;

        tracing::info!(
            key_status,
            base_url = %base.transport().base_url(),
            "DeepSeekProvider 已创建"
        );

        Ok(Self { base, keychain })
    }

    /// 创建带自定义重试配置的 DeepSeek Provider。
    pub fn with_retry_config(
        config: BackendConfig,
        keychain: Arc<Keychain>,
        timeout_secs: u64,
        retry_config: RetryConfig,
    ) -> RamariaResult<Self> {
        let api_key = keychain.get_api_key("deepseek").unwrap_or(None);
        let base = ProviderBase::with_retry_config(config, api_key, timeout_secs, retry_config)?;
        Ok(Self { base, keychain })
    }

    /// 从 keychain 获取 API key。
    fn resolve_api_key(&self) -> RamariaResult<Option<String>> {
        self.keychain.get_api_key("deepseek")
    }
}

// 由宏生成 LlmProvider trait 实现（chat/chat_stream/capability/config/validate/name）
impl_online_provider!(DeepSeekProvider, "deepseek", "DeepSeek");

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::traits::LlmProvider;
    use ramaria_core::types::LlmProvider as ProviderKind;

    #[test]
    fn deepseek_construction() {
        let config = BackendConfig::deepseek_default();
        let keychain = Arc::new(Keychain::new());
        let provider = DeepSeekProvider::new(config, keychain).expect("构造应成功");
        assert_eq!(provider.name(), "DeepSeek");
        assert_eq!(provider.config().provider, ProviderKind::DeepSeek);
        assert_eq!(provider.config().base_url, "https://api.deepseek.com/v1");
    }

    #[test]
    fn deepseek_capability() {
        let config = BackendConfig::deepseek_default();
        let keychain = Arc::new(Keychain::new());
        let provider = DeepSeekProvider::new(config, keychain).expect("构造应成功");
        let cap = provider.capability();
        assert_eq!(cap.provider, ProviderKind::DeepSeek);
        assert_eq!(cap.model_id, "deepseek-chat");
        assert!(cap.supports_streaming);
        assert!(cap.supports_json_mode);
        assert_eq!(cap.context_window, 65536);
    }
}
