//! crates/ramaria-channel/src/transport/telegram/tests.rs - Telegram 传输连接 / 轮询 / 发送 / 下载单元测试
//!
//! 设计特点:
//! - 本地最小 HTTP 服务端（tokio TcpListener 手写 HTTP/1.1）：按路径分发
//!   `getMe` / `getUpdates` / `sendMessage` / `getFile` 与文件下载，支持可控失败
//!   （令牌 401 / 轮询断连 / 发送错误码）驱动客户端各分支
//! - 状态序列经记录型 sink 断言；等待用轮询 + 超时（防挂起）
//! - 毫秒级连接参数（轮询 / 空批 / 退避）覆盖完整生命周期
//! - 下载器用例走真实 HTTP 两步链路（getFile + 文件下载）

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::config::TelegramRuntimeConfig;
use crate::events::ChannelEventSink;
use crate::runtime::download::ImageDownloader;

// =========================================================
// mock Bot API 服务端
// =========================================================

/// mock 服务端状态（探针与可控应答）。
struct MockState {
    calls: StdMutex<Vec<String>>,
    updates: StdMutex<VecDeque<Value>>,
    updates_bodies: StdMutex<Vec<Value>>,
    get_file_bodies: StdMutex<Vec<Value>>,
    sent: StdMutex<Vec<Value>>,
    fail_updates: AtomicBool,
    send_error: StdMutex<Option<(i64, String)>>,
    file_path: StdMutex<Option<String>>,
    file_size: StdMutex<Option<u64>>,
    file_bytes: StdMutex<Vec<u8>>,
    next_message_id: AtomicI64,
}

/// mock Telegram Bot API 服务端（一次请求一连接，响应带 `Connection: close`）。
struct MockTelegram {
    base_url: String,
    state: Arc<MockState>,
}

impl MockTelegram {
    /// 启动服务端（路径中的令牌不等于 `expected_token` 时回 401）。
    async fn start(expected_token: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定应成功");
        let addr = listener.local_addr().expect("地址应可读");
        let state = Arc::new(MockState {
            calls: StdMutex::new(Vec::new()),
            updates: StdMutex::new(VecDeque::new()),
            updates_bodies: StdMutex::new(Vec::new()),
            get_file_bodies: StdMutex::new(Vec::new()),
            sent: StdMutex::new(Vec::new()),
            fail_updates: AtomicBool::new(false),
            send_error: StdMutex::new(None),
            file_path: StdMutex::new(Some("photos/default.png".to_string())),
            file_size: StdMutex::new(None),
            file_bytes: StdMutex::new(Vec::new()),
            next_message_id: AtomicI64::new(1),
        });
        let expected = expected_token.to_string();
        let task_state = Arc::clone(&state);
        let _server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = Arc::clone(&task_state);
                let expected = expected.clone();
                tokio::spawn(async move {
                    handle_connection(stream, state, expected).await;
                });
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            state,
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 已处理方法序列（含令牌错误的请求）。
    fn calls(&self) -> Vec<String> {
        self.state.calls.lock().expect("调用记录锁不应中毒").clone()
    }

    /// 累计某方法的调用次数。
    fn call_count(&self, method: &str) -> usize {
        self.calls().iter().filter(|call| *call == method).count()
    }

    /// 预置一条待拉取的更新。
    fn push_update(&self, update: Value) {
        self.state
            .updates
            .lock()
            .expect("更新队列锁不应中毒")
            .push_back(update);
    }

    /// 已收到的 `sendMessage` 请求体。
    fn sent_messages(&self) -> Vec<Value> {
        self.state.sent.lock().expect("发送记录锁不应中毒").clone()
    }

    /// 已收到的 `getUpdates` 请求体。
    fn get_updates_bodies(&self) -> Vec<Value> {
        self.state
            .updates_bodies
            .lock()
            .expect("轮询记录锁不应中毒")
            .clone()
    }

    /// 已收到的 `getFile` 请求体。
    fn get_file_bodies(&self) -> Vec<Value> {
        self.state
            .get_file_bodies
            .lock()
            .expect("文件记录锁不应中毒")
            .clone()
    }

    /// 控制 `getUpdates` 是否直接断连（模拟网络失败）。
    fn set_fail_updates(&self, fail: bool) {
        self.state.fail_updates.store(fail, Ordering::SeqCst);
    }

    /// 控制 `sendMessage` 的错误应答（`None` 恢复成功）。
    fn set_send_error(&self, error: Option<(i64, String)>) {
        *self.state.send_error.lock().expect("错误配置锁不应中毒") = error;
    }

    /// 配置 `getFile` 应答与下载字节。
    fn set_file(&self, path: &str, size: Option<u64>, bytes: &[u8]) {
        *self.state.file_path.lock().expect("文件路径锁不应中毒") = Some(path.to_string());
        *self.state.file_size.lock().expect("文件体积锁不应中毒") = size;
        *self.state.file_bytes.lock().expect("文件字节锁不应中毒") = bytes.to_vec();
    }

    /// 配置 `getFile` 应答缺少文件路径。
    fn set_file_missing(&self) {
        *self.state.file_path.lock().expect("文件路径锁不应中毒") = None;
    }
}

/// 处理一条连接（读一个请求，按路由写响应）。
async fn handle_connection(mut stream: TcpStream, state: Arc<MockState>, expected: String) {
    let Some((method, path, body)) = read_request(&mut stream).await else {
        return;
    };

    // 文件下载路由：/file/bot{token}/{path}
    if method == "GET" && path.starts_with("/file/bot") {
        let token = path
            .trim_start_matches("/file/bot")
            .split('/')
            .next()
            .unwrap_or("");
        if token != expected {
            write_json(&mut stream, 401, &unauthorized_body()).await;
            return;
        }
        state
            .calls
            .lock()
            .expect("调用记录锁不应中毒")
            .push("file".to_string());
        let bytes = state.file_bytes.lock().expect("文件字节锁不应中毒").clone();
        write_octets(&mut stream, &bytes).await;
        return;
    }

    // API 路由：/bot{token}/{method}
    let Some(rest) = path.strip_prefix("/bot") else {
        write_json(&mut stream, 404, &not_found_body()).await;
        return;
    };
    let (token, api_method) = rest.split_once('/').unwrap_or((rest, ""));
    state
        .calls
        .lock()
        .expect("调用记录锁不应中毒")
        .push(api_method.to_string());
    if token != expected {
        write_json(&mut stream, 401, &unauthorized_body()).await;
        return;
    }

    match api_method {
        "getMe" => {
            let body = json!({
                "ok": true,
                "result": { "id": 42, "is_bot": true, "first_name": "Ramaria", "username": "testbot" }
            });
            write_json(&mut stream, 200, &body).await;
        }
        "getUpdates" => {
            let parsed = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
            state
                .updates_bodies
                .lock()
                .expect("轮询记录锁不应中毒")
                .push(parsed);
            if state.fail_updates.load(Ordering::SeqCst) {
                // 直接断开：模拟网络类失败
                return;
            }
            let updates: Vec<Value> = state
                .updates
                .lock()
                .expect("更新队列锁不应中毒")
                .drain(..)
                .collect();
            let body = json!({ "ok": true, "result": updates });
            write_json(&mut stream, 200, &body).await;
        }
        "sendMessage" => {
            let parsed = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
            state.sent.lock().expect("发送记录锁不应中毒").push(parsed);
            let configured = state.send_error.lock().expect("错误配置锁不应中毒").clone();
            match configured {
                Some((code, description)) => {
                    let body = json!({
                        "ok": false,
                        "error_code": code,
                        "description": description
                    });
                    write_json(&mut stream, 200, &body).await;
                }
                None => {
                    let message_id = state.next_message_id.fetch_add(1, Ordering::SeqCst);
                    let body = json!({ "ok": true, "result": { "message_id": message_id } });
                    write_json(&mut stream, 200, &body).await;
                }
            }
        }
        "getFile" => {
            let parsed = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
            state
                .get_file_bodies
                .lock()
                .expect("文件记录锁不应中毒")
                .push(parsed);
            let path = state.file_path.lock().expect("文件路径锁不应中毒").clone();
            let size = *state.file_size.lock().expect("文件体积锁不应中毒");
            let body = match (path, size) {
                (Some(path), Some(size)) => {
                    json!({ "ok": true, "result": { "file_path": path, "file_size": size } })
                }
                (Some(path), None) => json!({ "ok": true, "result": { "file_path": path } }),
                (None, _) => json!({ "ok": true, "result": {} }),
            };
            write_json(&mut stream, 200, &body).await;
        }
        _ => {
            write_json(&mut stream, 404, &not_found_body()).await;
        }
    }
}

/// 401 应答体（令牌不匹配）。
fn unauthorized_body() -> Value {
    json!({ "ok": false, "error_code": 401, "description": "Unauthorized" })
}

/// 404 应答体（未知方法）。
fn not_found_body() -> Value {
    json!({ "ok": false, "error_code": 404, "description": "Not Found" })
}

/// 读取一个 HTTP/1.1 请求（请求行 + 头部 + `Content-Length` 字节体）。
async fn read_request(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let header_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > 64 * 1024 {
            return None;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = headers.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().ok()?;
            }
        }
    }
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Some((method, path, body))
}

/// 写一个 JSON 响应（带 `Connection: close`）。
async fn write_json(stream: &mut TcpStream, status: u16, value: &Value) {
    let body = value.to_string();
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
}

/// 写一个二进制响应（带 `Connection: close`）。
async fn write_octets(stream: &mut TcpStream, bytes: &[u8]) {
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        bytes.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(bytes).await;
}

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

/// 毫秒级连接参数（轮询即时返回；空批延迟与退避走短周期）。
fn fast_options(base_url: &str) -> TelegramTransportOptions {
    TelegramTransportOptions {
        api_base_url: base_url.to_string(),
        poll_timeout_secs: 1,
        request_timeout: Duration::from_secs(2),
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_millis(400),
        empty_batch_delay: Duration::from_millis(20),
    }
}

/// 构造一台传输实例。
fn build_transport(base_url: &str, token: &str, sink: Arc<RecordingSink>) -> TelegramTransport {
    TelegramTransport::new(
        TelegramRuntimeConfig {
            bot_token: token.to_string(),
        },
        fast_options(base_url),
        sink,
    )
}

/// 私聊文本更新样例。
fn private_update(update_id: i64, message_id: i64, text: &str) -> Value {
    json!({
        "update_id": update_id,
        "message": {
            "message_id": message_id,
            "from": { "id": 10001, "is_bot": false, "first_name": "小明" },
            "chat": { "id": 10001, "first_name": "小明", "type": "private" },
            "date": 1_700_000_000,
            "text": text
        }
    })
}

/// 服务消息更新样例（bot 入群，无文本与媒体）。
fn service_update(update_id: i64) -> Value {
    json!({
        "update_id": update_id,
        "message": {
            "message_id": 900,
            "from": { "id": 42, "is_bot": true, "first_name": "Ramaria" },
            "chat": { "id": -1001234567890i64, "title": "群", "type": "group" },
            "date": 1_700_000_000,
            "new_chat_members": [{ "id": 42, "is_bot": true, "first_name": "Ramaria" }]
        }
    })
}

/// 等待下一个事件（超时 panic）。
async fn wait_event(transport: &TelegramTransport, timeout: Duration) -> TransportEvent {
    tokio::time::timeout(timeout, transport.next_event())
        .await
        .expect("等待事件不应超时")
        .expect("事件流应可用")
        .expect("事件应解析成功")
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

/// 轮询等待条件成立（超时 panic）。
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

/// 连接成功：状态流转 Connecting → Online；关停后 Disconnected。
#[tokio::test]
async fn connect_reports_states_and_reaches_online() {
    let mock = MockTelegram::start("tok").await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", Arc::clone(&sink));

    assert!(!transport.status().is_online());
    transport.connect().await.expect("连接应成功");
    assert!(transport.status().is_online());
    assert_eq!(
        sink.states(),
        vec![ConnectionState::Connecting, ConnectionState::Online]
    );
    assert_eq!(mock.call_count("getMe"), 1);

    transport.shutdown().await.expect("关停应成功");
    assert_eq!(transport.status().state, ConnectionState::Disconnected);
    assert_eq!(sink.states().last(), Some(&ConnectionState::Disconnected));
}

/// 连接幂等：重复 connect 不重复调用 getMe。
#[tokio::test]
async fn connect_is_idempotent() {
    let mock = MockTelegram::start("tok").await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);

    transport.connect().await.expect("首次连接应成功");
    transport.connect().await.expect("重复连接应直接成功");
    assert_eq!(mock.call_count("getMe"), 1, "不应重复鉴权");

    transport.shutdown().await.expect("关停应成功");
}

/// 令牌错误：getMe 401 → Auth 错误 + Error 态 + 不重连。
#[tokio::test]
async fn auth_failure_stops_reconnect() {
    let mock = MockTelegram::start("expected-token").await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "wrong-token", Arc::clone(&sink));

    let error = transport.connect().await.expect_err("错误令牌应失败");
    assert!(
        matches!(error, ChannelError::Auth(_)),
        "应为鉴权分类: {error:?}"
    );
    assert_eq!(transport.status().state, ConnectionState::Error);

    // 稳定窗口：鉴权失败不应进入重连
    let before = mock.call_count("getMe");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mock.call_count("getMe"), before, "鉴权失败不应重试");

    transport.shutdown().await.expect("关停应成功");
}

/// 关停：幂等、置 Disconnected、事件流关闭（next_event 返回 None）。
#[tokio::test]
async fn shutdown_is_idempotent_and_closes_event_stream() {
    let mock = MockTelegram::start("tok").await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);

    transport.connect().await.expect("连接应成功");
    transport.shutdown().await.expect("关停应成功");
    assert_eq!(transport.status().state, ConnectionState::Disconnected);
    transport.shutdown().await.expect("重复关停应幂等成功");

    assert!(transport.next_event().await.is_none(), "事件流应已关闭");
}

// =========================================================
// 轮询与 offset
// =========================================================

/// 轮询投递：预置更新经 getUpdates 进入事件流。
#[tokio::test]
async fn get_updates_delivers_events() {
    let mock = MockTelegram::start("tok").await;
    mock.push_update(private_update(5, 501, "你好"));
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);
    transport.connect().await.expect("连接应成功");

    let event = wait_event(&transport, Duration::from_secs(3)).await;
    assert_eq!(event.external_ref, "c2c:10001");
    assert_eq!(event.platform_message_id.as_deref(), Some("501"));
    assert_eq!(event.self_id.as_deref(), Some("42"));
    assert!(event.should_respond, "私聊应触发");

    transport.shutdown().await.expect("关停应成功");
}

/// offset 确认制：首轮不带 offset；批次处理后按最大 update_id + 1 推进。
#[tokio::test]
async fn offset_advances_after_delivery() {
    let mock = MockTelegram::start("tok").await;
    mock.push_update(private_update(5, 501, "你好"));
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);
    transport.connect().await.expect("连接应成功");
    let _ = wait_event(&transport, Duration::from_secs(3)).await;

    let mock_probe = &mock;
    wait_until(
        Duration::from_secs(3),
        move || mock_probe.get_updates_bodies().len() >= 2,
        "第二轮轮询",
    )
    .await;
    let bodies = mock.get_updates_bodies();
    assert!(bodies[0].get("offset").is_none(), "首轮不应带 offset");
    assert_eq!(bodies[1]["offset"], 6, "offset 应推进为最大 update_id + 1");
    assert_eq!(bodies[0]["allowed_updates"], json!(["message"]));

    transport.shutdown().await.expect("关停应成功");
}

/// 被忽略的更新同样推进 offset（服务消息不产出事件但确认）。
#[tokio::test]
async fn ignored_update_still_advances_offset() {
    let mock = MockTelegram::start("tok").await;
    mock.push_update(service_update(10));
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);
    transport.connect().await.expect("连接应成功");

    let mock_probe = &mock;
    wait_until(
        Duration::from_secs(3),
        move || mock_probe.get_updates_bodies().len() >= 2,
        "第二轮轮询",
    )
    .await;
    let bodies = mock.get_updates_bodies();
    assert_eq!(bodies[1]["offset"], 11, "被忽略的更新应照常确认");

    // 服务消息不产出事件
    let silent = tokio::time::timeout(Duration::from_millis(150), transport.next_event()).await;
    assert!(silent.is_err(), "服务消息不应产出事件");

    transport.shutdown().await.expect("关停应成功");
}

/// 空批次不产出事件（长轮询无更新时静默）。
#[tokio::test]
async fn empty_batch_emits_nothing() {
    let mock = MockTelegram::start("tok").await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);
    transport.connect().await.expect("连接应成功");

    let silent = tokio::time::timeout(Duration::from_millis(150), transport.next_event()).await;
    assert!(silent.is_err(), "空批次不应产出事件");

    transport.shutdown().await.expect("关停应成功");
}

/// 轮询网络失败：进入 Error 态并退避重试；恢复后回到 Online 且事件投递。
#[tokio::test]
async fn poll_failure_backs_off_and_recovers() {
    let mock = MockTelegram::start("tok").await;
    mock.set_fail_updates(true);
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", Arc::clone(&sink));
    transport.connect().await.expect("连接应成功");

    // 失败后进入 Error 态并持续重试（至少两次 getUpdates 尝试）
    wait_states(&sink, Duration::from_secs(3), |states| {
        states.contains(&ConnectionState::Error)
    })
    .await;
    let mock_probe = &mock;
    wait_until(
        Duration::from_secs(3),
        move || mock_probe.call_count("getUpdates") >= 2,
        "退避重试",
    )
    .await;

    // 恢复：网络正常后事件投递、状态回 Online
    mock.set_fail_updates(false);
    mock.push_update(private_update(20, 502, "恢复了吗"));
    let event = wait_event(&transport, Duration::from_secs(3)).await;
    assert_eq!(event.platform_message_id.as_deref(), Some("502"));
    wait_states(&sink, Duration::from_secs(3), |states| {
        states
            .iter()
            .filter(|state| **state == ConnectionState::Online)
            .count()
            >= 2
    })
    .await;

    transport.shutdown().await.expect("关停应成功");
}

// =========================================================
// 出站发送
// =========================================================

/// 发送成功：请求体聊天 ID 与文本正确（私聊 / 群聊两种标识）。
#[tokio::test]
async fn send_success_single_chunk() {
    let mock = MockTelegram::start("tok").await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);
    transport.connect().await.expect("连接应成功");

    transport
        .send(
            &OutboundTarget {
                external_ref: "c2c:10001".to_string(),
            },
            &OutboundMessage::text("你好"),
        )
        .await
        .expect("发送应成功");
    transport
        .send(
            &OutboundTarget {
                external_ref: "group:-1001234567890".to_string(),
            },
            &OutboundMessage::text("在吗"),
        )
        .await
        .expect("群聊发送应成功");

    let sent = mock.sent_messages();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["chat_id"], 10001);
    assert_eq!(sent[0]["text"], "你好");
    assert_eq!(sent[1]["chat_id"], -1001234567890i64);
    assert_eq!(sent[1]["text"], "在吗");

    transport.shutdown().await.expect("关停应成功");
}

/// 超长文本按 UTF-16 上限分片逐片发送。
#[tokio::test]
async fn send_long_text_splits_into_chunks() {
    let mock = MockTelegram::start("tok").await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);
    transport.connect().await.expect("连接应成功");

    let text = "x".repeat(4096 + 10);
    transport
        .send(
            &OutboundTarget {
                external_ref: "c2c:10001".to_string(),
            },
            &OutboundMessage::text(text.clone()),
        )
        .await
        .expect("分片发送应成功");

    let sent = mock.sent_messages();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["text"].as_str().map(str::len), Some(4096));
    assert_eq!(sent[1]["text"].as_str().map(str::len), Some(10));
    assert_eq!(
        format!(
            "{}{}",
            sent[0]["text"].as_str().unwrap_or_default(),
            sent[1]["text"].as_str().unwrap_or_default()
        ),
        text
    );

    transport.shutdown().await.expect("关停应成功");
}

/// 429 应答 → 协议错误（可退避重试的分类）；403 → 认证类（不可重试）。
#[tokio::test]
async fn send_error_classification() {
    let mock = MockTelegram::start("tok").await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);
    transport.connect().await.expect("连接应成功");
    let target = OutboundTarget {
        external_ref: "c2c:10001".to_string(),
    };

    mock.set_send_error(Some((429, "Too Many Requests: retry after 30".to_string())));
    let error = transport
        .send(&target, &OutboundMessage::text("你好"))
        .await
        .expect_err("429 应失败");
    assert!(
        matches!(error, ChannelError::Protocol(_)),
        "429 应为协议分类: {error:?}"
    );
    assert!(error.to_string().contains("429"));

    mock.set_send_error(Some((
        403,
        "Forbidden: bot was blocked by the user".to_string(),
    )));
    let error = transport
        .send(&target, &OutboundMessage::text("你好"))
        .await
        .expect_err("403 应失败");
    assert!(
        matches!(error, ChannelError::Auth(_)),
        "403 应为认证分类: {error:?}"
    );

    transport.shutdown().await.expect("关停应成功");
}

/// 空文本拒绝：不发请求；附件忽略后仍发文本。
#[tokio::test]
async fn send_rejects_empty_text_and_ignores_attachment() {
    let mock = MockTelegram::start("tok").await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);
    transport.connect().await.expect("连接应成功");
    let target = OutboundTarget {
        external_ref: "c2c:10001".to_string(),
    };

    let error = transport
        .send(&target, &OutboundMessage::text("   "))
        .await
        .expect_err("空文本应拒绝");
    assert!(matches!(error, ChannelError::Protocol(_)));
    assert!(mock.sent_messages().is_empty(), "空文本不应发出请求");

    let with_attachment = OutboundMessage {
        text: "看这个".to_string(),
        attachment: Some(crate::transport::OutboundAttachment::Url(
            "https://example.com/a.png".to_string(),
        )),
    };
    transport
        .send(&target, &with_attachment)
        .await
        .expect("附件忽略后应仅发文本");
    let sent = mock.sent_messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["text"], "看这个");

    transport.shutdown().await.expect("关停应成功");
}

/// 纯附件消息拒绝：传输不声明附件能力，空文本 + 附件即协议错误（不发请求）。
#[tokio::test]
async fn send_rejects_attachment_only_message() {
    let mock = MockTelegram::start("tok").await;
    let sink = Arc::new(RecordingSink::default());
    let transport = build_transport(mock.base_url(), "tok", sink);
    transport.connect().await.expect("连接应成功");

    assert!(
        !transport.supports_outbound_attachment(),
        "Telegram 不支持纯附件消息"
    );

    let error = transport
        .send(
            &OutboundTarget {
                external_ref: "c2c:10001".to_string(),
            },
            &OutboundMessage {
                text: String::new(),
                attachment: Some(crate::transport::OutboundAttachment::Bytes {
                    file_name: "cat.png".to_string(),
                    data: vec![1, 2, 3],
                }),
            },
        )
        .await
        .expect_err("纯附件消息应拒绝");
    assert!(matches!(error, ChannelError::Protocol(_)));
    assert!(mock.sent_messages().is_empty(), "不应发出请求");

    transport.shutdown().await.expect("关停应成功");
}

// =========================================================
// 媒体下载器
// =========================================================

/// getFile 两步下载：请求体携带文件标识，返回下载字节。
#[tokio::test]
async fn downloader_fetches_file_bytes() {
    let mock = MockTelegram::start("tok").await;
    let bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    mock.set_file("photos/a.png", Some(8), &bytes);
    let downloader =
        TelegramFileDownloader::with_options("tok", mock.base_url(), Duration::from_secs(2), 0);

    let data = downloader.download("photo-1").await.expect("下载应成功");
    assert_eq!(data, bytes);
    assert_eq!(mock.get_file_bodies()[0]["file_id"], "photo-1");
    assert!(mock.calls().contains(&"file".to_string()), "应发生文件下载");
}

/// getFile 体积预检：超限直接拒绝且不发起文件下载。
#[tokio::test]
async fn downloader_rejects_oversize_precheck() {
    let mock = MockTelegram::start("tok").await;
    mock.set_file("photos/big.png", Some(21 * 1024 * 1024), &[]);
    let downloader =
        TelegramFileDownloader::with_options("tok", mock.base_url(), Duration::from_secs(2), 0);

    let error = downloader.download("big").await.expect_err("超限应拒绝");
    assert!(matches!(error, ChannelError::Protocol(_)));
    assert!(
        !mock.calls().contains(&"file".to_string()),
        "预检拒绝不应发起文件下载"
    );
}

/// getFile 应答缺少文件路径 → 协议错误。
#[tokio::test]
async fn downloader_missing_file_path_is_protocol_error() {
    let mock = MockTelegram::start("tok").await;
    mock.set_file_missing();
    let downloader =
        TelegramFileDownloader::with_options("tok", mock.base_url(), Duration::from_secs(2), 0);

    let error = downloader
        .download("photo-1")
        .await
        .expect_err("缺路径应失败");
    assert!(matches!(error, ChannelError::Protocol(_)));
}

/// getFile 令牌错误 → 认证类错误。
#[tokio::test]
async fn downloader_auth_failure() {
    let mock = MockTelegram::start("expected-token").await;
    let downloader = TelegramFileDownloader::with_options(
        "wrong-token",
        mock.base_url(),
        Duration::from_secs(2),
        0,
    );

    let error = downloader
        .download("photo-1")
        .await
        .expect_err("令牌错误应失败");
    assert!(
        matches!(error, ChannelError::Auth(_)),
        "应为鉴权分类: {error:?}"
    );
}

/// 网络失败：错误文案不含令牌与地址。
#[tokio::test]
async fn downloader_network_error_masks_credentials() {
    let token = "super-secret-token";
    let downloader = TelegramFileDownloader::with_options(
        token,
        "http://127.0.0.1:1",
        Duration::from_millis(500),
        0,
    );

    let error = downloader
        .download("photo-1")
        .await
        .expect_err("网络失败应报错");
    let text = error.to_string();
    assert!(!text.contains(token), "错误文案不应含令牌: {text}");
    assert!(!text.contains("127.0.0.1"), "错误文案不应含地址: {text}");
}
