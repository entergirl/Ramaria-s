//! crates/ramaria-channel/src/runtime/worker/tests.rs - 通道事件编排单元测试
//!
//! 设计特点:
//! - 真实 SQLite 临时库 + 桩 LLM / 桩传输 / 桩下载器：覆盖全流程与各降级分支
//! - 断言以库内状态（消息 / 附件行）与桩记录（出站文本 / 调用计数 / 请求）为准
//! - 图片理解：应响应事件先理解后回复（同步断言）；超时降级后轮询等待后台回填
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实账号 / 网络调用 / 用户数据。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider, StoreCrud, StreamDelta};
use ramaria_core::types::{
    AttachmentStatus, BackendConfig, InboundSender, MessageRole, ModelCapability, Persona,
    PersonaKind, now_ms,
};
use ramaria_service::{Engine, EngineOptions};
use ramaria_storage::SqliteStorage;

use super::*;
use crate::ChannelError;
use crate::ChannelResult;
use crate::status::ChannelStatus;
use crate::transport::{InboundSegment, TransportEvent};

// =========================================================
// 桩实现
// =========================================================

/// 桩 LLM：固定文本回复 / 固定图片描述，记录调用次数与生成请求。
struct StubLlm {
    backend: BackendConfig,
    chat_reply: String,
    vision_reply: String,
    chat_calls: AtomicUsize,
    vision_calls: AtomicUsize,
    /// 图片理解延迟（模拟慢理解；0 = 立即返回）。
    vision_delay: StdMutex<Duration>,
    /// 已收到的生成请求（按调用顺序；供先理解后回复断言）。
    requests: StdMutex<Vec<ChatRequest>>,
}

impl StubLlm {
    fn new(chat_reply: &str, vision_reply: &str) -> Arc<Self> {
        Arc::new(Self {
            backend: BackendConfig::lm_studio_default(),
            chat_reply: chat_reply.to_string(),
            vision_reply: vision_reply.to_string(),
            chat_calls: AtomicUsize::new(0),
            vision_calls: AtomicUsize::new(0),
            vision_delay: StdMutex::new(Duration::ZERO),
            requests: StdMutex::new(Vec::new()),
        })
    }

    fn chat_calls(&self) -> usize {
        self.chat_calls.load(Ordering::SeqCst)
    }

    fn vision_calls(&self) -> usize {
        self.vision_calls.load(Ordering::SeqCst)
    }

    /// 设置图片理解延迟（模拟慢理解，覆盖超时降级路径）。
    fn set_vision_delay(&self, delay: Duration) {
        *self.vision_delay.lock().expect("图片延迟锁不应中毒") = delay;
    }

    /// 已收到的生成请求副本（按调用顺序）。
    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("请求记录锁不应中毒").clone()
    }
}

#[async_trait]
impl LlmProvider for StubLlm {
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        self.chat_calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("请求记录锁不应中毒")
            .push(request.clone());
        Ok(self.chat_reply.clone())
    }

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<
        std::pin::Pin<Box<dyn futures::Stream<Item = RamariaResult<StreamDelta>> + Send>>,
    > {
        self.chat_calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("请求记录锁不应中毒")
            .push(request.clone());
        let items = vec![Ok(StreamDelta {
            content: self.chat_reply.clone(),
            done: true,
            metadata: Some("stop".to_string()),
        })];
        Ok(Box::pin(futures::stream::iter(items)))
    }

    async fn chat_vision(
        &self,
        _request: &ChatRequest,
        _image_data_uris: &[String],
    ) -> RamariaResult<String> {
        let delay = *self.vision_delay.lock().expect("图片延迟锁不应中毒");
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        self.vision_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.vision_reply.clone())
    }

    fn capability(&self) -> &ModelCapability {
        &self.backend.capability
    }

    fn config(&self) -> &BackendConfig {
        &self.backend
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "StubLlm"
    }
}

/// 桩传输：记录出站发送与尝试次数，可注入发送失败。
struct StubTransport {
    status: StdMutex<ChannelStatus>,
    sends: StdMutex<Vec<(OutboundTarget, OutboundMessage)>>,
    attempts: AtomicUsize,
    remaining_failures: AtomicUsize,
    failure_retryable: AtomicBool,
}

impl StubTransport {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            status: StdMutex::new(ChannelStatus::new("qq")),
            sends: StdMutex::new(Vec::new()),
            attempts: AtomicUsize::new(0),
            remaining_failures: AtomicUsize::new(0),
            failure_retryable: AtomicBool::new(true),
        })
    }

    /// 注入后续 N 次发送失败。
    fn fail_next(&self, count: usize, retryable: bool) {
        self.remaining_failures.store(count, Ordering::SeqCst);
        self.failure_retryable.store(retryable, Ordering::SeqCst);
    }

    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    fn sent_texts(&self) -> Vec<String> {
        self.sends
            .lock()
            .expect("出站记录锁不应中毒")
            .iter()
            .map(|(_, message)| message.text.clone())
            .collect()
    }
}

#[async_trait]
impl Transport for StubTransport {
    fn channel(&self) -> &'static str {
        "qq"
    }

    async fn connect(&self) -> ChannelResult<()> {
        Ok(())
    }

    async fn shutdown(&self) -> ChannelResult<()> {
        Ok(())
    }

    async fn next_event(&self) -> Option<ChannelResult<TransportEvent>> {
        None
    }

    async fn send(&self, target: &OutboundTarget, message: &OutboundMessage) -> ChannelResult<()> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.remaining_failures.load(Ordering::SeqCst) > 0 {
            self.remaining_failures.fetch_sub(1, Ordering::SeqCst);
            let retryable = self.failure_retryable.load(Ordering::SeqCst);
            return Err(if retryable {
                ChannelError::Protocol("模拟瞬时发送失败".to_string())
            } else {
                ChannelError::Auth("模拟不可重试发送失败".to_string())
            });
        }
        self.sends
            .lock()
            .expect("出站记录锁不应中毒")
            .push((target.clone(), message.clone()));
        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.lock().expect("状态锁不应中毒").clone()
    }
}

/// 桩下载器：固定字节成功或恒失败。
struct StubDownloader {
    payload: StdMutex<Option<Vec<u8>>>,
    calls: AtomicUsize,
}

impl StubDownloader {
    fn succeeding(bytes: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            payload: StdMutex::new(Some(bytes.to_vec())),
            calls: AtomicUsize::new(0),
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            payload: StdMutex::new(None),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl ImageDownloader for StubDownloader {
    async fn download(&self, _url: &str) -> ChannelResult<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.payload.lock().expect("下载载荷锁不应中毒").clone() {
            Some(bytes) => Ok(bytes),
            None => Err(ChannelError::Protocol("模拟图片下载失败".to_string())),
        }
    }
}

// =========================================================
// 测试脚手架
// =========================================================

/// 临时目录（测试专用）。
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应可读")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-channel-worker-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
    dir
}

/// 装配真实 SQLite 引擎（图片理解声明开启；桩 LLM / 无嵌入）。
async fn worker_engine(tag: &str, llm: Arc<StubLlm>) -> (Arc<Engine>, Arc<SqliteStorage>, PathBuf) {
    let dir = temp_dir(tag);
    let db_path = dir.join("assistant.db");
    let _ = Engine::open_with(EngineOptions::new(db_path.clone()))
        .await
        .expect("引擎装配应成功");
    let pool = ramaria_storage::database::init_pool(Some(db_path))
        .await
        .expect("测试库初始化应成功");
    let storage = Arc::new(SqliteStorage::new(pool));
    let mut config = RamariaConfig::default();
    config.vision.model_supports_vision = true;
    let engine = Arc::new(Engine::from_parts(storage.clone(), llm, None, config));
    (engine, storage, dir)
}

/// 造绑定人格行。
async fn seed_binding_persona(storage: &SqliteStorage) {
    let persona = Persona::new(
        "rama-0001".to_string(),
        "Ramaria".to_string(),
        PersonaKind::Rama,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("写入人格应成功");
}

/// 编排参数（毫秒级；测试专用）。
fn worker_options() -> WorkerOptions {
    WorkerOptions {
        min_send_interval: Duration::from_millis(10),
        send_retry_delay: Duration::from_millis(10),
        send_max_attempts: 2,
        image_understand_timeout: Duration::from_secs(20),
    }
}

/// 构造私聊文本事件。
fn private_event(text: &str, message_id: &str) -> TransportEvent {
    TransportEvent {
        external_ref: "c2c:10001".to_string(),
        sender: InboundSender {
            platform_id: "10001".to_string(),
            uin: Some("10001".to_string()),
            display_name: "小明".to_string(),
            group_nickname: None,
            role: None,
        },
        platform_message_id: Some(message_id.to_string()),
        reply_to: None,
        created_at: now_ms(),
        segments: vec![InboundSegment::Text(text.to_string())],
        should_respond: true,
        self_id: Some("42".to_string()),
    }
}

/// 单事件编排（新建节流表；测试对节流无跨事件断言）。
async fn dispatch(
    engine: &Arc<Engine>,
    transport: &Arc<StubTransport>,
    downloader: &Arc<StubDownloader>,
    options: &WorkerOptions,
    event: TransportEvent,
) {
    let throttle = StdMutex::new(HashMap::new());
    handle_event(
        engine,
        &**transport,
        &**downloader,
        options,
        &throttle,
        event,
    )
    .await;
}

/// 显式放行绑定（自动建档默认禁用；白名单语义：启用后才响应）。
async fn allow(engine: &Arc<Engine>, external_ref: &str) {
    engine
        .channel_set_binding("qq", external_ref, None, Some(true), None)
        .await
        .expect("显式放行绑定应成功");
}

// =========================================================
// 用例
// =========================================================

/// 私聊文本全流程：落库（用户行归属锚点 + 助手行）→ 出站发送回复文本。
#[tokio::test]
async fn private_text_full_flow() {
    let llm = StubLlm::new("你好，我是 Ramaria。", "不应调用");
    let (engine, storage, dir) = worker_engine("private-flow", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-1"),
    )
    .await;

    let session = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应已创建");
    assert_eq!(session.persona_uid.as_deref(), Some("rama-0001"));

    let messages = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 2);
    let user_row = messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .expect("应有用户行");
    assert_eq!(user_row.persona_uid.as_deref(), Some("char-10001"));
    assert_eq!(user_row.sender_ref.as_deref(), Some("10001"));
    assert_eq!(user_row.sender_name.as_deref(), Some("小明"));
    assert_eq!(user_row.platform_message_id.as_deref(), Some("m-1"));
    assert!(user_row.fingerprint.is_some());
    let assistant_row = messages
        .iter()
        .find(|message| message.role == MessageRole::Assistant)
        .expect("应有助手行");
    assert_eq!(assistant_row.persona_uid.as_deref(), Some("rama-0001"));

    assert_eq!(
        transport.sent_texts(),
        vec!["你好，我是 Ramaria。".to_string()]
    );
    assert_eq!(llm.chat_calls(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 绑定未启用（自动建档默认禁用）：无写库、无发送、无生成；绑定行保留可见。
#[tokio::test]
async fn disabled_binding_drops_event() {
    let llm = StubLlm::new("不应调用", "不应调用");
    let (engine, storage, dir) = worker_engine("disabled", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-1"),
    )
    .await;

    assert!(
        storage
            .find_active_session_by_channel("qq", Some("c2c:10001"))
            .await
            .expect("查询会话应成功")
            .is_none(),
        "未启用绑定不应建会话"
    );
    assert!(transport.sent_texts().is_empty());
    assert_eq!(llm.chat_calls(), 0);
    let binding = storage
        .list_channel_bindings()
        .await
        .expect("查询绑定应成功")
        .into_iter()
        .find(|binding| binding.external_ref == "c2c:10001")
        .expect("自动建档行应保留（面板可见新会话请求）");
    assert!(!binding.enabled, "自动建档默认禁用");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 白名单：显式放行后同一事件正常回复并落库。
#[tokio::test]
async fn binding_responds_after_explicit_allow() {
    let llm = StubLlm::new("你好，我是 Ramaria。", "不应调用");
    let (engine, storage, dir) = worker_engine("whitelist-allow", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    engine
        .channel_set_binding("qq", "c2c:10001", None, Some(true), None)
        .await
        .expect("显式放行应成功");
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-allow"),
    )
    .await;

    assert_eq!(
        transport.sent_texts(),
        vec!["你好，我是 Ramaria。".to_string()]
    );
    assert_eq!(llm.chat_calls(), 1);
    assert!(
        storage
            .find_active_session_by_channel("qq", Some("c2c:10001"))
            .await
            .expect("查询会话应成功")
            .is_some(),
        "放行后应正常建会话"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 出站兜底：含 `||` 的回复在发送前替换为换行（库内原文不改）。
#[tokio::test]
async fn outbound_pipe_separator_is_sanitized() {
    let llm = StubLlm::new("你好 || 在的||怎么啦", "不应调用");
    let (engine, storage, dir) = worker_engine("outbound-pipe", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-pipe"),
    )
    .await;

    assert_eq!(
        transport.sent_texts(),
        vec!["你好\n在的\n怎么啦".to_string()],
        "出站文本不应残留分隔符"
    );

    // 库内回复原文保留多气泡契约（仅出站净化）
    let session = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    let assistant = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功")
        .into_iter()
        .find(|message| message.role == MessageRole::Assistant)
        .expect("应有助手行");
    assert_eq!(assistant.content, "你好 || 在的||怎么啦");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 重复事件：第二次不落库、不回复、不触发理解。
#[tokio::test]
async fn duplicate_event_skips_write_and_reply() {
    let llm = StubLlm::new("收到", "不应调用");
    let (engine, storage, dir) = worker_engine("duplicate", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();
    let options = worker_options();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &options,
        private_event("你好", "m-dup"),
    )
    .await;
    let vision_calls_after_first = llm.vision_calls();
    dispatch(
        &engine,
        &transport,
        &downloader,
        &options,
        private_event("你好", "m-dup"),
    )
    .await;

    let session = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    let messages = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 2, "重复事件不应二次入库");
    assert_eq!(transport.sent_texts().len(), 1, "重复事件不应再次回复");
    assert_eq!(llm.chat_calls(), 1);
    assert_eq!(
        llm.vision_calls(),
        vision_calls_after_first,
        "重复事件不应触发图片理解"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 图片成功：先理解后回复（返回时附件已 done，回复历史含描述）。
#[tokio::test]
async fn image_success_runs_understanding() {
    let llm = StubLlm::new("看到图了", "一只橘猫趴在窗台上");
    let (engine, storage, dir) = worker_engine("image-ok", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let image_bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];
    let downloader = StubDownloader::succeeding(&image_bytes);

    let mut event = private_event("", "m-img");
    event.segments = vec![InboundSegment::Image {
        url: "https://example.com/a.png".to_string(),
        size: Some(11),
        sub_type: Some("0".to_string()),
    }];
    dispatch(&engine, &transport, &downloader, &worker_options(), event).await;

    let session = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    let messages = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    let user_row = messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .expect("应有用户行");
    let expected_hash = crate::runtime::assemble::md5_hex(&image_bytes)[..8].to_string();
    assert!(
        user_row
            .content
            .contains(&format!("[图片#{expected_hash}]")),
        "正文应含占位符: {}",
        user_row.content
    );

    // 先理解后回复：dispatch 返回时（回复已生成发送）附件已回填完成，无需轮询
    let rows = storage
        .list_attachments_by_messages(&[user_row.id])
        .await
        .expect("查询附件应成功");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, AttachmentStatus::Done);
    assert_eq!(rows[0].description.as_deref(), Some("一只橘猫趴在窗台上"));
    assert_eq!(transport.sent_texts(), vec!["看到图了".to_string()]);

    // 回复生成时模型已能看到图片描述（历史窗口注入）
    let requests = llm.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .history
            .iter()
            .any(|message| message.content.contains("[图片: 一只橘猫趴在窗台上]")),
        "chat 请求历史应含图片描述: {:?}",
        requests[0].history
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 图片理解超时：不阻塞回复（按占位符继续），后台任务继续回填。
#[tokio::test]
async fn image_understand_timeout_degrades_and_backfills() {
    let llm = StubLlm::new("先回复", "一只橘猫趴在窗台上");
    llm.set_vision_delay(Duration::from_millis(300));
    let (engine, storage, dir) = worker_engine("image-timeout", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let image_bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];
    let downloader = StubDownloader::succeeding(&image_bytes);

    let mut options = worker_options();
    options.image_understand_timeout = Duration::from_millis(50);

    let mut event = private_event("", "m-img-timeout");
    event.segments = vec![InboundSegment::Image {
        url: "https://example.com/slow.png".to_string(),
        size: Some(11),
        sub_type: Some("0".to_string()),
    }];
    dispatch(&engine, &transport, &downloader, &options, event).await;

    // 超时降级：回复照常发出
    assert_eq!(transport.sent_texts(), vec!["先回复".to_string()]);

    // 后台任务未被取消：轮询等待附件最终回填完成（带超时）
    let session = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    let user_row_id = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功")
        .into_iter()
        .find(|message| message.role == MessageRole::User)
        .expect("应有用户行")
        .id;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let rows = loop {
        let rows = storage
            .list_attachments_by_messages(&[user_row_id])
            .await
            .expect("查询附件应成功");
        if rows
            .first()
            .is_some_and(|row| row.status == AttachmentStatus::Done)
        {
            break rows;
        }
        assert!(tokio::time::Instant::now() < deadline, "等待后台回填超时");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].description.as_deref(), Some("一只橘猫趴在窗台上"));
    // 能力探测一次（首图触发）+ 正式理解一次；超时降级不取消后台任务
    assert_eq!(llm.vision_calls(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 图片下载失败：正文降级 [图片]、无附件行、回复照常。
#[tokio::test]
async fn image_download_failure_degrades() {
    let llm = StubLlm::new("没收到图", "不应调用");
    let (engine, storage, dir) = worker_engine("image-fail", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    let mut event = private_event("", "m-img-fail");
    event.segments = vec![InboundSegment::Image {
        url: "https://example.com/b.png".to_string(),
        size: None,
        sub_type: None,
    }];
    dispatch(&engine, &transport, &downloader, &worker_options(), event).await;

    let session = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    let messages = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    let user_row = messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .expect("应有用户行");
    assert_eq!(user_row.content, "[图片]");
    let rows = storage
        .list_attachments_by_messages(&[user_row.id])
        .await
        .expect("查询附件应成功");
    assert!(rows.is_empty(), "下载失败不应产生附件行");
    assert_eq!(transport.sent_texts(), vec!["没收到图".to_string()]);
    assert_eq!(llm.vision_calls(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 群聊未触发响应：正常落库但不回复；图片理解留后台继续回填。
#[tokio::test]
async fn group_without_trigger_persists_only() {
    let llm = StubLlm::new("不应调用", "一只橘猫趴在窗台上");
    let (engine, storage, dir) = worker_engine("group-idle", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "group:20002").await;
    let transport = StubTransport::new();
    let image_bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];
    let downloader = StubDownloader::succeeding(&image_bytes);

    let mut event = private_event("", "g-1");
    event.external_ref = "group:20002".to_string();
    event.should_respond = false;
    event.segments = vec![InboundSegment::Image {
        url: "https://example.com/g.png".to_string(),
        size: Some(11),
        sub_type: Some("0".to_string()),
    }];
    dispatch(&engine, &transport, &downloader, &worker_options(), event).await;

    let session = storage
        .find_active_session_by_channel("qq", Some("group:20002"))
        .await
        .expect("查询会话应成功")
        .expect("会话应已创建");
    let messages = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 1, "未触发响应只落库不回复");
    assert!(transport.sent_texts().is_empty());
    assert_eq!(llm.chat_calls(), 0);

    // 不响应不影响图片理解：后台任务继续，轮询等待回填完成（带超时）
    let user_row_id = messages[0].id;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let rows = loop {
        let rows = storage
            .list_attachments_by_messages(&[user_row_id])
            .await
            .expect("查询附件应成功");
        if rows
            .first()
            .is_some_and(|row| row.status == AttachmentStatus::Done)
        {
            break rows;
        }
        assert!(tokio::time::Instant::now() < deadline, "等待后台回填超时");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].description.as_deref(), Some("一只橘猫趴在窗台上"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 发送可重试失败一次后成功：尝试两次、最终发出。
#[tokio::test]
async fn send_retry_recovers_after_one_failure() {
    let llm = StubLlm::new("重试后成功", "不应调用");
    let (engine, storage, dir) = worker_engine("send-retry", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    transport.fail_next(1, true);
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-retry"),
    )
    .await;

    assert_eq!(transport.attempts(), 2, "应重试一次");
    assert_eq!(transport.sent_texts(), vec!["重试后成功".to_string()]);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 发送不可重试失败：仅尝试一次、不发出（回复已落库）。
#[tokio::test]
async fn send_non_retryable_failure_not_retried() {
    let llm = StubLlm::new("不会发出", "不应调用");
    let (engine, storage, dir) = worker_engine("send-no-retry", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    transport.fail_next(1, false);
    let downloader = StubDownloader::failing();

    dispatch(
        &engine,
        &transport,
        &downloader,
        &worker_options(),
        private_event("你好", "m-no-retry"),
    )
    .await;

    assert_eq!(transport.attempts(), 1, "不可重试错误不应重试");
    assert!(transport.sent_texts().is_empty());

    // 回复已落库（生成不因发送失败回滚）
    let session = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    let messages = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空段序列事件：组装后正文为空即丢弃（不落库不发送）。
#[tokio::test]
async fn empty_text_event_dropped() {
    let llm = StubLlm::new("不应调用", "不应调用");
    let (engine, storage, dir) = worker_engine("empty-drop", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    let mut event = private_event("", "m-empty");
    event.segments = vec![InboundSegment::Text("   ".to_string())];
    dispatch(&engine, &transport, &downloader, &worker_options(), event).await;

    assert!(
        storage
            .find_active_session_by_channel("qq", Some("c2c:10001"))
            .await
            .expect("查询会话应成功")
            .is_none()
    );
    assert!(transport.sent_texts().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 纯表情消息（`[表情]` 占位段）：正常入库并回复（不被空正文丢弃）。
#[tokio::test]
async fn face_only_message_stored_and_replied() {
    let llm = StubLlm::new("收到你的表情了", "不应调用");
    let (engine, storage, dir) = worker_engine("face-only", Arc::clone(&llm)).await;
    seed_binding_persona(&storage).await;
    allow(&engine, "c2c:10001").await;
    let transport = StubTransport::new();
    let downloader = StubDownloader::failing();

    let mut event = private_event("", "m-face");
    event.segments = vec![InboundSegment::Text("[表情]".to_string())];
    dispatch(&engine, &transport, &downloader, &worker_options(), event).await;

    let session = storage
        .find_active_session_by_channel("qq", Some("c2c:10001"))
        .await
        .expect("查询会话应成功")
        .expect("表情消息应入库建会话");
    let user_row = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功")
        .into_iter()
        .find(|message| message.role == MessageRole::User)
        .expect("应有用户行");
    assert_eq!(user_row.content, "[表情]");
    assert_eq!(transport.sent_texts(), vec!["收到你的表情了".to_string()]);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 事件队列空转：桩传输 next_event 返回 None 即事件流终止（循环退出）。
#[tokio::test]
async fn worker_shutdown_stops_loop() {
    let llm = StubLlm::new("不应调用", "不应调用");
    let (engine, storage, dir) = worker_engine("worker-shutdown", llm).await;
    seed_binding_persona(&storage).await;
    let transport: Arc<dyn Transport> = StubTransport::new();
    let downloader = StubDownloader::failing();
    let runtime = Arc::new(ChannelRuntime::new(
        crate::config::ChannelRuntimeConfig {
            default_persona_uid: "rama-0001".to_string(),
            qq: None,
            telegram: None,
            proactive_enabled: false,
        },
        Arc::new(crate::events::NoopSink),
    ));

    let worker = ChannelWorker::spawn(
        Arc::clone(&engine),
        transport,
        Arc::clone(&runtime),
        downloader,
        worker_options(),
    );
    assert!(Arc::ptr_eq(worker.runtime(), &runtime));
    worker.shutdown().await;

    let _ = std::fs::remove_dir_all(&dir);
}
