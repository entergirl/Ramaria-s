//! crates/ramaria-service/src/browse/session.rs - Ramaria 会话浏览（列表 / 消息 / 详情）
//!
//! 设计特点:
//! - 列表聚合：开始时间倒序 + 单次消息计数聚合（聚合失败记告警并按 0 处理）
//! - 消息分页：全量正序（limit 为 None）或最新在前分页后翻正，单页上限 1000
//! - 分页钳制：列表 limit 下界 1；消息偏移负数按 0 处理；has_more 仅分页路径有效
//! - 空态语义：会话不存在返回业务校验错误（入口无需预判存在性）；无消息返回空集合

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;

use chrono::DateTime;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::{
    SessionBrowsePage, SessionBrowseRequest, SessionDetailView, SessionMessageView,
    SessionMessagesRequest, SessionMessagesView, SessionSummaryView,
};

use super::view::{message_view, to_datetime};

// =========================================================
// 常量
// =========================================================

/// 单页消息条数上限（防御超大分页请求）。
const MAX_MESSAGE_PAGE: i64 = 1000;

// =========================================================
// 会话浏览（列表 / 消息 / 详情）
// =========================================================

/// 会话列表浏览（按开始时间倒序，带消息计数聚合）。
///
/// 流程:
/// 1. 读取全部会话并按开始时间倒序；
/// 2. 单次聚合各会话消息数（聚合失败时记告警并按 0 处理，不阻塞列表）；
/// 3. 应用 offset 与可选 limit（`Some(0)` 按下界 1 处理）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 浏览请求（limit / offset）。
///
/// 返回:
/// - `items`（会话摘要）与 `total`（分页前的会话数）。
pub(crate) async fn sessions(
    engine: &Engine,
    req: SessionBrowseRequest,
) -> RamariaResult<SessionBrowsePage> {
    let storage = engine.storage_ref();
    let mut sorted = storage.list_sessions().await?;
    sorted.sort_by_key(|s| Reverse(s.started_at));

    let counts = match storage.count_messages_by_session().await {
        Ok(counts) => counts,
        Err(e) => {
            tracing::warn!(error = %e, "聚合会话消息数失败，消息数按 0 处理");
            HashMap::new()
        }
    };

    let total = sorted.len();
    let offset = req.offset.unwrap_or(0) as usize;
    let take = req
        .limit
        .map(|limit| limit.max(1) as usize)
        .unwrap_or(usize::MAX);

    let items: Vec<SessionSummaryView> = sorted
        .into_iter()
        .skip(offset)
        .take(take)
        .map(|s| {
            let message_count = counts.get(&s.id).copied().unwrap_or(0);
            SessionSummaryView {
                id: s.id,
                started_at: to_datetime(s.started_at),
                ended_at: s.ended_at.and_then(DateTime::from_timestamp_millis),
                persona_uid: s.persona_uid,
                channel: s.channel,
                external_ref: s.external_ref,
                message_count,
            }
        })
        .collect();

    tracing::debug!(returned = items.len(), total, "会话列表浏览完成");
    Ok(SessionBrowsePage { items, total })
}

/// 会话消息浏览（全量正序或最新在前分页后翻正）。
///
/// 语义:
/// - `limit` 为 None: 全量加载（时间正序），`has_more` 恒为 false；
/// - `limit` 为 Some: 按最新在前分页（`created_at DESC`）取页后翻正为时间正序，
///   单页条数经 [`MAX_MESSAGE_PAGE`] 钳制，偏移负数按 0 处理；`has_more` 表示
///   是否还有更早的消息未返回。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 浏览请求（session_id / limit / offset）。
///
/// 返回:
/// - 目标会话的消息集合；会话不存在时返回 `Validation` 错误（入口无需预判存在性）。
pub(crate) async fn session_messages(
    engine: &Engine,
    req: SessionMessagesRequest,
) -> RamariaResult<SessionMessagesView> {
    let storage = engine.storage_ref();
    let sid = req.session_id;

    // 会话不存在：显式报错（先判存在性，避免对未知会话走消息查询）
    if storage.get_session(sid).await?.is_none() {
        return Err(RamariaError::validation(format!("会话不存在: {sid}")));
    }

    let (messages, total, has_more) = message_page(storage, sid, req.limit, req.offset).await?;

    tracing::debug!(
        %sid,
        returned = messages.len(),
        total,
        has_more,
        "会话消息浏览完成"
    );
    Ok(SessionMessagesView {
        session_id: sid,
        total,
        has_more,
        messages,
    })
}

/// 会话详情读取（会话元数据 + 消息页）。
///
/// 流程:
/// 1. 读取会话记录（不存在 → `Validation` 错误）；
/// 2. 读取消息页（与 [`session_messages`] 同一实现：全量正序或分页后翻正）；
/// 3. 组装元数据与消息页。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `session_id`: 目标会话。
/// - `limit` / `offset`: 消息分页（`limit` 为 None 全量加载）。
///
/// 返回:
/// - 会话详情视图；会话不存在时返回 `Validation` 错误。
pub(crate) async fn session_detail(
    engine: &Engine,
    session_id: Uuid,
    limit: Option<i64>,
    offset: Option<i64>,
) -> RamariaResult<SessionDetailView> {
    let storage = engine.storage_ref();
    let session = storage
        .get_session(session_id)
        .await?
        .ok_or_else(|| RamariaError::validation(format!("会话不存在: {session_id}")))?;

    let (messages, total, has_more) = message_page(storage, session_id, limit, offset).await?;

    tracing::debug!(
        %session_id,
        returned = messages.len(),
        total,
        has_more,
        "会话详情读取完成"
    );
    Ok(SessionDetailView {
        id: session.id,
        started_at: to_datetime(session.started_at),
        ended_at: session.ended_at.and_then(DateTime::from_timestamp_millis),
        persona_uid: session.persona_uid,
        total_messages: total,
        has_more,
        messages,
    })
}

/// 会话消息计数（诊断用；查询失败按 0 处理）。
///
/// 说明:
/// - 供入口记录"该会话有多少条消息"的诊断日志，不承载业务判定；
/// - 查询失败不报错（计数缺失不影响主流程），仅记 warn。
pub(crate) async fn count_session_messages(engine: &Engine, session_id: Uuid) -> usize {
    match engine.storage_ref().count_messages(session_id).await {
        Ok(count) => count as usize,
        Err(e) => {
            tracing::warn!(%session_id, error = %e, "会话消息计数失败，按 0 处理");
            0
        }
    }
}

/// 读取会话消息页（全量正序或最新在前分页后翻正）。
///
/// 返回:
/// - `(消息视图, total, has_more)`；`total` 为会话消息总数，`has_more` 仅分页路径有效。
async fn message_page(
    storage: &Arc<dyn StorageBackend>,
    session_id: Uuid,
    limit: Option<i64>,
    offset: Option<i64>,
) -> RamariaResult<(Vec<SessionMessageView>, u32, bool)> {
    match limit {
        None => {
            let messages = storage.list_messages(session_id).await?;
            let total = messages.len() as u32;
            Ok((messages.iter().map(message_view).collect(), total, false))
        }
        Some(limit) => {
            let limit = limit.clamp(1, MAX_MESSAGE_PAGE);
            let offset = offset.unwrap_or(0).max(0);
            let mut messages = storage
                .list_messages_paginated(session_id, limit, offset)
                .await?;
            messages.reverse();
            let total = storage.count_messages(session_id).await?;
            let has_more = (offset + limit) < i64::from(total);
            Ok((messages.iter().map(message_view).collect(), total, has_more))
        }
    }
}
