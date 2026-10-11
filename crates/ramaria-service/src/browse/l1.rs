//! crates/ramaria-service/src/browse/l1.rs - Ramaria L1 记忆摘要浏览
//!
//! 设计特点:
//! - 双口径单一实现：按 persona 直查（索引 `(persona_uid, created_at)`，不受会话规模影响）
//!   与未吸收全量口径；无 persona 时逐人格取回后合并（与 L2 合并口径一致）
//! - 排序与分页：取回后按创建时间倒序，再应用 offset / limit
//! - 上限纪律：默认 200、limit 截断上限 1000；取回条数按 offset + limit 收敛
//! - 空态语义：无摘要返回空列表（非错误）

use std::cmp::Reverse;

use ramaria_core::error::{RamariaError, RamariaResult};
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::{L1BrowsePage, L1BrowseRequest, L1MemoryView};

use super::view::{DEFAULT_BROWSE_LIMIT, MAX_BROWSE_LIMIT, l1_view, normalize_persona};

// =========================================================
// L1 记忆摘要浏览
// =========================================================

/// L1 记忆摘要浏览。
///
/// 流程（按 `unabsorbed_only` 二选一，单一实现）:
/// - `false`（直查口径）: 指定 persona 时按人格直查最近摘要（存储层 `ORDER BY created_at
///   DESC LIMIT ?`，命中 `idx_memory_l1_persona_created`）；未指定 persona 时逐人格取回
///   最近摘要后合并排序（与 L2 合并口径一致）→ 按创建时间倒序 → 应用 offset → 截断 limit；
/// - `true`（未吸收口径）: 按 persona 全量读取未吸收摘要（persona 必填）→ 应用 offset / limit。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 浏览请求（persona / 未吸收开关 / limit / offset）。
///
/// 返回:
/// - `items` 与 `total`（排序后、截断前的条数）。
///
/// 说明:
/// - 直查口径的 `total` 为取回条数：按 `offset + limit + 1` 收敛，用于判断"是否还有下一页"
///   （取回数等于上限即表示还有更多），不承担精确计数语义；
/// - 未吸收口径不设 1000 上限（与全量分页的调用口径一致）。
pub(crate) async fn l1(engine: &Engine, req: L1BrowseRequest) -> RamariaResult<L1BrowsePage> {
    let storage = engine.storage_ref();
    let persona = normalize_persona(req.persona.as_deref());
    let offset = req.offset.unwrap_or(0) as usize;

    if req.unabsorbed_only {
        let Some(uid) = persona else {
            return Err(RamariaError::validation(
                "未吸收口径（unabsorbed_only=true）必须指定 persona",
            ));
        };
        let limit = req.limit.unwrap_or(DEFAULT_BROWSE_LIMIT) as usize;
        let all = storage.list_unabsorbed_l1(&uid).await?;
        let total = all.len();
        let items: Vec<L1MemoryView> = all.iter().skip(offset).take(limit).map(l1_view).collect();
        return Ok(L1BrowsePage { items, total });
    }

    // ---- 直查口径：取回条数按 offset + limit 收敛（+1 用于判断是否还有下一页） ----
    let limit = req
        .limit
        .unwrap_or(DEFAULT_BROWSE_LIMIT)
        .min(MAX_BROWSE_LIMIT);
    let fetch = limit
        .saturating_add(req.offset.unwrap_or(0))
        .saturating_add(1);

    let mut all: Vec<L1MemoryView> = match &persona {
        Some(uid) => storage
            .list_recent_l1_by_persona(uid, fetch)
            .await?
            .iter()
            .map(l1_view)
            .collect(),
        None => {
            // 无 persona：逐人格取回最近 fetch 条后合并（避免全表排序）
            let personas = storage.list_personas().await?;
            let mut merged: Vec<L1MemoryView> = Vec::new();
            for p in &personas {
                let rows = storage.list_recent_l1_by_persona(&p.uid, fetch).await?;
                merged.extend(rows.iter().map(l1_view));
            }
            merged
        }
    };

    all.sort_by_key(|v| Reverse(v.created_at));
    let total = all.len();
    let items: Vec<L1MemoryView> = all.into_iter().skip(offset).take(limit as usize).collect();

    tracing::debug!(
        returned = items.len(),
        total,
        persona = ?persona,
        "L1 记忆浏览完成"
    );
    Ok(L1BrowsePage { items, total })
}

/// 按会话读取 L1 摘要（封存结果的核对口径）。
///
/// 语义:
/// - 返回目标会话的全部摘要（存储层顺序）；会话不存在或无摘要均返回空列表（不报错）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `session_id`: 目标会话。
///
/// 返回:
/// - L1 摘要视图列表（空列表表示该会话尚无摘要）。
pub(crate) async fn l1_by_session(
    engine: &Engine,
    session_id: Uuid,
) -> RamariaResult<Vec<L1MemoryView>> {
    let storage = engine.storage_ref();
    let list = storage.list_memory_l1(session_id).await?;
    tracing::debug!(
        %session_id,
        returned = list.len(),
        "L1 摘要按会话读取完成"
    );
    Ok(list.iter().map(l1_view).collect())
}
