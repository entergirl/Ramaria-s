//! crates/ramaria-service/tests/suites/app_orchestration.rs - 装配、状态机、setup 与隐私门禁用例
//!
//! 设计特点:
//! - 覆盖引擎装配后的初始状态、setup 缺项诊断与状态推进、首次配置落库
//! - 覆盖隐私确认门禁（本地 provider 免确认 / 线上 provider 需确认 / 确认后放行）
//! - 覆盖生成入口的状态门禁与错误提示映射
//! - 全部使用内存存储 + mock LLM + 本地 mock 健康服务，不访问外部网络、不写真实 keychain

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{AppState, BackendConfig, LlmProvider as LlmProviderKind, MessageRole};
use ramaria_service::privacy::{PrivacyStatus, check_privacy, confirm_privacy};
use ramaria_service::{Engine, ErrorHint, SetupRequest, StreamEvent, error_title};

use crate::support::engine_env::{
    build_engine, build_engine_with_embedding, mark_ready, send_stream, stream_request,
    try_send_stream,
};
use crate::support::mock_backend::{MockEmbedding, MockFailingLlm, MockLlm, MockStorage};

// =========================================================
// 辅助函数
// =========================================================

// 辅助: 创建使用 MockLlm 的引擎
fn make_engine() -> (Arc<MockStorage>, Arc<MockLlm>, Arc<Engine>) {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的，我记住了。"));
    let engine = build_engine(
        Arc::clone(&storage),
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        RamariaConfig::default(),
    );
    (storage, llm, engine)
}

// 辅助: 创建使用 MockFailingLlm 的引擎
fn make_failing_engine(error_msg: &str) -> (Arc<MockStorage>, Arc<MockFailingLlm>, Arc<Engine>) {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::failing(error_msg));
    let engine = build_engine(
        Arc::clone(&storage),
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        RamariaConfig::default(),
    );
    (storage, llm, engine)
}

/// 启动本地 mock 健康探测服务：对任意请求返回 200（供首次配置探测通过）。
///
/// 说明:
/// - 返回 mock 服务 base_url；循环接受连接以吸收探测重试；
/// - 服务任务随测试 runtime 结束而终止。
async fn spawn_mock_health_server() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定 mock 端口应成功");
    let addr = listener.local_addr().expect("获取 mock 地址应成功");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            // 读到请求头结束即可（GET 无 body）
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            while buf.len() < 8192 {
                match socket.read(&mut tmp).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                }
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    format!("http://127.0.0.1:{}/v1", addr.port())
}

// =========================================================
// 构造与状态
// =========================================================

#[tokio::test]
async fn app_starts_in_needs_setup() {
    let (_, _, engine) = make_engine();
    assert_eq!(engine.current_state(), AppState::NeedsSetup);
}

#[tokio::test]
async fn app_state_transitions_to_degraded_without_embedding() {
    let (_, _, engine) = make_engine();

    // 就绪装配（后端配置 + 索引版本 + 状态刷新；无嵌入模型 → Degraded，但对话仍可用）
    let state = mark_ready(&engine).await.unwrap();
    assert_eq!(state, AppState::Degraded);
    assert_eq!(engine.current_state(), AppState::Degraded);
}

// =========================================================
// 设置流程
// =========================================================

#[tokio::test]
async fn setup_status_needs_backend() {
    let (_, _, engine) = make_engine();
    let status = engine.check_setup_status().await.unwrap();
    assert!(!status.backend_configured);
    assert!(!status.is_complete());
    assert!(!status.missing_items().is_empty());
}

#[tokio::test]
async fn setup_status_complete_after_config() {
    let storage = Arc::new(MockStorage::new());
    let config = BackendConfig::lm_studio_default();
    storage.save_backend_config(&config).await.unwrap();
    storage.set_index_version(1).await.unwrap();

    let llm = Arc::new(MockLlm::new("好的，我记住了。"));
    let embedding = Arc::new(MockEmbedding::new());
    let engine = build_engine_with_embedding(storage, llm, embedding, RamariaConfig::default());

    let status = engine.check_setup_status().await.unwrap();
    assert!(status.is_complete());
    assert_eq!(engine.refresh_setup_state().await.unwrap(), AppState::Ready);
}

#[tokio::test]
async fn run_setup_full_flow() {
    let storage = Arc::new(MockStorage::new());

    // 模拟设置完成后的索引标记
    storage.set_index_version(1).await.unwrap();

    let llm = Arc::new(MockLlm::new("好的，我记住了。"));
    let engine = build_engine(storage, llm, RamariaConfig::default());

    // 健康探测指向测试内 mock 服务，使首次配置探测确定性通过
    let health_url = spawn_mock_health_server().await;
    let state = engine
        .run_setup(&SetupRequest {
            provider: LlmProviderKind::LmStudio,
            model_id: String::new(),
            base_url: health_url,
            api_key: None,
        })
        .await
        .unwrap();
    // 无嵌入模型 → 首次配置返回 Degraded（向量通道降级，不阻塞对话）
    assert_eq!(state, AppState::Degraded);
}

// =========================================================
// 隐私确认
// =========================================================

#[tokio::test]
async fn privacy_local_provider_auto_approved() {
    let storage = MockStorage::new();
    let status = check_privacy(
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
    let status = check_privacy(
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

    let status = check_privacy(
        &storage,
        LlmProviderKind::DeepSeek,
        "https://api.deepseek.com/v1",
    )
    .await
    .unwrap();
    assert!(status.is_confirmed());
}

// =========================================================
// 流式生成集成测试
// =========================================================

#[tokio::test]
async fn send_message_rejects_when_not_ready() {
    let (_, _, engine) = make_engine();
    let result = try_send_stream(&engine, stream_request("你好", None, None)).await;
    match result {
        Err(err) => assert!(err.to_string().contains("尚未就绪")),
        Ok(_) => panic!("应返回错误"),
    }
}

#[tokio::test]
async fn send_message_flow_success() {
    let (_, _, engine) = make_engine();

    // 就绪装配（后端配置 + 索引版本 + 状态刷新）
    mark_ready(&engine).await.unwrap();
    assert_eq!(engine.current_state(), AppState::Degraded);

    // 发送消息（Degraded 状态下对话仍可用，仅向量通道降级）
    let outcome = send_stream(&engine, "你好", None, None).await;

    // 收集事件并验证
    let mut delta_count = 0usize;
    let mut done_seen = false;
    for event in &outcome.events {
        match event {
            StreamEvent::Delta { .. } => delta_count += 1,
            StreamEvent::Done { .. } => done_seen = true,
            StreamEvent::Error { .. } => {}
            // StreamEvent 为 #[non_exhaustive]，处理未来新增事件类型
            _ => {}
        }
    }

    assert!(done_seen, "流应以 Done 事件结束");
    assert!(delta_count > 0, "流应包含文本增量");
}

#[tokio::test]
async fn send_message_preserves_session() {
    let (storage, _, engine) = make_engine();

    // 就绪装配（Degraded 状态下 session 操作仍可用）
    mark_ready(&engine).await.unwrap();

    // 创建已知会话
    let session = storage.create_session(None).await.unwrap();
    let session_id = session.id;

    // 发送消息（使用已有会话）
    let _ = send_stream(&engine, "你好", None, Some(session_id)).await;

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
    let (storage, _, engine) = make_engine();

    // 就绪装配
    mark_ready(&engine).await.unwrap();

    // 发送消息（Degraded 状态下仍自动创建会话）
    let _ = send_stream(&engine, "测试", None, None).await;

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
        let (_, _, engine) = make_failing_engine(error_msg);

        // 就绪装配
        mark_ready(&engine).await.unwrap();
        assert_eq!(engine.current_state(), AppState::Degraded);

        // 发送消息（LLM 将失败）
        let outcome = send_stream(&engine, "测试消息", None, None).await;

        let mut error_seen = false;
        let mut done_seen = false;
        let mut delta_count = 0usize;

        for event in &outcome.events {
            match event {
                StreamEvent::Delta { .. } => delta_count += 1,
                StreamEvent::Done { .. } => done_seen = true,
                StreamEvent::Error { error, .. } => {
                    error_seen = true;
                    assert!(
                        error.contains("500") || error.contains("连接被拒绝"),
                        "错误事件应包含原始错误信息，实际: {error}"
                    );
                }
                // StreamEvent 为 #[non_exhaustive]，处理未来新增事件类型
                _ => {}
            }
        }

        assert!(error_seen, "LLM 失败时应产生 Error 事件");
        assert!(!done_seen, "LLM 失败时不应产生 Done 事件");
        assert_eq!(delta_count, 0, "LLM 失败时不应有 Delta 事件");
    }
}

// =========================================================
// 注入协调预算（[injection_budget].enabled=true）端到端路径
// =========================================================

/// 协调预算开启时流式生成走协调装配路径，对话仍正常完成。
///
/// 覆盖:
/// - 无 persona → Plain 降级 prompt；RAG 摘要经协调池裁剪（此处无记忆，
///   memory_context=None → 协调空转）。
/// - 协调路径不改变对外流事件语义（Delta + Done）。
#[tokio::test]
async fn send_message_coordinated_budget_enabled_succeeds() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的，我记住了。"));
    let mut config = RamariaConfig::default();
    config.injection_budget.enabled = true;
    config.injection_budget.max_injection_tokens = 100;
    let engine = build_engine(storage, llm, config);

    mark_ready(&engine).await.unwrap();
    assert_eq!(engine.current_state(), AppState::Degraded);

    let outcome = send_stream(&engine, "你好", None, None).await;
    let mut delta_count = 0usize;
    let mut done_seen = false;
    for event in &outcome.events {
        match event {
            StreamEvent::Delta { .. } => delta_count += 1,
            StreamEvent::Done { .. } => done_seen = true,
            StreamEvent::Error { .. } => {}
            _ => {}
        }
    }
    assert!(done_seen, "协调路径流应以 Done 结束");
    assert!(delta_count > 0, "协调路径流应包含文本增量");
}
