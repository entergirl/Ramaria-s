//! rust/crates/ramaria-app/src/stages/call_llm.rs - Stage 9: LLM 流式调用
//!
//! 设计特点:
//! - 调用 `ctx.llm.chat_stream(&chat_request)` 获取原始 `LlmRawStream`
//! - LLM 调用成功 → 设置 `data.llm_stream`，传递给 Stage 10
//! - LLM 调用失败 → 构造 Error 事件流（mpsc 单事件），设置 `data.output_stream`，不返回 Err
//!   - 此设计符合任务要求:"失败时构造 Error 事件流（非 Err 返回）"
//!   - Stage 10 检测到 output_stream 已预填充时直接透传，不执行保存逻辑
//! - 上层 send_message 可通过检查 output_stream 区分正常路径和错误路径
//! - 日志记录 request_id、session_id、provider 信息，便于问题定位

use async_trait::async_trait;
use futures::channel::mpsc;
use ramaria_core::error::RamariaError;

use crate::pipeline::{PipelineContext, PipelineData, PipelineError, PipelineStage};
use crate::stream_event::StreamEvent;

// =========================================================
// StageCallLlm
// =========================================================

/// Stage 9: LLM 流式调用。
///
/// 职责:
/// - 从 `PipelineData.chat_request` 读取构造完毕的请求
/// - 调用 `ctx.llm.chat_stream()` 获取原始 LLM 流
/// - 成功时设置 `data.llm_stream`
/// - 失败时构造 Error 事件流，设置 `data.output_stream`（绕过 Stage 10 保存逻辑）
///
/// 说明:
/// - 本 Stage 不直接返回 `PipelineError`（LLM 调用失败仍返回 Ok）
/// - 失败时构造 `StreamEvent::Error` 单事件流，通过 `data.output_stream` 透传给上层
/// - 此设计使 send_message 的返回类型保持一致性：成功和失败均返回 `SendMessageStream`
pub struct StageCallLlm;

impl StageCallLlm {
    /// 创建 StageCallLlm 实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for StageCallLlm {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineStage for StageCallLlm {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        "CallLlm"
    }

    async fn execute(
        &self,
        ctx: &PipelineContext,
        mut input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        // 前置条件：chat_request 必须存在
        let chat_request = match input.chat_request.as_ref() {
            Some(req) => req,
            None => {
                return Err(PipelineError::fatal(
                    "CallLlm",
                    RamariaError::validation(
                        "chat_request 未设置——Stage 8 (BuildRequest) 必须在前",
                    ),
                ));
            }
        };

        let request_id = chat_request.request_id;
        let session_id = input
            .session
            .as_ref()
            .map(|s| s.id.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        tracing::info!(
            request_id = %request_id,
            session_id = %session_id,
            provider = ctx.llm.name(),
            system_chars = chat_request.system_prompt.chars().count(),
            history_msgs = chat_request.history.len(),
            "StageCallLlm: 开始 LLM chat_stream 调用"
        );

        // 调用 LLM
        match ctx.llm.chat_stream(chat_request).await {
            Ok(stream) => {
                tracing::debug!(
                    request_id = %request_id,
                    "StageCallLlm: LLM chat_stream 调用成功"
                );
                input.llm_stream = Some(stream);
                Ok(input)
            }
            Err(e) => {
                // ★ 决策 D-V12-002：LLM 调用失败时不返回 PipelineError
                // 而是构造 Error 事件流（mpsc 单事件），通过 data.output_stream 透传
                // 上层接收到的是正常的 SendMessageStream，只是第一个（也是唯一一个）事件是 Error
                tracing::error!(
                    request_id = %request_id,
                    session_id = %session_id,
                    %e,
                    "StageCallLlm: LLM chat_stream 调用失败，构造 Error 事件流"
                );

                let (tx, rx) = mpsc::unbounded::<Result<StreamEvent, RamariaError>>();
                let error_event = StreamEvent::error(request_id, e.to_string());
                // unbounded_send 对空通道永不阻塞，无需等待
                let _ = tx.unbounded_send(Ok(error_event));

                // 将错误流设置为 output_stream，绕过 Stage 10 的正常保存逻辑
                input.output_stream = Some(Box::pin(rx));
                input.llm_stream = None;

                Ok(input)
            }
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::{MockStorage, simple_context};
    use futures::StreamExt;
    use ramaria_core::error::RamariaResult;
    use ramaria_core::traits::{ChatRequest, LlmProvider, StreamDelta};
    use ramaria_core::types::{BackendConfig, ModelCapability, Session};
    use std::pin::Pin;
    use std::sync::Arc;
    use uuid::Uuid;

    /// 构造含 chat_request 和 session 的 PipelineData。
    fn full_data() -> PipelineData {
        let request_id = Uuid::new_v4();
        let session = Session::new();
        let mut data = PipelineData::new(
            "你好".to_string(),
            Some("rama-0001".to_string()),
            Some(session.id),
            request_id,
        );
        data.session = Some(session);
        data.chat_request = Some(ChatRequest {
            system_prompt: "你是 Ramaria".into(),
            memory_context: None,
            history: vec![],
            user_message: "你好".into(),
            temperature: 0.7,
            max_tokens: 4096,
            request_id,
        });
        data
    }

    // =========================================================
    // 测试: name
    // =========================================================

    #[test]
    fn stage_name() {
        let stage = StageCallLlm::new();
        assert_eq!(stage.name(), "CallLlm");
    }

    // =========================================================
    // 测试: 正常路径——LLM 调用成功，llm_stream 被设置
    // =========================================================

    #[tokio::test]
    async fn llm_success_sets_stream() {
        let ctx = simple_context();
        let stage = StageCallLlm::new();
        let data = full_data();

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        assert!(
            output.llm_stream.is_some(),
            "llm_stream should be set on success"
        );
        assert!(
            output.output_stream.is_none(),
            "output_stream should NOT be set on success (Stage 10 handles it)"
        );
    }

    // =========================================================
    // 测试: LLM 失败 → 构造 Error 事件流，不返回 Err
    // =========================================================

    #[tokio::test]
    async fn llm_failure_creates_error_stream_not_err() {
        // 构造一个始终失败的 Mock LLM
        struct FailingLlm;
        #[async_trait::async_trait]
        impl LlmProvider for FailingLlm {
            async fn chat(&self, _req: &ChatRequest) -> RamariaResult<String> {
                Err(RamariaError::llm("mock failure"))
            }
            async fn chat_stream(
                &self,
                _req: &ChatRequest,
            ) -> RamariaResult<
                Pin<Box<dyn futures::Stream<Item = RamariaResult<StreamDelta>> + Send>>,
            > {
                Err(RamariaError::llm("mock stream failure"))
            }
            fn capability(&self) -> &ModelCapability {
                panic!("FailingLlm should not be queried for capability in error path")
            }
            fn config(&self) -> &BackendConfig {
                panic!("FailingLlm should not be queried for config in error path")
            }
            async fn validate(&self) -> RamariaResult<()> {
                Ok(())
            }
            fn name(&self) -> &'static str {
                "FailingLlm"
            }
        }

        // 手动构建带 FailingLlm 的 PipelineContext
        let storage: Arc<dyn ramaria_core::traits::StorageBackend> = Arc::new(MockStorage::new());
        let llm: Arc<dyn LlmProvider> = Arc::new(FailingLlm);
        let config = ramaria_core::config::RamariaConfig::default();
        let retriever = Arc::new(std::sync::RwLock::new(
            ramaria_memory::retriever::Retriever::new(),
        ));
        let keychain = Arc::new(ramaria_llm::keychain::Keychain::new());
        let lifecycle = Arc::new(crate::session_lifecycle::SessionLifecycle::new(
            config.clone(),
        ));
        let ctx = PipelineContext::new(storage, llm, None, config, retriever, keychain, lifecycle);

        let stage = StageCallLlm::new();
        let data = full_data();

        let result = stage.execute(&ctx, data).await;
        // 不应返回 Err
        assert!(result.is_ok(), "LLM failure should NOT return Err");

        let output = result.expect("should be Ok");
        // llm_stream 应为 None（LLM 调用失败）
        assert!(
            output.llm_stream.is_none(),
            "llm_stream should be None on failure"
        );
        // output_stream 应已设置（Error 事件流）
        assert!(
            output.output_stream.is_some(),
            "output_stream should be set (error event stream)"
        );

        // 验证错误流内容
        let mut stream = output.output_stream.unwrap();
        let event = stream
            .next()
            .await
            .expect("error stream should have at least one event")
            .expect("should be Ok(StreamEvent)");

        match event {
            StreamEvent::Error { error, .. } => {
                assert!(
                    error.contains("mock stream failure"),
                    "error message should contain original error"
                );
            }
            other => {
                let kind = match other {
                    StreamEvent::Delta { .. } => "Delta",
                    StreamEvent::Done { .. } => "Done",
                    StreamEvent::Error { .. } => "Error",
                };
                panic!("expected Error event, got {kind}");
            }
        }
    }

    // =========================================================
    // 测试: chat_request 未设置 → Fatal 错误
    // =========================================================

    #[tokio::test]
    async fn missing_chat_request_returns_fatal() {
        let ctx = simple_context();
        let stage = StageCallLlm::new();
        let mut data = full_data();
        data.chat_request = None;

        let result = stage.execute(&ctx, data).await;
        match result {
            Ok(_) => panic!("should fail with missing chat_request"),
            Err(err) => {
                assert!(!err.is_retryable());
                assert_eq!(err.stage(), "CallLlm");
            }
        }
    }

    // =========================================================
    // 测试: 正常流可通过 Stage 10 消费（端到端验证）
    // =========================================================

    #[tokio::test]
    async fn normal_stream_produces_content() {
        let ctx = simple_context();
        let stage = StageCallLlm::new();
        let data = full_data();

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        let mut stream = output.llm_stream.expect("llm_stream should be set");

        // 消费流（MockLlm 返回 "mock" + done=true）
        let mut total_content = String::new();
        let mut done = false;
        while let Some(delta_result) = stream.next().await {
            let delta = delta_result.expect("delta should be Ok");
            total_content.push_str(&delta.content);
            done = delta.done;
        }
        assert!(done, "stream should end with done=true");
        assert_eq!(total_content, "mock");
    }
}
