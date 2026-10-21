//! crates/ramaria-channel/src/transport/onebot/connection.rs - OneBot 连接维护模块
//!
//! 设计特点:
//! - 管理任务单点：连接建立 → 读循环（手动回 Pong / 心跳 Ping / 空闲超时）→
//!   断连收尾 → 指数退避重连；关停信号随时可打断退避与读循环
//! - 空闲按最后一个入站帧计时（心跳发出不续期），心跳间隔短于空闲阈值时判定仍生效
//! - 读循环三类入站帧：`echo` 应答配对到待应答表；事件帧解析为规范事件；
//!   Ping 手动回 Pong（不依赖底层自动机制）；Pong / Binary 忽略
//! - 断连收尾：清空写通道与待应答（在途调用收到关闭信号），中止写任务
//! - 状态错误文本仅简短原因（不含 URL 与凭据）

use std::sync::Arc;
use std::time::Duration;

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

use super::{Inner, QqTransportOptions, WsStream, lock, update_status};
use crate::config::QqRuntimeConfig;
use crate::events::{ChannelEventSink, RecallNoticeSink};
use crate::status::ConnectionState;
use crate::{ChannelError, ChannelResult};

// =========================================================
// 连接素材
// =========================================================

/// 已建立的连接素材（写通道 + 读写半）。
///
/// 说明:
/// - 由 [`prepare_connection`] 同步制备；`write_tx` 由调用方注册进共享状态，
///   此后出站可立即写入（写任务未启动前的消息进入通道缓冲）。
pub(crate) struct Connection {
    pub(crate) write_tx: mpsc::UnboundedSender<Message>,
    write_rx: mpsc::UnboundedReceiver<Message>,
    write_half: SplitSink<WsStream, Message>,
    read_half: SplitStream<WsStream>,
}

/// 由已升级的连接流制备连接素材（同步，无 I/O）。
pub(crate) fn prepare_connection(stream: WsStream) -> Connection {
    let (write_half, read_half) = stream.split();
    let (write_tx, write_rx) = mpsc::unbounded_channel();
    Connection {
        write_tx,
        write_rx,
        write_half,
        read_half,
    }
}

// =========================================================
// 管理任务上下文
// =========================================================

/// 管理任务上下文（配置 / 参数 / 宿主回调 / 共享状态）。
pub(crate) struct ManagerContext {
    pub(crate) config: QqRuntimeConfig,
    pub(crate) options: QqTransportOptions,
    pub(crate) sink: Arc<dyn ChannelEventSink>,
    pub(crate) recall_sink: Option<Arc<dyn RecallNoticeSink>>,
    pub(crate) inner: Arc<std::sync::Mutex<Inner>>,
}

// =========================================================
// 管理任务
// =========================================================

/// 后台管理任务：维护连接生命周期直至关停。
///
/// 流程:
/// - 首次连接已由 `connect` 建立并流转状态时直接进入读循环（不重复上报状态）；
/// - 否则按退避等待后自连：成功 → 状态 `Connecting → Online`；
/// - 连接失败（非鉴权类）→ 状态 `Error` → 退避翻倍继续；
/// - 鉴权失败 → 状态 `Error` → 任务退出（凭据问题重试无意义）；
/// - 读循环断开 → 收尾清理 → 状态 `Error` → 重连（退避从下界重新开始）；
/// - 收到关停信号 → 任务退出（状态由 `shutdown` 置 `Disconnected`）。
pub(crate) async fn manage(
    context: ManagerContext,
    mut shutdown_rx: oneshot::Receiver<()>,
    initial: Option<Connection>,
) {
    let mut backoff = context.options.reconnect_min;
    let mut pending_connection = initial;

    loop {
        let connection = match pending_connection.take() {
            Some(connection) => connection,
            None => {
                // 退避等待（可被关停信号打断）
                tokio::select! {
                    _ = &mut shutdown_rx => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                update_status(
                    &context.inner,
                    context.sink.as_ref(),
                    ConnectionState::Connecting,
                    None,
                );
                match connect_once(&context.config, context.options.api_timeout).await {
                    Ok(stream) => {
                        let connection = prepare_connection(stream);
                        {
                            let mut guard = lock(&context.inner);
                            guard.write_tx = Some(connection.write_tx.clone());
                        }
                        update_status(
                            &context.inner,
                            context.sink.as_ref(),
                            ConnectionState::Online,
                            None,
                        );
                        backoff = context.options.reconnect_min;
                        connection
                    }
                    Err(ChannelError::Auth(reason)) => {
                        tracing::warn!(channel = "qq", error = %reason, "OneBot 鉴权失败，停止重连");
                        update_status(
                            &context.inner,
                            context.sink.as_ref(),
                            ConnectionState::Error,
                            Some(reason),
                        );
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(channel = "qq", error = %error, "OneBot 重连失败，继续退避");
                        update_status(
                            &context.inner,
                            context.sink.as_ref(),
                            ConnectionState::Error,
                            Some(error.to_string()),
                        );
                        backoff = next_backoff(backoff, &context.options);
                        continue;
                    }
                }
            }
        };

        let reason = run_connection(&context, connection, &mut shutdown_rx).await;

        // 断连收尾：清空写通道与待应答（在途调用感知关闭）
        {
            let mut guard = lock(&context.inner);
            guard.write_tx = None;
            guard.pending.clear();
        }

        match reason {
            None => return, // 关停信号：由 shutdown 置 Disconnected
            Some(reason) => {
                tracing::info!(channel = "qq", reason = %reason, "OneBot 连接断开，准备重连");
                update_status(
                    &context.inner,
                    context.sink.as_ref(),
                    ConnectionState::Error,
                    Some(reason),
                );
            }
        }
    }
}

/// 单次连接会话：写任务 + 读循环，返回断开原因（None = 关停信号）。
async fn run_connection(
    context: &ManagerContext,
    connection: Connection,
    shutdown_rx: &mut oneshot::Receiver<()>,
) -> Option<String> {
    let Connection {
        write_tx,
        mut write_rx,
        mut write_half,
        mut read_half,
    } = connection;
    let writer = tokio::spawn(async move {
        while let Some(message) = write_rx.recv().await {
            if SinkExt::send(&mut write_half, message).await.is_err() {
                break;
            }
        }
    });

    let mut ping_tick = tokio::time::interval(context.options.ping_interval);
    ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping_tick.tick().await; // 消耗首个立即触发的 tick

    // 空闲判定以最后一个入站帧起算：心跳发出不续期，收帧才续期
    let mut last_inbound = tokio::time::Instant::now();
    let reason = loop {
        tokio::select! {
            _ = &mut *shutdown_rx => break None,
            _ = ping_tick.tick() => {
                if write_tx.send(Message::Ping(Vec::new().into())).is_err() {
                    break Some("连接已关闭".to_string());
                }
            }
            _ = tokio::time::sleep_until(last_inbound + context.options.idle_timeout) => {
                break Some("连接空闲超时".to_string());
            }
            incoming = read_half.next() => {
                match incoming {
                    None => break Some("连接已关闭".to_string()),
                    Some(Err(error)) => {
                        tracing::debug!(channel = "qq", error = %error, "OneBot 连接读取失败");
                        break Some("连接读取失败".to_string());
                    }
                    Some(Ok(message)) => {
                        last_inbound = tokio::time::Instant::now();
                        if let Some(reason) = handle_frame(context, message) {
                            break Some(reason);
                        }
                    }
                }
            }
        }
    };

    writer.abort();
    reason
}

// =========================================================
// 入站帧处理
// =========================================================

/// 处理一帧入站消息；返回 `Some(原因)` 表示断开。
fn handle_frame(context: &ManagerContext, message: Message) -> Option<String> {
    match message {
        Message::Text(text) => {
            handle_text(context, text.as_str());
            None
        }
        Message::Ping(payload) => {
            let write_tx = { lock(&context.inner).write_tx.clone() };
            let Some(write_tx) = write_tx else {
                return Some("连接已关闭".to_string());
            };
            // 手动回 Pong（不依赖底层自动机制）
            if write_tx.send(Message::Pong(payload)).is_err() {
                Some("连接已关闭".to_string())
            } else {
                None
            }
        }
        Message::Close(_) => Some("连接已关闭".to_string()),
        // Pong / Binary / Frame 忽略
        _ => None,
    }
}

/// 处理一帧文本：`echo` 应答配对（成功应答同时记录平台消息 ID），
/// 否则解析为入站事件投递。
fn handle_text(context: &ManagerContext, text: &str) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        tracing::warn!(channel = "qq", "OneBot 上报帧 JSON 解析失败，忽略");
        return;
    };
    let echo = value
        .get("echo")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(echo) = echo {
        let (pending, sent_ids) = {
            let guard = lock(&context.inner);
            (guard.pending.clone(), guard.sent_ids.clone())
        };
        // 应答到达即记录平台消息 ID（与后续事件的被回复判定同线程顺序执行）
        if let Ok(Some(message_id)) = super::api::classify_response(&value) {
            sent_ids.insert(message_id);
        }
        if !pending.resolve(&echo, value) {
            tracing::debug!(
                channel = "qq",
                "OneBot 应答未匹配到待处理调用（已超时或重复）"
            );
        }
        return;
    }
    let sent_ids = { lock(&context.inner).sent_ids.clone() };
    if let Some(event) = super::event::parse_event(text, &sent_ids) {
        super::emit_event(&context.inner, event);
    } else if let Some(notice) = super::event::parse_notice(text) {
        if let Some(sink) = &context.recall_sink {
            sink.on_recall(&notice);
        }
    }
}

// =========================================================
// 连接建立
// =========================================================

/// 建立一次 WebSocket 连接（含可选 Bearer 鉴权头与建立超时）。
///
/// 返回:
/// - 成功时返回已升级的连接流；
/// - 超时为 `Timeout`；握手 HTTP 401/403 为 `Auth`；其余为可重试的连接错误。
pub(crate) async fn connect_once(
    config: &QqRuntimeConfig,
    timeout: Duration,
) -> ChannelResult<WsStream> {
    let mut request = config
        .ws_url
        .as_str()
        .into_client_request()
        .map_err(|error| ChannelError::Protocol(format!("WebSocket 地址无效: {error}")))?;
    if let Some(token) = config.access_token.as_deref() {
        let value = format!("Bearer {token}");
        let header = HeaderValue::from_str(&value)
            .map_err(|_| ChannelError::Auth("访问令牌包含非法字符".to_string()))?;
        request.headers_mut().insert(AUTHORIZATION, header);
    }

    match tokio::time::timeout(timeout, tokio_tungstenite::connect_async(request)).await {
        Err(_) => Err(ChannelError::Timeout("连接超时".to_string())),
        Ok(Err(error)) => Err(classify_connect_error(error)),
        Ok(Ok((stream, _response))) => Ok(stream),
    }
}

/// 连接错误分类（错误文本仅简短原因，不含地址与凭据）。
fn classify_connect_error(error: tokio_tungstenite::tungstenite::Error) -> ChannelError {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Http(response) => {
            let status = response.status().as_u16();
            if status == 401 || status == 403 {
                ChannelError::Auth(format!("连接被拒绝（HTTP {status}）"))
            } else {
                ChannelError::Protocol(format!("握手失败（HTTP {status}）"))
            }
        }
        Error::Io(io_error) => match io_error.kind() {
            std::io::ErrorKind::ConnectionRefused => {
                ChannelError::Protocol("连接被拒绝".to_string())
            }
            std::io::ErrorKind::TimedOut => ChannelError::Timeout("连接超时".to_string()),
            _ => ChannelError::Protocol("连接失败".to_string()),
        },
        _ => ChannelError::Protocol("连接失败".to_string()),
    }
}

/// 退避序列推进：翻倍且不超过上界。
fn next_backoff(current: Duration, options: &QqTransportOptions) -> Duration {
    let doubled = current.saturating_mul(2);
    if doubled > options.reconnect_max {
        options.reconnect_max
    } else {
        doubled
    }
}
