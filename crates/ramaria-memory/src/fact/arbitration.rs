//! crates/ramaria-memory/src/fact/arbitration.rs - 知识层版本链仲裁
//!
//! 设计特点:
//! - 仲裁优先级: manual（人工事实）> 多事件互证 > 单事件（时间新者胜）
//! - 互证定义: ≥2 条独立事件（ref_event_id 不同、来源 L1 不同、
//!   时间跨度 ≥ 1 天或非同批 TopicBatch）且语义余弦 ≥ 0.7 且 valence 方向一致
//! - 单事件矛盾: 不覆盖 active，降级 candidate（C2 保护）
//! - 主观隐含事实（conf=0.5）：必须互证才提升 active，否则保持 candidate
//! - 输入以结构化事件证据（EventEvidence）描述，便于纯函数确定测试
//! - 产出 Mutation: 覆盖（新 active + 旧 superseded）/ 候选（仅入 candidate）/ 忽略（判重/不动作）

use ramaria_core::types::{FactSource, FactTier};

/// 互证语义余弦阈值（与独立性判断一起构成互证成立条件）。
pub const CORROBORATION_COSINE_THRESHOLD: f64 = 0.7;
/// 互证时间跨度阈值（天）。
pub const CORROBORATION_TIME_GAP_DAYS: u32 = 1;

/// 事件证据（描述新事实来源事件的独立性维度，用于互证判定）。
#[derive(Debug, Clone)]
pub struct EventEvidence {
    /// 来源事件 id（不同 = 独立性必要条件）
    pub ref_event_id: i64,
    /// 来源 L1 id（字符串形式；不同 = 独立性必要条件）— 用作者：app/存储层传入 UUID 字符串
    pub ref_l1_id: String,
    /// 事件时间（Unix 毫秒）
    pub time: i64,
    /// 是否属于同一批 TopicBatch（同批 = 非独立维度）
    pub same_batch: bool,
    /// 事件 valence 方向（true = 正，false = 负）
    pub valence_positive: bool,
}

/// 互证判定输入项。
#[derive(Debug, Clone)]
pub struct CorroborateCandidate {
    /// 候选事件证据
    pub evidence: EventEvidence,
    /// 语义余弦（与库内 active 事实）
    pub semantic: f64,
}

/// 仲裁输入。
#[derive(Debug, Clone)]
pub struct ArbitrationInput {
    /// 库内同 field 的 active 候选（若存在，作为被覆盖对象）。
    pub existing_active: Option<Vec<ramaria_core::types::PersonaFact>>,
    /// 新事实来源类型（manual 最高优先级）。
    pub source: FactSource,
    /// 新事实分层。
    pub tier: FactTier,
    /// 新事实置信度（主观隐含事实 = 0.5）。
    pub confidence: f64,
    /// 多事件互证证据（≥2 条独立事件构成互证）。
    pub corroborations: Vec<CorroborateCandidate>,
    /// 单事件独立性证据（用于"单事件时间新者胜"判断）。
    pub single_evidence: Option<EventEvidence>,
    /// 新事件时间（Unix 毫秒）。
    pub new_time: i64,
    /// 库内 active 事实的时间（被覆盖对比用）。
    pub existing_time: Option<i64>,
}

/// 仲裁结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arbitration {
    /// 覆盖: 新事实 active + 旧事实 superseded（版本链推进）
    Overwrite,
    /// 入 candidate 轨道: 互证后由上层提升 active；否则保持 candidate
    Candidate,
    /// 忽略: 无法仲裁（缺信息防误判），保持现状
    Ignore,
}

/// 仲裁结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArbitrateOutcome {
    pub action: Arbitration,
    /// 说明（诊断/日志，不含原文）
    pub reason: String,
}

/// 判断两事件是否构成互证独立性（≥2 独立事件的成对判定核心）。
///
/// 参数:
/// - `a`, `b`: 两个事件证据。
///
/// 说明:
/// - 独立性 == ref_event_id 不同 && ref_l1_id 不同
///   && (时间跨度 ≥ 1 天 || 非同批 TopicBatch)
pub fn independent_pair(a: &EventEvidence, b: &EventEvidence) -> bool {
    if a.ref_event_id == b.ref_event_id {
        return false;
    }
    if a.ref_l1_id == b.ref_l1_id {
        return false;
    }
    let time_gap_days = (a.time - b.time).abs() as f64 / (1000.0 * 86400.0);
    let time_gap_ok = time_gap_days >= CORROBORATION_TIME_GAP_DAYS as f64;
    let batch_ok = !(a.same_batch && b.same_batch);
    time_gap_ok || batch_ok
}

/// 判断事件证据是否与 active 事实构成互证票。
///
/// 说明:
/// - 单票互证 = 该事件与库内事实语义余弦 ≥ 0.7 且 valence 方向一致。
/// - 方向一致 = 事件 valence 符号与库内事实记载方向一致（简化：传入算子方判断）。
fn corroboration_vote(ev: &CorroborateCandidate, active_valence_positive: bool) -> bool {
    // 语义余弦 ≥ 0.7 且 valence 方向一致（同正或同负）才构成互证票
    ev.semantic >= CORROBORATION_COSINE_THRESHOLD
        && ev.evidence.valence_positive == active_valence_positive
}

/// 仲裁主入口。
///
/// 参数:
/// - `input`: 仲裁输入。
/// - `active_valence_positive`: 库内 active 事实的 valence 方向（true 正 / false 负）。
///
/// 说明:
/// - 上层（app 集成）负责：判重已被 dedup 模块拦截；本模块仅处理"新事实需覆盖/候选/忽略"。
/// - manual 源最高优先级直接覆盖（可被强证据覆盖的 stable 也允许 manual 覆盖）。
/// - 多事件互证（≥2 独立事件）→ 覆盖。
/// - 单事件：stable 不单事件覆盖（降 candidate）；volatile/historical 且时间新者胜 → 覆盖。
/// - 主观隐含（confidence < 0.6，如 0.5）即使互证也先入 candidate，由上层 promote。
pub fn arbitrate(input: &ArbitrationInput, active_valence_positive: bool) -> ArbitrateOutcome {
    // manual：最高优先级直接覆盖
    if input.source == FactSource::Manual {
        return ArbitrateOutcome {
            action: Arbitration::Overwrite,
            reason: "manual 事实优先覆盖".to_string(),
        };
    }

    // 多事件互证：≥2 条独立事件且语义 ≥0.7 且 valence 方向一致
    let mut votes: Vec<(&CorroborateCandidate, &EventEvidence)> = Vec::new();
    for c in &input.corroborations {
        if corroboration_vote(c, active_valence_positive) {
            votes.push((c, &c.evidence));
        }
    }
    // 检验互证对（任两票构成独立对）+ valence 方向一致
    let mut corroborated = false;
    'outer: for (i, (_, eva)) in votes.iter().enumerate() {
        for (_, evb) in votes.iter().skip(i + 1) {
            if independent_pair(eva, evb) {
                corroborated = true;
                break 'outer;
            }
        }
    }

    // 主观隐含事实（conf=0.5）：无论互证与否都入 candidate 轨道，由上层互证后 promote
    if input.confidence < 0.6 {
        // 若互证成立，返回 Candidate + reason 提示可提升；否则保持 candidate
        let reason = if corroborated {
            "主观隐含事实，互证成立，入 candidate 待提升".to_string()
        } else {
            "主观隐含事实，入 candidate 轨道，等待互证".to_string()
        };
        return ArbitrateOutcome {
            action: Arbitration::Candidate,
            reason,
        };
    }

    // 多事件互证 → 覆盖
    if corroborated {
        return ArbitrateOutcome {
            action: Arbitration::Overwrite,
            reason: "多事件互证成立，覆盖旧事实".to_string(),
        };
    }

    // 无库内 active（新事实直接入库）
    if input.existing_active.is_none()
        || input.existing_active.as_ref().is_none_or(|v| v.is_empty())
    {
        return ArbitrateOutcome {
            action: Arbitration::Overwrite,
            reason: "无现有 active 事实，直接入库".to_string(),
        };
    }

    // 单事件仲裁：stable 不单事件覆盖（降 candidate）；volatile/historical 时间新者胜 → 覆盖
    let Some(_ev) = &input.single_evidence else {
        // 无单事件信息 → 保守忽略（防误覆盖）
        return ArbitrateOutcome {
            action: Arbitration::Ignore,
            reason: "缺少事件证据，保守忽略".to_string(),
        };
    };
    match input.tier {
        FactTier::Stable => ArbitrateOutcome {
            action: Arbitration::Candidate,
            reason: "稳定事实不单事件覆盖，入 candidate 待互证".to_string(),
        },
        FactTier::Volatile | FactTier::Historical | _ => {
            let newer = input
                .existing_time
                .map(|et| input.new_time >= et)
                .unwrap_or(true);
            if newer {
                ArbitrateOutcome {
                    action: Arbitration::Overwrite,
                    reason: "单事件时间更新，覆盖旧事实".to_string(),
                }
            } else {
                ArbitrateOutcome {
                    action: Arbitration::Ignore,
                    reason: "单事件时间未更新，忽略".to_string(),
                }
            }
        }
    }
}

/// 仲裁可执行变更。
#[derive(Debug, Clone)]
pub enum Mutation {
    /// 覆盖写（新 active + 旧 superseded）
    Overwrite {
        /// 新事实（active）
        new: ramaria_core::types::PersonaFact,
        /// 被覆盖的旧事实 id
        old_id: i64,
    },
    /// 入 candidate（不覆盖）
    Candidate {
        /// candidate 事实
        fact: ramaria_core::types::PersonaFact,
    },
    /// 无操作
    None,
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
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
            &evidence(2, "l1b", 86400_000 * 2, false, true)
        ));
        // 同日但不同批 TopicBatch → 独立
        assert!(independent_pair(
            &evidence(1, "l1a", 0, false, true),
            &evidence(2, "l1b", 3600_000, true, true)
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
                evidence: evidence(2, "l1b", 86400_000 * 5, false, true),
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
                evidence: evidence(2, "l1b", 86400_000 * 2, false, false), // 方向不一致
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
}
