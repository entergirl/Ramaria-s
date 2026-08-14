//! crates/ramaria-llm/src/openai.rs - OpenAI Provider 实现
//!
//! 设计特点:
//! - 线上 LLM 后端，API key 从 OS keychain 实时读取
//! - 通过 `ProviderBase` 组合实现，共享 HTTP 传输和重试逻辑
//! - `LlmProvider` trait 实现由 `impl_online_provider!` 宏生成，消除与 DeepSeek 的 97% 重复
//! - 支持 streaming + JSON mode，context_window 128000
//!
//! 安全约束:
//! - API key 仅在调用 keychain 时出现内存中，不缓存、不记录日志
//! - 线上 provider 需要隐私确认后方可调用（由 app 层在调用前检查）
//! - 鉴权错误 (401/403) 不触发重试

use std::sync::Arc;

use crate::impl_online_provider;
use crate::impl_online_provider_constructors;
use crate::keychain::Keychain;
use crate::provider::ProviderBase;

// =========================================================
// OpenAIProvider
// =========================================================

/// OpenAI Provider。
///
/// 职责:
/// - 封装 OpenAI API（base_url: `https://api.openai.com/v1`）。
/// - API key 从 OS keychain 读取（service name: `"openai"`）。
/// - `LlmProvider` trait 由 `impl_online_provider!` 宏自动生成。
///
/// 用法:
/// ```ignore
/// let keychain = Arc::new(Keychain::new);
/// let config = BackendConfig::openai_default;
/// let provider = OpenAIProvider::new(config, keychain)?;
/// provider.validate().await?;
/// let reply = provider.chat(&request).await?;
/// ```
pub struct OpenAIProvider {
    base: ProviderBase,
    keychain: Arc<Keychain>,
}

// 构造器（new / with_retry_config / with_cache / resolve_api_key）由宏生成，
// 与 DeepSeek 共用同一实现（仅 service/display 名不同）。
impl_online_provider_constructors!(OpenAIProvider, "openai", "OpenAI");

// 由宏生成 LlmProvider trait 实现（chat/chat_stream/capability/config/validate/name）
impl_online_provider!(OpenAIProvider, "openai", "OpenAI");

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::traits::LlmProvider;
    use ramaria_core::types::BackendConfig;
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
