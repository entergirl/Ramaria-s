//! crates/ramaria-cli/tests/common/llm.rs - MockLlm（LlmProvider 实现）
//!
//! 设计特点:
//! - 返回预设回复的 LlmProvider，支持流式与非流式两路径
//! - 经 `common::MockLlm` 对外复用（mod.rs re-export）
//! - 仅服务 CLI 集成测试，不调用真实 LLM

use async_trait::async_trait;
use futures::{Stream, stream};
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider, StreamDelta};
use ramaria_core::types::{BackendConfig, LlmProvider as LlmProviderKind, ModelCapability};
use std::pin::Pin;

pub struct MockLlm {
    reply: String,
    model_capability: ModelCapability,
    config: BackendConfig,
}

impl MockLlm {
    /// 创建返回固定回复的 Mock LLM。
    pub fn new(reply: &str) -> Self {
        let config = BackendConfig::lm_studio_default();
        Self {
            reply: reply.to_string(),
            model_capability: ModelCapability {
                provider: LlmProviderKind::LmStudio,
                model_id: "mock-model".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
            config,
        }
    }

    /// 创建使用指定 BackendConfig 的 Mock LLM（用于 config 命令测试）。
    pub fn with_config(config: BackendConfig) -> Self {
        Self {
            reply: "mock reply".to_string(),
            model_capability: ModelCapability {
                provider: config.provider,
                model_id: config.capability.model_id.clone(),
                base_url: config.base_url.clone(),
                supports_streaming: config.capability.supports_streaming,
                supports_json_mode: config.capability.supports_json_mode,
                context_window: config.capability.context_window,
                max_output_tokens: config.capability.max_output_tokens,
            },
            config,
        }
    }
}

#[async_trait]
impl LlmProvider for MockLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        Ok(self.reply.clone())
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        let reply = self.reply.clone();
        let chars: Vec<char> = reply.chars().collect();
        let stream = stream::iter(chars.into_iter().enumerate().map(move |(i, c)| {
            let is_last = i == reply.chars().count() - 1;
            Ok(StreamDelta {
                content: c.to_string(),
                done: is_last,
                metadata: if is_last { Some("stop".into()) } else { None },
            })
        }));
        Ok(Box::pin(stream))
    }

    fn capability(&self) -> &ModelCapability {
        &self.model_capability
    }

    fn config(&self) -> &BackendConfig {
        &self.config
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "MockLlm"
    }
}
