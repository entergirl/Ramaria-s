//! crates/ramaria-service/tests/suites/support/engine_env.rs - 集成测试的引擎装配与事件流消费辅助
//!
//! 设计特点:
//! - 装配：以注入依赖（内存存储 / mock LLM / 可选预计算嵌入）构造 `Arc<Engine>`
//! - 就绪：`mark_ready` 复现入口就绪序列（后端配置 + 索引版本 + 状态刷新），
//!   供交互式生成路径通过状态门禁
//! - 生成：`send_stream` 消费事件流并汇总会话标识、增量全文、完成与错误标记
//! - 封存：`seal_full` 注册完整封存钩子链后封存目标会话（与长驻宿主同一条链路）
//! - 确定性：LLM / 嵌入全部为 mock，断言不依赖网络、真实模型与系统时钟绝对值

use std::sync::Arc;

use futures::StreamExt;
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend};
use ramaria_core::types::{AppState, BackendConfig};
use ramaria_service::{ChatStreamHandle, ChatStreamRequest, Engine, SealOutcome, StreamEvent};
use uuid::Uuid;

use super::mock_backend::MockStorage;

// =========================================================
// 引擎装配
// =========================================================

/// 以注入依赖构造引擎（无嵌入：向量通道降级，BM25 + 关键词镜像仍可用）。
pub fn build_engine(
    storage: Arc<MockStorage>,
    llm: Arc<dyn LlmProvider>,
    config: RamariaConfig,
) -> Arc<Engine> {
    Arc::new(Engine::from_parts(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        None,
        config,
    ))
}

/// 以注入依赖构造引擎（带嵌入：向量通道参与检索）。
pub fn build_engine_with_embedding(
    storage: Arc<MockStorage>,
    llm: Arc<dyn LlmProvider>,
    embedding: Arc<dyn EmbeddingProvider>,
    config: RamariaConfig,
) -> Arc<Engine> {
    Arc::new(Engine::from_parts(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        Some(embedding),
        config,
    ))
}

/// 推进引擎到"对话可用"状态（`Ready` / `Degraded`）。
///
/// 序列（与入口就绪口径一致）:
/// 1. 写入本地后端配置（`lm_studio` 缺省）；
/// 2. 写入索引版本（已构建）——避免状态停在 `Indexing`；
/// 3. 刷新设置状态并返回判定结果。
pub async fn mark_ready(engine: &Engine) -> RamariaResult<AppState> {
    engine
        .storage()
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await?;
    engine.storage().set_index_version(1).await?;
    engine.refresh_setup_state().await
}

// =========================================================
// 事件流消费
// =========================================================

/// 一次流式生成的事件汇总。
///
/// 字段约定:
/// - `session_id`: 事件流句柄给出的会话标识（本轮回复所属会话）；
/// - `text`: 全部 `Delta` 事件的增量拼接（错误路径可能为空或部分）；
/// - `done`: 是否收到 `Done` 事件（错误路径为 false）；
/// - `error`: 流内 `Error` 事件或流项 `Err` 的文本（无错误为 None）；
/// - `events`: 事件序列（按到达顺序，供序列断言）。
#[derive(Debug)]
pub struct StreamOutcome {
    pub session_id: Uuid,
    pub text: String,
    pub done: bool,
    pub error: Option<String>,
    pub events: Vec<StreamEvent>,
}

/// 消费事件流直到结束，汇总增量文本、完成标记与错误。
pub async fn drain(handle: ChatStreamHandle) -> StreamOutcome {
    let session_id = handle.session_id;
    let mut events = Vec::new();
    let mut text = String::new();
    let mut done = false;
    let mut error = None;

    let mut stream = handle.events;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                match &event {
                    StreamEvent::Delta { content, .. } => text.push_str(content),
                    StreamEvent::Done { .. } => done = true,
                    StreamEvent::Error { error: message, .. } => error = Some(message.clone()),
                    // 事件枚举为非穷尽：新增变体按"仅记录、不影响汇总口径"处理
                    _ => {}
                }
                events.push(event);
            }
            Err(e) => error = Some(e.to_string()),
        }
    }

    StreamOutcome {
        session_id,
        text,
        done,
        error,
        events,
    }
}

/// 发送一条消息并消费事件流（前置编排失败时返回错误，不启动流）。
pub async fn try_send_stream(
    engine: &Arc<Engine>,
    request: ChatStreamRequest,
) -> RamariaResult<StreamOutcome> {
    let handle = engine.chat_stream(request).await?;
    Ok(drain(handle).await)
}

/// 发送一条消息并消费事件流（期望前置编排成功；失败时 panic 并给出原因）。
pub async fn send_stream(
    engine: &Arc<Engine>,
    message: &str,
    persona: Option<&str>,
    session_id: Option<Uuid>,
) -> StreamOutcome {
    let request = stream_request(message, persona, session_id);
    try_send_stream(engine, request)
        .await
        .unwrap_or_else(|e| panic!("流式生成前置编排应成功，实际失败: {e}"))
}

/// 构造流式生成请求（无预置上文、无配置覆盖）。
pub fn stream_request(
    message: &str,
    persona: Option<&str>,
    session_id: Option<Uuid>,
) -> ChatStreamRequest {
    ChatStreamRequest {
        message: message.to_string(),
        persona: persona.map(str::to_string),
        session_id,
        seed_history: Vec::new(),
        config_override: None,
    }
}

// =========================================================
// 封存
// =========================================================

/// 注册完整封存钩子链并封存目标会话。
pub async fn seal_full(engine: &Arc<Engine>, session_id: Uuid) -> RamariaResult<SealOutcome> {
    engine.set_seal_hooks(ramaria_service::full_seal_hooks(engine.as_ref()));
    engine.seal(session_id).await
}

/// 发送一条消息并封存其会话（返回会话标识与增量汇总）。
pub async fn send_then_seal(
    engine: &Arc<Engine>,
    message: &str,
    persona: Option<&str>,
) -> (Uuid, StreamOutcome) {
    let outcome = send_stream(engine, message, persona, None).await;
    seal_full(engine, outcome.session_id)
        .await
        .unwrap_or_else(|e| panic!("封存应成功，实际失败: {e}"));
    (outcome.session_id, outcome)
}

/// 记录一次对话的会话标识：优先取事件流句柄，缺失时回退 `Done` 事件。
pub fn session_of(outcome: &StreamOutcome) -> Option<Uuid> {
    outcome.events.iter().rev().find_map(|event| match event {
        StreamEvent::Done { session_id, .. } => *session_id,
        _ => None,
    })
}

/// 事件类型标签序列（断言事件流形状用）。
pub fn kinds(outcome: &StreamOutcome) -> Vec<&'static str> {
    outcome.events.iter().map(StreamEvent::kind).collect()
}
