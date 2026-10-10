//! crates/ramaria-channel/src/status/tests.rs - 连接状态模块单元测试
//!
//! 设计特点:
//! - 覆盖状态流转与最近错误的清空规则
//! - 覆盖初始快照与在线判定
//! - 覆盖状态序列化口径（小写）

use super::*;

/// 初始快照：Disconnected / 无错误 / 时间 0。
#[test]
fn new_status_is_disconnected_without_error() {
    let status = ChannelStatus::new("qq");
    assert_eq!(status.channel, "qq");
    assert_eq!(status.state, ConnectionState::Disconnected);
    assert_eq!(status.last_error, None);
    assert_eq!(status.updated_at, 0);
    assert!(!status.is_online());
}

/// 流转到 Error 态保留传入错误；流转到 Online 后清空。
#[test]
fn error_state_keeps_error_and_online_clears_it() {
    let mut status = ChannelStatus::new("qq");

    status.transition(ConnectionState::Error, Some("认证失败".to_string()), 100);
    assert_eq!(status.state, ConnectionState::Error);
    assert_eq!(status.last_error.as_deref(), Some("认证失败"));
    assert_eq!(status.updated_at, 100);
    assert!(!status.is_online());

    status.transition(ConnectionState::Connecting, None, 200);
    assert_eq!(status.state, ConnectionState::Connecting);
    assert_eq!(status.last_error, None, "重连尝试应清空陈旧错误");
    assert_eq!(status.updated_at, 200);

    status.transition(ConnectionState::Online, None, 300);
    assert!(status.is_online());
    assert_eq!(status.last_error, None);
}

/// 进入非错误态时即使传入错误也强制忽略（不残留误导展示）。
#[test]
fn non_error_state_ignores_provided_error() {
    let mut status = ChannelStatus::new("telegram");
    status.transition(ConnectionState::Online, Some("不应保留".to_string()), 10);
    assert_eq!(status.state, ConnectionState::Online);
    assert_eq!(status.last_error, None, "非错误态不得保留错误文本");
}

/// Error 态允许无错误文本（None）。
#[test]
fn error_state_allows_missing_error_text() {
    let mut status = ChannelStatus::new("qq");
    status.transition(ConnectionState::Error, None, 50);
    assert_eq!(status.state, ConnectionState::Error);
    assert_eq!(status.last_error, None);
}

/// 状态序列化为小写字符串；as_str 与序列化口径一致。
#[test]
fn state_serializes_lowercase() {
    for (state, expected) in [
        (ConnectionState::Disconnected, "disconnected"),
        (ConnectionState::Connecting, "connecting"),
        (ConnectionState::Online, "online"),
        (ConnectionState::Error, "error"),
    ] {
        assert_eq!(state.as_str(), expected);
        let json = serde_json::to_string(&state).expect("序列化应成功");
        assert_eq!(json, format!("\"{expected}\""));
    }
}
