//! crates/ramaria-channel/src/sink/tests.rs - 通道主动投递接收端单元测试
//!
//! 设计特点:
//! - 以最小 MockTransport 记录出站调用并注入失败序列，覆盖发送 / 重试 / 分段 / 失败全景。
//! - 全部离线确定性：重试间隔注入为毫秒级，不依赖网络与真实平台。
//! - 断言口径：传输调用次数、送达参数、错误结果。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// 记录型传输：记录调用次数与送达参数，可注入失败序列（按序消费一次；
/// `None` 槽位表示该次调用放行成功）。
struct MockTransport {
    channel_name: &'static str,
    calls: AtomicUsize,
    failures: Mutex<VecDeque<Option<ChannelError>>>,
    sent: Mutex<Vec<(OutboundTarget, OutboundMessage)>>,
    supports_attachment: AtomicBool,
}

impl MockTransport {
    fn new(channel_name: &'static str) -> Self {
        Self {
            channel_name,
            calls: AtomicUsize::new(0),
            failures: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
            supports_attachment: AtomicBool::new(true),
        }
    }

    /// 声明出站附件能力（默认支持，与 OneBot 口径一致）。
    fn set_supports_attachment(&self, supported: bool) {
        self.supports_attachment.store(supported, Ordering::Relaxed);
    }

    /// 预置失败序列（每次 send 先消费一个；序列耗尽后成功）。
    fn fail_with(&self, errors: Vec<ChannelError>) {
        let mut queue = self.failures.lock().expect("失败序列锁不应中毒");
        for error in errors {
            queue.push_back(Some(error));
        }
    }

    /// 放行前 `successes` 次发送后，再按错误序列生效。
    fn fail_after(&self, successes: usize, errors: Vec<ChannelError>) {
        let mut queue = self.failures.lock().expect("失败序列锁不应中毒");
        for _ in 0..successes {
            queue.push_back(None);
        }
        for error in errors {
            queue.push_back(Some(error));
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

    fn supports_outbound_attachment(&self) -> bool {
        self.supports_attachment.load(Ordering::Relaxed)
    }

    async fn send(&self, target: &OutboundTarget, message: &OutboundMessage) -> ChannelResult<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if let Some(Some(error)) = self
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

/// 构造单通道传输映射。
fn transports_with(transport: &Arc<MockTransport>) -> HashMap<String, Arc<dyn Transport>> {
    let mut transports: HashMap<String, Arc<dyn Transport>> = HashMap::new();
    let handle: Arc<dyn Transport> = transport.clone();
    transports.insert(transport.channel_name.to_string(), handle);
    transports
}

/// 装配以单个 mock 传输为目标的接收端（毫秒级重试间隔；分段取默认值）。
fn sink_with(
    transport: &Arc<MockTransport>,
    max_attempts: u32,
) -> (ChannelProactiveSink, Arc<MockTransport>) {
    (
        ChannelProactiveSink::with_options(
            transports_with(transport),
            Duration::from_millis(10),
            max_attempts,
        ),
        transport.clone(),
    )
}

/// 装配分段参数注入的接收端（毫秒级重试与段间间隔）。
fn sink_with_segments(
    transport: &Arc<MockTransport>,
    max_attempts: u32,
    segment_interval: Duration,
    segment_max: usize,
) -> (ChannelProactiveSink, Arc<MockTransport>) {
    (
        ChannelProactiveSink::with_segment_options(
            transports_with(transport),
            Duration::from_millis(10),
            max_attempts,
            segment_interval,
            segment_max,
        ),
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

/// 出站拆条：含 `||` 的主动消息切段分次发出（吞并邻接空白）。
#[tokio::test]
async fn deliver_splits_pipe_segments() {
    let transport = Arc::new(MockTransport::new("qq"));
    let (sink, handle) = sink_with_segments(&transport, 2, Duration::from_millis(10), 5);

    let mut message = sample_message("qq", Some("c2c:10001"));
    message.content = "甲 || 乙||丙".to_string();
    sink.deliver(&message).await.expect("发送应成功");

    assert_eq!(handle.calls(), 3, "每段一次发送");
    let texts: Vec<String> = handle
        .sent()
        .into_iter()
        .map(|(_, outbound)| outbound.text)
        .collect();
    assert_eq!(texts, vec!["甲", "乙", "丙"]);
}

/// 分段失败语义：第 2 段不可重试失败 → Err，剩余段不再发送（已发送段不回滚）。
#[tokio::test]
async fn deliver_failure_after_partial_send_stops_remaining() {
    let transport = Arc::new(MockTransport::new("qq"));
    transport.fail_after(1, vec![ChannelError::Auth("凭据错误".to_string())]);
    let (sink, handle) = sink_with_segments(&transport, 2, Duration::from_millis(10), 5);

    let mut message = sample_message("qq", Some("c2c:10001"));
    message.content = "甲||乙||丙".to_string();
    let error = sink.deliver(&message).await.expect_err("分段失败应报错");
    assert!(
        error.to_string().contains("发送失败"),
        "错误应含发送失败语义: {error}"
    );
    assert_eq!(handle.calls(), 2, "第 2 段失败后不应继续尝试第 3 段");
    let texts: Vec<String> = handle
        .sent()
        .into_iter()
        .map(|(_, outbound)| outbound.text)
        .collect();
    assert_eq!(texts, vec!["甲"], "仅第 1 段已送达");
}

/// 默认构造：分段参数与 worker 出站口径一致（800ms × 5 段）。
#[test]
fn default_constructors_use_worker_segment_options() {
    let transport = Arc::new(MockTransport::new("qq"));

    let sink = ChannelProactiveSink::new(transports_with(&transport));
    assert_eq!(sink.segment_interval, Duration::from_millis(800));
    assert_eq!(sink.segment_max, 5);

    let sink = ChannelProactiveSink::with_options(
        transports_with(&transport),
        Duration::from_millis(10),
        2,
    );
    assert_eq!(sink.segment_interval, Duration::from_millis(800));
    assert_eq!(sink.segment_max, 5);
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

// =========================================================
// 表情指令投递
// =========================================================

/// 表情解析器桩：固定命中 / 未命中，记录调用次数。
struct MockStickerResolver {
    sticker: Option<OutboundSticker>,
    calls: AtomicUsize,
}

impl MockStickerResolver {
    fn hitting(file_name: &str, bytes: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            sticker: Some(OutboundSticker {
                file_name: file_name.to_string(),
                bytes: bytes.to_vec(),
            }),
            calls: AtomicUsize::new(0),
        })
    }

    fn missing() -> Arc<Self> {
        Arc::new(Self {
            sticker: None,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

#[async_trait]
impl StickerResolver for MockStickerResolver {
    async fn resolve(&self, _keyword: &str) -> Option<OutboundSticker> {
        self.calls.fetch_add(1, Ordering::Acquire);
        self.sticker.clone()
    }
}

/// 装配注入表情解析器的接收端（毫秒级重试；分段取默认值）。
fn sink_with_resolver(
    transport: &Arc<MockTransport>,
    resolver: &Arc<MockStickerResolver>,
) -> (ChannelProactiveSink, Arc<MockTransport>) {
    let handle: Arc<dyn StickerResolver> = resolver.clone();
    let sink = ChannelProactiveSink::with_options(
        transports_with(transport),
        Duration::from_millis(10),
        2,
    )
    .with_sticker_resolver(handle);
    (sink, transport.clone())
}

/// 命中：标记剥离，首段携带附件，其余段纯文本。
#[tokio::test]
async fn deliver_sticker_hit_attaches_first_segment() {
    let transport = Arc::new(MockTransport::new("qq"));
    let resolver = MockStickerResolver::hitting("cat.png", b"PNGDATA");
    let (sink, handle) = sink_with_resolver(&transport, &resolver);

    let mut message = sample_message("qq", Some("c2c:10001"));
    message.content = "送你[表情:猫]||到家".to_string();
    sink.deliver(&message).await.expect("发送应成功");

    assert_eq!(resolver.calls(), 1, "有关键词应调用一次解析");
    let sent = handle.sent();
    assert_eq!(sent.len(), 2, "拆条两段");
    assert_eq!(sent[0].1.text, "送你");
    assert_eq!(
        sent[0].1.attachment,
        Some(crate::transport::OutboundAttachment::Bytes {
            file_name: "cat.png".to_string(),
            data: b"PNGDATA".to_vec(),
        }),
        "首段应携带表情附件"
    );
    assert_eq!(sent[1].1.text, "到家");
    assert_eq!(sent[1].1.attachment, None);
}

/// 命中且剥离后文本为空：只发一条空文本带附件的消息。
#[tokio::test]
async fn deliver_sticker_only_sends_image_message() {
    let transport = Arc::new(MockTransport::new("qq"));
    let resolver = MockStickerResolver::hitting("cat.png", b"PNGDATA");
    let (sink, handle) = sink_with_resolver(&transport, &resolver);

    let mut message = sample_message("qq", Some("c2c:10001"));
    message.content = "[表情:猫]".to_string();
    sink.deliver(&message).await.expect("发送应成功");

    let sent = handle.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.text, "");
    assert!(sent[0].1.attachment.is_some(), "仅表情应携带附件");
}

/// 未注入解析器：标记剥离后按纯文字发送。
#[tokio::test]
async fn deliver_sticker_without_resolver_sends_text_only() {
    let transport = Arc::new(MockTransport::new("qq"));
    let (sink, handle) = sink_with(&transport, 2);

    let mut message = sample_message("qq", Some("c2c:10001"));
    message.content = "送你[表情:猫]到家".to_string();
    sink.deliver(&message).await.expect("发送应成功");

    let sent = handle.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.text, "送你到家");
    assert_eq!(sent[0].1.attachment, None);
}

/// 解析器未命中：降级纯文字发送。
#[tokio::test]
async fn deliver_sticker_missing_falls_back_to_text() {
    let transport = Arc::new(MockTransport::new("qq"));
    let resolver = MockStickerResolver::missing();
    let (sink, handle) = sink_with_resolver(&transport, &resolver);

    let mut message = sample_message("qq", Some("c2c:10001"));
    message.content = "送你[表情:猫]到家".to_string();
    sink.deliver(&message).await.expect("发送应成功");

    assert_eq!(resolver.calls(), 1);
    let sent = handle.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.text, "送你到家");
    assert_eq!(sent[0].1.attachment, None);
}

/// 仅表情命中且传输不支持纯附件：退化为表情占位文本（不重试、不带附件）。
#[tokio::test]
async fn deliver_sticker_only_falls_back_to_text_when_unsupported() {
    let transport = Arc::new(MockTransport::new("telegram"));
    transport.set_supports_attachment(false);
    let resolver = MockStickerResolver::hitting("cat.png", b"PNGDATA");
    let (sink, handle) = sink_with_resolver(&transport, &resolver);

    let mut message = sample_message("telegram", Some("c2c:10001"));
    message.content = "[表情:猫]".to_string();
    sink.deliver(&message).await.expect("退化文本发送应成功");

    let sent = handle.sent();
    assert_eq!(sent.len(), 1, "占位文本应一次发出");
    assert_eq!(sent[0].1.text, "[表情:猫]");
    assert_eq!(sent[0].1.attachment, None, "退化文本不应携带附件");
}

/// 仅表情且未命中：跳过发送且视为已完成（现有空文本口径）。
#[tokio::test]
async fn deliver_sticker_only_miss_skips() {
    let transport = Arc::new(MockTransport::new("qq"));
    let resolver = MockStickerResolver::missing();
    let (sink, handle) = sink_with_resolver(&transport, &resolver);

    let mut message = sample_message("qq", Some("c2c:10001"));
    message.content = "[表情:猫]".to_string();
    sink.deliver(&message).await.expect("跳过发送应视为完成");
    assert_eq!(handle.calls(), 0, "未命中且无文本不应发起发送");
}
