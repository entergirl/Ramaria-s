//! crates/ramaria-channel/src/tests.rs - 通道错误单元测试
//!
//! 设计特点:
//! - 覆盖错误分类短名与重试判定矩阵
//! - 覆盖错误展示文案（中文，可诊断）

use super::*;

/// 分类短名逐一映射。
#[test]
fn error_kinds_map_by_variant() {
    assert_eq!(ChannelError::Closed.kind(), "closed");
    assert_eq!(ChannelError::Auth("x".to_string()).kind(), "auth");
    assert_eq!(ChannelError::Protocol("x".to_string()).kind(), "protocol");
    assert_eq!(ChannelError::Timeout("x".to_string()).kind(), "timeout");
    assert_eq!(ChannelError::Internal("x".to_string()).kind(), "internal");
}

/// 重试判定：网络 / 超时 / 协议类可重试；认证与内部错误不可。
#[test]
fn retryable_matrix() {
    assert!(ChannelError::Closed.is_retryable());
    assert!(ChannelError::Timeout("超时".to_string()).is_retryable());
    assert!(ChannelError::Protocol("服务端忙".to_string()).is_retryable());
    assert!(!ChannelError::Auth("令牌无效".to_string()).is_retryable());
    assert!(!ChannelError::Internal("未预期".to_string()).is_retryable());
}

/// 展示文案为中文且携带原因文本。
#[test]
fn display_includes_reason() {
    assert_eq!(ChannelError::Closed.to_string(), "通道连接已关闭");
    assert!(
        ChannelError::Auth("令牌无效".to_string())
            .to_string()
            .contains("认证失败")
    );
    assert!(
        ChannelError::Protocol("缺字段".to_string())
            .to_string()
            .contains("缺字段")
    );
}
