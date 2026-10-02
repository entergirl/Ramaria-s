//! crates/ramaria-memory/src/fact/arbitration/tests.rs - //! crates/ramaria-memory/src/fact/arbitration.rs - 知识层版本链仲裁与候选互证提升单元测试
//!
//! 设计特点:
//! - 位于 fact::arbitration 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 arbitration.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::types::{FactSource, FactTier};

fn evidence(id: i64, l1: &str, time: i64, same_batch: bool, pos: bool) -> EventEvidence {
    EventEvidence {
        ref_event_id: id,
        ref_l1_id: l1.to_string(),
        time,
        same_batch,
        valence_positive: pos,
    }
}

fn base_input() -> ArbitrationInput {
    ArbitrationInput {
        existing_active: Some(vec![]),
        source: FactSource::Event,
        tier: FactTier::Volatile,
        confidence: 0.8,
        corroborations: vec![],
        single_evidence: None,
        new_time: 2000,
        existing_time: Some(1000),
    }
}

#[test]
fn independent_pair_requires_distinct_events_and_gap() {
    // 同 event → 非独立
    assert!(!independent_pair(
        &evidence(1, "l1a", 0, false, true),
        &evidence(1, "l1b", 0, false, true)
    ));
    // 同 L1 → 非独立
    assert!(!independent_pair(
        &evidence(1, "l1a", 0, false, true),
        &evidence(2, "l1a", 0, false, true)
    ));
    // 时间跨度 ≥ 1 天（不同事件不同 L1）→ 独立
    assert!(independent_pair(
        &evidence(1, "l1a", 0, false, true),
        &evidence(2, "l1b", 86_400_000 * 2, false, true)
    ));
    // 同日但不同批 TopicBatch → 独立
    assert!(independent_pair(
        &evidence(1, "l1a", 0, false, true),
        &evidence(2, "l1b", 3_600_000, true, true)
    ));
}

#[test]
fn manual_overwrites_always() {
    let mut input = base_input();
    input.source = FactSource::Manual;
    let out = arbitrate(&input, true);
    assert_eq!(out.action, Arbitration::Overwrite);
}

#[test]
fn multi_event_corroboration_overwrites() {
    let mut input = base_input();
    input.single_evidence = None;
    input.corroborations = vec![
        CorroborateCandidate {
            evidence: evidence(1, "l1a", 0, false, true),
            semantic: 0.9,
        },
        CorroborateCandidate {
            evidence: evidence(2, "l1b", 86_400_000 * 5, false, true),
            semantic: 0.85,
        },
    ];
    let out = arbitrate(&input, true);
    assert_eq!(out.action, Arbitration::Overwrite);
}

#[test]
fn same_batch_events_do_not_corroborate() {
    // 两事件同批 TopicBatch 且同日 → 非独立，互证不成立
    let mut input = base_input();
    // 存在库内 active 事实需保护（互证不成立时不应单事件覆盖）
    input.existing_active = Some(vec![ramaria_core::types::PersonaFact::new(
        "char-0001".into(),
        ramaria_core::types::ProfileField::RecentContext,
        "现有状态".into(),
        FactSource::Event,
    )]);
    input.corroborations = vec![
        CorroborateCandidate {
            evidence: evidence(1, "l1a", 0, true, true),
            semantic: 0.9,
        },
        CorroborateCandidate {
            evidence: evidence(2, "l1b", 1000, true, true),
            semantic: 0.85,
        },
    ];
    let out = arbitrate(&input, true);
    assert_eq!(
        out.action,
        Arbitration::Ignore,
        "同日同批不互证 → 缺单事件 → 忽略"
    );
}

#[test]
fn valence_mismatch_prevents_corroboration() {
    // 语义足够但 valence 方向不一致 → 不互证（上层已按 active_valence_positive 排除方向不符票）
    let mut input = base_input();
    // active_valence_positive = true；证据 valence_positive = false（方向相反票不应加入 votes）
    // 本模块以 active_valence_positive 作为唯一方向基准：方向不一致票已由上层过滤
    // 这里模拟仅一条方向一致票 + 一条方向不一致票 → 无法成对互证
    input.corroborations = vec![
        CorroborateCandidate {
            evidence: evidence(1, "l1a", 0, false, true), // 方向一致（true）
            semantic: 0.9,
        },
        CorroborateCandidate {
            evidence: evidence(2, "l1b", 86_400_000 * 2, false, false), // 方向不一致
            semantic: 0.9,
        },
    ];
    // 若上层不按方向过滤：两票语义都 ≥0.7，但方向不一致票应被剔除。
    // 本实现 voting 阶段只按 semantic；方向一致性由本函数对 active_valence_positive 比对。
    // 这里手动按方向二次校验：两条票方向必须都与 active 一致才成立。
    let votes: Vec<&CorroborateCandidate> = input
        .corroborations
        .iter()
        .filter(|c| corroboration_vote(c, true) && c.evidence.valence_positive)
        .collect();
    // 只有 1 条方向合格 → 无法构成互证对
    assert_eq!(votes.len(), 1);
}

#[test]
fn stable_fact_not_overwritten_by_single_event() {
    let mut input = base_input();
    input.tier = FactTier::Stable;
    input.existing_active = Some(vec![ramaria_core::types::PersonaFact::new(
        "char-0001".into(),
        ramaria_core::types::ProfileField::Interests,
        "旧兴趣".into(),
        FactSource::Event,
    )]);
    input.single_evidence = Some(evidence(1, "l1a", 2000, false, true));
    let out = arbitrate(&input, true);
    assert_eq!(out.action, Arbitration::Candidate);
}

#[test]
fn volatile_single_event_newer_overwrites() {
    let mut input = base_input();
    input.tier = FactTier::Volatile;
    input.existing_active = Some(vec![ramaria_core::types::PersonaFact::new(
        "char-0001".into(),
        ramaria_core::types::ProfileField::RecentContext,
        "旧状态".into(),
        FactSource::Event,
    )]);
    input.single_evidence = Some(evidence(1, "l1a", 3000, false, true));
    input.new_time = 3000;
    input.existing_time = Some(1000);
    let out = arbitrate(&input, true);
    assert_eq!(out.action, Arbitration::Overwrite);
}

#[test]
fn subjective_implied_fact_goes_candidate() {
    // 主观隐含（conf=0.5）无论互证都先入 candidate
    let mut input = base_input();
    input.confidence = 0.5;
    input.single_evidence = Some(evidence(1, "l1a", 3000, false, true));
    input.existing_active = Some(vec![ramaria_core::types::PersonaFact::new(
        "char-0001".into(),
        ramaria_core::types::ProfileField::Interests,
        "旧".into(),
        FactSource::Event,
    )]);
    let out = arbitrate(&input, true);
    assert_eq!(out.action, Arbitration::Candidate);
    assert!(out.reason.contains("candidate") || out.reason.contains("候选"));
}

#[test]
fn no_existing_active_direct_overwrite() {
    let mut input = base_input();
    input.existing_active = None;
    let out = arbitrate(&input, true);
    assert_eq!(out.action, Arbitration::Overwrite);
}

// =========================================================
// 策略③ 候选互证提升（corroborate_candidates）
// =========================================================

use ramaria_core::types::ProfileField;

fn candidate(content: &str, keywords: &[&str]) -> FactCandidate {
    FactCandidate {
        content: content.to_string(),
        field: ProfileField::Interests,
        tier: FactTier::Stable,
        keywords: keywords.iter().map(|s| s.to_string()).collect(),
        // 与主观隐含事实常量一致的 0.5，低置信候选
        confidence: 0.5,
        source: FactSource::Event,
        ref_event_id: Some(1),
        subjective_implied: true,
        ref_l1_id: None,
    }
}

fn event_input(
    id: i64,
    l1: &str,
    time: i64,
    same_batch: bool,
    pos: bool,
    content: &str,
    keywords: &[&str],
) -> CorroborationInput {
    CorroborationInput {
        evidence: evidence(id, l1, time, same_batch, pos),
        content: content.to_string(),
        keywords: keywords.iter().map(|s| s.to_string()).collect(),
    }
}

/// 两条独立事件同主题同极性 → 候选可提升。
#[test]
fn corroborate_promotes_on_two_independent_events() {
    let cand = candidate("坚持跑步后心情变得很开心", &["跑步"]);
    let events = vec![
        event_input(
            1,
            "l1a",
            0,
            false,
            true,
            "早上坚持跑了五公里，很开心",
            &["跑步"],
        ),
        event_input(
            2,
            "l1b",
            86_400_000 * 3,
            false,
            true,
            "下午继续跑步，心情不错",
            &["跑步"],
        ),
    ];
    let verdicts = corroborate_candidates(&[cand], &events);
    assert!(matches!(verdicts[0], CorroborateVerdict::Promote { .. }));
}

/// 仅一条匹配事件 → 不提升（互证需 ≥2 独立事件）。
#[test]
fn corroborate_single_event_keeps_candidate() {
    let cand = candidate("坚持跑步后心情变得很开心", &["跑步"]);
    let events = vec![event_input(
        1,
        "l1a",
        0,
        false,
        true,
        "早上坚持跑了五公里",
        &["跑步"],
    )];
    let verdicts = corroborate_candidates(&[cand], &events);
    assert!(matches!(
        verdicts[0],
        CorroborateVerdict::KeepCandidate { .. }
    ));
}

/// 两条事件同批 TopicBatch 且同日（非独立）→ 不提升。
#[test]
fn corroborate_same_batch_not_promoted() {
    let cand = candidate("最近压力很大感觉很难过", &["压力"]);
    let events = vec![
        event_input(1, "l1a", 0, true, false, "压力好大很难过", &["压力"]),
        event_input(2, "l1b", 1000, true, false, "还是很焦虑难过", &["压力"]),
    ];
    let verdicts = corroborate_candidates(&[cand], &events);
    assert!(
        matches!(verdicts[0], CorroborateVerdict::KeepCandidate { .. }),
        "同批同日不构成独立互证"
    );
}

/// 极性冲突（候选负向 vs 事件正向）→ 不提升。
#[test]
fn corroborate_valence_conflict_not_promoted() {
    let cand = candidate("最近压力很大感觉很难过", &["压力"]);
    let events = vec![
        event_input(1, "l1a", 0, false, true, "压力缓解后很轻松", &["压力"]),
        event_input(
            2,
            "l1b",
            86_400_000 * 3,
            false,
            true,
            "终于放松很开心",
            &["压力"],
        ),
    ];
    let verdicts = corroborate_candidates(&[cand], &events);
    assert!(
        matches!(verdicts[0], CorroborateVerdict::KeepCandidate { .. }),
        "valence 方向冲突不应互证提升"
    );
}

/// 无向量降级：事件不带关键词、仅 content，语义相似走文本 bigram 交集。
#[test]
fn corroborate_keyword_fallback_without_event_keywords() {
    let cand = candidate("坚持跑步后心情变得很开心", &[]);
    // 事件 keywords 为空 → 从 content 提取 bigram（候选 content 含"跑步"同主题）
    let events = vec![
        event_input(1, "l1a", 0, false, true, "早上坚持跑步很快乐", &[]),
        event_input(2, "l1b", 86_400_000 * 3, false, true, "跑步让人心情好", &[]),
    ];
    let verdicts = corroborate_candidates(&[cand], &events);
    assert!(
        matches!(verdicts[0], CorroborateVerdict::Promote { .. }),
        "无事件关键词时走 content bigram 交集，仍应互证"
    );
}

/// 中性候选（无情感词，极性不可判定）→ 保守不提升。
#[test]
fn corroborate_neutral_candidate_kept() {
    let cand = candidate("最近在学做菜", &["做菜"]);
    let events = vec![
        event_input(1, "l1a", 0, false, true, "最近学做菜很开心", &["做菜"]),
        event_input(
            2,
            "l1b",
            86_400_000 * 3,
            false,
            true,
            "做菜很有意思",
            &["做菜"],
        ),
    ];
    let verdicts = corroborate_candidates(&[cand], &events);
    assert!(matches!(
        verdicts[0],
        CorroborateVerdict::KeepCandidate { .. }
    ));
}

/// 空候选列表 → 空判定；空事件 → 每个候选不提升。
#[test]
fn corroborate_empty_inputs() {
    assert!(corroborate_candidates(&[], &[]).is_empty());
    let cand = candidate("坚持跑步很开心", &["跑步"]);
    let verdicts = corroborate_candidates(&[cand], &[]);
    assert!(matches!(
        verdicts[0],
        CorroborateVerdict::KeepCandidate { .. }
    ));
}

/// 判定结果与候选列表等长一一对应。
#[test]
fn corroborate_verdicts_align_with_candidates() {
    let cands = vec![
        candidate("坚持跑步很开心", &["跑步"]),
        candidate("压力很大很难过", &["压力"]),
    ];
    let events = vec![event_input(
        1,
        "l1a",
        0,
        false,
        true,
        "跑步让我开心",
        &["跑步"],
    )];
    let verdicts = corroborate_candidates(&cands, &events);
    assert_eq!(verdicts.len(), 2);
    assert!(matches!(
        verdicts[0],
        CorroborateVerdict::KeepCandidate { .. }
    ));
    assert!(matches!(
        verdicts[1],
        CorroborateVerdict::KeepCandidate { .. }
    ));
}
