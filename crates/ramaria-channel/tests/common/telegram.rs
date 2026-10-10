//! crates/ramaria-channel/tests/common/telegram.rs - mock Telegram Bot API 服务端
//!
//! 设计特点:
//! - 最小 HTTP/1.1 实现（tokio TcpListener 手写请求解析与响应）：路由
//!   `getMe` / `getUpdates` / `sendMessage` / `getFile` 与文件下载
//! - 令牌校验（路径令牌不符回 HTTP 401）；`getUpdates` 一次取走队列全部更新
//! - 探针：方法调用序列 / 已发消息 / 轮询请求体（offset 断言）/ 预置更新
//! - 响应带 `Content-Length` 与 `Connection: close`，JSON 应答体

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// =========================================================
// mock 服务端
// =========================================================

/// mock 服务端状态（探针与固定应答素材）。
struct MockState {
    token: String,
    calls: StdMutex<Vec<String>>,
    updates: StdMutex<VecDeque<Value>>,
    updates_bodies: StdMutex<Vec<Value>>,
    sent: StdMutex<Vec<Value>>,
    next_message_id: AtomicI64,
    file_path: String,
    file_bytes: Vec<u8>,
}

/// mock Telegram Bot API 服务端（一次请求一连接）。
///
/// 固定身份: Bot `id = 42` / `username = "testbot"`；`getFile` 固定返回
/// `file_path = "photos/image.png"` 与配置文件的字节数。
pub struct MockTelegramServer {
    base_url: String,
    state: Arc<MockState>,
}

impl MockTelegramServer {
    /// 启动服务端（`expected_token` 为路径令牌校验值；`file_bytes` 为下载内容）。
    pub async fn start(expected_token: &str, file_bytes: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定应成功");
        let addr = listener.local_addr().expect("地址应可读");
        let state = Arc::new(MockState {
            token: expected_token.to_string(),
            calls: StdMutex::new(Vec::new()),
            updates: StdMutex::new(VecDeque::new()),
            updates_bodies: StdMutex::new(Vec::new()),
            sent: StdMutex::new(Vec::new()),
            next_message_id: AtomicI64::new(1),
            file_path: "photos/image.png".to_string(),
            file_bytes,
        });
        let task_state = Arc::clone(&state);
        let _server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = Arc::clone(&task_state);
                tokio::spawn(async move {
                    handle_connection(stream, state).await;
                });
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            state,
        }
    }

    /// 服务端地址（指向 mock 的 `api_base_url`）。
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 已处理方法序列（getMe / getUpdates / sendMessage / getFile / file）。
    pub fn calls(&self) -> Vec<String> {
        self.state.calls.lock().expect("调用记录锁不应中毒").clone()
    }

    /// 已收到的 `sendMessage` 请求体（按到达顺序）。
    pub fn sent_messages(&self) -> Vec<Value> {
        self.state.sent.lock().expect("发送记录锁不应中毒").clone()
    }

    /// 已收到的 `getUpdates` 请求体（按到达顺序）。
    pub fn get_updates_bodies(&self) -> Vec<Value> {
        self.state
            .updates_bodies
            .lock()
            .expect("轮询记录锁不应中毒")
            .clone()
    }

    /// 预置一条待拉取的更新。
    pub fn push_update(&self, update: Value) {
        self.state
            .updates
            .lock()
            .expect("更新队列锁不应中毒")
            .push_back(update);
    }
}

/// 处理一条连接（读一个请求，按路由写响应）。
async fn handle_connection(mut stream: TcpStream, state: Arc<MockState>) {
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
        if token != state.token {
            write_json(&mut stream, 401, &unauthorized_body()).await;
            return;
        }
        state
            .calls
            .lock()
            .expect("调用记录锁不应中毒")
            .push("file".to_string());
        let bytes = state.file_bytes.clone();
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
    if token != state.token {
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
            let message_id = state.next_message_id.fetch_add(1, Ordering::SeqCst);
            let body = json!({ "ok": true, "result": { "message_id": message_id } });
            write_json(&mut stream, 200, &body).await;
        }
        "getFile" => {
            let size = state.file_bytes.len() as u64;
            let body = json!({
                "ok": true,
                "result": { "file_path": state.file_path, "file_size": size }
            });
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
