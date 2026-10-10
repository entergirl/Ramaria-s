//! crates/ramaria-channel/src/events.rs - Ramaria 通道宿主事件回调模块
//!
//! 设计特点:
//! - 宿主无关：运行时经 trait 回调上报事件，桌面实现转发 Tauri 事件
//! - 同步回调：实现方自行投递到宿主事件循环，回调内不做阻塞等待
//! - 无宿主场景（测试 / CLI）使用 [`NoopSink`] 丢弃事件

use crate::status::ChannelStatus;

// =========================================================
// 事件回调
// =========================================================

/// 通道宿主事件回调。
///
/// 职责:
/// - 运行时把连接状态变更上报宿主，宿主据此更新界面状态与展示最近错误。
///
/// 实现约定:
/// - 回调为同步调用且不阻塞：实现方自行投递到宿主事件循环（如 Tauri 事件发射），
///   不在回调内做网络或磁盘等待；
/// - 回调可能来自任意线程，实现须保证自身线程安全。
pub trait ChannelEventSink: Send + Sync {
    /// 连接状态变更回调（同步、不阻塞；实现方自行投递到宿主事件循环）。
    fn on_status_changed(&self, status: &ChannelStatus);
}

/// 空事件接收端（无宿主场景：测试与 CLI）。
///
/// 说明:
/// - 回调直接丢弃；运行时可在无宿主时以本类型占位，逻辑不分支。
#[derive(Debug, Default)]
pub struct NoopSink;

impl ChannelEventSink for NoopSink {
    fn on_status_changed(&self, _status: &ChannelStatus) {}
}

#[cfg(test)]
mod tests;
