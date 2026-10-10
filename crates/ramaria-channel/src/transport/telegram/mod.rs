//! crates/ramaria-channel/src/transport/telegram/mod.rs - Telegram Bot API 通道模块
//!
//! 设计特点:
//! - 长轮询形态：无需公网与监听端口；`offset` 确认制（推进即确认，被忽略的更新同样推进）
//! - 连接管理单点：`connect` 首次 `getMe` 鉴权 + 后台管理任务（轮询 / 空批延迟 / 退避重连）；
//!   网络类失败后后台持续自愈，鉴权失败不重连
//! - 状态流转统一：锁内更新快照、锁外通知宿主（不持锁回调）
//! - 事件流由管理任务唯一持有发送端：关停后 `next_event` 返回 None
//! - 出站为独立 HTTP 请求—应答（无配对缓存）；被动回复判定读 `reply_to_message.from`
//! - 隐私：令牌位于请求路径，错误文案与日志一律不携带令牌与完整地址

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use ramaria_core::types::now_ms;
use tokio::sync::{mpsc, oneshot};

use crate::config::TelegramRuntimeConfig;
use crate::events::ChannelEventSink;
use crate::status::{ChannelStatus, ConnectionState};
use crate::transport::{OutboundMessage, OutboundTarget, Transport, TransportEvent};
use crate::{ChannelError, ChannelResult};

mod api;
mod event;
mod file;
mod manager;

pub use file::TelegramFileDownloader;

// =========================================================
// Bot 身份
// =========================================================

/// Bot 身份（`getMe` 应答的存储形态）。
///
/// 字段约定:
/// - `id`: Bot 用户 ID（数字归一为字符串；被动回复判定与 `self_id` 使用）。
/// - `username`: Bot 用户名（不含 `@`；@ 提及与命令判定使用，可为空串）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BotIdentity {
    pub(crate) id: String,
    pub(crate) username: String,
}

// =========================================================
// 连接参数
// =========================================================

/// 连接管理参数（超时 / 退避均为可注入项，测试使用毫秒级值）。
///
/// 字段约定:
/// - `api_base_url`: Bot API 根地址（默认官方地址；测试指向 mock 服务端）。
/// - `poll_timeout_secs`: 长轮询时长（秒），随 `getUpdates` 请求体上报。
/// - `request_timeout`: 单次 HTTP 请求超时（须大于长轮询时长）。
/// - `backoff_min` / `backoff_max`: 失败退避的指数下界与上界。
/// - `empty_batch_delay`: 空批次后的最小等待（防忙轮询）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramTransportOptions {
    pub api_base_url: String,
    pub poll_timeout_secs: u64,
    pub request_timeout: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub empty_batch_delay: Duration,
}

impl Default for TelegramTransportOptions {
    fn default() -> Self {
        Self {
            api_base_url: "https://api.telegram.org".to_string(),
            poll_timeout_secs: 25,
            request_timeout: Duration::from_secs(40),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            empty_batch_delay: Duration::from_millis(500),
        }
    }
}

// =========================================================
// 传输实现
// =========================================================

/// Telegram Bot API 传输（单通道实例）。
///
/// 职责:
/// - 连接维护（`getMe` 鉴权 / 长轮询 / 退避重连）与状态上报；
/// - 入站事件拉取（`next_event`）与出站发送（`send`，分片经 HTTP 逐片请求）。
///
/// 并发约定:
/// - 内部状态经 `Mutex` 保护，锁不跨 await；事件流接收端经异步锁保护
///   （唯一消费者，单次 `recv` 持锁属预期用法）。
pub struct TelegramTransport {
    config: TelegramRuntimeConfig,
    options: TelegramTransportOptions,
    sink: Arc<dyn ChannelEventSink>,
    client: reqwest::Client,
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<TransportEvent>>,
    inner: Arc<Mutex<Inner>>,
}

/// 运行期共享状态（传输句柄与后台管理任务共同访问）。
pub(crate) struct Inner {
    pub(crate) status: ChannelStatus,
    pub(crate) bot: Option<BotIdentity>,
    pub(crate) events_tx: Option<mpsc::UnboundedSender<TransportEvent>>,
    pub(crate) shutdown_tx: Option<oneshot::Sender<()>>,
    pub(crate) manager: Option<tokio::task::JoinHandle<()>>,
}

impl TelegramTransport {
    /// 构造传输实例（未连接；事件流空）。
    pub fn new(
        config: TelegramRuntimeConfig,
        options: TelegramTransportOptions,
        sink: Arc<dyn ChannelEventSink>,
    ) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Self {
            config,
            options,
            sink,
            client: reqwest::Client::new(),
            events: tokio::sync::Mutex::new(events_rx),
            inner: Arc::new(Mutex::new(Inner {
                status: ChannelStatus::new("telegram"),
                bot: None,
                events_tx: Some(events_tx),
                shutdown_tx: None,
                manager: None,
            })),
        }
    }

    /// 启动后台管理任务（携带首次已取得身份，或从退避重试开始）。
    fn spawn_manager(&self, initial: Option<BotIdentity>) {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let context = manager::ManagerContext {
            config: self.config.clone(),
            options: self.options.clone(),
            sink: Arc::clone(&self.sink),
            inner: Arc::clone(&self.inner),
            client: self.client.clone(),
        };
        let handle = tokio::spawn(manager::manage(context, shutdown_rx, initial));
        let mut guard = lock(&self.inner);
        guard.shutdown_tx = Some(shutdown_tx);
        guard.manager = Some(handle);
    }
}

#[async_trait]
impl Transport for TelegramTransport {
    fn channel(&self) -> &'static str {
        "telegram"
    }

    /// 建立连接（幂等：管理任务在跑时直接成功）。
    ///
    /// 语义:
    /// - 首次 `getMe` 成功 → 保存身份 → 状态流转 `Connecting → Online` → 启动管理任务；
    /// - 鉴权失败（401 / 403）→ 状态 `Error` → 返回错误（不重连）；
    /// - 网络类失败 → 状态 `Error` → 仍启动管理任务进入后台自愈 → 返回错误。
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
        match api::get_me(
            &self.client,
            &self.config.bot_token,
            &self.options.api_base_url,
            self.options.request_timeout,
        )
        .await
        {
            Ok(identity) => {
                {
                    let mut guard = lock(&self.inner);
                    guard.bot = Some(identity.clone());
                }
                update_status(
                    &self.inner,
                    self.sink.as_ref(),
                    ConnectionState::Online,
                    None,
                );
                self.spawn_manager(Some(identity));
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
                    tracing::warn!(channel = "telegram", "Telegram 令牌鉴权失败，停止重连");
                } else {
                    tracing::warn!(
                        channel = "telegram",
                        "Telegram 首次连接失败，进入后台退避重试"
                    );
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
            // 关闭事件流：next_event 返回 None；轮询由关停信号打断
            guard.events_tx = None;
            guard.bot = None;
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

    /// 发送一条出站消息（超限按 UTF-16 单元分片，逐片请求）。
    ///
    /// 语义:
    /// - 附件形态不接线（忽略并记日志），仅发送文本；
    /// - 文本 trim 后为空 → 协议错误，不发请求；
    /// - 分片发送：任一片失败立即返回错误（已发出的分片不回滚）。
    ///
    /// 说明:
    /// - 出站为独立 HTTP 请求，不依赖轮询连接状态。
    async fn send(&self, target: &OutboundTarget, message: &OutboundMessage) -> ChannelResult<()> {
        if message.attachment.is_some() {
            tracing::warn!(channel = "telegram", "本版不支持出站附件，已忽略");
        }
        if message.text.trim().is_empty() {
            return Err(ChannelError::Protocol(
                "出站消息文本为空，取消发送".to_string(),
            ));
        }
        let chat_id = api::parse_chat_id(&target.external_ref)?;
        let chunks = api::split_message(&message.text, api::MESSAGE_LIMIT);
        if chunks.is_empty() {
            return Err(ChannelError::Protocol(
                "出站消息文本为空，取消发送".to_string(),
            ));
        }
        for (index, chunk) in chunks.iter().enumerate() {
            let result = api::send_message(
                &self.client,
                &self.config.bot_token,
                &self.options.api_base_url,
                chat_id,
                chunk,
                self.options.request_timeout,
            )
            .await;
            if let Err(error) = result {
                if index > 0 {
                    tracing::warn!(
                        channel = "telegram",
                        sent = index,
                        total = chunks.len(),
                        error = %error,
                        "Telegram 分片发送中断，已发出的分片不回滚"
                    );
                }
                return Err(error);
            }
        }
        Ok(())
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
