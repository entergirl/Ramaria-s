//! rust/crates/ramaria-app/tests/m1_integration.rs - M1 Pipeline 集成测试
//!
//! 测试覆盖:
//! - Pipeline 前 5 个 Stage 正常路径（Mock 全部依赖）
//! - State → Privacy → Session → History → Memory 全部通过
//! - 错误传播：FatalError 状态中止管线
//! - 错误传播：线上 provider 未确认隐私中止管线（Retryable）
//! - 空 session 正常通过
//! - L1 上下文预加载

mod mock_backend;

use std::sync::Arc;

use ramaria_app::pipeline::{PipelineContext, PipelineData, SendMessagePipeline};
use ramaria_app::session_lifecycle::SessionLifecycle;
use ramaria_app::stages::{
    StageCheckPrivacy, StageCheckState, StageLoadHistory, StageResolveSession, StageRetrieveMemory,
};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend};
use ramaria_core::types::{
    AppState, BackendConfig, LlmProvider as LlmProviderKind, MemoryL1, Message, MessageRole,
    MessageSource, PrivacyConsent,
};
use ramaria_llm::keychain::Keychain;
use ramaria_memory::retriever::Retriever;
use uuid::Uuid;

use mock_backend::{MockEmbedding, MockLlm, MockStorage};

// =========================================================
// 测试辅助
// =========================================================

/// 构建测试用 PipelineContext（使用功能完整的 Mock）。
fn make_pipeline_context(
    storage: Arc<MockStorage>,
    llm: Arc<MockLlm>,
    embedding: Option<Arc<MockEmbedding>>,
) -> PipelineContext {
    let config = RamariaConfig::default();
    let retriever = Arc::new(std::sync::Mutex::new(Retriever::new()));
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

/// 创建含 5 个 Stage 的 Pipeline。
fn make_pipeline() -> SendMessagePipeline {
    SendMessagePipeline::new(vec![
        Box::new(StageCheckState::new()),
        Box::new(StageCheckPrivacy::new()),
        Box::new(StageResolveSession::new()),
        Box::new(StageLoadHistory::new()),
        Box::new(StageRetrieveMemory::new()),
    ])
}

/// 创建带 Ready 状态的测试数据。
fn ready_data(user_input: &str) -> PipelineData {
    PipelineData::new(
        user_input.to_string(),
        Some("rama-0001".to_string()),
        None,
        Uuid::new_v4(),
    )
    .with_app_state(AppState::Ready)
}

// =========================================================
// Pipeline 正常路径测试
// =========================================================

#[tokio::test]
async fn pipeline_all_5_stages_succeed() {
    let ctx = make_pipeline_context(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::new("test reply")),
        None,
    );
    let pipeline = make_pipeline();
    let data = ready_data("你好");

    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_ok());
    let output = result.expect("all stages should succeed");

    // Stage 1: app_state 已通过
    assert_eq!(output.app_state, Some(AppState::Ready));

    // Stage 2: backend_config 已设置
    assert!(output.backend_config.is_some());

    // Stage 3: session 已创建
    assert!(output.session.is_some());
    assert!(output.session.as_ref().unwrap().ended_at.is_none());

    // Stage 4: history_messages 已加载（空 session 为合法场景）
    // recent_summaries 可为空（无 L1 数据）
    assert!(output.recent_summaries.is_empty());

    // Stage 5: memory_context 为空（空检索器）
    assert!(output.memory_context.is_none());
}

#[tokio::test]
async fn pipeline_with_messages_loads_history() {
    let storage = Arc::new(MockStorage::new());
    let session_id = Uuid::new_v4();
    storage.add_active_session(session_id);
    storage.add_messages(
        session_id,
        vec![
            Message::new(
                session_id,
                MessageRole::User,
                "你好".into(),
                MessageSource::Local,
            ),
            Message::new(
                session_id,
                MessageRole::Assistant,
                "你好！".into(),
                MessageSource::Online,
            ),
        ],
    );

    let ctx = make_pipeline_context(storage, Arc::new(MockLlm::new("test")), None);
    let pipeline = make_pipeline();

    // 使用指定 session_id
    let data = PipelineData::new(
        "继续聊".to_string(),
        Some("rama-0001".to_string()),
        Some(session_id),
        Uuid::new_v4(),
    )
    .with_app_state(AppState::Ready);

    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_ok());
    let output = result.expect("pipeline should succeed");
    assert_eq!(output.history_messages.len(), 2);
    assert_eq!(output.session.as_ref().unwrap().id, session_id);
}

#[tokio::test]
async fn pipeline_with_l1_summaries_preloads_context() {
    let storage = Arc::new(MockStorage::new());

    // 预填充 L1 摘要
    let mut l1 = MemoryL1::new(Uuid::new_v4(), "讨论了编程话题".into(), Some("下午".into()));
    l1.atmosphere = Some("融洽".into());
    l1.created_at = 1700000000000;
    storage.add_l1_summaries("rama-0001", vec![l1]);

    let ctx = make_pipeline_context(storage, Arc::new(MockLlm::new("test")), None);
    let pipeline = make_pipeline();
    let data = ready_data("你好");

    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_ok());
    let output = result.expect("pipeline should succeed");
    assert_eq!(output.recent_summaries.len(), 1);
    assert!(output.recent_summaries[0].contains("讨论了编程话题"));
    assert!(output.last_active_at.is_some());
}

#[tokio::test]
async fn pipeline_with_embedding_succeeds() {
    let ctx = make_pipeline_context(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::new("test")),
        Some(Arc::new(MockEmbedding::new())),
    );
    let pipeline = make_pipeline();
    let data = ready_data("测试查询");

    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn pipeline_preserves_user_input() {
    let ctx = make_pipeline_context(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::new("test")),
        None,
    );
    let pipeline = make_pipeline();
    let data = ready_data("这是一条测试消息");

    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_ok());
    let output = result.expect("pipeline should succeed");
    assert_eq!(output.user_input, "这是一条测试消息");
    assert_eq!(output.persona_uid.as_deref(), Some("rama-0001"));
}

// =========================================================
// 错误传播测试
// =========================================================

#[tokio::test]
async fn pipeline_stops_on_fatal_error_state() {
    let ctx = make_pipeline_context(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::new("test")),
        None,
    );
    let pipeline = make_pipeline();

    let data = PipelineData::new("test".into(), None, None, Uuid::new_v4())
        .with_app_state(AppState::FatalError);

    let result = pipeline.execute(&ctx, data).await;

    // Stage 1 (CheckState) 应拒绝 FatalError 状态
    assert!(result.is_err());
    let err = result.err().unwrap();
    assert!(!err.is_retryable());
    assert_eq!(err.stage(), "CheckState");
}

#[tokio::test]
async fn pipeline_stops_on_needs_setup_state() {
    let ctx = make_pipeline_context(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::new("test")),
        None,
    );
    let pipeline = make_pipeline();
    let data = PipelineData::new("test".into(), None, None, Uuid::new_v4())
        .with_app_state(AppState::NeedsSetup);

    let result = pipeline.execute(&ctx, data).await;

    assert!(result.is_err());
    let err = result.err().unwrap();
    assert_eq!(err.stage(), "CheckState");
}

#[tokio::test]
async fn pipeline_local_provider_skips_privacy() {
    // 本地 provider（LM Studio）跳过隐私确认
    let ctx = make_pipeline_context(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::new("test reply")),
        None,
    );
    let pipeline = make_pipeline();
    let data = ready_data("test");

    let result = pipeline.execute(&ctx, data).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn pipeline_online_provider_without_consent_returns_retryable() {
    // 线上 provider（DeepSeek）且无隐私确认 → Stage 2 返回 Retryable
    let deepseek_config = BackendConfig::deepseek_default();
    let ctx = make_pipeline_context(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::with_config("deepseek reply", deepseek_config)),
        None,
    );
    let pipeline = make_pipeline();
    let data = ready_data("test");

    let result = pipeline.execute(&ctx, data).await;

    let err = match result {
        Ok(_) => panic!("online provider without consent should fail"),
        Err(e) => e,
    };
    assert!(err.is_retryable());
    assert_eq!(err.stage(), "CheckPrivacy");
}

#[tokio::test]
async fn pipeline_online_provider_with_consent_succeeds() {
    // 线上 provider（DeepSeek）且有隐私确认 → 通过
    let storage = Arc::new(MockStorage::new());
    storage.add_privacy_consent(PrivacyConsent::new(
        LlmProviderKind::DeepSeek,
        "https://api.deepseek.com/v1".to_string(),
        true,
    ));

    let deepseek_config = BackendConfig::deepseek_default();
    let ctx = make_pipeline_context(
        storage,
        Arc::new(MockLlm::with_config("deepseek reply", deepseek_config)),
        None,
    );
    let pipeline = make_pipeline();
    let data = ready_data("test");

    let result = pipeline.execute(&ctx, data).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn pipeline_degraded_state_passes_with_warn() {
    let ctx = make_pipeline_context(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::new("test")),
        None,
    );
    let pipeline = make_pipeline();
    let data = PipelineData::new("test".into(), None, None, Uuid::new_v4())
        .with_app_state(AppState::Degraded);

    let result = pipeline.execute(&ctx, data).await;

    // Degraded 状态允许对话（仅向量通道不可用）
    assert!(result.is_ok());
}

// =========================================================
// Error → RamariaError 转换
// =========================================================

#[tokio::test]
async fn pipeline_error_converts_to_ramaria_error() {
    let ctx = make_pipeline_context(
        Arc::new(MockStorage::new()),
        Arc::new(MockLlm::new("test")),
        None,
    );
    let pipeline = make_pipeline();
    let data = PipelineData::new("test".into(), None, None, Uuid::new_v4())
        .with_app_state(AppState::FatalError);

    let result: Result<PipelineData, ramaria_core::error::RamariaError> =
        pipeline.execute(&ctx, data).await.map_err(|e| e.into());

    assert!(result.is_err());
    let err = result.err().unwrap();
    assert_eq!(err.category(), "validation");
}
