//! crates/ramaria-app/src/stages/build_request.rs - Stage 8: ChatRequest 构造
//!
//! 设计特点:
//! - 从 PipelineData 的 budgeted_* 字段构造 ChatRequest
//! - 含 request_id（流事件追踪）、temperature（来自 backend_config）、max_tokens
//! - 前置依赖检查: budgeted_system_prompt 未设置时返回 Fatal（Stage 7 必须在前）
//! - system_prompt 使用预算后的截断版本，memory_context 使用截断后版本
//! - 用户消息完整保留（不截断）
//! - 不涉及 I/O，纯数据组装，零异步等待

use async_trait::async_trait;
use ramaria_core::error::RamariaError;
use ramaria_core::traits::ChatRequest;

use crate::pipeline::{PipelineContext, PipelineData, PipelineError, PipelineStage};

// =========================================================
// StageBuildRequest
// =========================================================

/// Stage 8: ChatRequest 构造。
///
/// 职责:
/// - 从 PipelineData 中提取所有预算管理后的字段
/// - 构造 LLM provider 可接受的 `ChatRequest`
/// - 写入 `PipelineData.chat_request` 供 Stage 9 使用
///
/// 输入依赖:
/// - Stage 7 (TokenBudget): `budgeted_system_prompt`, `budgeted_memory_context`, `budgeted_history`
/// - Stage 2 (CheckPrivacy): `backend_config` (temperature, max_tokens)
/// - 输入参数: `user_input`, `request_id`
pub struct StageBuildRequest;

impl StageBuildRequest {
    /// 创建 StageBuildRequest 实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for StageBuildRequest {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineStage for StageBuildRequest {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        "BuildRequest"
    }

    async fn execute(
        &self,
        _ctx: &PipelineContext,
        mut input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        // 前置条件校验：budgeted_system_prompt 必须存在
        let system_prompt = input.budgeted_system_prompt.take().ok_or_else(|| {
            PipelineError::fatal(
                "BuildRequest",
                RamariaError::validation(
                    "budgeted_system_prompt 未设置——Stage 7 (TokenBudget) 必须在前",
                ),
            )
        })?;

        // backend_config 必须存在（含 temperature / max_tokens）
        let backend_config = input.backend_config.as_ref().ok_or_else(|| {
            PipelineError::fatal(
                "BuildRequest",
                RamariaError::validation("backend_config 未设置——Stage 2 (CheckPrivacy) 必须在前"),
            )
        })?;

        let chat_request = ChatRequest {
            system_prompt,
            memory_context: input.budgeted_memory_context.take(),
            history: std::mem::take(&mut input.budgeted_history),
            user_message: input.user_input.clone(),
            temperature: backend_config.temperature,
            max_tokens: backend_config.max_tokens,
            request_id: input.request_id,
            template_version: ramaria_memory::prompt::PROMPT_TEMPLATE_VERSION.to_string(),
        };

        tracing::debug!(
            request_id = %input.request_id,
            system_prompt_chars = chat_request.system_prompt.chars().count(),
            history_msgs = chat_request.history.len(),
            has_memory = chat_request.memory_context.is_some(),
            temperature = chat_request.temperature,
            max_tokens = chat_request.max_tokens,
            "StageBuildRequest: ChatRequest 已构造"
        );

        input.chat_request = Some(chat_request);
        Ok(input)
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::simple_context;
    use ramaria_core::traits::ChatMessage;
    use ramaria_core::types::{
        BackendConfig, LlmProvider as LlmProviderKind, MessageRole, ModelCapability,
    };
    use uuid::Uuid;

    /// 构造含完整前序数据的 PipelineData。
    fn full_data() -> PipelineData {
        let request_id = Uuid::new_v4();
        let mut data = PipelineData::new(
            "今天天气真好".to_string(),
            Some("rama-0001".to_string()),
            None,
            request_id,
        );
        // Stage 2 产出
        data.backend_config = Some(BackendConfig {
            provider: LlmProviderKind::LmStudio,
            base_url: "http://localhost:1234/v1".into(),
            embedding_model_id: None,
            embedding_model_path: None,
            temperature: 0.7,
            max_tokens: 4096,
            capability: ModelCapability {
                provider: LlmProviderKind::LmStudio,
                model_id: "test-model".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
        });
        // Stage 7 产出
        data.budgeted_system_prompt = Some("你是 Ramaria，一个善解人意的 AI 助手。".into());
        data.budgeted_memory_context = Some("用户之前讨论过天气话题。".into());
        data.budgeted_history = vec![
            ChatMessage {
                role: MessageRole::User,
                content: "你好".into(),
            },
            ChatMessage {
                role: MessageRole::Assistant,
                content: "你好！有什么可以帮你的吗？".into(),
            },
        ];
        data.estimated_tokens = 42;
        data
    }

    // =========================================================
    // 测试: name
    // =========================================================

    #[test]
    fn stage_name() {
        let stage = StageBuildRequest::new();
        assert_eq!(stage.name(), "BuildRequest");
    }

    // =========================================================
    // 测试: 正常路径——ChatRequest 构造完整
    // =========================================================

    #[tokio::test]
    async fn builds_complete_chat_request() {
        let ctx = simple_context();
        let stage = StageBuildRequest::new();
        let data = full_data();
        let request_id = data.request_id;
        let user_input = data.user_input.clone();

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        let req = output
            .chat_request
            .as_ref()
            .expect("chat_request should be set");

        assert_eq!(req.request_id, request_id);
        assert_eq!(req.user_message, user_input);
        assert!(req.system_prompt.contains("Ramaria"));
        assert_eq!(
            req.memory_context.as_deref(),
            Some("用户之前讨论过天气话题。")
        );
        assert_eq!(req.history.len(), 2);
        assert_eq!(req.temperature, 0.7);
        assert_eq!(req.max_tokens, 4096);
    }

    // =========================================================
    // 测试: 关键字段未设置 → Fatal 错误
    // =========================================================

    #[tokio::test]
    async fn missing_field_returns_fatal() {
        let cases: Vec<(&str, fn(&mut PipelineData))> = vec![
            ("system_prompt", |d| d.budgeted_system_prompt = None),
            ("backend_config", |d| d.backend_config = None),
        ];
        for (label, mutate) in cases {
            let ctx = simple_context();
            let stage = StageBuildRequest::new();
            let mut data = full_data();
            mutate(&mut data);

            let result = stage.execute(&ctx, data).await;
            match result {
                Ok(_) => panic!("should fail with missing {label}"),
                Err(err) => {
                    assert!(!err.is_retryable(), "missing {label} should be Fatal");
                    assert_eq!(err.stage(), "BuildRequest");
                }
            }
        }
    }

    // =========================================================
    // 测试: memory_context 为 None 时正常（ChatRequest.memory_context = None）
    // =========================================================

    #[tokio::test]
    async fn no_memory_context_ok() {
        let ctx = simple_context();
        let stage = StageBuildRequest::new();
        let mut data = full_data();
        data.budgeted_memory_context = None;

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        let req = output.chat_request.as_ref().expect("chat_request set");
        assert!(req.memory_context.is_none());
    }

    // =========================================================
    // 测试: 空历史正常
    // =========================================================

    #[tokio::test]
    async fn empty_history_ok() {
        let ctx = simple_context();
        let stage = StageBuildRequest::new();
        let mut data = full_data();
        data.budgeted_history = vec![];

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        let req = output.chat_request.as_ref().expect("chat_request set");
        assert!(req.history.is_empty());
    }

    // =========================================================
    // 测试: request_id 正确传递
    // =========================================================

    #[tokio::test]
    async fn request_id_preserved() {
        let ctx = simple_context();
        let stage = StageBuildRequest::new();
        let rid = Uuid::new_v4();
        let mut data = full_data();
        data.request_id = rid;

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        assert_eq!(output.chat_request.as_ref().unwrap().request_id, rid);
    }
}
