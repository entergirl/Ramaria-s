//! crates/ramaria-channel/src/transport/onebot/tests.rs - OneBot 传输连接管理单元测试
//!
//! 设计特点:
//! - 本地最小 WS 服务端（tokio-tungstenite 接受侧）：按行为模式（应答 / 静默 /
//!   立即关闭 / 拒握手 / 推事件）驱动客户端各分支，不依赖真实协议端
//! - 状态序列经记录型 sink 断言；等待用轮询 + 超时（防挂起）
//! - 毫秒级连接参数（心跳 / 空闲 / 退避）覆盖完整生命周期

// 握手回调签名由协议库固定（Err 为较大的 HTTP 响应结构），测试豁免该 lint
#![allow(clippy::result_large_err)]

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use super::*;
use crate::config::QqRuntimeConfig;
use crate::events::{ChannelEventSink, RecallNotice, RecallNoticeSink};

// =========================================================
// 测试基建
// =========================================================

/// 状态记录器（收集全部流转快照）。
#[derive(Default)]
struct RecordingSink {
    statuses: StdMutex<Vec<ChannelStatus>>,
}

impl RecordingSink {
    fn states(&self) -> Vec<ConnectionState> {
        self.statuses
            .lock()
            .expect("状态记录锁不应中毒")
            .iter()
            .map(|status| status.state)
            .collect()
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

/// 撤回通知记录器（收集读循环回调的撤回通知）。
#[derive(Default)]
struct RecordingRecallSink {
    notices: StdMutex<Vec<RecallNotice>>,
}

impl RecallNoticeSink for RecordingRecallSink {
    fn on_recall(&self, notice: &RecallNotice) {
        self.notices
            .lock()
            .expect("撤回记录锁不应中毒")
            .push(notice.clone());
    }
}

/// 通用连接参数（毫秒级；空闲阈值足够完成常规断言）。
fn fast_options() -> QqTransportOptions {
    QqTransportOptions {
        idle_timeout: Duration::from_millis(1_500),
        ping_interval: Duration::from_millis(300),
        api_timeout: Duration::from_millis(200),
        reconnect_min: Duration::from_millis(50),
        reconnect_max: Duration::from_millis(400),
    }
}

/// 空闲超时场景参数（空闲阈值短、心跳间隔长以免干扰）。
fn idle_options() -> QqTransportOptions {
    QqTransportOptions {
        idle_timeout: Duration::from_millis(120),
        ping_interval: Duration::from_secs(5),
        api_timeout: Duration::from_millis(200),
        reconnect_min: Duration::from_millis(50),
        reconnect_max: Duration::from_millis(400),
    }
}

/// mock 服务端行为（按连接生效）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerMode {
    /// 对文本请求自动回 `retcode=0` 应答（echo 配对）。
    Echo,
    /// 静默：记录帧但与不应答。
    Silent,
    /// 保持连接但不读取任何帧（不回 Pong，模拟对端失联）。
    Mute,
    /// 接受连接后立即主动关闭。
    CloseImmediately,
    /// 接受连接后回一条无 `echo` 的鉴权拒绝应答（retcode=1403）并关闭。
    AuthReject,
    /// 接受后立即断开 TCP（模拟握手失败）。
    RejectHandshake,
    /// 接受连接后推送一条事件文本。
    PushEvent(&'static str),
    /// 应答首个请求后推送一条事件文本。
    EchoThenPush(&'static str),
}

/// mock 服务端探针（连接数 / 文本帧 / Ping 数 / 建连时刻）。
#[derive(Default)]
struct ServerProbe {
    connections: Arc<AtomicUsize>,
    frames: Arc<StdMutex<Vec<Value>>>,
    pings: Arc<AtomicUsize>,
    accept_times: Arc<StdMutex<Vec<std::time::Instant>>>,
}

/// 启动 mock 服务端（循环接受连接，单连接生命周期按模式执行）。
async fn spawn_server(mode: ServerMode) -> (String, ServerProbe) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定应成功");
    let addr = listener.local_addr().expect("地址应可读");
    let probe = ServerProbe::default();
    let connections = Arc::clone(&probe.connections);
    let frames = Arc::clone(&probe.frames);
    let pings = Arc::clone(&probe.pings);
    let accept_times = Arc::clone(&probe.accept_times);

    let _server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            connections.fetch_add(1, Ordering::SeqCst);
            accept_times
                .lock()
                .expect("建连时刻锁不应中毒")
                .push(std::time::Instant::now());
            let frames = Arc::clone(&frames);
            let pings = Arc::clone(&pings);
            tokio::spawn(async move {
                if mode == ServerMode::RejectHandshake {
                    drop(stream);
                    return;
                }
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                if mode == ServerMode::CloseImmediately {
                    let _ = ws.close(None).await;
                    return;
                }
                if mode == ServerMode::AuthReject {
                    let refusal = json!({
                        "status": "failed",
                        "retcode": 1403,
                        "data": null,
                        "message": "token验证失败",
                        "wording": "token验证失败",
                        "echo": null,
                    });
                    let _ = ws.send(Message::Text(refusal.to_string().into())).await;
                    let _ = ws.close(None).await;
                    return;
                }
                if mode == ServerMode::Mute {
                    // 只持有连接、不轮询帧：客户端发来的 Ping 得不到 Pong
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    return;
                }
                if let ServerMode::PushEvent(event) = mode {
                    let _ = ws.send(Message::Text(event.to_string().into())).await;
                }

                let mut pushed = false;
                while let Some(Ok(message)) = ws.next().await {
                    match message {
                        Message::Text(text) => {
                            let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                                continue;
                            };
                            frames.lock().expect("帧记录锁不应中毒").push(value.clone());
                            let auto_reply =
                                matches!(mode, ServerMode::Echo | ServerMode::EchoThenPush(_));
                            let Some(echo) = value.get("echo") else {
                                continue;
                            };
                            if !auto_reply {
                                continue;
                            }
                            let reply = json!({
                                "status": "ok",
                                "retcode": 0,
                                "data": { "message_id": 9001 },
                                "echo": echo.clone(),
                            });
                            let _ = ws.send(Message::Text(reply.to_string().into())).await;
                            let push_event = match mode {
                                ServerMode::EchoThenPush(event) if !pushed => Some(event),
                                _ => None,
                            };
                            if let Some(event) = push_event {
                                pushed = true;
                                let _ = ws.send(Message::Text(event.to_string().into())).await;
                            }
                        }
                        Message::Ping(payload) => {
                            pings.fetch_add(1, Ordering::SeqCst);
                            let _ = ws.send(Message::Pong(payload)).await;
                        }
                        _ => {}
                    }
                }
            });
        }
    });

    (format!("ws://{addr}"), probe)
}

/// 启动鉴权 mock 服务端（校验 `Authorization: Bearer {expected}`，不匹配回 401）。
async fn spawn_auth_server(expected: &str) -> (String, Arc<AtomicUsize>) {
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
    use tokio_tungstenite::tungstenite::http::{Response as HttpResponse, StatusCode};

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定应成功");
    let addr = listener.local_addr().expect("地址应可读");
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    let expected = expected.to_string();

    let _server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let expected = expected.clone();
            tokio::spawn(async move {
                let callback = move |request: &Request,
                                     response: Response|
                      -> Result<Response, ErrorResponse> {
                    let authorized = request
                        .headers()
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .is_some_and(|value| value == expected);
                    if authorized {
                        Ok(response)
                    } else {
                        let mut refusal = HttpResponse::new(Some("unauthorized".to_string()));
                        *refusal.status_mut() = StatusCode::UNAUTHORIZED;
                        Err(refusal)
                    }
                };
                if let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await {
                    while let Some(Ok(_)) = ws.next().await {}
                }
            });
        }
    });

    (format!("ws://{addr}"), connections)
}

/// 构造一台传输实例（无鉴权）。
fn build_transport(
    url: &str,
    options: QqTransportOptions,
    sink: Arc<RecordingSink>,
) -> OneBotTransport {
    OneBotTransport::new(
        QqRuntimeConfig {
            ws_url: url.to_string(),
            access_token: None,
        },
        options,
        sink,
    )
}

/// 轮询等待状态序列满足断言（超时 panic 并携带当前序列）。
async fn wait_states(
    sink: &RecordingSink,
    timeout: Duration,
    predicate: impl Fn(&[ConnectionState]) -> bool,
) -> Vec<ConnectionState> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let states = sink.states();
        if predicate(&states) {
            return states;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "等待状态超时，当前序列: {states:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 轮询等待整型条件成立（超时 panic）。
async fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool, label: &str) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !predicate() {
        assert!(tokio::time::Instant::now() < deadline, "等待超时: {label}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// =========================================================
// 连接生命周期
// =========================================================

/// 连接成功：状态流转 Connecting → Online，关停后 Disconnected。
#[tokio::test]
async fn connect_reports_states_and_reaches_online() {
    let (url, probe) = spawn_server(ServerMode::Echo).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, fast_options(), Arc::clone(&sink));

    assert!(!transport.status().is_online());
    transport.connect().await.expect("连接应成功");
    assert_eq!(transport.status().state, ConnectionState::Online);
    assert_eq!(
        sink.states(),
        vec![ConnectionState::Connecting, ConnectionState::Online]
    );
    assert_eq!(probe.connections.load(Ordering::SeqCst), 1);

    transport.shutdown().await.expect("关停应成功");
    assert_eq!(transport.status().state, ConnectionState::Disconnected);
    assert_eq!(sink.states().last(), Some(&ConnectionState::Disconnected));
}

/// 连接幂等：重复 connect 不新建连接。
#[tokio::test]
async fn connect_is_idempotent() {
    let (url, probe) = spawn_server(ServerMode::Echo).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, fast_options(), sink);

    transport.connect().await.expect("首次连接应成功");
    transport.connect().await.expect("重复连接应直接成功");
    assert_eq!(probe.connections.load(Ordering::SeqCst), 1, "不应重复建连");

    transport.shutdown().await.expect("关停应成功");
}

/// 鉴权：正确令牌连通；错误令牌为 Auth 错误且不重连。
#[tokio::test]
async fn auth_token_gates_connection() {
    let (url, connections) = spawn_auth_server("Bearer secret").await;

    let ok_sink = Arc::new(RecordingSink::default());
    let ok_transport = OneBotTransport::new(
        QqRuntimeConfig {
            ws_url: url.clone(),
            access_token: Some("secret".to_string()),
        },
        fast_options(),
        ok_sink,
    );
    ok_transport.connect().await.expect("正确令牌应连通");
    assert!(ok_transport.status().is_online());

    let bad_sink = Arc::new(RecordingSink::default());
    let bad_transport = OneBotTransport::new(
        QqRuntimeConfig {
            ws_url: url.clone(),
            access_token: Some("wrong".to_string()),
        },
        fast_options(),
        bad_sink,
    );
    let error = bad_transport.connect().await.expect_err("错误令牌应失败");
    assert!(
        matches!(error, ChannelError::Auth(_)),
        "应为鉴权分类: {error:?}"
    );
    assert_eq!(bad_transport.status().state, ConnectionState::Error);

    // 鉴权失败不进入重连：等待一段退避时间后连接数不变
    let before = connections.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        connections.load(Ordering::SeqCst),
        before,
        "鉴权失败不应重连"
    );

    ok_transport.shutdown().await.expect("关停应成功");
    bad_transport.shutdown().await.expect("关停应成功");
}

/// 断线重连：服务端主动关闭后客户端退避重连并恢复 Online。
#[tokio::test]
async fn disconnect_triggers_reconnect() {
    let (url, probe) = spawn_server(ServerMode::CloseImmediately).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, fast_options(), Arc::clone(&sink));

    transport.connect().await.expect("首次连接应成功");
    let states = wait_states(&sink, Duration::from_secs(5), |states| {
        states.contains(&ConnectionState::Error)
            && states
                .iter()
                .filter(|state| **state == ConnectionState::Online)
                .count()
                >= 2
    })
    .await;
    assert!(states.contains(&ConnectionState::Error), "应出现断线错误态");
    assert!(probe.connections.load(Ordering::SeqCst) >= 2, "应发生重连");

    transport.shutdown().await.expect("关停应成功");
}

/// 运行期鉴权拒绝：协议端回无 `echo` 的 retcode=1403 后断开，上报原因并停止重连。
#[tokio::test]
async fn runtime_auth_rejection_stops_reconnect() {
    let (url, probe) = spawn_server(ServerMode::AuthReject).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, fast_options(), Arc::clone(&sink));

    transport.connect().await.expect("首连应成功");
    wait_until(
        Duration::from_secs(2),
        || {
            transport
                .status()
                .last_error
                .as_deref()
                .is_some_and(|text| text.contains("鉴权失败"))
        },
        "应上报鉴权失败原因",
    )
    .await;

    assert_eq!(transport.status().state, ConnectionState::Error);
    let states = sink.states();
    assert!(
        states.contains(&ConnectionState::Online),
        "断连前应已进入在线态: {states:?}"
    );
    assert_eq!(
        probe.connections.load(Ordering::SeqCst),
        1,
        "鉴权拒绝后不应重连"
    );

    transport.shutdown().await.expect("关停应成功");
}

/// 空闲超时：无任何入站帧超过阈值即判断开并重连。
#[tokio::test]
async fn idle_timeout_triggers_reconnect() {
    let (url, probe) = spawn_server(ServerMode::Silent).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, idle_options(), Arc::clone(&sink));

    transport.connect().await.expect("首次连接应成功");
    wait_states(&sink, Duration::from_secs(5), |states| {
        states.contains(&ConnectionState::Error)
            && states
                .iter()
                .filter(|state| **state == ConnectionState::Online)
                .count()
                >= 2
    })
    .await;
    assert!(
        probe.connections.load(Ordering::SeqCst) >= 2,
        "空闲超时应触发重连"
    );

    transport.shutdown().await.expect("关停应成功");
}

/// 主动心跳：客户端按间隔发送 WS Ping 帧。
#[tokio::test]
async fn client_sends_ping_frames() {
    let (url, probe) = spawn_server(ServerMode::Silent).await;
    let sink = Arc::new(RecordingSink::default());
    let options = QqTransportOptions {
        ping_interval: Duration::from_millis(60),
        idle_timeout: Duration::from_secs(5),
        ..fast_options()
    };
    let transport = build_transport(&url, options, sink);

    transport.connect().await.expect("连接应成功");
    let pings = Arc::clone(&probe.pings);
    wait_until(
        Duration::from_secs(2),
        move || pings.load(Ordering::SeqCst) >= 1,
        "应收到客户端 Ping",
    )
    .await;

    transport.shutdown().await.expect("关停应成功");
}

/// 心跳间隔短于空闲阈值：心跳发出不续期，无入站帧仍按空闲判定并重连。
#[tokio::test]
async fn idle_timeout_survives_frequent_pings() {
    let (url, probe) = spawn_server(ServerMode::Mute).await;
    let sink = Arc::new(RecordingSink::default());
    let options = QqTransportOptions {
        ping_interval: Duration::from_millis(40),
        idle_timeout: Duration::from_millis(160),
        ..fast_options()
    };
    let transport = build_transport(&url, options, Arc::clone(&sink));

    transport.connect().await.expect("连接应成功");
    let states = wait_states(&sink, Duration::from_secs(5), |states| {
        states.contains(&ConnectionState::Error)
            && states
                .iter()
                .filter(|state| **state == ConnectionState::Online)
                .count()
                >= 2
    })
    .await;
    assert!(
        states.contains(&ConnectionState::Error),
        "心跳不应延长空闲判定，应出现断线错误态"
    );
    assert!(
        probe.connections.load(Ordering::SeqCst) >= 2,
        "空闲超时应触发重连"
    );

    transport.shutdown().await.expect("关停应成功");
}

/// 关停：幂等、置 Disconnected、事件流关闭（next_event 返回 None）。
#[tokio::test]
async fn shutdown_is_idempotent_and_closes_event_stream() {
    let (url, _probe) = spawn_server(ServerMode::Echo).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, fast_options(), sink);

    transport.connect().await.expect("连接应成功");
    transport.shutdown().await.expect("关停应成功");
    assert_eq!(transport.status().state, ConnectionState::Disconnected);
    transport.shutdown().await.expect("重复关停应幂等成功");

    assert!(transport.next_event().await.is_none(), "事件流应已关闭");
}

/// 退避递增：连续连接失败时重试间隔逐步增大。
#[tokio::test]
async fn reconnect_backoff_grows() {
    let (url, probe) = spawn_server(ServerMode::RejectHandshake).await;
    let sink = Arc::new(RecordingSink::default());
    let options = QqTransportOptions {
        reconnect_min: Duration::from_millis(80),
        reconnect_max: Duration::from_millis(640),
        ..fast_options()
    };
    let transport = build_transport(&url, options, sink);

    let first = transport.connect().await;
    assert!(first.is_err(), "首次连接应失败（服务端拒握手）");
    assert_eq!(transport.status().state, ConnectionState::Error);

    let connections = Arc::clone(&probe.connections);
    wait_until(
        Duration::from_secs(3),
        move || connections.load(Ordering::SeqCst) >= 3,
        "应观察到至少三次连接尝试",
    )
    .await;

    // 间隔递增：退避从 80ms 翻倍（80 → 160 → 320）
    let times = probe
        .accept_times
        .lock()
        .expect("建连时刻锁不应中毒")
        .clone();
    assert!(times.len() >= 3, "应记录至少三次建连时刻: {}", times.len());
    let first_gap = times[1].duration_since(times[0]);
    let second_gap = times[2].duration_since(times[1]);
    assert!(
        first_gap >= Duration::from_millis(60),
        "首次退避应接近下界: {first_gap:?}"
    );
    assert!(
        second_gap > first_gap,
        "退避应递增: {first_gap:?} -> {second_gap:?}"
    );

    transport.shutdown().await.expect("关停应成功");
}

// =========================================================
// 出站与入站
// =========================================================

/// 出站发送：请求动作与参数正确，echo 配对成功。
#[tokio::test]
async fn send_success_pairs_echo_response() {
    let (url, probe) = spawn_server(ServerMode::Echo).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, fast_options(), sink);
    transport.connect().await.expect("连接应成功");

    let target = OutboundTarget {
        external_ref: "c2c:10001".to_string(),
    };
    transport
        .send(&target, &OutboundMessage::text("你好"))
        .await
        .expect("发送应成功");

    let frames = probe.frames.lock().expect("帧记录锁不应中毒").clone();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["action"], "send_private_msg");
    assert_eq!(frames[0]["params"]["user_id"], 10001);
    assert_eq!(frames[0]["params"]["message"][0]["data"]["text"], "你好");

    transport.shutdown().await.expect("关停应成功");
}

/// 被回复判定闭环：发送后缓存的平台消息 ID 命中后续 reply 事件。
#[tokio::test]
async fn sent_message_id_enables_reply_detection() {
    let reply_event = r#"{
        "post_type": "message",
        "message_type": "group",
        "group_id": 20002,
        "user_id": 10001,
        "self_id": 42,
        "time": 1700000000,
        "message": [
            { "type": "reply", "data": { "id": "9001" } },
            { "type": "text", "data": { "text": "回复你" } }
        ]
    }"#;
    let (url, _probe) = spawn_server(ServerMode::EchoThenPush(reply_event)).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, fast_options(), sink);
    transport.connect().await.expect("连接应成功");

    let target = OutboundTarget {
        external_ref: "c2c:10001".to_string(),
    };
    transport
        .send(&target, &OutboundMessage::text("在吗"))
        .await
        .expect("发送应成功");

    let event = tokio::time::timeout(Duration::from_secs(2), transport.next_event())
        .await
        .expect("等待事件不应超时")
        .expect("应有事件")
        .expect("事件应解析成功");
    assert_eq!(event.reply_to.as_deref(), Some("9001"));
    assert!(event.should_respond, "回复本机已发消息应触发响应");

    transport.shutdown().await.expect("关停应成功");
}

/// 出站超时：无效应答时按 api_timeout 判超时。
#[tokio::test]
async fn send_times_out_without_response() {
    let (url, _probe) = spawn_server(ServerMode::Silent).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, fast_options(), sink);
    transport.connect().await.expect("连接应成功");

    let target = OutboundTarget {
        external_ref: "c2c:10001".to_string(),
    };
    let error = transport
        .send(&target, &OutboundMessage::text("你好"))
        .await
        .expect_err("无效应答应超时");
    assert!(
        matches!(error, ChannelError::Timeout(_)),
        "应为超时分类: {error:?}"
    );

    transport.shutdown().await.expect("关停应成功");
}

/// 未连接时发送：立即返回 Closed。
#[tokio::test]
async fn send_without_connection_is_closed() {
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport("ws://127.0.0.1:1", fast_options(), sink);
    let target = OutboundTarget {
        external_ref: "c2c:10001".to_string(),
    };
    let error = transport
        .send(&target, &OutboundMessage::text("你好"))
        .await
        .expect_err("未连接发送应失败");
    assert!(matches!(error, ChannelError::Closed));
}

/// 入站事件投递：推送帧经解析进入事件流。
#[tokio::test]
async fn inbound_event_delivered() {
    let private_event = r#"{
        "post_type": "message",
        "message_type": "private",
        "message_id": 1,
        "user_id": 10001,
        "self_id": 42,
        "time": 1700000000,
        "message": [{ "type": "text", "data": { "text": "你好" } }],
        "sender": { "user_id": 10001, "nickname": "小明" }
    }"#;
    let (url, _probe) = spawn_server(ServerMode::PushEvent(private_event)).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, fast_options(), sink);

    transport.connect().await.expect("连接应成功");
    let event = tokio::time::timeout(Duration::from_secs(2), transport.next_event())
        .await
        .expect("等待事件不应超时")
        .expect("应有事件")
        .expect("事件应解析成功");
    assert_eq!(event.external_ref, "c2c:10001");
    assert!(event.should_respond);

    transport.shutdown().await.expect("关停应成功");
}

/// 出站附件接线：URL / 字节图片随文本发出；超限降级；纯空文本拒绝。
#[tokio::test]
async fn outbound_attachment_sends_image_segment_and_empty_text_rejected() {
    let (url, probe) = spawn_server(ServerMode::Echo).await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(&url, fast_options(), sink);
    transport.connect().await.expect("连接应成功");

    let target = OutboundTarget {
        external_ref: "c2c:10001".to_string(),
    };
    let with_attachment = OutboundMessage {
        text: "看这个".to_string(),
        attachment: Some(crate::transport::OutboundAttachment::Url(
            "https://example.com/a.png".to_string(),
        )),
    };
    transport
        .send(&target, &with_attachment)
        .await
        .expect("文本 + URL 图片应发送成功");

    let bytes_message = OutboundMessage {
        text: String::new(),
        attachment: Some(crate::transport::OutboundAttachment::Bytes {
            file_name: "cat.png".to_string(),
            data: b"PNGDATA".to_vec(),
        }),
    };
    transport
        .send(&target, &bytes_message)
        .await
        .expect("仅字节图片应发送成功");

    let oversize_message = OutboundMessage {
        text: "先说话".to_string(),
        attachment: Some(crate::transport::OutboundAttachment::Bytes {
            file_name: "big.png".to_string(),
            data: vec![0u8; OUTBOUND_IMAGE_MAX_BYTES + 1],
        }),
    };
    transport
        .send(&target, &oversize_message)
        .await
        .expect("超限附件应降级纯文本发送");

    let empty = transport
        .send(&target, &OutboundMessage::text("   "))
        .await
        .expect_err("空文本应拒绝");
    assert!(matches!(empty, ChannelError::Protocol(_)));

    let frames = probe.frames.lock().expect("帧记录锁不应中毒").clone();
    assert_eq!(frames.len(), 3, "三次成功发送各一帧");
    assert_eq!(frames[0]["params"]["message"][0]["data"]["text"], "看这个");
    assert_eq!(frames[0]["params"]["message"][1]["type"], "image");
    assert_eq!(
        frames[0]["params"]["message"][1]["data"]["file"],
        "https://example.com/a.png"
    );
    assert_eq!(
        frames[1]["params"]["message"].as_array().map(Vec::len),
        Some(1),
        "仅图片应单段"
    );
    assert_eq!(frames[1]["params"]["message"][0]["type"], "image");
    let encoded = frames[1]["params"]["message"][0]["data"]["file"]
        .as_str()
        .expect("file 应为字符串");
    assert_eq!(
        encoded,
        format!("base64://{}", super::api::base64_encode(b"PNGDATA"))
    );
    assert_eq!(
        frames[2]["params"]["message"].as_array().map(Vec::len),
        Some(1),
        "超限附件降级后仅文本段"
    );
    assert_eq!(frames[2]["params"]["message"][0]["type"], "text");

    transport.shutdown().await.expect("关停应成功");
}

/// 撤回通知投递：notice 帧经连接读循环回调到注册的接收端。
#[tokio::test]
async fn recall_notice_reaches_sink() {
    let notice_frame = r#"{
        "post_type": "notice",
        "notice_type": "friend_recall",
        "user_id": 10001,
        "self_id": 42,
        "time": 1700000000,
        "message_id": 777
    }"#;
    let (url, _probe) = spawn_server(ServerMode::PushEvent(notice_frame)).await;
    let sink = Arc::new(RecordingSink::default());
    let recall_sink = Arc::new(RecordingRecallSink::default());
    let handle: Arc<dyn RecallNoticeSink> = recall_sink.clone();
    let transport = build_transport(&url, fast_options(), sink).with_recall_sink(handle);
    transport.connect().await.expect("连接应成功");

    let mut waited = Duration::ZERO;
    let step = Duration::from_millis(10);
    while waited < Duration::from_secs(2) {
        if !recall_sink
            .notices
            .lock()
            .expect("撤回记录锁不应中毒")
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(step).await;
        waited += step;
    }
    let notices = recall_sink
        .notices
        .lock()
        .expect("撤回记录锁不应中毒")
        .clone();
    assert_eq!(notices.len(), 1, "应收到一条撤回通知");
    assert_eq!(notices[0].channel, "qq");
    assert_eq!(notices[0].external_ref, "c2c:10001");
    assert_eq!(notices[0].platform_message_id, "777");
    assert_eq!(notices[0].operator_id.as_deref(), Some("10001"));
    assert_eq!(notices[0].occurred_at, 1_700_000_000_000);

    transport.shutdown().await.expect("关停应成功");
}
