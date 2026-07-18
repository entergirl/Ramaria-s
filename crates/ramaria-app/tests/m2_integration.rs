//! rust/crates/ramaria-app/tests/m2_integration.rs - M2 Pipeline 全流程集成测试
//!
//! 测试覆盖:
//! - 全 10 个 Stage Pipeline 正常路径（Mock 全部依赖）
//! - Stage 1-10 顺序执行，每个 Stage 产出正确
//! - Persona 绑定验证：session.persona_uid 传播到消息
//! - 流式转发验证：LLM 输出通过 Stage 9→10 正确转发
//! - 错误传播：缺失前置数据 → PipelineError（Fatal）
//! - 错误路径：Stage 9 LLM 失败 → Error 事件流透传

mod mock_backend;

use std::sync::Arc;

use futures::StreamExt;
use ramaria_app::pipeline::{PipelineContext, PipelineData, SendMessagePipeline};
use ramaria_app::session_lifecycle::SessionLifecycle;
use ramaria_app::stages::{
    StageBuildPrompt, StageBuildRequest, StageCallLlm, StageCheckPrivacy, StageCheckState,
    StageLoadHistory, StagePersistMessage, StageResolveSession, StageRetrieveMemory,
    StageTokenBudget,
};
use ramaria_app::stream_event::StreamEvent;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend};
use ramaria_core::types::{
    AppState, BackendConfig, LlmProvider as LlmProviderKind, MessageRole, Persona, PersonaKind,
    PrivacyConsent,
};
use ramaria_llm::keychain::Keychain;
use ramaria_memory::retriever::Retriever;
use uuid::Uuid;

use mock_backend::{MockEmbedding, MockLlm, MockStorage};

// =========================================================
// 测试辅助
// =========================================================

/// 构建测试用 PipelineContext。
fn make_ctx(
    storage: Arc<MockStorage>,
    llm: Arc<MockLlm>,
    embedding: Option<Arc<MockEmbedding>>,
) -> PipelineContext {
    let config = RamariaConfig::default();
    let retriever = Arc::new(std::sync::RwLock::new(Retriever::new()));
    let keychain = Arc::new(Keychain::new());
    let lifecycle = Arc::new(SessionLifecycle::new(config.clone()));

    PipelineContext::new(
        storage as Arc<dyn StorageBackend>,
        llm as Arc<dyn LlmProvider>,
        embedding.map(|e| e as Arc<dyn EmbeddingProvider>),
        config,
        retriever,
        keychain,
        lifecycle,
    )
}

/// 创建含全部 10 个 Stage 的 Pipeline。
fn full_pipeline() -> SendMessagePipeline {
    SendMessagePipeline::new(vec![
        Box::new(StageCheckState::new()),
        Box::new(StageCheckPrivacy::new()),
        Box::new(StageResolveSession::new()),
        Box::new(StageLoadHistory::new()),
        Box::new(StageRetrieveMemory::new()),
        Box::new(StageBuildPrompt::new()),
        Box::new(StageTokenBudget::new()),
        Box::new(StageBuildRequest::new()),
        Box::new(StageCallLlm::new()),
        Box::new(StagePersistMessage::new()),
    ])
}

/// 创建带 Ready 状态的测试数据。
fn ready_data(user_input: &str, persona_uid: Option<&str>) -> PipelineData {
    PipelineData::new(
        user_input.to_string(),
        persona_uid.map(|s| s.to_string()),
        None,
        Uuid::new_v4(),
    )
    .with_app_state(AppState::Ready)
}

/// 创建测试用 Persona。
fn test_persona(uid: &str, name: &str) -> Persona {
    Persona::new(
        uid.to_string(),
        name.to_string(),
        PersonaKind::User,
        1,
        "local".to_string(),
    )
}

// =========================================================
// 全流程正常路径测试
// =========================================================

/// 全 10 个 Stage 顺序执行成功，产出正确的 PipelineData。
#[tokio::test]
async fn full_10_stage_pipeline_succeeds() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("你好！这是测试回复"));
    let ctx = make_ctx(Arc::clone(&storage) as Arc<MockStorage>, llm, None);
    let pipeline = full_pipeline();
    let data = ready_data("你好", Some("rama-0001"));

    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_ok(), "full pipeline should succeed");
    let output = result.expect("pipeline should succeed");

    // Stage 1: app_state 已通过
    assert_eq!(output.app_state, Some(AppState::Ready));

    // Stage 2: backend_config 已设置
    assert!(output.backend_config.is_some());

    // Stage 3: session 已创建
    let session = output.session.as_ref().expect("session should be set");
    assert!(session.ended_at.is_none());
    // v1.2: session 应绑定 persona_uid
    assert_eq!(session.persona_uid.as_deref(), Some("rama-0001"));

    // Stage 6: system_prompt 已装配
    assert!(output.system_prompt.is_some());
    assert!(
        output.system_prompt.as_ref().unwrap().contains("Ramaria"),
        "system_prompt should contain default Ramaria text"
    );

    // Stage 7: token 预算已应用（通过 estimated_tokens 间接验证）
    // Note: budgeted_system_prompt/budgeted_memory_context/budgeted_history
    // 已被 Stage 8 (BuildRequest) take() 消费，不再可用
    assert!(
        output.estimated_tokens > 0,
        "token budget should estimate tokens"
    );

    // Stage 8: ChatRequest 已构造（内含预算管理后的值）
    assert!(output.chat_request.is_some());
    let req = output.chat_request.as_ref().unwrap();
    assert_eq!(req.user_message, "你好");
    assert!(req.system_prompt.contains("Ramaria"));
    // 新 session 无历史消息，history 为空是正常行为

    // Stage 9-10: 流转发已完成, output_stream 已设置
    assert!(output.output_stream.is_some());
}

/// 验证 stream_forward_task 正确转发 LLM 输出。
#[tokio::test]
async fn pipeline_forwards_llm_stream_correctly() {
    let storage = Arc::new(MockStorage::new());
    // 使用多字符回复，验证逐字转发
    let llm = Arc::new(MockLlm::new("你好世界！"));
    let ctx = make_ctx(Arc::clone(&storage) as Arc<MockStorage>, llm, None);
    let pipeline = full_pipeline();
    let data = ready_data("测试", Some("rama-0001"));

    let result = pipeline.execute(&ctx, data).await;
    assert!(result.is_ok());

    let mut output = result.expect("should succeed");
    let mut stream = output
        .output_stream
        .take()
        .expect("output_stream must be set");

    // 消费流，收集所有 Delta 事件
    let mut all_content = String::new();
    let mut delta_count = 0;
    let mut got_done = false;

    while let Some(event_result) = stream.next().await {
        match event_result.expect("stream event should be Ok") {
            StreamEvent::Delta { content, .. } => {
                all_content.push_str(&content);
                delta_count += 1;
            }
            StreamEvent::Done { total_chars, .. } => {
                got_done = true;
                assert!(total_chars > 0);
            }
            StreamEvent::Error { error, .. } => {
                panic!("unexpected error in stream: {error}");
            }
            _ => {} // StreamEvent is #[non_exhaustive]
        }
    }

    assert!(got_done, "stream should end with Done event");
    assert_eq!(all_content, "你好世界！");
    // MockLlm 按字符拆分，5 个中文字符 = 5 个 Delta
    assert_eq!(delta_count, 5);
}

/// 验证用户消息携带 persona_uid（v1.2 Session-Persona 绑定）。
#[tokio::test]
async fn user_message_has_persona_uid() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("回复"));
    let ctx = make_ctx(Arc::clone(&storage) as Arc<MockStorage>, llm, None);
    let pipeline = full_pipeline();
    let data = ready_data("你好", Some("char-0001"));

    let result = pipeline.execute(&ctx, data).await;
    assert!(result.is_ok());

    // 消费输出流确保后台任务完成
    let mut output = result.expect("should succeed");
    let mut stream = output.output_stream.take().unwrap();
    while let Some(_) = stream.next().await {}

    // 获取 session 并验证消息
    let session = output.session.as_ref().unwrap();
    let messages = storage
        .list_messages(session.id)
        .await
        .expect("list_messages should succeed");

    let user_msg = messages
        .iter()
        .find(|m| m.role == MessageRole::User)
        .expect("user message should exist");

    // v1.2: 用户消息应携带 persona_uid
    assert_eq!(
        user_msg.persona_uid.as_deref(),
        Some("char-0001"),
        "user message should carry session persona_uid"
    );

    let assistant_msg = messages
        .iter()
        .find(|m| m.role == MessageRole::Assistant)
        .expect("assistant message should exist");

    assert_eq!(
        assistant_msg.persona_uid.as_deref(),
        Some("char-0001"),
        "assistant message should carry persona_uid"
    );
}

/// Persona 切换场景：创建→发消息→验证 session 归属正确。
#[tokio::test]
async fn persona_switch_creates_new_session_with_correct_persona() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("回复"));
    let ctx = make_ctx(Arc::clone(&storage) as Arc<MockStorage>, llm, None);

    // 第一轮：persona "char-0001"
    let pipeline = full_pipeline();
    let data1 = ready_data("消息1", Some("char-0001"));
    let result1 = pipeline.execute(&ctx, data1).await;
    assert!(result1.is_ok());
    let out1 = result1.expect("first pipeline should succeed");
    let sid1 = out1.session.as_ref().unwrap().id;
    assert_eq!(
        out1.session.as_ref().unwrap().persona_uid.as_deref(),
        Some("char-0001")
    );

    // 消费流
    let mut stream1 = out1.output_stream.unwrap();
    while let Some(_) = stream1.next().await {}

    // 第二轮：切换 persona "char-0002"，前端传新 session_id=None
    let pipeline2 = full_pipeline();
    let data2 = ready_data("消息2", Some("char-0002"));
    let result2 = pipeline2.execute(&ctx, data2).await;
    assert!(result2.is_ok());
    let out2 = result2.expect("second pipeline should succeed");
    let sid2 = out2.session.as_ref().unwrap().id;

    // 应创建不同的 session
    assert_ne!(
        sid1, sid2,
        "different persona should create different session"
    );
    assert_eq!(
        out2.session.as_ref().unwrap().persona_uid.as_deref(),
        Some("char-0002")
    );

    // 消费流
    let mut stream2 = out2.output_stream.unwrap();
    while let Some(_) = stream2.next().await {}

    // 验证两条消息各自归属正确的 persona
    let msgs1 = storage.list_messages(sid1).await.unwrap();
    let user1 = msgs1.iter().find(|m| m.role == MessageRole::User).unwrap();
    assert_eq!(user1.persona_uid.as_deref(), Some("char-0001"));

    let msgs2 = storage.list_messages(sid2).await.unwrap();
    let user2 = msgs2.iter().find(|m| m.role == MessageRole::User).unwrap();
    assert_eq!(user2.persona_uid.as_deref(), Some("char-0002"));
}

/// 无 persona_uid 时 session.persona_uid 为 None（存量兼容）。
#[tokio::test]
async fn no_persona_uid_session_is_none() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("回复"));
    let ctx = make_ctx(Arc::clone(&storage) as Arc<MockStorage>, llm, None);
    let pipeline = full_pipeline();
    let data = ready_data("测试", None);

    let result = pipeline.execute(&ctx, data).await;
    assert!(result.is_ok());

    let output = result.expect("should succeed");
    assert!(output.session.as_ref().unwrap().persona_uid.is_none());
}

// =========================================================
// DB persona 数据 → System Prompt 验证
// =========================================================

/// 有 DB persona 数据时，system_prompt 应为结构化装配。
#[tokio::test]
async fn db_persona_produces_structured_prompt() {
    let storage = Arc::new(MockStorage::new());

    // 创建 persona
    let persona = test_persona("char-0001", "测试角色");
    storage.create_persona(&persona).await.unwrap();

    let llm = Arc::new(MockLlm::new("回复"));
    let ctx = make_ctx(Arc::clone(&storage) as Arc<MockStorage>, llm, None);
    let pipeline = full_pipeline();
    let data = ready_data("你好", Some("char-0001"));

    let result = pipeline.execute(&ctx, data).await;
    assert!(result.is_ok());

    let output = result.expect("should succeed");
    let prompt = output.system_prompt.as_ref().unwrap();

    // 有 persona 数据时，assemble_prompt 会引用 persona 名称
    // traits/facts 为空 + persona.toml 不存在 → 降级到 5-Block 装配（含 persona 名）
    assert!(!prompt.is_empty(), "prompt should be non-empty");
    assert!(
        prompt.contains("测试角色") || prompt.contains("Ramaria"),
        "prompt should contain persona name or Ramaria fallback"
    );
}

// =========================================================
// 错误路径测试
// =========================================================

/// Stage 9 LLM 失败 → output_stream 含 Error 事件（不返回 PipelineError）。
#[tokio::test]
async fn llm_failure_produces_error_stream() {
    let storage: Arc<dyn StorageBackend> = Arc::new(MockStorage::new());
    // MockFailingLlm 始终返回错误
    let failing_llm: Arc<dyn LlmProvider> =
        Arc::new(mock_backend::MockLlm::failing("mock connection refused"));
    let config = RamariaConfig::default();
    let retriever = Arc::new(std::sync::RwLock::new(Retriever::new()));
    let keychain = Arc::new(Keychain::new());
    let lifecycle = Arc::new(SessionLifecycle::new(config.clone()));
    let ctx = PipelineContext::new(
        storage,
        failing_llm,
        None,
        config,
        retriever,
        keychain,
        lifecycle,
    );

    let pipeline = full_pipeline();
    let data = ready_data("测试", Some("rama-0001"));

    let result = pipeline.execute(&ctx, data).await;
    // Pipeline 应返回 Ok（Stage 9 失败时构造 Error 事件流，不返回 Err）
    assert!(
        result.is_ok(),
        "pipeline should return Ok even when LLM fails"
    );

    let mut output = result.expect("should succeed");
    let mut stream = output
        .output_stream
        .take()
        .expect("output_stream must be set");

    let event = stream
        .next()
        .await
        .expect("stream should have at least one event")
        .expect("should be Ok");

    match event {
        StreamEvent::Error { error, .. } => {
            assert!(
                error.contains("mock connection refused"),
                "error should contain original message"
            );
        }
        _other => panic!("expected Error event, got Delta/Done"),
    }
}

/// Stage 8 校验：缺失 budgeted_system_prompt → Fatal（需跳过 Stage 6 测试）。
/// 在全 10 Stage 管线中，Stage 6 总会设置 system_prompt，故此场景在实际运行中不会发生。
/// 此处用不含 Stage 6 的管线验证 Stage 7→8 的前置校验。
#[tokio::test]
async fn stage_8_missing_budgeted_prompt_returns_fatal() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("test"));
    let ctx = make_ctx(Arc::clone(&storage) as Arc<MockStorage>, llm, None);

    // 使用不含 Stage 6 的管线，模拟 budgeted_system_prompt 缺失
    let pipeline = SendMessagePipeline::new(vec![
        Box::new(StageCheckState::new()),
        Box::new(StageCheckPrivacy::new()),
        Box::new(StageResolveSession::new()),
        Box::new(StageLoadHistory::new()),
        Box::new(StageRetrieveMemory::new()),
        // Stage 6 被跳过——budgeted_system_prompt 不会被设置
        Box::new(StageTokenBudget::new()),
        Box::new(StageBuildRequest::new()),
    ]);

    let data = ready_data("测试", Some("rama-0001"));

    let result = pipeline.execute(&ctx, data).await;
    let err = match result {
        Ok(_) => panic!("should fail at TokenBudget without Stage 6"),
        Err(e) => e,
    };
    assert!(!err.is_retryable());
    assert_eq!(err.stage(), "TokenBudget");
}

/// FatalError 状态 → Stage 1 即中止。
#[tokio::test]
async fn fatal_error_state_stops_pipeline_immediately() {
    let ctx = make_ctx(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::new("test")),
        None,
    );
    let pipeline = full_pipeline();
    let data = PipelineData::new("test".into(), None, None, Uuid::new_v4())
        .with_app_state(AppState::FatalError);

    let result = pipeline.execute(&ctx, data).await;
    let err = match result {
        Ok(_) => panic!("should fail at CheckState"),
        Err(e) => e,
    };
    assert!(!err.is_retryable());
    assert_eq!(err.stage(), "CheckState");
}

// =========================================================
// 线上 provider 隐私确认测试
// =========================================================

/// 线上 provider + 已确认 → 全流程通过。
#[tokio::test]
async fn online_provider_with_consent_full_pipeline() {
    let storage = Arc::new(MockStorage::new());
    storage.add_privacy_consent(PrivacyConsent::new(
        LlmProviderKind::DeepSeek,
        "https://api.deepseek.com/v1".to_string(),
        true,
    ));

    let deepseek_config = BackendConfig::deepseek_default();
    let llm = Arc::new(MockLlm::with_config("deepseek reply", deepseek_config));
    let ctx = make_ctx(Arc::clone(&storage) as Arc<MockStorage>, llm, None);
    let pipeline = full_pipeline();
    let data = ready_data("测试", Some("rama-0001"));

    let result = pipeline.execute(&ctx, data).await;
    assert!(result.is_ok());
}

/// 线上 provider + 未确认 → Stage 2 返回 Retryable。
#[tokio::test]
async fn online_provider_without_consent_stops_at_privacy() {
    let storage = Arc::new(MockStorage::new());
    let deepseek_config = BackendConfig::deepseek_default();
    let llm = Arc::new(MockLlm::with_config("deepseek reply", deepseek_config));
    let ctx = make_ctx(Arc::clone(&storage) as Arc<MockStorage>, llm, None);
    let pipeline = full_pipeline();
    let data = ready_data("测试", Some("rama-0001"));

    let result = pipeline.execute(&ctx, data).await;
    let err = match result {
        Ok(_) => panic!("online provider without consent should fail"),
        Err(e) => e,
    };
    assert!(err.is_retryable());
    assert_eq!(err.stage(), "CheckPrivacy");
}
