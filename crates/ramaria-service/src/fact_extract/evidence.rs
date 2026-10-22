//! crates/ramaria-service/src/fact_extract/evidence.rs - Ramaria 互证事件池组装
//!
//! 设计特点:
//! - 互证池由本批事件与库内最近事件合成，同批来源标记为非独立维度
//! - 每条事件的来源 L1 经 event_sources 反查，失败按空源处理（宁缺毋滥）
//! - 读取失败只降级不阻塞抽取主流程

use std::collections::HashSet;

use ramaria_core::traits::StorageBackend;
use ramaria_core::types::MemoryEvent;
use ramaria_memory::fact::CorroborationInput;
use ramaria_memory::fact::arbitration::EventEvidence;
use tracing::{debug, warn};

// =========================================================
// 互证事件池组装
// =========================================================

/// 组装该 persona 的既有事件摘要（本批 new_events + storage 最近事件，受限条数）。
///
/// 说明:
/// - 本批事件标记 `same_batch=true`（同批 TopicBatch 非独立维度）；库内最近事件标记 false。
/// - 每条事件的来源 L1 通过 event_sources 反查；反查失败记空（空 L1 → 不与他事件互证，
///   宁缺毋滥）。读取失败只 warn 并继续，不阻塞抽取主流程。
pub(super) async fn build_corroboration_pool(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    new_events: &[MemoryEvent],
) -> Vec<CorroborationInput> {
    let mut seen: HashSet<i64> = HashSet::with_capacity(new_events.len());
    let mut pool: Vec<CorroborationInput> = Vec::new();
    for ev in new_events {
        if ev.persona_uid != persona_uid {
            continue;
        }
        seen.insert(ev.id);
        pool.push(corroboration_from_event(storage, ev, true).await);
    }
    // 库内最近事件作为不同批佐证来源（同样按 persona 隔离，storage 已过滤）
    let recent = match storage.list_recent_events(persona_uid, 64).await {
        Ok(events) => events,
        Err(e) => {
            warn!(persona_uid, error = %e, "事实抽取：读取最近事件失败，互证池仅含本批事件");
            Vec::new()
        }
    };
    for ev in recent {
        if seen.contains(&ev.id) {
            continue;
        }
        seen.insert(ev.id);
        pool.push(corroboration_from_event(storage, &ev, false).await);
    }
    pool
}

/// 事件 → 互证输入（来源 L1 反查失败用空串，表示无法判定的保守源）。
async fn corroboration_from_event(
    storage: &dyn StorageBackend,
    ev: &MemoryEvent,
    same_batch: bool,
) -> CorroborationInput {
    let l1_label = source_l1_label(storage, ev.id).await;
    let content = ev
        .paraphrase
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| ev.summary.clone());
    CorroborationInput {
        evidence: EventEvidence {
            ref_event_id: ev.id,
            ref_l1_id: l1_label,
            time: ev.start,
            same_batch,
            valence_positive: ev.valence >= 0.0,
        },
        content,
        keywords: split_keywords(ev.keywords.as_deref()),
    }
}

/// 按事件 id 反查首条来源 L1（失败/缺失 → 空串）。
async fn source_l1_label(storage: &dyn StorageBackend, event_id: i64) -> String {
    match storage.list_event_sources_by_event(event_id).await {
        Ok(list) => list
            .first()
            .map(|s| s.l1_id.to_string())
            .unwrap_or_default(),
        Err(e) => {
            debug!(event_id, error = %e, "事实互证：反查事件来源 L1 失败，按空源处理");
            String::new()
        }
    }
}

// =========================================================
// 小工具
// =========================================================

/// 关键词拆分：与抽取侧解析口径一致（逗号/顿号/英文逗号，去空白）。
fn split_keywords(raw: Option<&str>) -> Vec<String> {
    raw.map(|s| {
        s.split([',', '，', '、'])
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect()
    })
    .unwrap_or_default()
}
