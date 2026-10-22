//! crates/ramaria-channel/tests/common/mod.rs - 通道端到端测试基建
//!
//! 设计特点:
//! - mock 协议端（最小 WS 服务端）：可配置令牌校验、自动应答出站请求、
//!   主动推送事件、主动断开连接（驱动客户端重连）
//! - 真实 SQLite 临时库 + 桩 LLM / 桩下载器：全链路不依赖外部服务
//! - 轮询等待（带超时）覆盖异步链路断言
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实账号 / 网络调用 / 用户数据。

#![allow(dead_code)]
// 握手回调签名由协议库固定（Err 为较大的 HTTP 响应结构），测试基建豁免该 lint
#![allow(clippy::result_large_err)]

pub mod telegram;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use ramaria_channel::{
    ChannelError, ChannelEventSink, ChannelResult, ChannelRuntime, ChannelRuntimeConfig,
    ChannelStatus, ConnectionState, ImageDownloader, NoopSink, OneBotTransport, QqRuntimeConfig,
    QqTransportOptions, RecallDispatcher,
};
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider, StoreCrud, StreamDelta};
use ramaria_core::types::{BackendConfig, ModelCapability, Persona, PersonaKind};
use ramaria_service::{Engine, EngineOptions};
use ramaria_storage::SqliteStorage;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

// =========================================================
// mock 协议端
// =========================================================

/// 服务端指令（测试向活跃连接下发）。
enum ServerCommand {
    Push(String),
    Close,
}

/// mock 协议端状态（探针集合）。
pub struct ServerState {
    pub connections: AtomicUsize,
    received: StdMutex<Vec<Value>>,
    commands: StdMutex<Vec<mpsc::UnboundedSender<ServerCommand>>>,
    next_message_id: AtomicI64,
}

/// mock NapCat 服务端（最小 WS 实现）。
///
/// 行为:
/// - 可选令牌校验（不匹配回 HTTP 401）；
/// - 出站请求自动回应答（`retcode=0` + 递增 `message_id`），并记录全部文本帧；
/// - [`MockNapCatServer::push_event`] 向活跃连接推送事件；
/// - [`MockNapCatServer::disconnect_all`] 主动关闭全部连接（驱动重连）。
pub struct MockNapCatServer {
    url: String,
    state: Arc<ServerState>,
}

impl MockNapCatServer {
    /// 启动服务端（`token = Some(..)` 时校验 Bearer 令牌）。
    pub async fn start(token: Option<&str>) -> Self {
        use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
        use tokio_tungstenite::tungstenite::http::{Response as HttpResponse, StatusCode};

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定应成功");
        let addr = listener.local_addr().expect("地址应可读");
        let state = Arc::new(ServerState {
            connections: AtomicUsize::new(0),
            received: StdMutex::new(Vec::new()),
            commands: StdMutex::new(Vec::new()),
            next_message_id: AtomicI64::new(1),
        });
        let expected = token.map(|token| format!("Bearer {token}"));
        let state_task = Arc::clone(&state);

        let _accept = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                state_task.connections.fetch_add(1, Ordering::SeqCst);
                let state = Arc::clone(&state_task);
                let expected = expected.clone();
                tokio::spawn(async move {
                    let callback = move |request: &Request,
                                         response: Response|
                          -> Result<Response, ErrorResponse> {
                        let authorized = match &expected {
                            Some(want) => request
                                .headers()
                                .get("authorization")
                                .and_then(|value| value.to_str().ok())
                                .is_some_and(|value| value == want),
                            None => true,
                        };
                        if authorized {
                            Ok(response)
                        } else {
                            let mut refusal = HttpResponse::new(Some("unauthorized".to_string()));
                            *refusal.status_mut() = StatusCode::UNAUTHORIZED;
                            Err(refusal)
                        }
                    };
                    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await
                    else {
                        return;
                    };
                    let (tx, mut rx) = mpsc::unbounded_channel::<ServerCommand>();
                    state.commands.lock().expect("指令表锁不应中毒").push(tx);
                    loop {
                        tokio::select! {
                            command = rx.recv() => match command {
                                Some(ServerCommand::Push(text)) => {
                                    let _ = ws.send(Message::Text(text.into())).await;
                                }
                                Some(ServerCommand::Close) => {
                                    let _ = ws.close(None).await;
                                    break;
                                }
                                None => break,
                            },
                            incoming = ws.next() => match incoming {
                                Some(Ok(Message::Text(text))) => {
                                    let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                                        continue;
                                    };
                                    if let Some(echo) = value.get("echo") {
                                        let message_id = state.next_message_id.fetch_add(1, Ordering::SeqCst);
                                        let reply = json!({
                                            "status": "ok",
                                            "retcode": 0,
                                            "data": { "message_id": message_id },
                                            "echo": echo.clone(),
                                        });
                                        let _ = ws.send(Message::Text(reply.to_string().into())).await;
                                    }
                                    state
                                        .received
                                        .lock()
                                        .expect("接收记录锁不应中毒")
                                        .push(value);
                                }
                                Some(Ok(Message::Ping(payload))) => {
                                    let _ = ws.send(Message::Pong(payload)).await;
                                }
                                Some(Ok(_)) => {}
                                Some(Err(_)) | None => break,
                            }
                        }
                    }
                });
            }
        });

        Self {
            url: format!("ws://{addr}"),
            state,
        }
    }

    /// 客户端连接地址。
    pub fn url(&self) -> &str {
        &self.url
    }

    /// 当前累计连接数。
    pub fn connections(&self) -> usize {
        self.state.connections.load(Ordering::SeqCst)
    }

    /// 已收到的出站请求（按到达顺序）。
    pub fn received(&self) -> Vec<Value> {
        self.state
            .received
            .lock()
            .expect("接收记录锁不应中毒")
            .clone()
    }

    /// 已收到的指定动作请求。
    pub fn received_actions(&self, action: &str) -> Vec<Value> {
        self.received()
            .into_iter()
            .filter(|value| value.get("action").and_then(Value::as_str) == Some(action))
            .collect()
    }

    /// 向全部活跃连接推送一条事件文本。
    pub fn push_event(&self, event: &str) {
        for command in self.state.commands.lock().expect("指令表锁不应中毒").iter() {
            let _ = command.send(ServerCommand::Push(event.to_string()));
        }
    }

    /// 主动关闭全部活跃连接（驱动客户端重连）。
    pub fn disconnect_all(&self) {
        for command in self.state.commands.lock().expect("指令表锁不应中毒").iter() {
            let _ = command.send(ServerCommand::Close);
        }
    }
}

// =========================================================
// 桩 LLM
// =========================================================

/// 桩 LLM：固定文本回复 / 固定图片描述。
pub struct MiniLlm {
    backend: BackendConfig,
    chat_reply: String,
    vision_reply: String,
}

impl MiniLlm {
    pub fn new(chat_reply: &str, vision_reply: &str) -> Arc<Self> {
        Arc::new(Self {
            backend: BackendConfig::lm_studio_default(),
            chat_reply: chat_reply.to_string(),
            vision_reply: vision_reply.to_string(),
        })
    }
}

#[async_trait]
impl LlmProvider for MiniLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        Ok(self.chat_reply.clone())
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<
        std::pin::Pin<Box<dyn futures::Stream<Item = RamariaResult<StreamDelta>> + Send>>,
    > {
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
        "MiniLlm"
    }
}

// =========================================================
// 桩下载器
// =========================================================

/// 桩下载器：固定字节成功或恒失败。
pub struct StubImageDownloader {
    payload: StdMutex<Option<Vec<u8>>>,
}

impl StubImageDownloader {
    pub fn succeeding(bytes: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            payload: StdMutex::new(Some(bytes.to_vec())),
        })
    }

    pub fn failing() -> Arc<Self> {
        Arc::new(Self {
            payload: StdMutex::new(None),
        })
    }
}

#[async_trait]
impl ImageDownloader for StubImageDownloader {
    async fn download(&self, _url: &str) -> ChannelResult<Vec<u8>> {
        match self.payload.lock().expect("下载载荷锁不应中毒").clone() {
            Some(bytes) => Ok(bytes),
            None => Err(ChannelError::Protocol("模拟图片下载失败".to_string())),
        }
    }
}

// =========================================================
// 状态记录端
// =========================================================

/// 状态记录端（收集全部流转快照，供序列断言）。
#[derive(Default)]
pub struct RecordingSink {
    statuses: StdMutex<Vec<ChannelStatus>>,
}

impl RecordingSink {
    /// 全部状态项副本。
    pub fn statuses(&self) -> Vec<ChannelStatus> {
        self.statuses.lock().expect("状态记录锁不应中毒").clone()
    }

    /// 状态序列（仅状态枚举）。
    pub fn states(&self) -> Vec<ConnectionState> {
        self.statuses().iter().map(|status| status.state).collect()
    }
}

impl ChannelEventSink for RecordingSink {
    fn on_status_changed(&self, status: &ChannelStatus) {
        self.statuses
            .lock()
            .expect("状态记录锁不应中毒")
            .push(status.clone());
    }
}

// =========================================================
// 引擎与运行时装配
// =========================================================

/// 临时目录（测试专用）。
pub fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应可读")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-channel-e2e-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
    dir
}

/// 装配真实 SQLite 引擎（图片理解声明开启）。
pub async fn e2e_engine(
    tag: &str,
    llm: Arc<MiniLlm>,
) -> (Arc<Engine>, Arc<SqliteStorage>, PathBuf) {
    let dir = temp_dir(tag);
    let db_path = dir.join("ramaria.db");
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
pub async fn seed_binding_persona(storage: &SqliteStorage) {
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

/// e2e 连接参数（毫秒级恢复节奏）。
pub fn e2e_transport_options() -> QqTransportOptions {
    QqTransportOptions {
        idle_timeout: Duration::from_secs(30),
        ping_interval: Duration::from_secs(5),
        api_timeout: Duration::from_millis(500),
        reconnect_min: Duration::from_millis(50),
        reconnect_max: Duration::from_millis(400),
    }
}

/// 构造传输实例（状态上报终点由调用方指定）。
pub fn e2e_transport(
    server: &MockNapCatServer,
    sink: Arc<dyn ChannelEventSink>,
    token: Option<&str>,
) -> Arc<OneBotTransport> {
    Arc::new(OneBotTransport::new(
        QqRuntimeConfig {
            ws_url: server.url().to_string(),
            access_token: token.map(str::to_string),
        },
        e2e_transport_options(),
        sink,
    ))
}

/// 构造传输实例并注册撤回通知分发（服务层引擎接收撤回处理）。
pub fn e2e_transport_with_recall(
    server: &MockNapCatServer,
    sink: Arc<dyn ChannelEventSink>,
    token: Option<&str>,
    engine: Arc<Engine>,
) -> Arc<OneBotTransport> {
    Arc::new(
        OneBotTransport::new(
            QqRuntimeConfig {
                ws_url: server.url().to_string(),
                access_token: token.map(str::to_string),
            },
            e2e_transport_options(),
            sink,
        )
        .with_recall_sink(Arc::new(RecallDispatcher::new(engine))),
    )
}

/// 构造通道运行时（无宿主事件端）。
pub fn e2e_runtime() -> Arc<ChannelRuntime> {
    Arc::new(ChannelRuntime::new(
        ChannelRuntimeConfig {
            default_persona_uid: "rama-0001".to_string(),
            qq: None,
            telegram: None,
            proactive_delivery_enabled: false,
        },
        Arc::new(NoopSink),
    ))
}

// =========================================================
// 轮询等待
// =========================================================

/// 轮询等待异步条件成立（带超时）。
pub async fn wait_for<F, Fut>(label: &str, timeout: Duration, mut probe: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if probe().await {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "等待超时: {label}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
