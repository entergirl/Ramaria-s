//! crates/ramaria-channel/src/lib.rs - Ramaria 社交通道门面
//!
//! 设计特点:
//! - 协议壳定位：只做平台协议翻译与运行时编排；业务（会话解析 / 记忆写入 / 回复生成）一律经 `ramaria-service`
//! - 依赖纪律：只依赖 `ramaria-core` 与 `ramaria-service`；不直连 storage / keychain（凭据由宿主经 service 注入）
//! - 宿主无关：运行时不含 Tauri / CLI 概念，状态经回调 trait 上报宿主
//! - 通道错误自成一域（[`ChannelError`]）：连接 / 认证 / 协议 / 超时 / 内部五类，携带可重试判定
//!
//! 模块划分:
//! - `config`：运行期装配参数（由 `[channel]` 配置 + 凭据组装）；
//! - `status`：连接状态机（快照与流转规则）；
//! - `events`：宿主事件回调 trait（桌面转发 Tauri 事件）；
//! - `transport`：传输抽象（收事件 / 发出站 / 连接维护 / 状态查询）；
//! - `runtime`：宿主无关运行时（状态簿与状态上报）。

pub mod config;
pub mod events;
pub mod runtime;
pub mod status;
pub mod transport;

pub use config::{ChannelRuntimeConfig, QqRuntimeConfig, TelegramRuntimeConfig};
pub use events::{ChannelEventSink, NoopSink};
pub use runtime::ChannelRuntime;
pub use runtime::assemble::{AssembledInbound, DownloadedImage, assemble_inbound};
pub use runtime::download::{HttpImageDownloader, ImageDownloader, MAX_IMAGE_BYTES};
pub use runtime::worker::{ChannelWorker, WorkerOptions};
pub use status::{ChannelStatus, ConnectionState};
pub use transport::onebot::{OneBotTransport, QqTransportOptions, SentMessageCache};
pub use transport::{
    InboundSegment, OutboundAttachment, OutboundMessage, OutboundTarget, Transport, TransportEvent,
};

// =========================================================
// 通道错误
// =========================================================

/// 通道操作错误。
///
/// 职责:
/// - 承载通道层（连接 / 认证 / 协议 / 超时 / 内部）的可恢复失败分类；
/// - 提供重试判定（[`ChannelError::is_retryable`]）与日志短名（[`ChannelError::kind`]）。
///
/// 说明:
/// - 本错误域与 `RamariaError` 分离：通道失败语义（如风控拒绝）不等价于存储或
///   校验错误，由运行时按分类决定退避与上报口径。
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("通道连接已关闭")]
    Closed,
    #[error("通道认证失败: {0}")]
    Auth(String),
    #[error("协议错误: {0}")]
    Protocol(String),
    #[error("请求超时: {0}")]
    Timeout(String),
    #[error("内部错误: {0}")]
    Internal(String),
}

impl ChannelError {
    /// 错误分类短名（日志与状态展示用）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Auth(_) => "auth",
            Self::Protocol(_) => "protocol",
            Self::Timeout(_) => "timeout",
            Self::Internal(_) => "internal",
        }
    }

    /// 是否可重试。
    ///
    /// 判定口径:
    /// - `Closed` / `Timeout`: 可重试（网络类，重连或重发即可恢复）；
    /// - `Protocol`: 可重试——协议层错误多为服务端瞬时状态（如接口忙），
    ///   由调用方按退避策略重试；重复失败会自然耗尽重试预算；
    /// - `Auth` / `Internal`: 不可重试——凭据错误重试无意义，内部错误属实现缺陷。
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Closed | Self::Timeout(_) | Self::Protocol(_) => true,
            Self::Auth(_) | Self::Internal(_) => false,
        }
    }
}

/// 通道操作结果。
pub type ChannelResult<T> = Result<T, ChannelError>;

#[cfg(test)]
mod tests;
