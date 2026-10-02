//! crates/ramaria-service/tests/suites/support/mock_backend/llm.rs - Mock LlmProvider
//!
//! 设计特点:
//! - `MockLlm`: 返回预设回复，流式按字符切分并仅在末字符标记 `done`
//! - `MockLlm::with_config`: 支持线上 provider 场景（能力与配置取自入参）
//! - `MockLlm::last_request`: 记录最近一次 chat / chat_stream 请求，供 prompt 内容断言
//! - `MockFailingLlm`: 始终返回 LLM 错误，覆盖错误处理与降级路径
//! - 两者均不发起网络请求，可直接放入 `Arc<dyn LlmProvider>`

use std::pin::Pin;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::{Stream, stream};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatRequest, LlmProvider, StreamDelta};
use ramaria_core::types::{BackendConfig, LlmProvider as LlmProviderKind, ModelCapability};

// =========================================================
// MockLlm
// =========================================================

/// Mock LLM Provider，返回预设回复。
pub struct MockLlm {
    reply: String,
    model_capability: ModelCapability,
    config: BackendConfig,
    /// 最近一次 chat/chat_stream 请求（桥接注入等 prompt 断言用）
    last_request: Mutex<Option<ChatRequest>>,
}

impl MockLlm {
    /// 创建返回固定回复的 Mock LLM。
    #[allow(dead_code)]
    pub fn new(reply: &str) -> Self {
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
            config: BackendConfig::lm_studio_default(),
            last_request: Mutex::new(None),
        }
    }

    /// 使用自定义 BackendConfig 创建 Mock LLM（支持线上 provider 测试）。
    #[allow(dead_code)]
    pub fn with_config(reply: &str, config: BackendConfig) -> Self {
        let capability = config.capability.clone();
        Self {
            reply: reply.to_string(),
            model_capability: capability,
            config,
            last_request: Mutex::new(None),
        }
    }

    /// 创建返回错误的 Mock LLM。
    #[allow(dead_code)]
    pub fn failing(error_msg: &str) -> MockFailingLlm {
        MockFailingLlm {
            error_msg: error_msg.to_string(),
            model_capability: ModelCapability {
                provider: LlmProviderKind::LmStudio,
                model_id: "mock-model".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
            config: BackendConfig::lm_studio_default(),
        }
    }
}

impl MockLlm {
    /// 最近一次 chat 请求（供 prompt 内容断言）。
    #[allow(dead_code)]
    pub fn last_request(&self) -> Option<ChatRequest> {
        self.last_request.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmProvider for MockLlm {
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        *self.last_request.lock().unwrap() = Some(request.clone());
        Ok(self.reply.clone())
    }

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        *self.last_request.lock().unwrap() = Some(request.clone());
        let reply = self.reply.clone();
        let chars: Vec<char> = reply.chars().collect();

        let stream = stream::iter(chars.into_iter().enumerate().map(move |(i, c)| {
            Ok(StreamDelta {
                content: c.to_string(),
                done: i == reply.chars().count() - 1,
                metadata: if i == reply.chars().count() - 1 {
                    Some("stop".into())
                } else {
                    None
                },
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

// =========================================================
// MockFailingLlm — 始终返回错误的 Mock
// =========================================================

/// Mock LLM Provider，始终返回错误（用于测试错误处理路径）。
#[allow(dead_code)]
pub struct MockFailingLlm {
    error_msg: String,
    model_capability: ModelCapability,
    config: BackendConfig,
}

#[async_trait]
impl LlmProvider for MockFailingLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        Err(RamariaError::llm(self.error_msg.clone()))
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        Err(RamariaError::llm(self.error_msg.clone()))
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
        "MockFailingLlm"
    }
}
