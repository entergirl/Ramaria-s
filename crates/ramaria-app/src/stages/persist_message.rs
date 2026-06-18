//! rust/crates/ramaria-app/src/stages/persist_message.rs - Stage 10: 消息保存 + 流事件转发
//!
//! 设计特点:
//! - `tokio::spawn` 后台任务转发 LLM 流事件 → `SendMessageStream`
//! - 保存用户消息（会话归属、不含 persona_uid——发言人是用户自己）
//! - 保存助手完整回复（含 persona_uid，前端据此区分"谁在回复"）
//! - Stage 9 失败时（output_stream 已预填充 Error 事件流）→ 直接透传，不执行保存
//! - 流中错误转发为 StreamEvent::Error，收集完整回复后发送 StreamEvent::Done
//! - 日志记录 request_id、session_id、reply_chars、duration_ms，便于性能监控
//! - 不持有跨 .await 的 MutexGuard（所有 I/O 通过 ctx.storage）

use std::sync::Arc;

use async_trait::async_trait;
use futures::channel::mpsc;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{Message, MessageRole, MessageSource, now_ms};
use uuid::Uuid;

use crate::pipeline::{LlmRawStream, PipelineContext, PipelineData, PipelineError, PipelineStage};
use crate::stream_event::StreamEvent;

// =========================================================
// StagePersistMessage
// =========================================================

/// Stage 10: 消息保存 + 流事件转发。
///
/// 职责:
/// - 若 `data.output_stream` 已预填充（Stage 9 错误路径）→ 直接透传
/// - 消费 `data.llm_stream`（LLM 原始流）
/// - 后台 `tokio::spawn` 执行 `stream_forward_task`：
///   - 转发每个 StreamDelta → StreamEvent::Delta
///   - 保存用户消息（MessageRole::User）
///   - 收集完整 assistant 回复 → 保存（MessageRole::Assistant）
///   - 发送 StreamEvent::Done（含 total_chars、backend_id）
///   - 流中错误 → StreamEvent::Error
/// - 设置 `data.output_stream = SendMessageStream`
///
/// 说明:
/// - 本 Stage 立即返回 Ok（后台任务异步执行），不等待 LLM 流完成
/// - send_message 接收 output_stream 后返回给 CLI/Desktop 消费
pub struct StagePersistMessage;

impl StagePersistMessage {
    /// 创建 StagePersistMessage 实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for StagePersistMessage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineStage for StagePersistMessage {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        "PersistMessage"
    }

    async fn execute(
        &self,
        ctx: &PipelineContext,
        mut input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        // ★ 错误路径透传：Stage 9 在 LLM 失败时已预填充 output_stream
        if input.output_stream.is_some() {
            tracing::warn!(
                request_id = %input.request_id,
                "StagePersistMessage: output_stream 已预填充（Stage 9 错误路径），跳过消息保存"
            );
            return Ok(input);
        }

        // 正常路径：消费 llm_stream，启动后台转发任务
        let llm_stream = input.llm_stream.take().ok_or_else(|| {
            PipelineError::fatal(
                "PersistMessage",
                RamariaError::validation(
                    "llm_stream 未设置——Stage 9 (CallLlm) 必须在前，且 output_stream 未被预填充",
                ),
            )
        })?;

        let session = input.session.as_ref().ok_or_else(|| {
            PipelineError::fatal(
                "PersistMessage",
                RamariaError::validation("session 未设置——Stage 3 (ResolveSession) 必须在前"),
            )
        })?;

        let storage = Arc::clone(&ctx.storage);
        let session_id = session.id;
        let user_message = input.user_input.clone();
        let request_id = input.request_id;
        let persona_uid = input.persona_uid.clone();

        // 创建 mpcs 通道，后台任务通过 tx 发送 StreamEvent
        let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();

        tracing::info!(
            request_id = %request_id,
            session_id = %session_id,
            persona_uid = persona_uid.as_deref().unwrap_or("rama"),
            "StagePersistMessage: 启动后台流式转发任务"
        );

        tokio::spawn(async move {
            stream_forward_task(
                storage,
                llm_stream,
                tx,
                session_id,
                user_message,
                request_id,
                persona_uid,
            )
            .await;
        });

        // 立即返回——后台任务异步执行，不阻塞管线
        input.output_stream = Some(Box::pin(rx));
        Ok(input)
    }
}

// =========================================================
// 流式转发后台任务
// =========================================================

/// 后台 tokio 任务：从 LLM 原始流读取 delta，转发为 StreamEvent，收集完整回复并保存。
///
/// 职责:
/// - 消费 `raw_stream`（LLM provider 返回的 `Stream<StreamDelta>`）
/// - 将每个 `StreamDelta` 转换为 `StreamEvent::Delta` 通过 `tx` 发送
/// - 流结束时发送 `StreamEvent::Done`
/// - 流中错误转发为 `StreamEvent::Error`
/// - 收集完整 assistant 回复文本
/// - 保存 user message + assistant message 到 storage
///
/// 参数:
/// - `storage`: 存储后端（Arc 克隆传入，独立于调用者生命周期）
/// - `raw_stream`: LLM provider 返回的原始 delta 流
/// - `tx`: 事件发送通道（UnboundedSender，接收端为 send_message 的返回流）
/// - `session_id`: 消息所属 session
/// - `user_message`: 用户原始输入
/// - `request_id`: 本次请求唯一标识
/// - `persona_uid`: 当前对话人格 UID（None = rama 自身）
async fn stream_forward_task(
    storage: Arc<dyn StorageBackend>,
    raw_stream: LlmRawStream,
    tx: mpsc::UnboundedSender<RamariaResult<StreamEvent>>,
    session_id: Uuid,
    user_message: String,
    request_id: Uuid,
    persona_uid: Option<String>,
) {
    use futures::StreamExt;

    futures::pin_mut!(raw_stream);

    let mut full_reply = String::new();
    let mut backend_id: Option<String> = None;
    let mut has_error = false;
    let start_ms = now_ms();

    // 1. 保存用户消息
    //    v1.2: 用户消息现在也携带 persona_uid，表示"在此 persona 的对话上下文中"
    //    前端据此可按 persona 过滤消息（多角色场景下区分"在对谁说话"）
    let user_msg = Message::new(
        session_id,
        MessageRole::User,
        user_message,
        MessageSource::Local,
    )
    .with_persona_uid(persona_uid.clone());
    if let Err(e) = storage.save_message(&user_msg).await {
        tracing::error!(%e, request_id = %request_id, "保存用户消息失败");
        let _ = tx.unbounded_send(Err(e));
        return;
    }

    // 2. 消费 LLM 原始流
    while let Some(delta_result) = raw_stream.next().await {
        match delta_result {
            Ok(delta) => {
                full_reply.push_str(&delta.content);

                // 转发 Delta 事件给前端
                let event = StreamEvent::delta(request_id, delta.content);
                if tx.unbounded_send(Ok(event)).is_err() {
                    // 接收端已断开（例如用户关闭了对话框），停止转发
                    tracing::debug!(
                        request_id = %request_id,
                        "接收端已断开，停止流式转发"
                    );
                    return;
                }

                if delta.done {
                    backend_id = delta.metadata;
                    break;
                }
            }
            Err(e) => {
                has_error = true;
                tracing::error!(%e, request_id = %request_id, "LLM 流错误");
                let event = StreamEvent::error(request_id, e.to_string());
                let _ = tx.unbounded_send(Ok(event));
                break;
            }
        }
    }

    // 3. 保存 assistant 消息（仅在非错误且有内容时）
    //    助手消息携带 persona_uid，用于前端在左侧气泡显示"谁在回复"
    if !has_error && !full_reply.is_empty() {
        let assistant_msg = Message::new(
            session_id,
            MessageRole::Assistant,
            full_reply.clone(),
            MessageSource::Online,
        )
        .with_persona_uid(persona_uid);
        if let Err(e) = storage.save_message(&assistant_msg).await {
            tracing::error!(%e, request_id = %request_id, "保存 assistant 消息失败");
        }
    }

    // 4. 发送 Done 事件（仅在无错误时——错误已通过 Error 事件发送）
    if !has_error {
        let done_event = StreamEvent::done(request_id, backend_id, full_reply.chars().count());
        let _ = tx.unbounded_send(Ok(done_event));
    }

    let elapsed_ms = now_ms() - start_ms;
    tracing::info!(
        request_id = %request_id,
        session_id = %session_id,
        reply_chars = full_reply.chars().count(),
        has_error,
        duration_ms = elapsed_ms,
        "send_message 流式回复完成"
    );
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::{MockStorage, simple_context};
    use futures::stream;
    use ramaria_core::traits::StreamDelta;
    use ramaria_core::types::Session;
    use std::sync::Arc;
    use uuid::Uuid;

    /// 构造含完整前序数据的 PipelineData。
    fn full_data() -> PipelineData {
        let request_id = Uuid::new_v4();
        let session = Session::new();
        let mut data = PipelineData::new(
            "你好世界".to_string(),
            Some("rama-0001".to_string()),
            Some(session.id),
            request_id,
        );
        data.session = Some(session);
        // 构造一个简单的 Mock LLM 流（内容 "mock reply", done=true）
        data.llm_stream = Some(Box::pin(stream::iter(vec![Ok(StreamDelta {
            content: "mock reply".into(),
            done: true,
            metadata: Some("stop".into()),
        })])));
        data
    }

    // =========================================================
    // 测试: name
    // =========================================================

    #[test]
    fn stage_name() {
        let stage = StagePersistMessage::new();
        assert_eq!(stage.name(), "PersistMessage");
    }

    // =========================================================
    // 测试: 正常路径——output_stream 被设置
    // =========================================================

    #[tokio::test]
    async fn normal_path_sets_output_stream() {
        let ctx = simple_context();
        let stage = StagePersistMessage::new();
        let data = full_data();

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        assert!(
            output.output_stream.is_some(),
            "output_stream should be set"
        );
    }

    // =========================================================
    // 测试: Stage 9 错误路径透传（output_stream 已预填充）
    // =========================================================

    #[tokio::test]
    async fn error_path_pass_through() {
        let ctx = simple_context();
        let stage = StagePersistMessage::new();
        let mut data = full_data();
        // 模拟 Stage 9 失败后预填充的 Error 流
        let request_id = data.request_id;
        let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();
        let error_event = StreamEvent::error(request_id, "mock LLM failure".into());
        let _ = tx.unbounded_send(Ok(error_event));
        data.output_stream = Some(Box::pin(rx));
        // llm_stream 在 Stage 9 错误路径中为 None
        data.llm_stream = None;

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let output = result.expect("should succeed");
        assert!(
            output.output_stream.is_some(),
            "output_stream should be preserved"
        );
    }

    // =========================================================
    // 测试: llm_stream 和 output_stream 均为 None → Fatal 错误
    // =========================================================

    #[tokio::test]
    async fn both_streams_none_returns_fatal() {
        let ctx = simple_context();
        let stage = StagePersistMessage::new();
        let mut data = full_data();
        data.llm_stream = None;
        data.output_stream = None;

        let result = stage.execute(&ctx, data).await;
        match result {
            Ok(_) => panic!("should fail when both streams are None"),
            Err(err) => {
                assert!(!err.is_retryable());
                assert_eq!(err.stage(), "PersistMessage");
            }
        }
    }

    // =========================================================
    // 测试: session 未设置 → Fatal 错误
    // =========================================================

    #[tokio::test]
    async fn missing_session_returns_fatal() {
        let ctx = simple_context();
        let stage = StagePersistMessage::new();
        let mut data = full_data();
        data.session = None;

        let result = stage.execute(&ctx, data).await;
        match result {
            Ok(_) => panic!("should fail with missing session"),
            Err(err) => {
                assert!(!err.is_retryable());
                assert_eq!(err.stage(), "PersistMessage");
            }
        }
    }

    // =========================================================
    // 测试: 正常流内容验证（消费 output_stream）
    // =========================================================

    #[tokio::test]
    async fn output_stream_contains_content() {
        let ctx = simple_context();
        let stage = StagePersistMessage::new();
        let data = full_data();

        let result = stage.execute(&ctx, data).await;
        assert!(result.is_ok());

        let mut output = result.expect("should succeed");
        let mut stream = output
            .output_stream
            .take()
            .expect("output_stream must be set");

        use futures::StreamExt;
        let mut delta_content = String::new();
        let mut done = false;

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(StreamEvent::Delta { content, .. }) => {
                    delta_content.push_str(&content);
                }
                Ok(StreamEvent::Done { total_chars, .. }) => {
                    assert!(total_chars > 0);
                    done = true;
                }
                Ok(StreamEvent::Error { error, .. }) => {
                    panic!("unexpected error: {error}");
                }
                Err(e) => {
                    panic!("unexpected stream error: {e}");
                }
            }
        }

        assert!(done, "stream should end with Done event");
        assert_eq!(delta_content, "mock reply");
    }

    // =========================================================
    // 测试: stream_forward_task 保存用户消息到 MockStorage
    // =========================================================

    #[tokio::test]
    async fn forward_task_saves_user_message() {
        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();
        storage.add_active_session(session_id);
        let request_id = Uuid::new_v4();

        // 构造 LLM 流：单条 "hello" delta + done
        let raw_stream: LlmRawStream = Box::pin(stream::iter(vec![Ok(StreamDelta {
            content: "hello".into(),
            done: true,
            metadata: Some("stop".into()),
        })]));

        let (tx, mut rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();

        // 在 spawn 中运行
        let storage_clone = Arc::clone(&storage) as Arc<dyn StorageBackend>;
        tokio::spawn(async move {
            stream_forward_task(
                storage_clone,
                raw_stream,
                tx,
                session_id,
                "你好".to_string(),
                request_id,
                Some("rama-0001".to_string()),
            )
            .await;
        });

        // 消费输出流
        use futures::StreamExt;
        let mut done = false;
        while let Some(event_result) = rx.next().await {
            if let Ok(StreamEvent::Done { .. }) = event_result {
                done = true;
            }
        }
        assert!(done);

        // 验证用户消息已保存
        let messages = storage
            .list_messages(session_id)
            .await
            .expect("list_messages ok");
        let user_msgs: Vec<_> = messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .collect();
        assert_eq!(user_msgs.len(), 1, "user message should be saved");
        assert_eq!(user_msgs[0].content, "你好");
        assert_eq!(
            user_msgs[0].persona_uid.as_deref(),
            Some("rama-0001"),
            "v1.2: user message should carry session persona_uid"
        );

        // 验证助手消息已保存（含 persona_uid）
        let assistant_msgs: Vec<_> = messages
            .iter()
            .filter(|m| m.role == MessageRole::Assistant)
            .collect();
        assert_eq!(assistant_msgs.len(), 1, "assistant message should be saved");
        assert_eq!(assistant_msgs[0].content, "hello");
        assert_eq!(assistant_msgs[0].persona_uid.as_deref(), Some("rama-0001"));
    }

    // =========================================================
    // 测试: LLM 流中错误 → Error 事件 + 不保存 assistant 消息
    // =========================================================

    #[tokio::test]
    async fn llm_stream_error_produces_error_event() {
        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();
        storage.add_active_session(session_id);
        let request_id = Uuid::new_v4();

        // 构造 LLM 流：先发一条 delta，然后报错
        let raw_stream: LlmRawStream = Box::pin(stream::iter(vec![
            Ok(StreamDelta {
                content: "partial".into(),
                done: false,
                metadata: None,
            }),
            Err(RamariaError::llm("mid-stream failure")),
        ]));

        let (tx, mut rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();

        let storage_clone = Arc::clone(&storage) as Arc<dyn StorageBackend>;
        tokio::spawn(async move {
            stream_forward_task(
                storage_clone,
                raw_stream,
                tx,
                session_id,
                "测试".to_string(),
                request_id,
                None,
            )
            .await;
        });

        // 消费输出流
        use futures::StreamExt;
        let mut has_delta = false;
        let mut has_error = false;
        while let Some(event_result) = rx.next().await {
            match event_result {
                Ok(StreamEvent::Delta { .. }) => has_delta = true,
                Ok(StreamEvent::Error { .. }) => has_error = true,
                Ok(StreamEvent::Done { .. }) => {
                    panic!("should not get Done after error");
                }
                Err(_) => {}
            }
        }

        assert!(has_delta, "should receive partial delta before error");
        assert!(has_error, "should receive error event");

        // 验证 assistant 消息未保存（因为流中有错误）
        let messages = storage
            .list_messages(session_id)
            .await
            .expect("list_messages ok");
        let assistant_msgs: Vec<_> = messages
            .iter()
            .filter(|m| m.role == MessageRole::Assistant)
            .collect();
        assert!(
            assistant_msgs.is_empty(),
            "assistant message should NOT be saved on stream error"
        );
    }
}
