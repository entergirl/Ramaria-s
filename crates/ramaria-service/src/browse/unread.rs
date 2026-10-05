//! crates/ramaria-service/src/browse/unread.rs - Ramaria 会话未读标记与汇总
//!
//! 设计特点:
//! - 标记已读：按会话推进已读时间到当前（幂等；会话不存在也视为成功）
//! - 未读汇总：单次聚合全部会话未读数求和（按 u32 饱和），供托盘徽标与全局未读提示
//! - 时间由用例层统一取当前时间，存储层只负责写入

use ramaria_core::error::RamariaResult;
use uuid::Uuid;

use crate::engine::Engine;

// =========================================================
// 会话未读标记与汇总
// =========================================================

/// 标记会话已读（推进该会话的已读时间到当前）。
///
/// 说明:
/// - 幂等：会话不存在或重复标记均成功；
/// - 已读后该会话中早于当前时间的本地助手消息不再计入未读。
pub(crate) async fn mark_session_read(engine: &Engine, session_id: Uuid) -> RamariaResult<()> {
    let at_ms = ramaria_core::types::now_ms();
    engine
        .storage_ref()
        .mark_session_read(session_id, at_ms)
        .await
}

/// 全部会话的未读消息总数（托盘徽标与全局未读提示口径）。
///
/// 返回:
/// - 未读总数；求和结果按 u32 饱和（实际量级远低于上限）。
pub(crate) async fn unread_total(engine: &Engine) -> RamariaResult<u32> {
    let counts = engine.storage_ref().list_unread_counts().await?;
    let total: u64 = counts.values().map(|&count| u64::from(count)).sum();
    Ok(total.min(u64::from(u32::MAX)) as u32)
}
