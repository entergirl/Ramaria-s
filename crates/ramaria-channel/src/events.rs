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

// =========================================================
// 撤回通知回调
// =========================================================

/// 撤回通知（平台撤回事件的规范形态）。
///
/// 字段约定:
/// - `channel`: 通道标识（`qq` / `telegram`）；
/// - `external_ref`: 会话外部标识（`c2c:{uin}` 私聊 / `group:{群号}` 群聊）；
/// - `platform_message_id`: 被撤回消息的平台侧消息 ID；
/// - `operator_id`: 执行撤回的账号（平台未提供时 None）；
/// - `self_id`: 本机账号标识（平台未提供时 None）；
/// - `occurred_at`: 撤回发生时刻（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq)]
pub struct RecallNotice {
    pub channel: &'static str,
    pub external_ref: String,
    pub platform_message_id: String,
    pub operator_id: Option<String>,
    pub self_id: Option<String>,
    pub occurred_at: i64,
}

/// 撤回通知接收端（宿主注册制）。
///
/// 实现约定:
/// - 回调为同步调用且不阻塞：由读循环直接调用，实现方自行 spawn 异步处理
///   （撤回处理含生成与投递，耗时较长）；
/// - 回调可能来自任意线程，实现须保证自身线程安全。
pub trait RecallNoticeSink: Send + Sync {
    /// 撤回通知回调（同步、不阻塞；实现方自行 spawn 异步处理）。
    fn on_recall(&self, notice: &RecallNotice);
}

#[cfg(test)]
mod tests;
