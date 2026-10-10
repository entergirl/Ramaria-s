//! crates/ramaria-channel/src/transport/onebot/mod.rs - OneBot v11 正向 WS 通道模块
//!
//! 设计特点:
//! - 正向 WS 客户端形态：协议端（如 NapCat）监听，本层作为客户端连接并维护长连接
//! - 连接管理单点：`connect` 首次尝试 + 后台管理任务（心跳 / 空闲检测 / 退避重连）；
//!   网络类失败后后台持续自愈（协议端后启动可自动连上），鉴权失败不重连
//! - 状态流转统一：锁内更新快照、锁外通知宿主（不持锁回调）
//! - 事件流由管理任务唯一持有发送端：关停后 `next_event` 返回 None
//! - 出站经 `echo` 配对（见 `api` 模块）；本机已发消息 ID 供被回复判定（见 `event` 模块）

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use ramaria_core::types::now_ms;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::tungstenite::Message;

use crate::config::QqRuntimeConfig;
use crate::events::ChannelEventSink;
use crate::status::{ChannelStatus, ConnectionState};
use crate::transport::{OutboundMessage, OutboundTarget, Transport, TransportEvent};
use crate::{ChannelError, ChannelResult};

mod api;
mod connection;
pub(crate) mod event;

pub use api::SentMessageCache;

/// WebSocket 流类型（本版仅 `ws://` 明文，保留两态统一类型）。
pub(crate) type WsStream =
    tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

// =========================================================
// 连接参数
// =========================================================

/// 连接管理参数（超时 / 心跳 / 退避均为可注入项，测试使用毫秒级值）。
///
/// 字段约定:
/// - `idle_timeout`: 无任何入站帧的判定时长（超过视为断线）。
/// - `ping_interval`: 客户端主动发送 WS Ping 的间隔。
/// - `api_timeout`: 出站调用等待 `echo` 应答的时长（兼作连接建立超时）。
/// - `reconnect_min` / `reconnect_max`: 断线重连的指数退避下界与上界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QqTransportOptions {
    pub idle_timeout: Duration,
    pub ping_interval: Duration,
    pub api_timeout: Duration,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
}

impl Default for QqTransportOptions {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(90),
            ping_interval: Duration::from_secs(30),
            api_timeout: Duration::from_secs(15),
            reconnect_min: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(30),
        }
    }
}

// =========================================================
// 传输实现
// =========================================================

/// OneBot v11 正向 WS 传输（单通道实例）。
///
/// 职责:
/// - 连接维护（鉴权 / 心跳 / 空闲检测 / 退避重连）与状态上报；
/// - 入站事件拉取（`next_event`）与出站发送（`send`，经 `echo` 配对）。
///
/// 并发约定:
/// - 内部状态经 `Mutex` 保护，锁不跨 await；事件流接收端经异步锁保护
///   （唯一消费者，单次 `recv` 持锁属预期用法）。
pub struct OneBotTransport {
    config: QqRuntimeConfig,
    options: QqTransportOptions,
    sink: Arc<dyn ChannelEventSink>,
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<TransportEvent>>,
    inner: Arc<Mutex<Inner>>,
    echo_counter: AtomicU64,
}

/// 运行期共享状态（传输句柄与后台管理任务共同访问）。
pub(crate) struct Inner {
    pub(crate) status: ChannelStatus,
    pub(crate) write_tx: Option<mpsc::UnboundedSender<Message>>,
    pub(crate) pending: Arc<api::PendingCalls>,
    pub(crate) sent_ids: Arc<SentMessageCache>,
    pub(crate) events_tx: Option<mpsc::UnboundedSender<TransportEvent>>,
    pub(crate) shutdown_tx: Option<oneshot::Sender<()>>,
    pub(crate) manager: Option<tokio::task::JoinHandle<()>>,
}

impl OneBotTransport {
    /// 构造传输实例（未连接；事件流空）。
    pub fn new(
        config: QqRuntimeConfig,
        options: QqTransportOptions,
        sink: Arc<dyn ChannelEventSink>,
    ) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Self {
            config,
            options,
            sink,
            events: tokio::sync::Mutex::new(events_rx),
            inner: Arc::new(Mutex::new(Inner {
                status: ChannelStatus::new("qq"),
                write_tx: None,
                pending: Arc::new(api::PendingCalls::default()),
                sent_ids: Arc::new(SentMessageCache::new()),
                events_tx: Some(events_tx),
                shutdown_tx: None,
                manager: None,
            })),
            echo_counter: AtomicU64::new(1),
        }
    }

    /// 启动后台管理任务（携带首次已建连接素材，或从退避重连开始）。
    fn spawn_manager(&self, initial: Option<connection::Connection>) {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let context = connection::ManagerContext {
            config: self.config.clone(),
            options: self.options.clone(),
            sink: Arc::clone(&self.sink),
            inner: Arc::clone(&self.inner),
        };
        let handle = tokio::spawn(connection::manage(context, shutdown_rx, initial));
        let mut guard = lock(&self.inner);
        guard.shutdown_tx = Some(shutdown_tx);
        guard.manager = Some(handle);
    }
}

#[async_trait]
impl Transport for OneBotTransport {
    fn channel(&self) -> &'static str {
        "qq"
    }

    /// 建立连接（幂等：管理任务在跑时直接成功）。
    ///
    /// 语义:
    /// - 首次尝试成功 → 状态流转 `Connecting → Online` → 启动管理任务；
    /// - 鉴权失败（握手 HTTP 401/403）→ 状态 `Error` → 返回错误（不重连）；
    /// - 网络类失败 → 状态 `Error` → 仍启动管理任务进入退避重连 → 返回错误。
    async fn connect(&self) -> ChannelResult<()> {
        {
            let guard = lock(&self.inner);
            if guard
                .manager
                .as_ref()
                .is_some_and(|handle| !handle.is_finished())
            {
                return Ok(());
            }
        }

        // 已关停实例重启：清理残留句柄并重建事件流通道（next_event 恢复可用）
        let rebuild_events = {
            let mut guard = lock(&self.inner);
            guard.manager = None;
            guard.shutdown_tx = None;
            if guard.events_tx.is_none() {
                let (tx, rx) = mpsc::unbounded_channel();
                guard.events_tx = Some(tx);
                Some(rx)
            } else {
                None
            }
        };
        if let Some(rx) = rebuild_events {
            *self.events.lock().await = rx;
        }

        update_status(
            &self.inner,
            self.sink.as_ref(),
            ConnectionState::Connecting,
            None,
        );
        match connection::connect_once(&self.config, self.options.api_timeout).await {
            Ok(stream) => {
                // 同步制备并注册写通道：`connect` 返回后出站立即可用
                let connection = connection::prepare_connection(stream);
                {
                    let mut guard = lock(&self.inner);
                    guard.write_tx = Some(connection.write_tx.clone());
                }
                update_status(
                    &self.inner,
                    self.sink.as_ref(),
                    ConnectionState::Online,
                    None,
                );
                self.spawn_manager(Some(connection));
                Ok(())
            }
            Err(error) => {
                let auth_failure = matches!(error, ChannelError::Auth(_));
                update_status(
                    &self.inner,
                    self.sink.as_ref(),
                    ConnectionState::Error,
                    Some(error.to_string()),
                );
                if auth_failure {
                    tracing::warn!(channel = "qq", "OneBot 鉴权失败，停止重连");
                } else {
                    tracing::warn!(channel = "qq", "OneBot 首次连接失败，进入退避重连");
                    self.spawn_manager(None);
                }
                Err(error)
            }
        }
    }

    /// 关闭连接并释放资源（幂等：未运行时直接成功）。
    async fn shutdown(&self) -> ChannelResult<()> {
        let (shutdown_tx, manager) = {
            let mut guard = lock(&self.inner);
            // 关闭事件流与写通道：next_event 返回 None，在途发送立即失败
            guard.events_tx = None;
            guard.write_tx = None;
            guard.pending.clear();
            (guard.shutdown_tx.take(), guard.manager.take())
        };
        let Some(shutdown_tx) = shutdown_tx else {
            return Ok(());
        };
        let _ = shutdown_tx.send(());
        if let Some(handle) = manager {
            let _ = handle.await;
        }
        update_status(
            &self.inner,
            self.sink.as_ref(),
            ConnectionState::Disconnected,
            None,
        );
        Ok(())
    }

    async fn next_event(&self) -> Option<ChannelResult<TransportEvent>> {
        let mut guard = self.events.lock().await;
        guard.recv().await.map(Ok)
    }

    async fn send(&self, target: &OutboundTarget, message: &OutboundMessage) -> ChannelResult<()> {
        if message.attachment.is_some() {
            tracing::warn!(channel = "qq", "本版不支持出站附件，已忽略");
        }
        if message.text.trim().is_empty() {
            return Err(ChannelError::Protocol(
                "出站消息文本为空，取消发送".to_string(),
            ));
        }

        let (write_tx, pending) = {
            let guard = lock(&self.inner);
            (guard.write_tx.clone(), Arc::clone(&guard.pending))
        };
        let Some(write_tx) = write_tx else {
            return Err(ChannelError::Closed);
        };

        let echo = format!(
            "ramaria-{}",
            self.echo_counter.fetch_add(1, Ordering::Relaxed)
        );
        let request = api::build_text_request(&target.external_ref, &message.text, &echo)?;
        let response_rx = pending.register(echo.clone());
        if write_tx
            .send(Message::Text(request.to_string().into()))
            .is_err()
        {
            pending.remove(&echo);
            return Err(ChannelError::Closed);
        }

        match tokio::time::timeout(self.options.api_timeout, response_rx).await {
            Err(_) => {
                pending.remove(&echo);
                Err(ChannelError::Timeout("OneBot 应答超时".to_string()))
            }
            Ok(Err(_)) => Err(ChannelError::Closed),
            // 应答已由连接读取路径分类并记录平台消息 ID，此处只确认成功 / 传递失败
            Ok(Ok(value)) => api::classify_response(&value).map(|_| ()),
        }
    }

    fn status(&self) -> ChannelStatus {
        lock(&self.inner).status.clone()
    }
}

// =========================================================
// 共享辅助
// =========================================================

/// 获取互斥锁（中毒时取回内部数据，保持通道可用）。
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 状态流转：锁内更新快照，锁外通知宿主（不持锁回调）。
pub(crate) fn update_status(
    inner: &Mutex<Inner>,
    sink: &dyn ChannelEventSink,
    state: ConnectionState,
    last_error: Option<String>,
) {
    let snapshot = {
        let mut guard = lock(inner);
        guard.status.transition(state, last_error, now_ms());
        guard.status.clone()
    };
    sink.on_status_changed(&snapshot);
}

/// 投递一条入站事件（事件流已关闭时丢弃）。
pub(crate) fn emit_event(inner: &Mutex<Inner>, event: TransportEvent) {
    let sender = { lock(inner).events_tx.clone() };
    if let Some(sender) = sender {
        let _ = sender.send(event);
    }
}

#[cfg(test)]
mod tests;
