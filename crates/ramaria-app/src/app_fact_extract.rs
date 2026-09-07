//! crates/ramaria-app/src/app_fact_extract.rs - 知识事实自动抽取编排（auto_fact_detect 增强层）
//!
//! 设计特点:
//! - 总开关门控：`knowledge.auto_fact_detect == true` 才执行任何抽取；关闭时直接返回零报告，
//!   行为与上一版本完全一致（不抽取、不落库）。
//! - 常规轨道：客观/混合且置信达标事件沿用 `RuleExtractor`（规则兜底基线）。
//! - 增强轨道（召回兜底，非主力）：主观/低置信事件走策略①字段感知隐含候选；
//!   本批 L1 的 evidence_notes 逐条走策略②线索→断言候选（persona 归属校验）。
//! - 判重：同 field 内容级精确去重（无向量兜底，保幂等）+ `check_dedup` 向量语义（可选，
//!   embedding 不可用保守降级为不误杀）。
//! - 互证提升：低置信候选经 `corroborate_candidates`（策略③，≥2 独立事件 + valence 一致）
//!   守卫后落 active；未互证保持 candidate。
//! - 版本链：常规轨道经 `arbitrate` 判定覆盖/候选；`Overwrite` 走 `save_fact_with_version`，
//!   不破坏既有仲裁红线（stable 不单事件覆盖、极性冲突不提升）。
//! - 静默降级：storage 单条失败记 warn 计数后继续，单条不影响整批。
//! - 报告只含计数与 content-free 的 reason 摘要，不含原文/摘要/关键词。

use std::collections::{HashMap, HashSet};

use ramaria_core::config::KnowledgeConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingProvider, StorageBackend};
use ramaria_core::types::{FactStatus, MemoryEvent, MemoryL1, PersonaFact, now_ms};
use ramaria_memory::fact::arbitration::{EventEvidence, arbitrate};
use ramaria_memory::fact::{
    Arbitration, ArbitrationInput, CorroborateVerdict, CorroborationInput, DedupInput,
    DedupVerdict, FactCandidate, RuleExtractor, check_dedup, corroborate_candidates,
    extract_from_l1_evidence, extract_implied_fact_from_event, should_extract,
};
use tracing::{debug, info, warn};

/// 事实抽取报告：仅数值与 reason 摘要，不含原文。
#[derive(Debug, Clone, Default)]
pub struct FactExtractionReport {
    /// 开关关闭（未执行任何抽取）
    pub gated_off: bool,
    /// 常规轨道候选数（客观/混合达标事件）
    pub regular_candidates: usize,
    /// 策略① 隐含候选数（主观/低置信事件）
    pub implied_candidates: usize,
    /// 策略② L1 线索候选数
    pub l1_candidates: usize,
    /// 判重拦截数（内容级/向量语义）
    pub deduped: usize,
    /// 提升为 active 数（互证 Promote / 常规无旧事实直接入库）
    pub promoted_active: usize,
    /// 版本链覆盖数（save_fact_with_version）
    pub overwritten: usize,
    /// 落 candidate 数
    pub candidates_saved: usize,
    /// 仲裁忽略数（保守不动）
    pub ignored: usize,
    /// 存储/读取错误数（静默降级）
    pub errors: usize,
    /// content-free 的决策摘要（有长度上限，防日志膨胀）
    pub reasons: Vec<String>,
}

/// 对候选的单条处理结论（供主循环聚合计数）。
#[derive(Debug, Default)]
struct CandidateOutcome {
    deduped: bool,
    promoted_active: bool,
    overwritten: bool,
    saved_candidate: bool,
    ignored: bool,
    errored: bool,
    /// content-free 决策摘要（可选）
    reason: Option<String>,
}

impl CandidateOutcome {
    fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

// =========================================================
// 编排入口
// =========================================================

/// 执行 persona 的一批新 L1 / 新事件的 auto_fact_detect 事实抽取。
///
/// 用法:
/// - 由 L2 离线学习管线在"某 persona 的事件提取成功、本批 L1 与新建事件均已落库"后调用一次；
///   调用方保证传入的是"本批新产"数据（幂等由本函数判重兜底，不在此做全局扫描重建）。
///
/// 参数:
/// - `storage`: 存储后端（事实读取/落库统一走 storage trait）。
/// - `config`: [knowledge] 配置；`auto_fact_detect=false` 时直接返回零报告。
/// - `persona_uid`: 目标 persona（严格隔离）。
/// - `new_l1`: 本批新 L1（策略②线索来源；含 evidence_notes 才产出候选）。
/// - `new_events`: 本批新事件（常规 + 策略①来源；同时作为互证事件池）。
/// - `embedder`: 可选语义向量计算器；None 时判重走内容级精确去重（保守降级）。
///
/// 返回:
/// - `FactExtractionReport`：各策略计数 + content-free 决策摘要；不含原文。
pub async fn run_fact_extraction(
    storage: &dyn StorageBackend,
    config: &KnowledgeConfig,
    persona_uid: &str,
    new_l1: &[MemoryL1],
    new_events: &[MemoryEvent],
    embedder: Option<&dyn EmbeddingProvider>,
) -> FactExtractionReport {
    let mut report = FactExtractionReport::default();

    // 总开关门控：关闭 = 与上一版本完全一致（不抽取、不落库）
    if !config.auto_fact_detect {
        report.gated_off = true;
        info!(
            persona_uid,
            "auto_fact_detect 关闭，跳过知识事实抽取（与上一版本行为一致）"
        );
        return report;
    }

    info!(
        persona_uid,
        l1_count = new_l1.len(),
        event_count = new_events.len(),
        "auto_fact_detect 开启，开始知识事实抽取增强层"
    );

    // ---- 1. 收集候选（常规轨道 + 策略① + 策略②）----
    let mut candidates: Vec<FactCandidate> = Vec::new();
    for ev in new_events {
        // persona 隔离：事件必须归属目标 persona
        if ev.persona_uid != persona_uid {
            continue;
        }
        if should_extract(ev) {
            // 常规轨道（客观/混合且达标）：沿用规则兜底基线，仅取常规产物
            for c in RuleExtractor::extract_from_event(ev) {
                if !c.subjective_implied {
                    candidates.push(c);
                    report.regular_candidates += 1;
                }
            }
        } else if let Some(c) = extract_implied_fact_from_event(ev) {
            // 策略①：主观/低置信事件 → 字段感知隐含候选（替代规则固定 Interests 分支）
            candidates.push(c);
            report.implied_candidates += 1;
        }
    }
    for l1 in new_l1 {
        if let Some(notes) = &l1.evidence_notes {
            for note in notes {
                // 策略②：线索→断言候选；persona 不一致由函数内部拒绝
                if let Some(c) = extract_from_l1_evidence(persona_uid, l1, note) {
                    candidates.push(c);
                    report.l1_candidates += 1;
                }
            }
        }
    }

    if candidates.is_empty() {
        debug!(persona_uid, "事实抽取：本批无候选，跳过判重/互证/落库");
        return report;
    }

    // ---- 2. 互证事件池（本批新事件 + 库内最近事件，读取失败降级为空）----
    let pool = build_corroboration_pool(storage, persona_uid, new_events).await;

    // 新事件时间索引（供常规轨道单事件仲裁的"时间新者胜"判断）
    let mut event_time: HashMap<i64, i64> = HashMap::with_capacity(new_events.len());
    for ev in new_events {
        event_time.insert(ev.id, ev.start);
    }

    // ---- 3. 逐候选判重 → 仲裁/互证 → 落库 ----
    for cand in candidates {
        let outcome =
            handle_candidate(storage, persona_uid, &cand, &pool, &event_time, embedder).await;
        report.deduped += usize::from(outcome.deduped);
        report.promoted_active += usize::from(outcome.promoted_active);
        report.overwritten += usize::from(outcome.overwritten);
        report.candidates_saved += usize::from(outcome.saved_candidate);
        report.ignored += usize::from(outcome.ignored);
        report.errors += usize::from(outcome.errored);
        if let Some(r) = outcome.reason {
            push_reason(&mut report.reasons, r);
        }
    }

    info!(
        persona_uid,
        regular = report.regular_candidates,
        implied = report.implied_candidates,
        l1_evidence = report.l1_candidates,
        deduped = report.deduped,
        promoted = report.promoted_active,
        overwritten = report.overwritten,
        candidates_saved = report.candidates_saved,
        ignored = report.ignored,
        errors = report.errors,
        "auto_fact_detect 事实抽取完成"
    );
    report
}

// =========================================================
// 候选处理（判重 / 仲裁 / 互证 / 落库）
// =========================================================

/// 处理单个候选：判重后按置信轨道仲裁或互证提升，并落库。
async fn handle_candidate(
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

// =========================================================
// 互证事件池组装
// =========================================================

/// 组装该 persona 的既有事件摘要（本批 new_events + storage 最近事件，受限条数）。
///
/// 说明:
/// - 本批事件标记 `same_batch=true`（同批 TopicBatch 非独立维度）；库内最近事件标记 false。
/// - 每条事件的来源 L1 通过 event_sources 反查；反查失败记空（空 L1 → 不与他事件互证，
///   宁缺毋滥）。读取失败只 warn 并继续，不阻塞抽取主流程。
async fn build_corroboration_pool(
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

/// reason 摘要追加（限长防日志膨胀；不含原文）。
fn push_reason(reasons: &mut Vec<String>, reason: impl Into<String>) {
    if reasons.len() >= 32 {
        return;
    }
    let mut r = reason.into();
    if r.chars().count() > 80 {
        r = r.chars().take(80).collect();
    }
    reasons.push(r);
}
