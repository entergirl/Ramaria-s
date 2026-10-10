//! crates/ramaria-channel/src/events/tests.rs - 宿主事件回调模块单元测试
//!
//! 设计特点:
//! - 覆盖 NoopSink 可构造且回调可安全调用
//! - 覆盖 trait 对象可用（Arc<dyn ChannelEventSink> 形态）

use std::sync::Arc;

use super::*;
use crate::ConnectionState;

/// NoopSink 可构造、可回调（丢弃事件不产生副作用）。
#[test]
fn noop_sink_accepts_callbacks() {
    let sink = NoopSink;
    let mut status = ChannelStatus::new("qq");
    status.transition(ConnectionState::Online, None, 1);
    sink.on_status_changed(&status);
}

/// 回调 trait 可作 trait 对象使用（宿主注入形态）。
#[test]
fn sink_is_usable_as_trait_object() {
    let sink: Arc<dyn ChannelEventSink> = Arc::new(NoopSink);
    let status = ChannelStatus::new("telegram");
    sink.on_status_changed(&status);
}
