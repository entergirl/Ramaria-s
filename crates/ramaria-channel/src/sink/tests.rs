//! crates/ramaria-channel/src/sink/tests.rs - 通道主动投递接收端单元测试
//!
//! 设计特点:
//! - 以最小 MockTransport 记录出站调用并注入失败序列，覆盖发送 / 重试 / 失败全景。
//! - 全部离线确定性：重试间隔注入为毫秒级，不依赖网络与真实平台。
//! - 断言口径：传输调用次数、送达参数、错误结果。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::status::ChannelStatus;
use crate::transport::TransportEvent;
use crate::{ChannelError, ChannelResult};
use ramaria_core::types::new_id;

// =========================================================
// mock 传输
// =========================================================

/// 记录型传输：记录调用次数与送达参数，可注入失败序列（按序消费一次）。
struct MockTransport {
    channel_name: &'static str,
    calls: AtomicUsize,
    failures: Mutex<VecDeque<ChannelError>>,
    sent: Mutex<Vec<(OutboundTarget, OutboundMessage)>>,
}

impl MockTransport {
    fn new(channel_name: &'static str) -> Self {
        Self {
            channel_name,
            calls: AtomicUsize::new(0),
            failures: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
        }
    }

    /// 预置失败序列（每次 send 先消费一个；序列耗尽后成功）。
    fn fail_with(&self, errors: Vec<ChannelError>) {
        let mut queue = self.failures.lock().expect("失败序列锁不应中毒");
        for error in errors {
            queue.push_back(error);
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }

    fn sent(&self) -> Vec<(OutboundTarget, OutboundMessage)> {
        self.sent.lock().expect("出站记录锁不应中毒").clone()
    }
}

#[async_trait]
impl Transport for MockTransport {
    fn channel(&self) -> &'static str {
        self.channel_name
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
        self.calls.fetch_add(1, Ordering::Relaxed);
        if let Some(error) = self
            .failures
            .lock()
            .expect("失败序列锁不应中毒")
            .pop_front()
        {
            return Err(error);
        }
        self.sent
            .lock()
            .expect("出站记录锁不应中毒")
            .push((target.clone(), message.clone()));
        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        ChannelStatus::new(self.channel_name)
    }
}

// =========================================================
// 辅助
// =========================================================

/// 构造一条投递负载。
fn sample_message(channel: &str, external_ref: Option<&str>) -> ProactiveMessage {
    ProactiveMessage {
        message_id: new_id(),
        content: "主动消息内容".to_string(),
        session_id: new_id(),
        persona: "rama-0001".to_string(),
        source: "event".to_string(),
        created_at: 1_700_000_000_000,
        channel: channel.to_string(),
        external_ref: external_ref.map(str::to_string),
    }
}

/// 装配以单个 mock 传输为目标的接收端（毫秒级重试间隔）。
fn sink_with(
    transport: &Arc<MockTransport>,
    max_attempts: u32,
) -> (ChannelProactiveSink, Arc<MockTransport>) {
    let mut transports: HashMap<String, Arc<dyn Transport>> = HashMap::new();
    let handle: Arc<dyn Transport> = transport.clone();
    transports.insert(transport.channel_name.to_string(), handle);
    (
        ChannelProactiveSink::with_options(transports, Duration::from_millis(10), max_attempts),
        transport.clone(),
    )
}

// =========================================================
// 用例
// =========================================================

/// 发送成功：传输收到正确外部标识与文本。
#[tokio::test]
async fn deliver_sends_to_transport() {
    let transport = Arc::new(MockTransport::new("qq"));
    let (sink, handle) = sink_with(&transport, 2);

    let message = sample_message("qq", Some("c2c:10001"));
    sink.deliver(&message).await.expect("发送应成功");

    assert_eq!(handle.calls(), 1);
    let sent = handle.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.external_ref, "c2c:10001");
    assert_eq!(sent[0].1.text, "主动消息内容");
    assert_eq!(sent[0].1.attachment, None);
}

/// 通道未装配：Err 且零发送。
#[tokio::test]
async fn deliver_missing_transport_errs() {
    let transport = Arc::new(MockTransport::new("qq"));
    let (sink, handle) = sink_with(&transport, 2);

    let message = sample_message("telegram", Some("c2c:10001"));
    let error = sink.deliver(&message).await.expect_err("未装配应报错");
    assert!(
        error.to_string().contains("通道未装配"),
        "错误应含语义: {error}"
    );
    assert_eq!(handle.calls(), 0, "未装配不应发起发送");
}

/// 缺外部标识：Err 且零发送。
#[tokio::test]
async fn deliver_missing_external_ref_errs() {
    let transport = Arc::new(MockTransport::new("qq"));
    let (sink, handle) = sink_with(&transport, 2);

    let message = sample_message("qq", None);
    let error = sink.deliver(&message).await.expect_err("缺标识应报错");
    assert!(
        error.to_string().contains("外部会话标识"),
        "错误应含语义: {error}"
    );
    assert_eq!(handle.calls(), 0);

    // 空白标识同样拒绝
    let blank = sample_message("qq", Some("   "));
    sink.deliver(&blank).await.expect_err("空白标识应报错");
    assert_eq!(handle.calls(), 0);
}

/// 空 / 空白内容：Err 且零发送。
#[tokio::test]
async fn deliver_empty_content_errs() {
    let transport = Arc::new(MockTransport::new("qq"));
    let (sink, handle) = sink_with(&transport, 2);

    for content in ["", "   "] {
        let mut message = sample_message("qq", Some("c2c:10001"));
        message.content = content.to_string();
        sink.deliver(&message).await.expect_err("空内容应报错");
    }
    assert_eq!(handle.calls(), 0);
}

/// 可重试失败：第一次可重试错误 → 第二次成功。
#[tokio::test]
async fn deliver_retries_retryable_failure() {
    let transport = Arc::new(MockTransport::new("qq"));
    transport.fail_with(vec![ChannelError::Protocol("接口忙".to_string())]);
    let (sink, handle) = sink_with(&transport, 2);

    let message = sample_message("qq", Some("c2c:10001"));
    sink.deliver(&message).await.expect("重试后应成功");

    assert_eq!(handle.calls(), 2, "应经历一次重试");
    assert_eq!(handle.sent().len(), 1, "仅成功的一次送达");
}

/// 不可重试失败（认证）：立即 Err，只调用一次。
#[tokio::test]
async fn deliver_no_retry_on_auth_error() {
    let transport = Arc::new(MockTransport::new("qq"));
    transport.fail_with(vec![
        ChannelError::Auth("凭据错误".to_string()),
        ChannelError::Auth("凭据错误".to_string()),
    ]);
    let (sink, handle) = sink_with(&transport, 3);

    let message = sample_message("qq", Some("c2c:10001"));
    let error = sink.deliver(&message).await.expect_err("认证失败应报错");
    assert!(
        error.to_string().contains("qq") && error.to_string().contains("auth"),
        "错误应含通道名与分类: {error}"
    );
    assert_eq!(handle.calls(), 1, "不可重试错误不应重试");
    assert!(handle.sent().is_empty());
}

/// 可重试错误耗尽尝试：调用次数 = max_attempts 后 Err。
#[tokio::test]
async fn deliver_exhausts_attempts() {
    let transport = Arc::new(MockTransport::new("qq"));
    transport.fail_with(vec![
        ChannelError::Closed,
        ChannelError::Closed,
        ChannelError::Closed,
    ]);
    let (sink, handle) = sink_with(&transport, 3);

    let message = sample_message("qq", Some("c2c:10001"));
    sink.deliver(&message).await.expect_err("耗尽重试应报错");
    assert_eq!(handle.calls(), 3, "调用次数应等于上限");
    assert!(handle.sent().is_empty());
}

/// 出站兜底：含 `||` 的主动消息在发送前替换为换行（吞并邻接空白）。
#[tokio::test]
async fn deliver_sanitizes_pipe_separator() {
    let transport = Arc::new(MockTransport::new("qq"));
    let (sink, handle) = sink_with(&transport, 2);

    let mut message = sample_message("qq", Some("c2c:10001"));
    message.content = "甲 || 乙||丙".to_string();
    sink.deliver(&message).await.expect("发送应成功");

    let sent = handle.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.text, "甲\n乙\n丙");
}

/// 净化后为空（仅分隔符与空白）：跳过发送且视为已完成。
#[tokio::test]
async fn deliver_skips_when_sanitized_empty() {
    let transport = Arc::new(MockTransport::new("qq"));
    let (sink, handle) = sink_with(&transport, 2);

    let mut message = sample_message("qq", Some("c2c:10001"));
    message.content = "  ||  ".to_string();
    sink.deliver(&message).await.expect("跳过发送应视为完成");
    assert_eq!(handle.calls(), 0, "净化后为空不应发起发送");
}
