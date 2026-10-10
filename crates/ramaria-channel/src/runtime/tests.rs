//! crates/ramaria-channel/src/runtime/tests.rs - 通道运行时模块单元测试
//!
//! 设计特点:
//! - 覆盖状态上报：快照更新、同通道覆盖、多通道独立
//! - 覆盖宿主回调：收到更新后的快照拷贝，且回调在状态更新之后触发
//! - 覆盖状态列表的通道名排序稳定性

use std::sync::Mutex;

use super::*;
use crate::config::ChannelRuntimeConfig;
use crate::status::ConnectionState;

/// 记录型事件接收端（回调调用序列断言用）。
#[derive(Default)]
struct RecordingSink {
    calls: Mutex<Vec<ChannelStatus>>,
}

impl ChannelEventSink for RecordingSink {
    fn on_status_changed(&self, status: &ChannelStatus) {
        self.calls
            .lock()
            .expect("回调记录锁不应中毒")
            .push(status.clone());
    }
}

fn runtime_with_sink(sink: Arc<RecordingSink>) -> ChannelRuntime {
    ChannelRuntime::new(
        ChannelRuntimeConfig {
            default_persona_uid: "rama-0001".to_string(),
            qq: None,
            telegram: None,
            proactive_enabled: false,
        },
        sink,
    )
}

/// 上报后快照更新，宿主回调收到同一快照拷贝。
#[test]
fn report_updates_snapshot_and_notifies_sink() {
    let sink = Arc::new(RecordingSink::default());
    let runtime = runtime_with_sink(Arc::clone(&sink));

    assert_eq!(runtime.status("qq"), None, "初始无状态记录");

    let mut status = ChannelStatus::new("qq");
    status.transition(ConnectionState::Online, None, 100);
    runtime.report_status(status);

    let stored = runtime.status("qq").expect("应记录 qq 状态");
    assert_eq!(stored.state, ConnectionState::Online);
    assert_eq!(stored.updated_at, 100);

    let calls = sink.calls.lock().expect("回调记录锁不应中毒");
    assert_eq!(calls.len(), 1, "每次上报应触发一次回调");
    assert_eq!(calls[0].channel, "qq");
    assert_eq!(calls[0].state, ConnectionState::Online);
}

/// 同通道重复上报覆盖旧快照（含错误清空规则）。
#[test]
fn repeated_report_overwrites_same_channel() {
    let sink = Arc::new(RecordingSink::default());
    let runtime = runtime_with_sink(sink);

    let mut failed = ChannelStatus::new("qq");
    failed.transition(ConnectionState::Error, Some("断线".to_string()), 10);
    runtime.report_status(failed);

    let mut recovered = ChannelStatus::new("qq");
    recovered.transition(ConnectionState::Connecting, None, 20);
    runtime.report_status(recovered);

    let stored = runtime.status("qq").expect("应记录 qq 状态");
    assert_eq!(stored.state, ConnectionState::Connecting);
    assert_eq!(stored.last_error, None, "覆盖后不应残留旧错误");
    assert_eq!(runtime.statuses().len(), 1, "同通道不应新增记录");
}

/// 多通道独立记录，列表按通道名升序稳定。
#[test]
fn statuses_are_isolated_and_sorted() {
    let sink = Arc::new(RecordingSink::default());
    let runtime = runtime_with_sink(sink);

    let mut telegram = ChannelStatus::new("telegram");
    telegram.transition(ConnectionState::Connecting, None, 30);
    runtime.report_status(telegram);

    let mut qq = ChannelStatus::new("qq");
    qq.transition(ConnectionState::Online, None, 20);
    runtime.report_status(qq);

    let statuses = runtime.statuses();
    let names: Vec<&str> = statuses.iter().map(|s| s.channel.as_str()).collect();
    assert_eq!(names, vec!["qq", "telegram"], "应按通道名升序");

    assert_eq!(
        runtime.status("qq").expect("qq 应存在").state,
        ConnectionState::Online
    );
    assert_eq!(
        runtime.status("telegram").expect("telegram 应存在").state,
        ConnectionState::Connecting
    );
    assert_eq!(runtime.status("unknown"), None);
}

/// 配置快照按构造值透传。
#[test]
fn config_snapshot_is_exposed() {
    let sink = Arc::new(RecordingSink::default());
    let runtime = runtime_with_sink(sink);
    assert_eq!(runtime.config().default_persona_uid, "rama-0001");
    assert!(!runtime.config().proactive_enabled);
}
