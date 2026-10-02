//! crates/ramaria-llm/src/transport/tests.rs - HTTP 传输层单元测试
//!
//! 设计特点:
//! - 覆盖 SSE 行解析各分支与流内 error 载荷上抛
//! - 覆盖 http_error 状态码分类与响应体截断
//! - 覆盖 chat_stream 请求 body（思考模式禁用）与 mock SSE 服务端链路
//! - 覆盖背压不丢 delta 与首事件 / 整体分级超时

use super::*;
use futures::channel::mpsc;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StreamDelta;

// ---- parse_sse_line ----

/// parse_sse_line 各分支参数化验证：空行/注释/事件行 → None；
/// [DONE]/内容增量/finish_reason → Some(Ok(delta))；非法 JSON → Some(Err)。
#[test]
fn parse_sse_line_cases() {
    enum Expect {
        None,                  // None
        Done(&'static str),    // Some(Ok(done=true, metadata=Some(x)))
        Content(&'static str), // Some(Ok(content=x, done=false))
        Err,                   // Some(Err)
    }
    let cases = [
        ("", Expect::None),
        ("   ", Expect::None),
        (": heartbeat", Expect::None),
        (":ok", Expect::None),
        ("event: ping", Expect::None),
        ("data: [DONE]", Expect::Done("[DONE]")),
        (
            r#"data: {"choices":[{"delta":{"content":"你好"},"finish_reason":null}]}"#,
            Expect::Content("你好"),
        ),
        (
            r#"data: {"choices":[{"delta":{"content":""},"finish_reason":"stop"}]}"#,
            Expect::Done("stop"),
        ),
        ("data: {not valid json}", Expect::Err),
        ("  data: [DONE]  ", Expect::Done("[DONE]")),
        ("data:[DONE]", Expect::Done("[DONE]")),
        (
            r#"data:{"choices":[{"delta":{"content":"测试"},"finish_reason":null}]}"#,
            Expect::Content("测试"),
        ),
    ];
    for (line, expect) in cases {
        let parsed = parse_sse_line(line);
        match (parsed, expect) {
            (None, Expect::None) => {}
            (None, _) => panic!("line={line:?} 应返回 Some"),
            (Some(_), Expect::None) => panic!("line={line:?} 应返回 None"),
            (Some(result), Expect::Err) => {
                let err = result.unwrap_err();
                assert_eq!(err.category(), "llm", "line={line:?}");
                assert!(err.context().contains("SSE data 解析失败"), "line={line:?}");
            }
            (Some(result), Expect::Done(meta)) => {
                let d = result.expect("应为 Ok");
                assert!(d.done, "line={line:?} 应 done");
                assert_eq!(d.metadata.as_deref(), Some(meta), "line={line:?}");
                assert!(d.content.is_empty(), "line={line:?}");
            }
            (Some(result), Expect::Content(c)) => {
                let d = result.expect("应为 Ok");
                assert_eq!(d.content, c, "line={line:?}");
                assert!(!d.done, "line={line:?}");
                assert!(d.metadata.is_none(), "line={line:?}");
            }
        }
    }
}

/// SSE 流内 error 载荷必须上抛为 llm 错误，不能被静默吞成空 delta。
#[test]
fn parse_sse_line_surfaces_error_payload() {
    let line =
        r#"data: {"error":{"message":"rate limited","type":"rate_limit_error","code":"429"}}"#;
    let result = parse_sse_line(line).expect("error 载荷应返回 Some(Err)");
    let err = result.expect_err("error 载荷应上抛错误");
    assert_eq!(err.category(), "llm");
    assert!(err.context().contains("rate limited"), "摘要应含 message");
    assert!(err.context().contains("rate_limit_error"), "摘要应含 type");
}

/// 未知结构（无 choices 字段 / choices 为空数组）返回 None 跳过，不产生空 delta。
#[test]
fn parse_sse_line_skips_unknown_payload() {
    assert!(parse_sse_line(r#"data: {"foo":"bar"}"#).is_none());
    assert!(parse_sse_line(r#"data: {"choices":[]}"#).is_none());
}

/// summarize_stream_error 兼容 string 形态，并对无可读字段给出兜底文案。
#[test]
fn summarize_stream_error_string_and_fallback() {
    let from_string = summarize_stream_error(&serde_json::json!("boom"));
    assert_eq!(from_string, "boom");

    let fallback = summarize_stream_error(&serde_json::json!({"unexpected": 1}));
    assert_eq!(fallback, "[未知错误形态]");
}

// ---- http_error ----

/// http_error 各状态码分支参数化验证。
#[test]
fn http_error_cases() {
    let cases = [
        (401, r#"{"error":"Invalid API key"}"#, "鉴权失败"),
        (429, "Too many requests", "频率超限"),
        (500, "Internal error", "服务端错误"),
        (400, r#"{"error":"model not found"}"#, "请求错误"),
    ];
    for (status, body, keyword) in cases {
        let err = http_error(status, body);
        assert_eq!(err.category(), "llm");
        assert!(
            err.context().contains(keyword),
            "status={status} 应包含 {keyword}"
        );
        assert!(
            err.context().contains(&status.to_string()),
            "status={status}"
        );
    }
    // 长 body 应截断
    let long_body = "x".repeat(1000);
    let err = http_error(422, &long_body);
    assert!(err.context().len() < 700); // 500 + 前缀长度
}

// ---- chat_stream 请求 body 构造（思考模式禁用）----

/// 启动本地 mock SSE server：捕获 POST `/chat/completions` 的请求 body，
/// 返回一段固定 SSE 流。返回 `(base_url, 捕获的请求 body)`。
async fn spawn_mock_sse_server() -> (
    String,
    std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定 mock 端口应成功");
    let addr = listener.local_addr().expect("获取 mock 地址应成功");
    let captured: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured_clone = std::sync::Arc::clone(&captured);

    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut socket, _) = listener.accept().await.expect("mock 应收到连接");

        // 读取完整请求：先读到头部结束（\r\n\r\n），再按 Content-Length 补齐 body
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        let header_end = loop {
            let n = socket.read(&mut tmp).await.expect("mock 读请求头应成功");
            if n == 0 {
                break None;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break Some(pos + 4);
            }
        };
        let header_end = header_end.expect("mock 应收到请求头");
        let head = String::from_utf8_lossy(&buf[..header_end]);
        let content_len: usize = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().parse().ok()))
            .flatten()
            .unwrap_or(0);
        while buf.len() < header_end + content_len {
            let n = socket.read(&mut tmp).await.expect("mock 读 body 应成功");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }

        let body_str = String::from_utf8_lossy(&buf[header_end..header_end + content_len]);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body_str) {
            *captured_clone.lock().expect("mock 捕获锁应可用") = Some(v);
        }

        // 返回固定 SSE 流（一个内容增量 + [DONE]）
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n"
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            sse.len(),
            sse
        );
        socket
            .write_all(resp.as_bytes())
            .await
            .expect("mock 写响应应成功");
        let _ = socket.shutdown().await;
    });

    (format!("http://{}", addr), captured)
}

/// chat_stream 请求 body 必须含 `thinking.type == "disabled"`。
///
/// 背景:
/// - 思考模式下 temperature/top_p 等采样参数不生效（官方文档 Input and
///   Output Parameters："设置不报错但不生效"），输出由思考主导、不可复现；
///   chat 非流式已修复，chat_stream 未同步导致对话链路同参数不可复现。
/// - 通过 mock server 捕获真实发送的请求 body，断言字段存在且取值正确。
#[tokio::test]
async fn chat_stream_body_disables_thinking() {
    use futures::StreamExt;

    let (base_url, captured) = spawn_mock_sse_server().await;
    let transport =
        OpenAiTransport::new(base_url, Some("test-key".into()), 5).expect("构造 transport 应成功");

    let messages = vec![serde_json::json!({"role": "user", "content": "你好"})];
    let mut stream = transport
        .chat_stream(&messages, "deepseek-chat", 0.0, 512)
        .await
        .expect("chat_stream 应成功");

    // 消费流直到 [DONE]，验证链路完整可用
    let mut received = String::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(d) if d.done => break,
            Ok(d) => received.push_str(&d.content),
            Err(e) => panic!("流内错误: {e}"),
        }
    }
    assert_eq!(received, "你好", "应收到 mock 返回的流内容");

    // 断言请求 body：思考模式必须禁用，其余字段正确透传
    let body = captured
        .lock()
        .expect("捕获锁应可用")
        .clone()
        .expect("应捕获到请求 body");
    assert_eq!(
        body["thinking"]["type"], "disabled",
        "流式请求必须禁用思考模式（temperature 才能生效）"
    );
    assert_eq!(body["stream"], true, "stream 应为 true");
    assert_eq!(body["model"], "deepseek-chat", "model 应正确");
    assert_eq!(body["temperature"], 0.0, "temperature 应透传");
    assert_eq!(body["max_tokens"], 512, "max_tokens 应透传");
    assert_eq!(body["messages"][0]["role"], "user", "messages 应透传");
}

// =========================================================
// 背压与分级超时（决策 D-V17-013 / 备忘 §二 15）
// =========================================================

/// channel 满时不丢 delta：70 个事件（> 默认容量 64）经小容量 channel 全部送达。
///
/// 回归红线 4：流式修复后长回复不截断、无静默丢 delta。
#[tokio::test]
async fn channel_full_backpressure_no_delta_lost() {
    use futures::StreamExt;
    use futures::stream;

    // 构造 70 个增量事件（超过默认 channel 容量 64，触发满/背压）
    let events: Vec<String> = (0..70)
        .map(|i| {
            format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"块{i}\"}},\"finish_reason\":null}}]}}\n\n"
            )
        })
        .collect();
    let byte_stream = stream::iter(events.into_iter().map(|e| Ok(bytes::Bytes::from(e))));

    // 小容量 channel（2）强制触发满 → 背压等待消费
    let (tx, mut rx) = mpsc::channel::<RamariaResult<StreamDelta>>(2);

    // 消费者并行运行，消费完内容后记录
    let consumer = tokio::spawn(async move {
        let mut received = String::new();
        let mut count = 0usize;
        while let Some(item) = rx.next().await {
            match item {
                Ok(d) if d.done => break,
                Ok(d) => {
                    received.push_str(&d.content);
                    count += 1;
                }
                Err(e) => panic!("流内错误: {e}"),
            }
            // 模拟慢消费（让出执行权），确保背压路径被触发
            tokio::task::yield_now().await;
        }
        (count, received)
    });

    sse_read_loop_inner(
        byte_stream,
        tx,
        Some("test-rid".to_string()),
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(30),
    )
    .await;

    let (count, received) = consumer.await.expect("消费者应完成");
    assert_eq!(
        count, 70,
        "所有 delta 都应送达（背压不丢内容），实际 {count}"
    );
    assert!(received.contains("块0"), "应收到首个增量");
    assert!(received.contains("块69"), "应收到末个增量");
}

/// 首事件超时：服务端 60s（测试注入 50ms）内无任何数据 → 报错退出。
///
/// 分级超时之首包快速失败：避免服务端挂起时长时间无反馈。
#[tokio::test]
async fn first_event_timeout_sends_error() {
    use futures::StreamExt;
    use futures::stream;

    // 永远 pending 的 stream：模拟服务端接受连接后不发送任何数据（挂起）
    let byte_stream = stream::pending::<Result<bytes::Bytes, reqwest::Error>>();
    let (tx, mut rx) = mpsc::channel::<RamariaResult<StreamDelta>>(4);

    sse_read_loop_inner(
        byte_stream,
        tx,
        Some("test-rid".to_string()),
        std::time::Duration::from_millis(50),
        std::time::Duration::from_secs(10),
    )
    .await;

    let item = rx.next().await.expect("应收到错误事件");
    let err = item.expect_err("应为错误事件");
    assert!(
        err.context().contains("首事件超时"),
        "错误信息应含首事件超时，got: {}",
        err.context()
    );
}

/// 整体超时：流永不结束（服务端持续发送但无 [DONE]）→ 整体超时兜底报错。
///
/// 分级超时之整体兜底：长流只受整体超时约束，不因首包已到而无限等待。
#[tokio::test]
async fn stream_overall_timeout_sends_error() {
    use futures::StreamExt;

    // 无限发送 SSE 注释行（`: 心跳`）的 stream：服务端持续有数据但不产生事件、
    // 也永不 [DONE]——验证整体超时兜底。unfold 每 1ms 生成一行，流永不结束；
    // boxed() 使 !Unpin 的 unfold 流满足 sse_read_loop_inner 的 Unpin 约束。
    let byte_stream = futures::stream::unfold(0u64, |i| async move {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        Some((Ok(bytes::Bytes::from(": 心跳\n\n")), i + 1))
    })
    .boxed();
    let (tx, mut rx) = mpsc::channel::<RamariaResult<StreamDelta>>(8);

    // 消费者并行消费，记录是否收到整体超时错误
    let consumer = tokio::spawn(async move {
        let mut saw_overall_timeout = false;
        while let Some(item) = rx.next().await {
            match item {
                Ok(d) if d.done => break,
                Ok(_) => {}
                Err(e) => {
                    saw_overall_timeout = e.context().contains("流式读取超时");
                    break;
                }
            }
        }
        saw_overall_timeout
    });

    sse_read_loop_inner(
        byte_stream,
        tx,
        Some("test-rid".to_string()),
        std::time::Duration::from_millis(20),
        std::time::Duration::from_millis(100),
    )
    .await;

    let saw_overall_timeout = consumer.await.expect("消费者应完成");
    assert!(
        saw_overall_timeout,
        "整体超时应发送错误事件（服务端持续发送但无 [DONE]）"
    );
}
