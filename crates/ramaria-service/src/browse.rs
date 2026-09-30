//! crates/ramaria-service/src/browse.rs - 记忆与会话浏览用例（查询用例组）
//!
//! 设计特点:
//! - 纯读取：L1 / L2 / L3 / 性格画像 / 事实 / 证据链 / 会话列表 / 会话消息与详情，不修改任何状态
//! - 单份实现覆盖两条调用链路：桌面口径与 CLI 口径的差异由请求参数表达（未吸收过滤 / 分页），
//!   不写第二份实现
//! - 逐段独立降级：消息计数聚合失败按 0 处理、证据链单条查询失败跳过，不阻塞整体读取
//! - 分页与上限钳制：单页消息上限 1000、L1 / L2 截断上限 1000、会话扫描上限 500、
//!   证据链事件扫描上限 5000，防御超大请求
//! - 隐私：视图为结构化数据（摘要 / 事件 / 性格 / 事实陈述），不含 utt 原文块

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    EvidenceDirection, MemoryEvent, MemoryL1, Message, PersonaFact, PersonalityTrait, TraitLayer,
    TraitStatus,
};
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::{
    ChannelOverviewView, EvidenceEventView, EvidenceL1SourceView, FactBrowsePage,
    FactBrowseRequest, FactDetailView, FactEntryView, GroupedFactsView, L1BrowsePage,
    L1BrowseRequest, L1MemoryView, L2BrowsePage, L2BrowseRequest, L2EventView, L3TraitView,
    PersonalityProfileView, ProfileStatusView, SessionBrowsePage, SessionBrowseRequest,
    SessionDetailView, SessionMessageView, SessionMessagesRequest, SessionMessagesView,
    SessionSummaryView, TraitDetailView, TraitEvidenceRequest, TraitEvidenceView,
};

// =========================================================
// 常量（浏览口径的默认值与上限）
// =========================================================

/// 浏览类用例的默认返回条数（L1 / L2 缺省取该值）。
const DEFAULT_BROWSE_LIMIT: u32 = 200;

/// 浏览类用例的单次返回条数上限（按会话收集口径的 L1 与 L2 超过时截断）。
const MAX_BROWSE_LIMIT: u32 = 1000;

/// L1 按会话收集时的会话扫描上限（只取最近的前 N 个会话）。
const MAX_SESSION_SCAN: usize = 500;

/// 单页消息条数上限（防御超大分页请求）。
const MAX_MESSAGE_PAGE: i64 = 1000;

/// 证据链构建时的事件扫描上限（一次取回该人格的近期事件）。
const MAX_EVENTS_SCAN: i64 = 5000;

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
// L3 性格标签浏览 / 三层画像 / 数据状态
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

/// L3 三层性格画像（按 base / primary / accent 分组）。
///
/// 流程:
/// 1. 校验人格 uid 并确认人格存在（不存在 → Validation 错误）；
/// 2. 读取该人格全部性格标签，仅保留生效（Active）标签；
/// 3. 按分层归组，层内按 `seq` 升序。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona_uid`: 目标人格 uid。
///
/// 返回:
/// - 三层画像视图；无人格画像时三层均为空数组（非错误）。
pub(crate) async fn personality_profile(
    engine: &Engine,
    persona_uid: &str,
) -> RamariaResult<PersonalityProfileView> {
    let uid = require_persona_uid(persona_uid)?;
    let storage = engine.storage_ref();
    if storage.get_persona_by_uid(&uid).await?.is_none() {
        return Err(RamariaError::validation(format!("人格不存在: uid={uid}")));
    }

    let traits = storage.list_traits_by_persona(&uid).await?;
    let mut base = Vec::new();
    let mut primary = Vec::new();
    let mut accent = Vec::new();
    for t in &traits {
        if t.status != TraitStatus::Active {
            continue;
        }
        let view = trait_detail_view(t);
        match t.layer {
            TraitLayer::Base => base.push(view),
            TraitLayer::Primary => primary.push(view),
            TraitLayer::Accent => accent.push(view),
            // 未知分层（non_exhaustive）走此分支静默跳过
            _ => {}
        }
    }
    base.sort_by_key(|v| v.seq);
    primary.sort_by_key(|v| v.seq);
    accent.sort_by_key(|v| v.seq);

    tracing::debug!(
        %uid,
        base = base.len(),
        primary = primary.len(),
        accent = accent.len(),
        "L3 三层画像读取完成"
    );
    Ok(PersonalityProfileView {
        persona_uid: uid,
        base,
        primary,
        accent,
    })
}

/// 画像数据状态（有效样本量与可信度区间）。
///
/// 判定口径:
/// - `n_total_eff` = 生效标签的 `evidence` 之和；
/// - `< 5` → `insufficient`；`5..20` → `preliminary`；`≥ 20` → `trusted`。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona_uid`: 目标人格 uid。
///
/// 返回:
/// - 有效样本量、生效标签数与状态文本；人格不存在时返回 Validation 错误。
pub(crate) async fn profile_status(
    engine: &Engine,
    persona_uid: &str,
) -> RamariaResult<ProfileStatusView> {
    let uid = require_persona_uid(persona_uid)?;
    let storage = engine.storage_ref();
    if storage.get_persona_by_uid(&uid).await?.is_none() {
        return Err(RamariaError::validation(format!("人格不存在: uid={uid}")));
    }

    let traits = storage.list_traits_by_persona(&uid).await?;
    let active: Vec<&PersonalityTrait> = traits
        .iter()
        .filter(|t| matches!(t.status, TraitStatus::Active))
        .collect();
    let n_total_eff: f64 = active.iter().map(|t| t.evidence).sum();
    let active_count = active.len();

    let (status, status_text) = if n_total_eff < 5.0 {
        (
            "insufficient",
            format!("数据不足（有效样本量: {n_total_eff:.1}）—— 继续对话以积累更多数据"),
        )
    } else if n_total_eff < 20.0 {
        (
            "preliminary",
            format!("初步画像（有效样本量: {n_total_eff:.1}）—— 画像有一定参考价值，建议继续积累"),
        )
    } else {
        (
            "trusted",
            format!("可信画像（有效样本量: {n_total_eff:.1}，共 {active_count} 项性格标签）"),
        )
    };

    tracing::debug!(%uid, n_total_eff, active_count, status, "画像数据状态读取完成");
    Ok(ProfileStatusView {
        persona_uid: uid,
        n_total_eff,
        active_trait_count: active_count,
        status: status.to_string(),
        status_text,
    })
}

// =========================================================
// 性格标签证据链
// =========================================================

/// 查询指定性格标签的完整证据溯源链。
///
/// 链路: trait → trait_evidence → memory_events → event_sources → memory_l1 → evidence_notes。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 证据链请求（persona 必填；trait_id 须为正整数）。
///
/// 返回:
/// - 单元素列表（证据链视图）；无证据记录时返回单条空链（非错误）。
///
/// 说明:
/// - 单条查询失败隔离：事件不存在 / L1 溯源查询失败 / L1 不存在均记录告警后跳过，
///   不阻塞整条证据链；
/// - 统计 `support` / `contradict` / `neutral` 三类证据数量分布。
pub(crate) async fn trait_evidence(
    engine: &Engine,
    req: TraitEvidenceRequest,
) -> RamariaResult<Vec<TraitEvidenceView>> {
    let uid = require_persona_uid(&req.persona)?;
    if req.trait_id <= 0 {
        return Err(RamariaError::validation("trait_id 必须为正整数"));
    }

    let storage = engine.storage_ref();

    // ---- 1. 定位目标性格标签 ----
    let traits = storage.list_traits_by_persona(&uid).await?;
    let target = traits
        .into_iter()
        .find(|t| t.id == req.trait_id)
        .ok_or_else(|| {
            RamariaError::validation(format!(
                "性格标签不存在: trait_id={}, persona_uid={uid}",
                req.trait_id
            ))
        })?;
    let trait_label = target.trait_label;

    // ---- 2. 该标签的全部证据记录 ----
    let evidence_records = storage.list_evidence_by_trait(req.trait_id).await?;
    if evidence_records.is_empty() {
        tracing::debug!(trait_id = req.trait_id, %uid, "trait 无证据记录，返回空链");
        return Ok(vec![TraitEvidenceView {
            trait_id: req.trait_id,
            trait_label,
            total_evidence: 0,
            support_count: 0,
            contradict_count: 0,
            neutral_count: 0,
            evidence_events: Vec::new(),
        }]);
    }

    let support_count = evidence_records
        .iter()
        .filter(|e| matches!(e.direction, EvidenceDirection::Support))
        .count();
    let contradict_count = evidence_records
        .iter()
        .filter(|e| matches!(e.direction, EvidenceDirection::Contradict))
        .count();
    let neutral_count = evidence_records
        .len()
        .saturating_sub(support_count + contradict_count);

    // ---- 3. 预加载该人格的事件（构建 event_id → event 查找表）----
    let all_events = storage
        .list_events_by_persona(&uid, 0, MAX_EVENTS_SCAN)
        .await?;
    let event_map: HashMap<i64, &MemoryEvent> = all_events.iter().map(|e| (e.id, e)).collect();

    // ---- 4. 逐条证据构建事件链（单条失败跳过，不阻塞整体）----
    let mut evidence_events: Vec<EvidenceEventView> = Vec::with_capacity(evidence_records.len());
    for record in &evidence_records {
        let Some(event) = event_map.get(&record.event_id).copied() else {
            tracing::warn!(
                event_id = record.event_id,
                trait_id = req.trait_id,
                "证据记录引用了不存在的事件，跳过"
            );
            continue;
        };

        let l1_sources = match storage.list_event_sources_by_event(event.id).await {
            Ok(sources) => sources,
            Err(e) => {
                tracing::warn!(event_id = event.id, error = %e, "查询事件 L1 溯源失败，跳过该事件");
                continue;
            }
        };

        let mut l1_views: Vec<EvidenceL1SourceView> = Vec::with_capacity(l1_sources.len());
        for src in &l1_sources {
            match storage.get_memory_l1(src.l1_id).await {
                Ok(Some(l1)) => {
                    l1_views.push(EvidenceL1SourceView {
                        l1_id: l1.id,
                        summary: l1.summary,
                        evidence_notes: l1
                            .evidence_notes
                            .unwrap_or_default()
                            .into_iter()
                            .map(|note| note.text)
                            .collect(),
                        atmosphere: l1.atmosphere,
                        valence: l1.valence,
                        weight: src.weight,
                    });
                }
                Ok(None) => {
                    tracing::warn!(
                        l1_id = %src.l1_id,
                        event_id = event.id,
                        "事件溯源引用了不存在的 L1 记录，跳过"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        l1_id = %src.l1_id,
                        event_id = event.id,
                        error = %e,
                        "查询 L1 记录失败，跳过该条溯源"
                    );
                }
            }
        }

        evidence_events.push(EvidenceEventView {
            event_id: event.id,
            title: event.title.clone(),
            summary: event.summary.clone(),
            confidence: event.confidence,
            valence: event.valence,
            salience: event.salience,
            attitude: event.attitude.clone(),
            paraphrase: event.paraphrase.clone(),
            motives: event.motives.clone(),
            l1_sources: l1_views,
        });
    }

    tracing::debug!(
        trait_id = req.trait_id,
        %uid,
        total = evidence_records.len(),
        events_loaded = evidence_events.len(),
        support = support_count,
        contradict = contradict_count,
        neutral = neutral_count,
        "性格标签证据链读取完成"
    );

    Ok(vec![TraitEvidenceView {
        trait_id: req.trait_id,
        trait_label,
        total_evidence: evidence_records.len(),
        support_count,
        contradict_count,
        neutral_count,
        evidence_events,
    }])
}

// =========================================================
// 知识事实浏览（列表 / 详情 / 分组）
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

// =========================================================
// 会话浏览（列表 / 消息）
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

// =========================================================
// 视图组装与输入归一（文件内私有）
// =========================================================

/// 归一化人格 uid（空白视为未提供）。
fn normalize_persona(persona: Option<&str>) -> Option<String> {
    persona
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// 校验人格 uid（空白拒绝），返回去除首尾空白后的值。
fn require_persona_uid(raw: &str) -> RamariaResult<String> {
    let uid = raw.trim().to_string();
    if uid.is_empty() {
        return Err(RamariaError::validation("人格 UID 不能为空"));
    }
    Ok(uid)
}

/// 毫秒时间戳转换为 UTC 时间（非法值回退 Unix 纪元）。
fn to_datetime(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

/// 组装 L1 摘要浏览视图（伴随字段保留原始空值）。
fn l1_view(m: &MemoryL1) -> L1MemoryView {
    L1MemoryView {
        id: m.id,
        session_id: m.session_id,
        summary: m.summary.clone(),
        keywords: m.keywords.clone(),
        atmosphere: m.atmosphere.clone(),
        time_period: m.time_period.clone(),
        context_json: m.context_json.clone(),
        valence: m.valence,
        salience: m.salience,
        persona_uid: m.persona_uid.clone(),
        created_at: m.created_at,
    }
}

/// 组装 L2 事件浏览视图。
fn l2_view(e: &MemoryEvent) -> L2EventView {
    L2EventView {
        id: e.id,
        persona_uid: e.persona_uid.clone(),
        title: e.title.clone(),
        summary: e.summary.clone(),
        keywords: e.keywords.clone(),
        valence: e.valence,
        confidence: e.confidence,
        presentation: e.presentation,
        share: e.share,
        attitude: e.attitude.clone(),
        salience: e.salience,
        created_at: e.created_at,
    }
}

/// 组装 L3 性格标签浏览视图。
fn l3_view(t: &PersonalityTrait) -> L3TraitView {
    L3TraitView {
        id: t.id,
        persona_uid: t.persona_uid.clone(),
        layer: t.layer,
        label: t.trait_label.clone(),
        meaning: t.meaning.clone(),
        confidence: t.confidence,
        evidence: t.evidence,
        consistency: t.consistency,
        status: t.status,
        created_at: t.created_at,
    }
}

/// 组装三层画像的标签详细视图。
fn trait_detail_view(t: &PersonalityTrait) -> TraitDetailView {
    TraitDetailView {
        id: t.id,
        label: t.trait_label.clone(),
        meaning: t.meaning.clone(),
        confidence: t.confidence,
        evidence: t.evidence,
        consistency: t.consistency,
        layer: t.layer,
        not_meaning: t.not_meaning.clone(),
        trigger: t.trigger.clone(),
        suppress: t.suppress.clone(),
        related: t.related.clone(),
        seq: t.seq,
        source: t.source,
        status: t.status,
        created_at: t.created_at,
    }
}

/// 组装知识事实条目视图（全字段）。
fn fact_view(f: &PersonaFact) -> FactEntryView {
    FactEntryView {
        id: f.id,
        persona_uid: f.persona_uid.clone(),
        field: f.field,
        content: f.content.clone(),
        source: f.source,
        status: f.status,
        tier: f.tier,
        version_of: f.version_of,
        confidence: f.confidence,
        keyword_hint: f.keyword_hint.clone(),
        ref_event_id: f.ref_event_id,
        ref_l1_id: f.ref_l1_id,
        created_at: f.created_at,
        updated_at: f.updated_at,
    }
}

/// 组装消息浏览条目视图。
fn message_view(m: &Message) -> SessionMessageView {
    SessionMessageView {
        id: m.id,
        role: m.role,
        content: m.content.clone(),
        created_at: m.created_at,
        source: m.source,
        persona_uid: m.persona_uid.clone(),
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        engine_with_db, seed_channel_session, seed_l1, seed_messages, seed_persona,
        seed_session_with_messages,
    };
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::{
        EvidenceNote, FactSource, MessageRole, MessageSource, PersonaFact, ProfileField,
        TraitEvidence, TraitSource,
    };
    use ramaria_storage::SqliteStorage;
    use std::time::Duration;
    use uuid::Uuid;

    /// 造一条事件（start / created_at 取同一时间戳，便于排序断言）。
    async fn seed_event(
        storage: &SqliteStorage,
        persona: &str,
        title: &str,
        created_at: i64,
    ) -> i64 {
        let mut ev = MemoryEvent::new(
            persona.to_string(),
            title.to_string(),
            format!("{title}的摘要"),
            created_at,
            created_at + 1_000,
        );
        ev.created_at = created_at;
        storage.save_event(&ev).await.expect("写入事件应成功")
    }

    /// 造一条性格标签（可指定分层 / 层内序号 / 有效证据量）。
    async fn seed_trait(
        storage: &SqliteStorage,
        persona: &str,
        label: &str,
        layer: TraitLayer,
        seq: i32,
        evidence: f64,
    ) -> i64 {
        let mut t = PersonalityTrait::new(
            persona.to_string(),
            layer,
            label.to_string(),
            format!("{label}的具体含义"),
            TraitSource::Manual,
            seq,
        );
        t.evidence = evidence;
        storage.save_trait(&t).await.expect("写入性格标签应成功")
    }

    /// 造一条知识事实（active，返回 id）。
    async fn seed_fact(
        storage: &SqliteStorage,
        persona: &str,
        field: ProfileField,
        content: &str,
    ) -> i64 {
        let f = PersonaFact::new(
            persona.to_string(),
            field,
            content.to_string(),
            FactSource::Manual,
        );
        storage.save_fact(&f).await.expect("写入事实应成功")
    }

    /// L1 桌面口径：按会话收集 + persona 过滤 + 创建时间倒序 + limit 截断。
    #[tokio::test]
    async fn l1_desktop_scope_filters_orders_and_truncates() {
        let (engine, storage, dir) = engine_with_db("browse-l1-desktop").await;
        seed_persona(&storage, "char-0001").await;
        seed_persona(&storage, "char-0002").await;
        seed_l1(&storage, "char-0001", "摘要 A", Some("考试"), 1_000).await;
        seed_l1(&storage, "char-0002", "摘要 B", None, 2_000).await;
        seed_l1(&storage, "char-0001", "摘要 C", None, 3_000).await;

        // persona 过滤：只含 char-0001 的两条，创建时间倒序
        let page = engine
            .memory_l1(L1BrowseRequest {
                persona: Some("char-0001".to_string()),
                unabsorbed_only: false,
                limit: None,
                offset: None,
            })
            .await
            .expect("L1 浏览应成功");
        assert_eq!(page.total, 2);
        let summaries: Vec<&str> = page.items.iter().map(|m| m.summary.as_str()).collect();
        assert_eq!(summaries, vec!["摘要 C", "摘要 A"], "应按创建时间倒序");
        assert!(
            page.items
                .iter()
                .all(|m| m.persona_uid.as_deref() == Some("char-0001"))
        );
        let a_item = page
            .items
            .iter()
            .find(|m| m.summary == "摘要 A")
            .expect("摘要 A 应出现");
        assert_eq!(a_item.keywords.as_deref(), Some("考试"), "伴随字段应透传");

        // 无 persona：三条全含
        let all = engine
            .memory_l1(L1BrowseRequest {
                persona: None,
                unabsorbed_only: false,
                limit: None,
                offset: None,
            })
            .await
            .expect("L1 浏览应成功");
        assert_eq!(all.total, 3);
        let summaries: Vec<&str> = all.items.iter().map(|m| m.summary.as_str()).collect();
        assert_eq!(summaries, vec!["摘要 C", "摘要 B", "摘要 A"]);

        // limit 截断：取最新一条
        let limited = engine
            .memory_l1(L1BrowseRequest {
                persona: None,
                unabsorbed_only: false,
                limit: Some(1),
                offset: None,
            })
            .await
            .expect("L1 浏览应成功");
        assert_eq!(limited.items.len(), 1);
        assert_eq!(limited.items[0].summary, "摘要 C");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L1 未吸收口径：分页 + total 为分页前条数；persona 缺省报业务校验错误。
    #[tokio::test]
    async fn l1_unabsorbed_scope_paginates() {
        let (engine, storage, dir) = engine_with_db("browse-l1-unabsorbed").await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(&storage, "char-0001", "摘要 1", None, 1_000).await;
        let absorbed = seed_l1(&storage, "char-0001", "摘要 2", None, 2_000).await;
        seed_l1(&storage, "char-0001", "摘要 3", None, 3_000).await;
        storage
            .mark_l1_absorbed(&[absorbed])
            .await
            .expect("标记吸收应成功");

        // 全量：未吸收 2 条（按创建时间升序取回）
        let page = engine
            .memory_l1(L1BrowseRequest {
                persona: Some("char-0001".to_string()),
                unabsorbed_only: true,
                limit: None,
                offset: None,
            })
            .await
            .expect("未吸收浏览应成功");
        assert_eq!(page.total, 2, "total 为分页前条数");
        let summaries: Vec<&str> = page.items.iter().map(|m| m.summary.as_str()).collect();
        assert_eq!(summaries, vec!["摘要 1", "摘要 3"]);

        // 分页：offset 1 + limit 1 → 第二条
        let page = engine
            .memory_l1(L1BrowseRequest {
                persona: Some("char-0001".to_string()),
                unabsorbed_only: true,
                limit: Some(1),
                offset: Some(1),
            })
            .await
            .expect("未吸收浏览应成功");
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].summary, "摘要 3");

        // persona 缺省：业务校验错误
        let err = engine
            .memory_l1(L1BrowseRequest {
                persona: None,
                unabsorbed_only: true,
                limit: None,
                offset: None,
            })
            .await
            .expect_err("未吸收口径需 persona");
        assert_eq!(err.category(), "validation");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L2 persona 口径：分页 + total 为全量计数。
    #[tokio::test]
    async fn l2_persona_scope_paginates_with_total() {
        let (engine, storage, dir) = engine_with_db("browse-l2-persona").await;
        seed_persona(&storage, "char-0001").await;
        for i in 0..5_i64 {
            seed_event(&storage, "char-0001", &format!("事件{i}"), 1_000 + i * 10).await;
        }

        let page = engine
            .memory_l2(L2BrowseRequest {
                persona: Some("char-0001".to_string()),
                limit: Some(2),
                offset: Some(1),
            })
            .await
            .expect("L2 浏览应成功");
        assert_eq!(page.total, 5, "total 为分页前全量计数");
        assert_eq!(page.items.len(), 2);
        // 按 start DESC：offset 1 跳过最新一条
        assert_eq!(page.items[0].title, "事件3");
        assert_eq!(page.items[1].title, "事件2");
        assert_eq!(page.items[0].persona_uid, "char-0001");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L2 合并口径：逐 persona 取回后合并倒序截断，total 为合并条数。
    #[tokio::test]
    async fn l2_all_personas_merges_and_truncates() {
        let (engine, storage, dir) = engine_with_db("browse-l2-merge").await;
        seed_persona(&storage, "char-0001").await;
        seed_persona(&storage, "char-0002").await;
        seed_event(&storage, "char-0001", "一A", 1_000).await;
        seed_event(&storage, "char-0001", "一B", 2_000).await;
        seed_event(&storage, "char-0002", "二A", 3_000).await;
        seed_event(&storage, "char-0002", "二B", 4_000).await;

        let page = engine
            .memory_l2(L2BrowseRequest {
                persona: None,
                limit: Some(3),
                offset: None,
            })
            .await
            .expect("L2 合并浏览应成功");
        assert_eq!(page.total, 4, "total 为合并后（截断前）条数");
        let titles: Vec<&str> = page.items.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, vec!["二B", "二A", "一B"], "应按创建时间倒序截断");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L3 两分支：persona 过滤与全人格合并。
    #[tokio::test]
    async fn l3_both_scopes() {
        let (engine, storage, dir) = engine_with_db("browse-l3").await;
        seed_persona(&storage, "char-0001").await;
        seed_persona(&storage, "char-0002").await;
        seed_trait(&storage, "char-0001", "温和", TraitLayer::Base, 1, 2.0).await;
        seed_trait(&storage, "char-0001", "幽默", TraitLayer::Primary, 1, 1.0).await;
        seed_trait(&storage, "char-0002", "直率", TraitLayer::Base, 1, 1.0).await;

        let scoped = engine
            .memory_l3(Some("char-0001"))
            .await
            .expect("L3 浏览应成功");
        assert_eq!(scoped.len(), 2);
        assert!(scoped.iter().all(|t| t.persona_uid == "char-0001"));

        let merged = engine.memory_l3(None).await.expect("L3 浏览应成功");
        assert_eq!(merged.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 三层画像：分组 / 层内 seq 排序 / 生效过滤 / 人格不存在报错。
    #[tokio::test]
    async fn personality_profile_groups_and_filters() {
        let (engine, storage, dir) = engine_with_db("browse-profile").await;
        seed_persona(&storage, "char-0001").await;
        seed_trait(&storage, "char-0001", "底色二", TraitLayer::Base, 2, 1.0).await;
        seed_trait(&storage, "char-0001", "底色一", TraitLayer::Base, 1, 1.0).await;
        seed_trait(&storage, "char-0001", "主色", TraitLayer::Primary, 1, 1.0).await;
        seed_trait(&storage, "char-0001", "点缀", TraitLayer::Accent, 1, 1.0).await;
        // 非生效标签不参与展示
        let deprecated =
            seed_trait(&storage, "char-0001", "旧标签", TraitLayer::Base, 3, 1.0).await;
        storage
            .update_trait_status(deprecated, TraitStatus::Deprecated)
            .await
            .expect("更新状态应成功");

        let profile = engine
            .personality_profile("char-0001")
            .await
            .expect("三层画像应成功");
        assert_eq!(profile.persona_uid, "char-0001");
        assert_eq!(profile.base.len(), 2);
        assert_eq!(profile.primary.len(), 1);
        assert_eq!(profile.accent.len(), 1);
        let base_labels: Vec<&str> = profile.base.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(base_labels, vec!["底色一", "底色二"], "层内按 seq 升序");

        // 人格不存在 / 空 uid：业务校验错误
        let err = engine
            .personality_profile("char-9999")
            .await
            .expect_err("人格不存在应报错");
        assert_eq!(err.category(), "validation");
        let err = engine
            .personality_profile("  ")
            .await
            .expect_err("空 uid 应报错");
        assert_eq!(err.category(), "validation");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 画像数据状态：三档阈值与描述文本。
    #[tokio::test]
    async fn profile_status_thresholds() {
        let (engine, storage, dir) = engine_with_db("browse-status").await;
        seed_persona(&storage, "char-0001").await;
        let t1 = seed_trait(&storage, "char-0001", "标签一", TraitLayer::Base, 1, 3.0).await;
        let t2 = seed_trait(&storage, "char-0001", "标签二", TraitLayer::Base, 2, 0.0).await;

        // 3.0 → insufficient
        let status = engine
            .profile_status("char-0001")
            .await
            .expect("状态读取应成功");
        assert_eq!(status.status, "insufficient");
        assert_eq!(status.active_trait_count, 2);
        assert!((status.n_total_eff - 3.0).abs() < f64::EPSILON);
        assert!(
            status.status_text.contains("数据不足"),
            "文案: {}",
            status.status_text
        );

        // 3.0 + 10.0 = 13.0 → preliminary
        storage
            .update_trait_confidence(t1, 0.8, 13.0, 0.5)
            .await
            .expect("更新证据量应成功");
        let status = engine
            .profile_status("char-0001")
            .await
            .expect("状态读取应成功");
        assert_eq!(status.status, "preliminary");
        assert!((status.n_total_eff - 13.0).abs() < f64::EPSILON);

        // 13.0 + 7.0 = 20.0 → trusted（下边界）
        storage
            .update_trait_confidence(t2, 0.9, 7.0, 0.5)
            .await
            .expect("更新证据量应成功");
        let status = engine
            .profile_status("char-0001")
            .await
            .expect("状态读取应成功");
        assert_eq!(status.status, "trusted");
        assert!((status.n_total_eff - 20.0).abs() < f64::EPSILON);
        assert!(
            status.status_text.contains("可信画像"),
            "文案: {}",
            status.status_text
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 事实浏览：字段过滤 + 分页 + total 为分页前条数。
    #[tokio::test]
    async fn facts_filter_and_paginate() {
        let (engine, storage, dir) = engine_with_db("browse-facts").await;
        seed_persona(&storage, "char-0001").await;
        seed_fact(&storage, "char-0001", ProfileField::Interests, "喜欢露营").await;
        seed_fact(&storage, "char-0001", ProfileField::Interests, "喜欢摄影").await;
        seed_fact(&storage, "char-0001", ProfileField::BasicInfo, "住在杭州").await;

        // 全字段：3 条
        let page = engine
            .memory_facts(FactBrowseRequest {
                persona: "char-0001".to_string(),
                field: None,
                limit: None,
                offset: None,
            })
            .await
            .expect("事实浏览应成功");
        assert_eq!(page.total, 3);
        assert_eq!(page.items.len(), 3);

        // 字段过滤：仅兴趣爱好 2 条
        let page = engine
            .memory_facts(FactBrowseRequest {
                persona: "char-0001".to_string(),
                field: Some(ProfileField::Interests),
                limit: None,
                offset: None,
            })
            .await
            .expect("事实浏览应成功");
        assert_eq!(page.total, 2);
        assert!(
            page.items
                .iter()
                .all(|f| f.field == ProfileField::Interests)
        );

        // 分页：limit 1 offset 1 → 1 条，total 保留
        let page = engine
            .memory_facts(FactBrowseRequest {
                persona: "char-0001".to_string(),
                field: None,
                limit: Some(1),
                offset: Some(1),
            })
            .await
            .expect("事实浏览应成功");
        assert_eq!(page.total, 3);
        assert_eq!(page.items.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 事实详情与分组：版本链仅在多版本时入版本表。
    #[tokio::test]
    async fn fact_detail_and_grouped_version_chains() {
        let (engine, storage, dir) = engine_with_db("browse-fact-detail").await;
        seed_persona(&storage, "char-0001").await;

        // 版本链：old → new（覆盖写）
        let mut old = PersonaFact::new(
            "char-0001".to_string(),
            ProfileField::Interests,
            "喜欢摄影".to_string(),
            FactSource::Manual,
        );
        let old_id = storage.save_fact(&old).await.expect("写入旧事实应成功");
        old.id = old_id;
        let fresh = PersonaFact::new(
            "char-0001".to_string(),
            ProfileField::Interests,
            "喜欢旅行".to_string(),
            FactSource::Manual,
        );
        let fresh_id = storage
            .save_fact_with_version(&old, &fresh)
            .await
            .expect("覆盖写应成功");
        // 单版本事实（不入版本表）
        let single = seed_fact(&storage, "char-0001", ProfileField::BasicInfo, "住在杭州").await;

        // 详情：版本链含旧、新两条（链头最早在前）
        let detail = engine
            .memory_fact_detail(fresh_id)
            .await
            .expect("详情读取应成功")
            .expect("事实应存在");
        assert_eq!(detail.fact.id, fresh_id);
        assert_eq!(detail.versions.len(), 2);
        assert_eq!(detail.versions[0].id, old_id, "链头最早在前");
        assert_eq!(detail.versions[1].id, fresh_id);

        // 不存在：None
        assert!(
            engine
                .memory_fact_detail(99_999)
                .await
                .expect("详情读取应成功")
                .is_none()
        );

        // 分组：Interests 组含新事实；版本表只含多版本事实
        let grouped = engine
            .memory_facts_grouped("char-0001")
            .await
            .expect("分组读取应成功");
        let interest = grouped
            .grouped
            .get(ProfileField::Interests.label())
            .expect("兴趣爱好分组应存在");
        assert_eq!(interest.len(), 1);
        assert_eq!(interest[0].id, fresh_id);
        assert_eq!(grouped.versions.len(), 1);
        assert!(grouped.versions.contains_key(&fresh_id));
        assert!(
            !grouped.versions.contains_key(&single),
            "单版本事实不入版本表"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 证据链：三类统计 + 事件来源链 + 空链与参数校验。
    #[tokio::test]
    async fn trait_evidence_chain_and_boundaries() {
        let (engine, storage, dir) = engine_with_db("browse-evidence").await;
        seed_persona(&storage, "char-0001").await;
        let trait_id = seed_trait(&storage, "char-0001", "温和", TraitLayer::Base, 1, 2.0).await;

        // 事件一（带 L1 溯源与证据片段）；事件二（无溯源）
        let event1 = seed_event(&storage, "char-0001", "事件一", 1_000).await;
        let event2 = seed_event(&storage, "char-0001", "事件二", 2_000).await;

        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        let mut l1 = MemoryL1::new(
            session.id,
            "备考期间保持耐心".to_string(),
            Some("夜间".to_string()),
        );
        l1.persona_uid = Some("char-0001".to_string());
        l1.evidence_notes = Some(vec![EvidenceNote::new("连续几天复习到深夜")]);
        l1.created_at = 1_500;
        storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
        storage
            .save_event_source(event1, l1.id, 0.8)
            .await
            .expect("写入事件溯源应成功");

        // 证据：support ×1 + contradict ×1 + neutral ×1
        for (event_id, direction) in [
            (event1, EvidenceDirection::Support),
            (event2, EvidenceDirection::Contradict),
            (event2, EvidenceDirection::Neutral),
        ] {
            let evidence = TraitEvidence::new(trait_id, event_id, direction, 0.8);
            storage
                .save_evidence(&evidence)
                .await
                .expect("写入证据应成功");
        }

        let chains = engine
            .memory_trait_evidence(TraitEvidenceRequest {
                persona: "char-0001".to_string(),
                trait_id,
            })
            .await
            .expect("证据链应成功");
        assert_eq!(chains.len(), 1);
        let chain = &chains[0];
        assert_eq!(chain.trait_id, trait_id);
        assert_eq!(chain.trait_label, "温和");
        assert_eq!(chain.total_evidence, 3);
        assert_eq!(chain.support_count, 1);
        assert_eq!(chain.contradict_count, 1);
        assert_eq!(chain.neutral_count, 1);
        assert_eq!(chain.evidence_events.len(), 3);
        let ev1 = chain
            .evidence_events
            .iter()
            .find(|e| e.event_id == event1)
            .expect("事件一应出现");
        assert_eq!(ev1.l1_sources.len(), 1);
        assert_eq!(
            ev1.l1_sources[0].evidence_notes,
            vec!["连续几天复习到深夜".to_string()]
        );
        assert!((ev1.l1_sources[0].weight - 0.8).abs() < f64::EPSILON);
        assert_eq!(ev1.l1_sources[0].l1_id, l1.id);

        // 空证据链：无证据标签返回单条空链
        let bare = seed_trait(&storage, "char-0001", "未验证", TraitLayer::Accent, 2, 0.0).await;
        let chains = engine
            .memory_trait_evidence(TraitEvidenceRequest {
                persona: "char-0001".to_string(),
                trait_id: bare,
            })
            .await
            .expect("证据链应成功");
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].total_evidence, 0);
        assert!(chains[0].evidence_events.is_empty());

        // trait_id <= 0 / 空 persona：业务校验错误
        let err = engine
            .memory_trait_evidence(TraitEvidenceRequest {
                persona: "char-0001".to_string(),
                trait_id: 0,
            })
            .await
            .expect_err("非法 trait_id 应报错");
        assert_eq!(err.category(), "validation");
        let err = engine
            .memory_trait_evidence(TraitEvidenceRequest {
                persona: "  ".to_string(),
                trait_id,
            })
            .await
            .expect_err("空 persona 应报错");
        assert_eq!(err.category(), "validation");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话列表：消息计数聚合 + 开始时间倒序 + 分页 + total + 分页钳制。
    #[tokio::test]
    async fn sessions_aggregate_counts_and_paginate() {
        let (engine, storage, dir) = engine_with_db("browse-sessions").await;
        seed_persona(&storage, "char-0001").await;

        let s1 = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        tokio::time::sleep(Duration::from_millis(2)).await;
        let s2 = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        tokio::time::sleep(Duration::from_millis(2)).await;
        let s3 = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");

        seed_messages(&storage, s1.id, "char-0001", 2, 1_000).await;
        seed_messages(&storage, s3.id, "char-0001", 3, 3_000).await;
        // s2 无消息：计数应为 0

        // 全量：按开始时间倒序（s3 → s2 → s1），计数聚合正确
        let page = engine
            .session_list(SessionBrowseRequest {
                limit: None,
                offset: None,
            })
            .await
            .expect("会话列表应成功");
        assert_eq!(page.total, 3);
        let ids: Vec<Uuid> = page.items.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![s3.id, s2.id, s1.id]);
        let counts: Vec<u32> = page.items.iter().map(|s| s.message_count).collect();
        assert_eq!(counts, vec![3, 0, 2], "无消息会话计数按 0");
        assert!(page.items[1].ended_at.is_none());
        assert_eq!(page.items[0].channel, "local");

        // 分页：limit 2 → 2 条；offset 2 → 1 条
        let page = engine
            .session_list(SessionBrowseRequest {
                limit: Some(2),
                offset: None,
            })
            .await
            .expect("会话列表应成功");
        assert_eq!(page.total, 3);
        assert_eq!(page.items.len(), 2);
        let page = engine
            .session_list(SessionBrowseRequest {
                limit: Some(2),
                offset: Some(2),
            })
            .await
            .expect("会话列表应成功");
        assert_eq!(page.items.len(), 1);

        // 分页钳制：limit 0 → 下界 1
        let page = engine
            .session_list(SessionBrowseRequest {
                limit: Some(0),
                offset: None,
            })
            .await
            .expect("会话列表应成功");
        assert_eq!(page.items.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话消息：全量 / 分页翻正 / 钳制 / 负偏移 / has_more 边界 / 不存在会话报错。
    #[tokio::test]
    async fn session_messages_full_and_paged() {
        let (engine, storage, dir) = engine_with_db("browse-messages").await;
        seed_persona(&storage, "char-0001").await;
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        seed_messages(&storage, session.id, "char-0001", 5, 1_000).await;

        // 全量：时间正序、total 5、has_more false
        let view = engine
            .session_messages(SessionMessagesRequest {
                session_id: session.id,
                limit: None,
                offset: None,
            })
            .await
            .expect("消息浏览应成功");
        assert_eq!(view.total, 5);
        assert!(!view.has_more);
        let contents: Vec<&str> = view.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            contents,
            vec![
                "消息内容 0",
                "消息内容 1",
                "消息内容 2",
                "消息内容 3",
                "消息内容 4"
            ]
        );
        assert_eq!(view.messages[0].role, MessageRole::User);
        assert_eq!(view.messages[0].source, MessageSource::Local);
        assert_eq!(view.messages[0].persona_uid.as_deref(), Some("char-0001"));

        // 分页：limit 2 → 最新 2 条（页内时间正序）、has_more true
        let view = engine
            .session_messages(SessionMessagesRequest {
                session_id: session.id,
                limit: Some(2),
                offset: None,
            })
            .await
            .expect("消息浏览应成功");
        let contents: Vec<&str> = view.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["消息内容 3", "消息内容 4"]);
        assert_eq!(view.total, 5);
        assert!(view.has_more);

        // 不整除边界：offset 2 + limit 2 → has_more true
        let view = engine
            .session_messages(SessionMessagesRequest {
                session_id: session.id,
                limit: Some(2),
                offset: Some(2),
            })
            .await
            .expect("消息浏览应成功");
        let contents: Vec<&str> = view.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["消息内容 1", "消息内容 2"]);
        assert!(view.has_more);

        // 正好整除边界：offset 3 + limit 2 → has_more false
        let view = engine
            .session_messages(SessionMessagesRequest {
                session_id: session.id,
                limit: Some(2),
                offset: Some(3),
            })
            .await
            .expect("消息浏览应成功");
        let contents: Vec<&str> = view.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["消息内容 0", "消息内容 1"]);
        assert!(!view.has_more);

        // 钳制与负偏移：limit 0 → 1 条；offset 负数按 0 处理
        let view = engine
            .session_messages(SessionMessagesRequest {
                session_id: session.id,
                limit: Some(0),
                offset: Some(-3),
            })
            .await
            .expect("消息浏览应成功");
        assert_eq!(view.messages.len(), 1);
        assert_eq!(view.messages[0].content, "消息内容 4");
        assert!(view.has_more);

        // 会话不存在：业务校验错误（入口无需预判）
        let err = engine
            .session_messages(SessionMessagesRequest {
                session_id: Uuid::new_v4(),
                limit: None,
                offset: None,
            })
            .await
            .expect_err("不存在的会话应显式报错");
        assert_eq!(err.category(), "validation");
        assert!(
            err.to_string().contains("会话不存在"),
            "错误文案应含会话不存在: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话消息：会话存在但无消息 → 空集合 + total 0 + has_more false（非错误）。
    #[tokio::test]
    async fn session_messages_for_existing_empty_session() {
        let (engine, storage, dir) = engine_with_db("browse-messages-empty").await;
        seed_persona(&storage, "char-0001").await;
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");

        let view = engine
            .session_messages(SessionMessagesRequest {
                session_id: session.id,
                limit: None,
                offset: None,
            })
            .await
            .expect("空会话应返回空消息集合");
        assert_eq!(view.session_id, session.id);
        assert_eq!(view.total, 0);
        assert!(view.messages.is_empty());
        assert!(!view.has_more);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话详情：元数据 + 消息页字段逐项；分页 has_more 口径与消息浏览一致。
    #[tokio::test]
    async fn session_detail_returns_metadata_and_message_page() {
        let (engine, storage, dir) = engine_with_db("browse-session-detail").await;
        seed_persona(&storage, "char-0001").await;
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        seed_messages(&storage, session.id, "char-0001", 5, 1_000).await;

        // 全量：元数据透传 + 消息时间正序 + has_more false
        let detail = engine
            .session_detail(session.id, None, None)
            .await
            .expect("详情读取应成功");
        assert_eq!(detail.id, session.id);
        assert_eq!(detail.started_at.timestamp_millis(), session.started_at);
        assert_eq!(detail.ended_at, None);
        assert_eq!(detail.persona_uid.as_deref(), Some("char-0001"));
        assert_eq!(detail.total_messages, 5);
        assert!(!detail.has_more);
        let contents: Vec<&str> = detail.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents[0], "消息内容 0");
        assert_eq!(contents[4], "消息内容 4");

        // 分页：最新 2 条（页内时间正序）+ has_more true
        let paged = engine
            .session_detail(session.id, Some(2), None)
            .await
            .expect("详情读取应成功");
        assert_eq!(paged.total_messages, 5);
        assert!(paged.has_more);
        let contents: Vec<&str> = paged.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["消息内容 3", "消息内容 4"]);

        // 不存在会话：业务校验错误
        let err = engine
            .session_detail(Uuid::new_v4(), None, None)
            .await
            .expect_err("不存在的会话应显式报错");
        assert_eq!(err.category(), "validation");
        assert!(err.to_string().contains("会话不存在"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话消息计数：正常计数；不存在会话按 0（诊断口径，不报错）。
    #[tokio::test]
    async fn count_session_messages_tolerates_missing_session() {
        let (engine, storage, dir) = engine_with_db("browse-count-messages").await;
        seed_persona(&storage, "char-0001").await;
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        seed_messages(&storage, session.id, "char-0001", 3, 1_000).await;

        assert_eq!(engine.count_session_messages(session.id).await, 3);
        assert_eq!(engine.count_session_messages(Uuid::new_v4()).await, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 按会话读取 L1：空会话返回空列表；写入后字段与存储一致（不报错）。
    #[tokio::test]
    async fn l1_by_session_returns_session_summaries() {
        let (engine, storage, dir) = engine_with_db("browse-l1-by-session").await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        // 空会话：空列表（非错误）
        let empty = engine
            .memory_l1_by_session(session_id)
            .await
            .expect("按会话查询应成功");
        assert!(empty.is_empty(), "无摘要会话应返回空列表");

        // 写入两条摘要：全部返回且字段透传
        for (idx, summary) in ["第一段", "第二段"].iter().enumerate() {
            let mut l1 = MemoryL1::new(session_id, summary.to_string(), None);
            l1.persona_uid = Some("char-0001".to_string());
            l1.created_at = 2_000 + idx as i64;
            storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
        }

        let items = engine
            .memory_l1_by_session(session_id)
            .await
            .expect("按会话查询应成功");
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| item.session_id == session_id));
        assert!(
            items
                .iter()
                .all(|item| item.persona_uid.as_deref() == Some("char-0001")),
            "归属应随摘要透传"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 通道概览：空通道为 0 / None；仅统计目标通道（local 不计入）；关闭后活跃数下降。
    #[tokio::test]
    async fn channel_overview_counts_active_and_latest_activity() {
        let (engine, storage, dir) = engine_with_db("browse-channel-overview").await;
        seed_persona(&storage, "char-0001").await;

        // 空通道：计数 0、无活动时间
        let empty = engine
            .channel_overview("mcp")
            .await
            .expect("统计空通道应成功");
        assert_eq!(empty.active_sessions, 0);
        assert_eq!(empty.last_activity_ms, None);

        // mcp 通道两个会话 + local 通道一个会话（local 不应计入 mcp 统计）
        seed_channel_session(&storage, "char-0001", "mcp", Some("client-A"), 1, 1_000).await;
        let s2 =
            seed_channel_session(&storage, "char-0001", "mcp", Some("client-B"), 1, 2_000).await;
        seed_channel_session(&storage, "char-0001", "local", None, 1, 9_000).await;

        let overview = engine
            .channel_overview("mcp")
            .await
            .expect("统计 mcp 通道应成功");
        assert_eq!(overview.active_sessions, 2, "两个 mcp 会话均活跃");
        assert_eq!(
            overview.last_activity_ms,
            Some(2_000),
            "最近活动取 mcp 通道消息，local 会话（9000）不计入"
        );

        // 关闭一个会话：活跃数下降；最近活动时间不受影响（消息仍在库中）
        storage.close_session(s2).await.expect("关闭会话应成功");
        let after = engine
            .channel_overview("mcp")
            .await
            .expect("再次统计应成功");
        assert_eq!(after.active_sessions, 1, "关闭一个会话后活跃数应下降");
        assert_eq!(after.last_activity_ms, Some(2_000));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
