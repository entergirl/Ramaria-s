//! crates/ramaria-service/src/browse/l1.rs - Ramaria L1 记忆摘要浏览
//!
//! 设计特点:
//! - 双口径单一实现：按会话收集（扫描最近会话逐个读取，可选 persona 过滤）与未吸收全量口径
//! - 排序与分页：按会话口径按创建时间倒序后应用 offset / limit；未吸收口径按存储顺序分页
//! - 上限纪律：默认 200、按会话口径截断上限 1000、会话扫描上限 500
//! - 空态语义：会话无摘要返回空列表（非错误）

use std::cmp::Reverse;

use ramaria_core::error::{RamariaError, RamariaResult};
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::{L1BrowsePage, L1BrowseRequest, L1MemoryView};

use super::view::{DEFAULT_BROWSE_LIMIT, MAX_BROWSE_LIMIT, l1_view, normalize_persona};

// =========================================================
// 常量
// =========================================================

/// L1 按会话收集时的会话扫描上限（只取最近的前 N 个会话）。
const MAX_SESSION_SCAN: usize = 500;

// =========================================================
// L1 记忆摘要浏览
// =========================================================

/// L1 记忆摘要浏览。
///
/// 流程（按 `unabsorbed_only` 二选一，单一实现）:
/// - `false`（按会话收集）: 按开始时间取最近会话（上限 [`MAX_SESSION_SCAN`]）逐个读取
///   L1 摘要，可选按 persona 过滤；收集满 limit 后停止收集 → 按创建时间倒序
///   → 应用 offset → 截断 limit；
/// - `true`（未吸收口径）: 按 persona 全量读取未吸收摘要（persona 必填）→ 应用 offset / limit。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 浏览请求（persona / 未吸收开关 / limit / offset）。
///
/// 返回:
/// - `items` 与 `total`（排序后、分页前的条数）。
///
/// 说明:
/// - 未吸收口径不设 1000 上限（与全量分页的调用口径一致）；按会话收集口径上限 1000。
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

    // 按会话收集：逐个会话读取摘要，收集满 limit 后停止（与桌面口径一致）
    let limit = req
        .limit
        .unwrap_or(DEFAULT_BROWSE_LIMIT)
        .min(MAX_BROWSE_LIMIT);
    let sessions = storage.list_sessions().await?;
    let mut all: Vec<L1MemoryView> = Vec::new();
    for session in sessions.iter().take(MAX_SESSION_SCAN) {
        let list = storage.list_memory_l1(session.id).await?;
        for m in list {
            if let Some(uid) = &persona {
                if m.persona_uid.as_deref() != Some(uid.as_str()) {
                    continue;
                }
            }
            all.push(l1_view(&m));
        }
        if all.len() >= limit as usize {
            break;
        }
    }

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
