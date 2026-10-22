//! crates/ramaria-service/src/fact_extract/candidate.rs - Ramaria 事实候选处理
//!
//! 设计特点:
//! - 判重：内容级精确去重保幂等，向量语义判重可选（embedding 不可用时保守降级）
//! - 常规轨道按置信与仲裁结论决定新增 / 覆盖 / 候选
//! - 低置信候选经互证提升守卫后落 active，未互证保持 candidate
//! - 落库统一走 storage fact API，覆盖一律经版本链

use std::collections::HashMap;

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingProvider, StorageBackend};
use ramaria_core::types::{FactStatus, PersonaFact, now_ms};
use ramaria_memory::fact::arbitration::{EventEvidence, arbitrate};
use ramaria_memory::fact::{
    Arbitration, ArbitrationInput, CorroborateVerdict, CorroborationInput, DedupInput,
    DedupVerdict, FactCandidate, check_dedup, corroborate_candidates,
};
use tracing::{debug, warn};

/// 对候选的单条处理结论（供主循环聚合计数）。
#[derive(Debug, Default)]
pub(super) struct CandidateOutcome {
    pub(super) deduped: bool,
    pub(super) promoted_active: bool,
    pub(super) overwritten: bool,
    pub(super) saved_candidate: bool,
    pub(super) ignored: bool,
    pub(super) errored: bool,
    /// content-free 决策摘要（可选）
    pub(super) reason: Option<String>,
}

impl CandidateOutcome {
    fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}
// =========================================================
// 候选处理（判重 / 仲裁 / 互证 / 落库）
// =========================================================

/// 处理单个候选：判重后按置信轨道仲裁或互证提升，并落库。
pub(super) async fn handle_candidate(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    cand: &FactCandidate,
    pool: &[CorroborationInput],
    event_time: &HashMap<i64, i64>,
    embedder: Option<&dyn EmbeddingProvider>,
) -> CandidateOutcome {
    // ---- 读取同 field 库内事实（失败 → 跳过该候选，保守不写）----
    let active = match storage
        .list_active_facts_by_field(persona_uid, cand.field)
        .await
    {
        Ok(f) => f,
        Err(e) => {
            warn!(
                persona_uid,
                field = %cand.field.as_str(),
                error = %e,
                "事实抽取：读取同 field active 事实失败，跳过该候选"
            );
            return CandidateOutcome {
                errored: true,
                ..Default::default()
            };
        }
    };
    let all_same_field = match storage.list_facts_by_persona(persona_uid, cand.field).await {
        Ok(f) => f,
        Err(e) => {
            warn!(
                persona_uid,
                field = %cand.field.as_str(),
                error = %e,
                "事实抽取：读取同 field 全量事实失败，跳过该候选"
            );
            return CandidateOutcome {
                errored: true,
                ..Default::default()
            };
        }
    };

    // ---- 判重 ----
    if dedup_candidate(cand, &active, &all_same_field, embedder).await {
        return CandidateOutcome {
            deduped: true,
            ..Default::default()
        }
        .reason("dedup_content_or_semantic");
    }

    // ---- 轨道分派 ----
    let old = active.first().cloned();
    if cand.confidence >= 0.6 {
        handle_regular(storage, persona_uid, cand, old.as_ref(), event_time).await
    } else {
        handle_low_confidence(storage, persona_uid, cand, pool).await
    }
}

/// 内容级 + 向量语义判重（命中任一即视为重复）。
async fn dedup_candidate(
    cand: &FactCandidate,
    active: &[PersonaFact],
    all_same_field: &[PersonaFact],
    embedder: Option<&dyn EmbeddingProvider>,
) -> bool {
    // 规则 A：内容级精确去重（覆盖 active + candidate，含幂等兜底；superseded 不参与防回捞）
    if all_same_field.iter().any(|f| {
        matches!(f.status, FactStatus::Active | FactStatus::Candidate) && f.content == cand.content
    }) {
        return true;
    }
    // 规则 B：向量语义判重（embedding 可用时对 active 逐条双条件判重）
    let Some(emb) = embedder else {
        return false;
    };
    let new_vector = match emb.embed(&cand.content).await {
        Ok(v) => Some(v),
        Err(e) => {
            debug!(error = %e, "事实抽取：候选向量计算失败，仅走内容级判重");
            None
        }
    };
    if active.is_empty() || new_vector.is_none() {
        return false;
    }
    let mut existing_vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(active.len());
    let mut vectors_ready = true;
    for f in active {
        match emb.embed(&f.content).await {
            Ok(v) => existing_vectors.push(Some(v)),
            Err(e) => {
                debug!(error = %e, "事实抽取：库内事实向量计算失败，单条降级");
                existing_vectors.push(None);
                vectors_ready = false;
            }
        }
    }
    let _ = vectors_ready; // 单条失败仅影响该条语义判定（None → 不判重复）
    let input = DedupInput {
        existing: active.to_vec(),
        new_keywords: cand.keywords.clone(),
        new_vector,
    };
    matches!(
        check_dedup(&input, &existing_vectors),
        DedupVerdict::Duplicate
    )
}

/// 常规轨道（confidence ≥ 0.6）：经 arbitrate 判定覆盖/候选/忽略。
async fn handle_regular(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    cand: &FactCandidate,
    old: Option<&PersonaFact>,
    event_time: &HashMap<i64, i64>,
) -> CandidateOutcome {
    // 事件时间驱动"时间新者胜"；缺时间时用当前时间（保守兜底）
    let ts = cand
        .ref_event_id
        .and_then(|eid| event_time.get(&eid).copied())
        .unwrap_or_else(now_ms);
    let single_evidence = cand.ref_event_id.map(|eid| EventEvidence {
        ref_event_id: eid,
        ref_l1_id: cand.ref_l1_id.map(|u| u.to_string()).unwrap_or_default(),
        time: ts,
        same_batch: true,
        valence_positive: true,
    });
    let arb_input = ArbitrationInput {
        existing_active: old.map(|o| vec![o.clone()]),
        source: cand.source,
        tier: cand.tier,
        confidence: cand.confidence,
        corroborations: Vec::new(),
        single_evidence,
        new_time: ts,
        existing_time: old.map(|o| o.created_at),
    };
    let outcome = arbitrate(&arb_input, true);
    match outcome.action {
        Arbitration::Overwrite => persist_active(storage, persona_uid, cand, old).await,
        Arbitration::Candidate => {
            match save_fact_with_status(storage, persona_uid, cand, FactStatus::Candidate).await {
                Ok(_) => CandidateOutcome {
                    saved_candidate: true,
                    ..Default::default()
                },
                Err(_e) => CandidateOutcome {
                    errored: true,
                    ..Default::default()
                }
                .reason("save_candidate_error"),
            }
        }
        Arbitration::Ignore => CandidateOutcome {
            ignored: true,
            ..Default::default()
        },
    }
}

/// 低置信轨道（主观隐含 0.5 / L1 线索 0.55）：互证 Promote → active；否则保持 candidate。
async fn handle_low_confidence(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    cand: &FactCandidate,
    pool: &[CorroborationInput],
) -> CandidateOutcome {
    // 排除候选自身的同源事件（同 event / 同 L1）作为佐证——不能自己佐证自己
    let filtered: Vec<CorroborationInput> = pool
        .iter()
        .filter(|e| {
            if cand.ref_event_id == Some(e.evidence.ref_event_id) {
                return false;
            }
            if let Some(l1_id) = cand.ref_l1_id
                && e.evidence.ref_l1_id == l1_id.to_string()
            {
                return false;
            }
            true
        })
        .cloned()
        .collect();

    let verdicts = corroborate_candidates(std::slice::from_ref(cand), &filtered);
    let Some(verdict) = verdicts.first() else {
        // 理论上与输入等长；防御性兜底保持 candidate
        return CandidateOutcome {
            errored: true,
            ..Default::default()
        }
        .reason("corroborate_empty_verdict");
    };

    match verdict {
        CorroborateVerdict::Promote { .. } => {
            // 互证成立（≥2 独立事件 + valence 一致）：提升为 active。
            // 覆盖已有 active（版本链推进）仅由常规轨道 arbitrate 授权；
            // 低置信 Promote 一律新增 active，绝不擅自覆盖旧 active（版本链红线）。
            match save_fact_with_status(storage, persona_uid, cand, FactStatus::Active).await {
                Ok(_) => CandidateOutcome {
                    promoted_active: true,
                    ..Default::default()
                },
                Err(e) => CandidateOutcome {
                    errored: true,
                    ..Default::default()
                }
                .reason(format!("save_fact_error:{e}")),
            }
        }
        CorroborateVerdict::KeepCandidate { .. } => {
            match save_fact_with_status(storage, persona_uid, cand, FactStatus::Candidate).await {
                Ok(_) => CandidateOutcome {
                    saved_candidate: true,
                    ..Default::default()
                },
                Err(e) => CandidateOutcome {
                    errored: true,
                    ..Default::default()
                }
                .reason(format!("save_candidate_error:{e}")),
            }
        }
    }
}

// =========================================================
// 落库 helper（统一走 storage fact API，严禁绕过版本链）
// =========================================================

/// 落 active：有旧 active 且经仲裁授权覆盖 → 版本链；否则新增 active。
async fn persist_active(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    cand: &FactCandidate,
    old: Option<&PersonaFact>,
) -> CandidateOutcome {
    if let Some(old_fact) = old {
        let fresh = candidate_to_fact(persona_uid, cand, FactStatus::Active);
        match storage.save_fact_with_version(old_fact, &fresh).await {
            Ok(_) => CandidateOutcome {
                overwritten: true,
                ..Default::default()
            },
            Err(e) => CandidateOutcome {
                errored: true,
                ..Default::default()
            }
            .reason(format!("save_fact_with_version_error:{e}")),
        }
    } else {
        let fact = candidate_to_fact(persona_uid, cand, FactStatus::Active);
        match storage.save_fact(&fact).await {
            Ok(_) => CandidateOutcome {
                promoted_active: true,
                ..Default::default()
            },
            Err(e) => CandidateOutcome {
                errored: true,
                ..Default::default()
            }
            .reason(format!("save_fact_error:{e}")),
        }
    }
}

/// 按指定状态落库（candidate 或普通 active 写入）。
async fn save_fact_with_status(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    cand: &FactCandidate,
    status: FactStatus,
) -> RamariaResult<i64> {
    let fact = candidate_to_fact(persona_uid, cand, status);
    storage.save_fact(&fact).await
}

/// 候选 → 可落库的 PersonaFact（版本链/状态由调用方语义决定）。
fn candidate_to_fact(persona_uid: &str, c: &FactCandidate, status: FactStatus) -> PersonaFact {
    let mut f = PersonaFact::new(
        persona_uid.to_string(),
        c.field,
        c.content.clone(),
        c.source,
    );
    f.status = status;
    f.tier = c.tier;
    f.confidence = c.confidence;
    f.keyword_hint = if c.keywords.is_empty() {
        None
    } else {
        Some(c.keywords.join(","))
    };
    f.ref_event_id = c.ref_event_id;
    f.ref_l1_id = c.ref_l1_id;
    f
}
