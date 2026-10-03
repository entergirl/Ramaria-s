//! crates/ramaria-service/src/proactive/sink.rs - Ramaria 主动消息投递接收端
//!
//! 设计特点:
//! - 注册制：宿主实现 [`ProactiveSink`] 并注册到引擎；未注册时调度静默丢弃（降级不阻塞）
//! - 投递结果由实现返回：失败不记选题冷却（允许下一调度窗口重试）
//! - 消息只含投递所需元数据；内容不进日志（调用方纪律）

use ramaria_core::error::RamariaResult;
use uuid::Uuid;

// =========================================================
// 投递负载与接收端契约
// =========================================================

/// 主动消息（投递负载）。
///
/// 字段约定:
/// - `message_id`: 落库消息 id（宿主幂等处理与通知点击定位依据）；
/// - `content`: 消息全文（assistant 已落库内容）；
/// - `session_id`: 消息落点会话；
/// - `persona`: 目标人格 uid；
/// - `source`: 选题来源标识（透传）；
/// - `created_at`: 投递时间（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProactiveMessage {
    /// 落库消息 id（宿主幂等处理与通知点击定位依据）。
    pub message_id: Uuid,
    pub content: String,
    pub session_id: Uuid,
    pub persona: String,
    pub source: String,
    pub created_at: i64,
}

/// 主动消息投递接收端（宿主注册制）。
///
/// 契约:
/// - `deliver` 在调度线程内同步调用，实现须快速返回（重活转移给宿主自己的事件循环）；
/// - 返回 `Err` 表示投递失败：调度不记选题冷却，允许下一窗口重试。
pub trait ProactiveSink: Send + Sync {
    /// 投递一条主动消息。
    ///
    /// 参数:
    /// - `message`: 投递负载（全文与落点元数据）。
    ///
    /// 返回:
    /// - `Ok(())`: 投递成功；
    /// - `Err`: 投递失败（调度按"未投递"处理，允许重试）。
    fn deliver(&self, message: &ProactiveMessage) -> RamariaResult<()>;
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
