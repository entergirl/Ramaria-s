//! crates/ramaria-channel/src/transport/mod.rs - Ramaria 传输抽象模块
//!
//! 设计特点:
//! - 协议无关：运行时持有 `Transport` trait 对象，互不感知平台协议细节
//! - 拉取式事件流：`next_event` 逐条返回规范态入站事件，None 表示流正常终止
//! - 出站本版只实现文本路径；附件模型先落地不接线
//! - 入站统一用 core `InboundMessage`；传输类型不外泄协议字段

use async_trait::async_trait;
use ramaria_core::types::InboundMessage;

use crate::ChannelResult;
use crate::status::ChannelStatus;

// =========================================================
// 传输抽象
// =========================================================

/// 通道传输（单通道实例：连接维护 / 事件流消费 / 出站发送）。
///
/// 职责:
/// - 屏蔽平台协议差异：连接与鉴权、入站事件拉取、出站消息发送、连接状态查询。
///
/// 实现约定:
/// - `next_event` 为拉取式：每条事件一次调用；返回 `None` 表示事件流正常终止
///   （连接已关闭且不再产出）；错误经 `Some(Err(..))` 返回后事件流仍可继续拉取；
/// - `send` 只承载已由运行时组装好的规范出站消息，不做内容加工；
/// - `status` 返回当前连接状态快照（可随时读取，不阻塞）。
#[async_trait]
pub trait Transport: Send + Sync {
    /// 通道标识（qq / telegram）。
    fn channel(&self) -> &'static str;

    /// 建立连接并完成鉴权（幂等：已连接时直接成功）。
    async fn connect(&self) -> ChannelResult<()>;

    /// 关闭连接并释放资源（幂等：未连接时直接成功）。
    async fn shutdown(&self) -> ChannelResult<()>;

    /// 拉取下一个入站事件；None = 事件流正常终止（连接已关闭）。
    async fn next_event(&self) -> Option<ChannelResult<TransportEvent>>;

    /// 发送一条出站消息。
    async fn send(&self, target: &OutboundTarget, message: &OutboundMessage) -> ChannelResult<()>;

    /// 当前连接状态快照。
    fn status(&self) -> ChannelStatus;
}

// =========================================================
// 入站事件
// =========================================================

/// 运行时消费的入站事件（协议解析后的规范态）。
///
/// 字段约定:
/// - `external_ref`: 会话外部标识（`c2c:{uin}` 私聊 / `group:{群号}` 群聊）。
/// - `message`: core 入站消息规范模型。
/// - `should_respond`: 响应判定结果（私聊逐条响应 / 群聊仅 @ 与被回复响应）。
#[derive(Debug, Clone, PartialEq)]
pub struct TransportEvent {
    pub external_ref: String,
    pub message: InboundMessage,
    pub should_respond: bool,
}

// =========================================================
// 出站模型
// =========================================================

/// 出站目标（本传输实例对应单通道，target 只需会话标识）。
///
/// 字段约定:
/// - `external_ref`: 会话外部标识（与入站事件同一命名口径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundTarget {
    pub external_ref: String,
}

/// 出站消息（本版只实现 text 路径；附件模型先落地不接线）。
///
/// 字段约定:
/// - `text`: 文本内容（发送方负责分片前的原始文本）。
/// - `attachment`: 可选附件（预留模型；本版协议封装不接线）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundMessage {
    pub text: String,
    pub attachment: Option<OutboundAttachment>,
}

/// 出站附件形态（预留模型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundAttachment {
    /// 远端引用（平台可直接拉取的 URL）。
    Url(String),
    /// 字节内容（本地下载 / 生成的二进制）。
    Bytes { file_name: String, data: Vec<u8> },
}

impl OutboundMessage {
    /// 构造纯文本出站消息（无附件）。
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachment: None,
        }
    }
}

#[cfg(test)]
mod tests;
