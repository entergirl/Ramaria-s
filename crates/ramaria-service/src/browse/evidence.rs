//! crates/ramaria-service/src/browse/evidence.rs - Ramaria 性格标签证据链浏览
//!
//! 设计特点:
//! - 完整溯源链：trait → trait_evidence → memory_events → event_sources → memory_l1 → evidence_notes
//! - 单条查询失败隔离：事件不存在 / L1 溯源查询失败 / L1 不存在均记录告警后跳过，不阻塞整条证据链
//! - 事件预加载：一次取回该人格的近期事件构建查找表，避免逐条查询事件
//! - 统计 support / contradict / neutral 三类证据数量分布

use std::collections::HashMap;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{EvidenceDirection, MemoryEvent};

use crate::engine::Engine;
use crate::types::{
    EvidenceEventView, EvidenceL1SourceView, TraitEvidenceRequest, TraitEvidenceView,
};

use super::view::require_persona_uid;

// =========================================================
// 常量
// =========================================================

/// 证据链构建时的事件扫描上限（一次取回该人格的近期事件）。
const MAX_EVENTS_SCAN: i64 = 5000;

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
