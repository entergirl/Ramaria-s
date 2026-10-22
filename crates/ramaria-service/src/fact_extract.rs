//! crates/ramaria-service/src/fact_extract.rs - 知识事实自动抽取编排（auto_fact_detect 增强层）
//!
//! 设计特点:
//! - 总开关门控：`knowledge.auto_fact_detect == true` 才执行任何抽取；关闭时直接返回零报告
//!   （不抽取、不落库）。
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

mod candidate;
mod evidence;

use std::collections::HashMap;

use ramaria_core::config::KnowledgeConfig;
use ramaria_core::traits::{EmbeddingProvider, StorageBackend};
use ramaria_core::types::{MemoryEvent, MemoryL1};
use ramaria_memory::fact::{
    FactCandidate, RuleExtractor, extract_from_l1_evidence, extract_implied_fact_from_event,
    should_extract,
};
use tracing::{debug, info};

use candidate::handle_candidate;
use evidence::build_corroboration_pool;

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

    // 总开关门控：关闭 = 不抽取、不落库
    if !config.auto_fact_detect {
        report.gated_off = true;
        info!(persona_uid, "auto_fact_detect 关闭，跳过知识事实抽取");
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
// 小工具
// =========================================================

/// reason 摘要追加（限长防日志膨胀；不含原文）。
fn push_reason(reasons: &mut Vec<String>, reason: impl Into<String>) {
    if reasons.len() >= 32 {
        return;
    }
    let r: String = reason.into();
    let r = ramaria_core::text::truncate_chars_bare(&r, 80);
    reasons.push(r);
}
// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockLlm, engine_with_llm_and_config, seed_persona};
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::StoreCrud;

    /// 开关门控：`auto_fact_detect = false` → 零报告且不写任何事实。
    #[tokio::test]
    async fn gated_off_writes_nothing() {
        let mut config = RamariaConfig::default();
        config.knowledge.auto_fact_detect = false;
        let (engine, storage, dir) =
            engine_with_llm_and_config("fact-gate", MockLlm::local(), config).await;
        seed_persona(&storage, "char-0001").await;

        let report = run_fact_extraction(
            engine.storage_ref().as_ref(),
            &engine.config().knowledge,
            "char-0001",
            &[],
            &[],
            None,
        )
        .await;

        assert!(report.gated_off, "开关关闭应标记 gated_off");
        assert!(
            storage
                .list_active_facts_by_persona("char-0001")
                .await
                .expect("读取事实应成功")
                .is_empty(),
            "开关关闭不应写入任何事实"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
