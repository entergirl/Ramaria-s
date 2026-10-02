//! crates/ramaria-service/src/browse/facts.rs - Ramaria 知识事实浏览（列表 / 详情 / 分组）
//!
//! 设计特点:
//! - 活跃事实口径：只读 `active` 记录，可选按字段过滤后分页（total 为分页前条数）
//! - 版本链折叠：分组视图仅把多版本（链长 > 1）事实放入版本表，单条查询失败静默跳过
//! - 详情语义：事实不存在返回 None（非错误），版本链含自身且链头最早在前
//! - 只读：不修改任何状态

use std::collections::HashMap;

use ramaria_core::error::RamariaResult;

use crate::engine::Engine;
use crate::types::{
    FactBrowsePage, FactBrowseRequest, FactDetailView, FactEntryView, GroupedFactsView,
};

use super::view::{fact_view, require_persona_uid};

// =========================================================
// 知识事实浏览
// =========================================================

/// 知识事实浏览（活跃事实，可选按字段过滤后分页）。
///
/// 流程:
/// 1. 校验人格 uid（必填）；
/// 2. 按字段过滤（Some）或全字段（None）读取活跃事实；
/// 3. `total` 取分页前条数，应用 offset 与可选 limit。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 浏览请求（persona / field / limit / offset）。
///
/// 返回:
/// - `items`（全字段视图）与 `total`（分页前的条数）。
pub(crate) async fn facts(
    engine: &Engine,
    req: FactBrowseRequest,
) -> RamariaResult<FactBrowsePage> {
    let uid = require_persona_uid(&req.persona)?;
    let storage = engine.storage_ref();

    let all = match req.field {
        Some(field) => storage.list_active_facts_by_field(&uid, field).await?,
        None => storage.list_active_facts_by_persona(&uid).await?,
    };
    let total = all.len();
    let offset = req.offset.unwrap_or(0) as usize;

    let items: Vec<FactEntryView> = match req.limit {
        Some(limit) => all
            .iter()
            .skip(offset)
            .take(limit as usize)
            .map(fact_view)
            .collect(),
        None => all.iter().skip(offset).map(fact_view).collect(),
    };

    tracing::debug!(%uid, returned = items.len(), total, "知识事实浏览完成");
    Ok(FactBrowsePage { items, total })
}

/// 单条事实详情（含完整版本链）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `id`: 事实 id。
///
/// 返回:
/// - `Some(详情)`：事实存在（版本链含自身，链头最早在前）；
/// - `None`：事实不存在（不报错）。
pub(crate) async fn fact_detail(engine: &Engine, id: i64) -> RamariaResult<Option<FactDetailView>> {
    let storage = engine.storage_ref();
    let Some(fact) = storage.get_fact_by_id(id).await? else {
        return Ok(None);
    };
    let versions = storage.list_fact_versions(id).await?;
    Ok(Some(FactDetailView {
        fact: fact_view(&fact),
        versions: versions.iter().map(fact_view).collect(),
    }))
}

/// 按字段分组的知识事实（含多版本事实的版本链折叠数据）。
///
/// 流程:
/// 1. 校验人格 uid（必填）；
/// 2. 读取活跃事实并按字段展示名分组；
/// 3. 逐条回溯版本链，仅多版本（链长 > 1）事实入版本表；单条查询失败静默跳过。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona`: 目标人格 uid。
///
/// 返回:
/// - `grouped`（字段分组）与 `versions`（版本链表）。
pub(crate) async fn facts_grouped(
    engine: &Engine,
    persona: &str,
) -> RamariaResult<GroupedFactsView> {
    let uid = require_persona_uid(persona)?;
    let storage = engine.storage_ref();
    let active = storage.list_active_facts_by_persona(&uid).await?;

    let mut grouped: HashMap<String, Vec<FactEntryView>> = HashMap::new();
    for f in &active {
        grouped
            .entry(f.field.label().to_string())
            .or_default()
            .push(fact_view(f));
    }

    let mut versions: HashMap<i64, Vec<FactEntryView>> = HashMap::new();
    for f in &active {
        if let Ok(chain) = storage.list_fact_versions(f.id).await {
            if chain.len() > 1 {
                versions.insert(f.id, chain.iter().map(fact_view).collect());
            }
        }
    }

    tracing::debug!(
        %uid,
        active_count = active.len(),
        version_chains = versions.len(),
        "知识事实分组读取完成"
    );
    Ok(GroupedFactsView {
        persona_uid: uid,
        grouped,
        versions,
    })
}
