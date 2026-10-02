//! crates/ramaria-service/src/browse/l2_l3.rs - Ramaria L2 事件与 L3 性格标签浏览
//!
//! 设计特点:
//! - L2 双口径：persona 过滤分页（total 取全量计数）与全人格合并后统一倒序截断
//! - L3 双口径：persona 过滤与全人格合并（无数据时为空列表，不报错）
//! - 排序稳定：合并口径按创建时间倒序后截断，limit 经上限钳制
//! - 只读：不修改任何状态

use std::cmp::Reverse;

use ramaria_core::error::RamariaResult;
use ramaria_core::types::MemoryEvent;

use crate::engine::Engine;
use crate::types::{L2BrowsePage, L2BrowseRequest, L2EventView, L3TraitView};

use super::view::{DEFAULT_BROWSE_LIMIT, MAX_BROWSE_LIMIT, l2_view, l3_view, normalize_persona};

// =========================================================
// L2 事件浏览
// =========================================================

/// L2 事件浏览。
///
/// 流程:
/// - persona 有值: 按分页参数从存储层取事件（`start` 倒序），`total` 取该 persona 的全量计数；
/// - persona 无值: 逐个 persona 取回最近 limit 条事件后合并，按创建时间倒序并截断 limit，
///   `total` 为合并后的条数。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 浏览请求（persona / limit / offset）。
///
/// 返回:
/// - `items` 与 `total`（分页前的条数）。
pub(crate) async fn l2(engine: &Engine, req: L2BrowseRequest) -> RamariaResult<L2BrowsePage> {
    let storage = engine.storage_ref();
    let persona = normalize_persona(req.persona.as_deref());
    let limit = req
        .limit
        .unwrap_or(DEFAULT_BROWSE_LIMIT)
        .min(MAX_BROWSE_LIMIT);
    let offset = req.offset.unwrap_or(0);

    let Some(uid) = persona else {
        // 合并口径：逐 persona 取回最近 limit 条后统一排序截断
        let personas = storage.list_personas().await?;
        let mut all: Vec<MemoryEvent> = Vec::new();
        for p in &personas {
            let mut events = storage
                .list_events_by_persona(&p.uid, 0, i64::from(limit))
                .await?;
            all.append(&mut events);
        }
        all.sort_by_key(|e| Reverse(e.created_at));
        let total = all.len();
        all.truncate(limit as usize);
        let items: Vec<L2EventView> = all.iter().map(l2_view).collect();
        tracing::debug!(
            returned = total.min(limit as usize),
            total,
            "L2 事件浏览完成（合并口径）"
        );
        return Ok(L2BrowsePage { items, total });
    };

    let events = storage
        .list_events_by_persona(&uid, i64::from(offset), i64::from(limit))
        .await?;
    let total = storage.count_events_by_persona(&uid).await? as usize;
    let items: Vec<L2EventView> = events.iter().map(l2_view).collect();

    tracing::debug!(returned = events.len(), total, "L2 事件浏览完成");
    Ok(L2BrowsePage { items, total })
}

// =========================================================
// L3 性格标签浏览
// =========================================================

/// L3 性格标签浏览（扁平列表）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona`: 目标人格 uid（None = 合并全部人格的标签）。
///
/// 返回:
/// - 性格标签视图列表（无数据时为空列表，不报错）。
pub(crate) async fn l3(engine: &Engine, persona: Option<&str>) -> RamariaResult<Vec<L3TraitView>> {
    let storage = engine.storage_ref();
    match normalize_persona(persona) {
        Some(uid) => {
            let traits = storage.list_traits_by_persona(&uid).await?;
            Ok(traits.iter().map(l3_view).collect())
        }
        None => {
            let personas = storage.list_personas().await?;
            let mut all: Vec<L3TraitView> = Vec::new();
            for p in &personas {
                let traits = storage.list_traits_by_persona(&p.uid).await?;
                all.extend(traits.iter().map(l3_view));
            }
            Ok(all)
        }
    }
}
