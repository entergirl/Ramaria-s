//! crates/ramaria-service/src/browse/channel.rs - Ramaria 通道会话概览
//!
//! 设计特点:
//! - 只读聚合：活跃会话数 + 最近活动时间，单条 SQL 以子查询完成两个聚合（避免两次往返）
//! - 连接池依赖：需 Engine 持有 SQLite 连接池句柄，未附着时返回存储层错误
//! - 空通道语义：返回 `active_sessions = 0` 且 `last_activity_ms = None`（非错误）
//! - 供宿主展示通道活动（如桌面 MCP 接入面板）

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::engine::Engine;
use crate::types::ChannelOverviewView;

// =========================================================
// 通道会话概览
// =========================================================

/// 通道会话概览读取（活跃会话数 + 最近活动时间）。
///
/// 参数:
/// - `engine`: 服务层引擎（需持有 SQLite 连接池句柄）。
/// - `channel`: 来源通道（如 `mcp` / `local`）。
///
/// 返回:
/// - [`ChannelOverviewView`]；空通道返回 `active_sessions = 0` 且 `last_activity_ms = None`。
///
/// 说明:
/// - 只读聚合查询，供宿主展示通道活动（如桌面 MCP 接入面板）；
/// - 单条 SQL 以子查询完成两个聚合，避免两次往返。
pub(crate) async fn channel_overview(
    engine: &Engine,
    channel: &str,
) -> RamariaResult<ChannelOverviewView> {
    let pool = engine
        .sqlite_pool()
        .ok_or_else(|| RamariaError::storage("统计通道概览需要 SQLite 连接池（注入构造未附着）"))?;
    let overview = ramaria_storage::repo::sessions::channel_overview(&pool, channel).await?;
    tracing::debug!(
        chain = %channel,
        active_sessions = overview.active_sessions,
        "通道会话概览读取完成"
    );
    Ok(ChannelOverviewView {
        active_sessions: overview.active_sessions,
        last_activity_ms: overview.last_activity_ms,
    })
}
