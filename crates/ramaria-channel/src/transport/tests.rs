//! crates/ramaria-channel/src/transport/tests.rs - 传输抽象模块单元测试
//!
//! 设计特点:
//! - 以最小 MockTransport 立起 `Arc<dyn Transport>` 对象形态，验证 trait 形状可用
//! - 覆盖入站事件拉取顺序与流终止（None）语义
//! - 覆盖出站消息构造辅助与发送参数透传

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::*;
use crate::status::ConnectionState;
use ramaria_core::types::InboundSender;

/// 最小传输实现（内存事件队列 + 记录出站），用于验证 trait 对象形态。
struct MockTransport {
    status: Mutex<ChannelStatus>,
    events: Mutex<VecDeque<TransportEvent>>,
    sent: Mutex<Vec<(OutboundTarget, OutboundMessage)>>,
}

impl MockTransport {
    fn new() -> Self {
        Self {
            status: Mutex::new(ChannelStatus::new("qq")),
            events: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
        }
    }

    fn push_event(&self, event: TransportEvent) {
        self.events
            .lock()
            .expect("事件队列锁不应中毒")
            .push_back(event);
    }
}

#[async_trait]
impl Transport for MockTransport {
    fn channel(&self) -> &'static str {
        "qq"
    }

    async fn connect(&self) -> ChannelResult<()> {
        let mut status = self.status.lock().expect("状态锁不应中毒");
        status.transition(ConnectionState::Online, None, 100);
        Ok(())
    }

    async fn shutdown(&self) -> ChannelResult<()> {
        let mut status = self.status.lock().expect("状态锁不应中毒");
        status.transition(ConnectionState::Disconnected, None, 200);
        Ok(())
    }

    async fn next_event(&self) -> Option<ChannelResult<TransportEvent>> {
        self.events
            .lock()
            .expect("事件队列锁不应中毒")
            .pop_front()
            .map(Ok)
    }

    async fn send(&self, target: &OutboundTarget, message: &OutboundMessage) -> ChannelResult<()> {
        self.sent
            .lock()
            .expect("出站记录锁不应中毒")
            .push((target.clone(), message.clone()));
        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.lock().expect("状态锁不应中毒").clone()
    }
}

/// 构造一条最小入站事件。
fn sample_event(external_ref: &str, should_respond: bool) -> TransportEvent {
    TransportEvent {
        external_ref: external_ref.to_string(),
        sender: InboundSender {
            platform_id: "u_1".to_string(),
            uin: Some("10001".to_string()),
            display_name: "小明".to_string(),
            group_nickname: None,
            role: None,
        },
        platform_message_id: Some("m_1".to_string()),
        reply_to: None,
        created_at: 1_700_000_000_000,
        segments: vec![InboundSegment::Text("你好".to_string())],
        should_respond,
        self_id: Some("42".to_string()),
    }
}

/// trait 对象形态：连接 / 拉事件 / 发消息 / 查状态 / 关闭全链路可调用。
#[tokio::test]
async fn transport_trait_object_full_cycle() {
    let mock = Arc::new(MockTransport::new());
    mock.push_event(sample_event("c2c:10001", true));
    let transport: Arc<dyn Transport> = mock.clone();

    assert_eq!(transport.channel(), "qq");
    assert!(!transport.status().is_online());

    transport.connect().await.expect("连接应成功");
    assert!(transport.status().is_online());

    let event = transport
        .next_event()
        .await
        .expect("应有事件")
        .expect("事件应解析成功");
    assert_eq!(event.external_ref, "c2c:10001");
    assert!(event.should_respond);
    assert!(
        transport.next_event().await.is_none(),
        "队列耗尽应返回 None"
    );

    let target = OutboundTarget {
        external_ref: "c2c:10001".to_string(),
    };
    transport
        .send(&target, &OutboundMessage::text("回复"))
        .await
        .expect("发送应成功");
    {
        let sent = mock.sent.lock().expect("出站记录锁不应中毒");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0.external_ref, "c2c:10001");
        assert_eq!(sent[0].1.text, "回复");
        assert_eq!(sent[0].1.attachment, None);
    }

    transport.shutdown().await.expect("关闭应成功");
    assert_eq!(transport.status().state, ConnectionState::Disconnected);
}

/// 出站消息构造辅助：text 无附件；附件模型可持有 URL 与字节两种形态。
#[test]
fn outbound_message_constructors() {
    let text = OutboundMessage::text("hi");
    assert_eq!(text.text, "hi");
    assert_eq!(text.attachment, None);

    let with_url = OutboundMessage {
        text: String::new(),
        attachment: Some(OutboundAttachment::Url(
            "https://example.com/a.png".to_string(),
        )),
    };
    assert!(matches!(
        with_url.attachment,
        Some(OutboundAttachment::Url(_))
    ));

    let with_bytes = OutboundMessage {
        text: "看这个".to_string(),
        attachment: Some(OutboundAttachment::Bytes {
            file_name: "a.png".to_string(),
            data: vec![1, 2, 3],
        }),
    };
    assert!(matches!(
        with_bytes.attachment,
        Some(OutboundAttachment::Bytes { .. })
    ));
}
