//! crates/ramaria-app/tests/app_integration.rs - App 编排集成测试
//!
//! 测试覆盖:
//! - App 构造与初始状态
//! - 设置流程 (NeedsSetup → Ready)
//! - 隐私确认流程
//! - send_message 完整管线（mock LLM）
//! - send_message 状态检查（非 Ready 拒绝）
//! - LLM 错误处理
//! - StreamEvent 流完整性
//!
//! 安全约束:
//! - 全部使用 MockStorage + MockLlm，不涉及真实网络或数据库
//! - API key 不出现于测试代码中

mod mock_backend;

use std::sync::Arc;

use futures::StreamExt;
use ramaria_app::app::App;
use ramaria_app::privacy::PrivacyStatus;
use ramaria_app::stream_event::StreamEvent;
use ramaria_app::{
    ErrorHint, check_setup_status, confirm_privacy, determine_state, error_title, run_setup,
};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{AppState, BackendConfig, LlmProvider as LlmProviderKind, MessageRole};
use ramaria_llm::keychain::Keychain;

use mock_backend::{MockLlm, MockStorage};

// 辅助: 创建使用 MockFailingLlm 的 App
fn make_failing_app(error_msg: &str) -> (Arc<MockStorage>, Arc<mock_backend::MockFailingLlm>, App) {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::failing(error_msg));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn ramaria_core::traits::StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        config,
        keychain,
    );
    (storage, llm, app)
}

// =========================================================
// 辅助函数
// =========================================================

fn make_app() -> (Arc<MockStorage>, Arc<MockLlm>, App) {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的，我记住了。"));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn ramaria_core::traits::StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        config,
        keychain,
    );
    (storage, llm, app)
}

// =========================================================
// 构造与状态
// =========================================================

#[tokio::test]
async fn app_starts_in_needs_setup() {
    let (_, _, app) = make_app();
    assert_eq!(app.current_state(), AppState::NeedsSetup);
}

#[tokio::test]
async fn app_state_transitions_to_degraded_without_embedding() {
    let (storage, _, app) = make_app();

    // 保存后端配置
    let backend_config = BackendConfig::lm_studio_default();
    storage.save_backend_config(&backend_config).await.unwrap();

    // 设置索引版本
    storage.set_index_version(1).await.unwrap();

    // 刷新状态（无嵌入模型 → Degraded，但对话仍可用）
    let state = app.refresh_setup_state().await.unwrap();
    assert_eq!(state, AppState::Degraded);
    assert_eq!(app.current_state(), AppState::Degraded);
}

// =========================================================
// 设置流程
// =========================================================

#[tokio::test]
async fn setup_status_needs_backend() {
    let storage = MockStorage::new();
    let status = check_setup_status(&storage, false).await.unwrap();
    assert!(!status.backend_configured);
    assert!(!status.is_complete());
    assert!(!status.missing_items().is_empty());
}

#[tokio::test]
async fn setup_status_complete_after_config() {
    let storage = MockStorage::new();
    let config = BackendConfig::lm_studio_default();
    storage.save_backend_config(&config).await.unwrap();
    storage.set_index_version(1).await.unwrap();

    let status = check_setup_status(&storage, true).await.unwrap();
    assert!(status.is_complete());
    assert_eq!(determine_state(&status), AppState::Ready);
}

#[tokio::test]
async fn run_setup_full_flow() {
    let storage = MockStorage::new();
    let config = BackendConfig::lm_studio_default();

    // 模拟设置完成后的索引标记
    storage.set_index_version(1).await.unwrap();

    let state = run_setup(&storage, &config).await.unwrap();
    // run_setup 内部传入 embedding_available=false，返回 Degraded
    assert_eq!(state, AppState::Degraded);
}

// =========================================================
// 隐私确认
// =========================================================

#[tokio::test]
async fn privacy_local_provider_auto_approved() {
    let storage = MockStorage::new();
    let status = ramaria_app::privacy::check_privacy(
        &storage,
        LlmProviderKind::LmStudio,
        "http://localhost:1234/v1",
    )
    .await
    .unwrap();
    assert_eq!(status, PrivacyStatus::NotNeeded);
    assert!(status.is_confirmed());
}

#[tokio::test]
async fn privacy_online_provider_needs_confirm() {
    let storage = MockStorage::new();
    let status = ramaria_app::privacy::check_privacy(
        &storage,
        LlmProviderKind::DeepSeek,
        "https://api.deepseek.com/v1",
    )
    .await
    .unwrap();
    assert!(status.needs_user_action());
}

#[tokio::test]
async fn privacy_confirm_then_check_passes() {
    let storage = MockStorage::new();
    confirm_privacy(
        &storage,
        LlmProviderKind::DeepSeek,
        "https://api.deepseek.com/v1",
        true,
    )
    .await
    .unwrap();

    let status = ramaria_app::privacy::check_privacy(
        &storage,
        LlmProviderKind::DeepSeek,
        "https://api.deepseek.com/v1",
    )
    .await
    .unwrap();
    assert!(status.is_confirmed());
}

// =========================================================
// send_message 集成测试
// =========================================================

#[tokio::test]
async fn send_message_rejects_when_not_ready() {
    let (_, _, app) = make_app();
    let result = app.send_message("你好", None, None).await;
    match result {
        Err(err) => assert!(err.to_string().contains("尚未就绪")),
        Ok(_) => panic!("应返回错误"),
    }
}

#[tokio::test]
async fn send_message_flow_success() {
    let (storage, _, app) = make_app();

    // 准备: 设置 → Ready
    storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .unwrap();
    storage.set_index_version(1).await.unwrap();
    app.refresh_setup_state().await.unwrap();
    assert_eq!(app.current_state(), AppState::Degraded);

    // 发送消息（Degraded 状态下对话仍可用，仅向量通道降级）
    let mut stream = app.send_message("你好", None, None).await.unwrap();

    // 收集事件并验证
    let mut delta_count = 0usize;
    let mut done_seen = false;
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(StreamEvent::Delta { .. }) => delta_count += 1,
            Ok(StreamEvent::Done { .. }) => done_seen = true,
            Ok(StreamEvent::Error { .. }) => {}
            // StreamEvent 为 #[non_exhaustive]，处理未来新增事件类型
            Ok(_) => {}
            Err(_) => {}
        }
    }

    assert!(done_seen, "流应以 Done 事件结束");
    assert!(delta_count > 0, "流应包含文本增量");
}

#[tokio::test]
async fn send_message_preserves_session() {
    let (storage, _, app) = make_app();

    // 准备
    storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .unwrap();
    storage.set_index_version(1).await.unwrap();
    app.refresh_setup_state().await.unwrap();
    // Degraded 状态下 session 操作仍可用

    // 创建已知会话
    let session = storage.create_session(None).await.unwrap();
    let session_id = session.id;

    // 发送消息（使用已有会话）
    let mut stream = app
        .send_message("你好", None, Some(session_id))
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    // 验证: 会话中有消息
    let messages = storage.list_messages(session_id).await.unwrap();
    assert!(!messages.is_empty(), "会话中应有消息");

    // 应有 user 消息和 assistant 消息
    let has_user = messages.iter().any(|m| m.role == MessageRole::User);
    let has_assistant = messages.iter().any(|m| m.role == MessageRole::Assistant);
    assert!(has_user, "应有 user 消息");
    assert!(has_assistant, "应有 assistant 消息");
}

#[tokio::test]
async fn send_message_creates_new_session() {
    let (storage, _, app) = make_app();

    // 准备
    storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .unwrap();
    storage.set_index_version(1).await.unwrap();
    app.refresh_setup_state().await.unwrap();

    // 发送消息（Degraded 状态下仍自动创建会话）
    let mut stream = app.send_message("测试", None, None).await.unwrap();
    while stream.next().await.is_some() {}

    // 验证: 有活跃会话
    let sessions = storage.list_active_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
}

// =========================================================
// 错误处理
// =========================================================

#[tokio::test]
async fn error_hint_maps_correctly() {
    let err = ramaria_core::error::RamariaError::llm("连接超时");
    let hint = ErrorHint::from_error(&err);
    assert_eq!(hint.title, "LLM 服务错误");
    assert!(hint.retryable);
}

#[tokio::test]
async fn error_title_works() {
    let err = ramaria_core::error::RamariaError::privacy("未确认");
    assert_eq!(error_title(&err), "隐私设置未完成");
}

// =========================================================
// 配置流程
// =========================================================

// （原 set_state_transitions_traceable 为琐碎 setter/getter 往返测试、
//  backend_config_accessible 仅断言默认配置常量，
//  check_privacy_integration 与 privacy_local_provider_auto_approved 重复，均已删除）

// =========================================================
// MockFailingLlm 错误路径集成测试（LLM 失败仅发 Error 不发 Done）
// =========================================================
// 验证 LLM 失败时:
// 1. 流中包含 Error 事件（用户可感知错误）
// 2. 流中不包含 Done 事件（LLM 失败仅发 Error 不发 Done）
// 3. 错误事件内容与 MockFailingLlm 的错误消息一致

#[tokio::test]
async fn send_message_failing_llm_cases() {
    // 两个失败场景：HTTP 500 内部错误 / 连接被拒绝
    for error_msg in [
        "LLM 服务返回 500 内部错误",
        "无法连接到 LLM 服务: 连接被拒绝",
    ] {
        let (storage, _, app) = make_failing_app(error_msg);

        // 准备: 设置 → Ready
        storage
            .save_backend_config(&BackendConfig::lm_studio_default())
            .await
            .unwrap();
        storage.set_index_version(1).await.unwrap();
        app.refresh_setup_state().await.unwrap();
        assert_eq!(app.current_state(), AppState::Degraded);

        // 发送消息（LLM 将失败）
        let mut stream = app.send_message("测试消息", None, None).await.unwrap();

        let mut error_seen = false;
        let mut done_seen = false;
        let mut delta_count = 0usize;

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(StreamEvent::Delta { .. }) => delta_count += 1,
                Ok(StreamEvent::Done { .. }) => done_seen = true,
                Ok(StreamEvent::Error { error, .. }) => {
                    error_seen = true;
                    assert!(
                        error.contains("500") || error.contains("连接被拒绝"),
                        "错误事件应包含原始错误信息，实际: {error}"
                    );
                }
                // StreamEvent 为 #[non_exhaustive]，处理未来新增事件类型
                Ok(_) => {}
                Err(_) => {}
            }
        }

        assert!(error_seen, "LLM 失败时应产生 Error 事件");
        assert!(!done_seen, "LLM 失败时不应产生 Done 事件");
        assert_eq!(delta_count, 0, "LLM 失败时不应有 Delta 事件");
    }
}
