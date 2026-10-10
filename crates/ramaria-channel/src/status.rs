//! crates/ramaria-channel/src/status.rs - Ramaria 通道连接状态模块
//!
//! 设计特点:
//! - 连接状态机四态：断开 / 连接中 / 在线 / 错误；状态快照供宿主展示
//! - 进入非错误态强制清空最近错误（避免陈旧错误残留误导展示）
//! - 时间统一 Unix 毫秒；状态流转由调用方提供时间戳（便于测试与批量对齐）

use serde::{Deserialize, Serialize};

// =========================================================
// 连接状态
// =========================================================

/// 通道连接状态。
///
/// 状态流转:
/// - `Disconnected` → `Connecting` → `Online`（连接成功）；
/// - 任一态 → `Error`（连接失败 / 断线 / 协议异常，可携带最近错误）；
/// - 重连尝试回到 `Connecting`，成功后回 `Online`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Online,
    Error,
}

impl ConnectionState {
    /// 返回序列化口径的小写字符串（disconnected / connecting / online / error）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Online => "online",
            Self::Error => "error",
        }
    }
}

// =========================================================
// 状态快照
// =========================================================

/// 通道状态快照。
///
/// 职责:
/// - 承载单通道的当前连接状态、最近错误与更新时间，供宿主展示与事件上报。
///
/// 字段约定:
/// - `channel`: 通道标识（`qq` / `telegram`）。
/// - `state`: 当前连接状态。
/// - `last_error`: 最近错误（脱敏后文本）；仅 `Error` 态有值，非错误态恒为空。
/// - `updated_at`: 最近一次流转时间（Unix 毫秒；初始为 0）。
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelStatus {
    pub channel: String,
    pub state: ConnectionState,
    pub last_error: Option<String>,
    pub updated_at: i64,
}

impl ChannelStatus {
    /// 创建初始状态快照（`Disconnected` / 无错误 / 时间 0）。
    pub fn new(channel: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            state: ConnectionState::Disconnected,
            last_error: None,
            updated_at: 0,
        }
    }

    /// 状态流转：更新状态、最近错误与时间戳。
    ///
    /// 规则:
    /// - 进入非 `Error` 态强制清空 `last_error`（传入值被忽略并记 debug 日志）；
    /// - 进入 `Error` 态保留传入的最近错误（None 表示未携带错误文本）。
    ///
    /// 参数:
    /// - `state`: 目标状态。
    /// - `last_error`: 最近错误文本（仅 `Error` 态采纳）。
    /// - `updated_at`: 流转时间（Unix 毫秒）。
    pub fn transition(
        &mut self,
        state: ConnectionState,
        last_error: Option<String>,
        updated_at: i64,
    ) {
        let last_error = match state {
            ConnectionState::Error => last_error,
            _ => {
                if last_error.is_some() {
                    tracing::debug!(
                        channel = %self.channel,
                        "通道进入非错误状态，忽略携带的错误信息"
                    );
                }
                None
            }
        };
        self.state = state;
        self.last_error = last_error;
        self.updated_at = updated_at;
    }

    /// 是否在线。
    pub fn is_online(&self) -> bool {
        self.state == ConnectionState::Online
    }
}

#[cfg(test)]
mod tests;
