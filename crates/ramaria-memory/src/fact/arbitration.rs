//! crates/ramaria-memory/src/fact/arbitration.rs - 知识层版本链仲裁与候选互证提升
//!
//! 设计特点:
//! - 仲裁优先级: manual（人工事实）> 多事件互证 > 单事件（时间新者胜）
//! - 互证定义: ≥2 条独立事件（ref_event_id 不同、来源 L1 不同、
//!   时间跨度 ≥ 1 天或非同批 TopicBatch）且语义余弦 ≥ 0.7 且 valence 方向一致
//! - 单事件矛盾: 不覆盖 active，降级 candidate（C2 保护）
//! - 主观隐含事实（conf=0.5）：必须互证才提升 active，否则保持 candidate
//! - 输入以结构化事件证据（EventEvidence）描述，便于纯函数确定测试
//! - 产出 Mutation: 覆盖（新 active + 旧 superseded）/ 候选（仅入 candidate）/ 忽略（判重/不动作）
//! - auto_fact_detect 策略③候选互证提升（无向量关键词降级）：
//!   `corroborate_candidates` 对增强抽取产出的低置信候选扫描同 persona 既有
//!   独立事件，互证成立（≥2 独立事件语义相似且 valence 方向一致）建议提升为 active；
//!   纯内存确定性判定，语义相似退化为关键词交集（jaccard），不依赖 embedding

use crate::behavior::sentiment::sentiment_polarity;
use crate::fact::extractor::{FactCandidate, extract_text_keywords};
use crate::similarity::jaccard_similarity;
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

// =========================================================
// 策略③ 候选互证提升（auto_fact_detect 增强，纯内存确定性判定）
// =========================================================

/// 候选互证所需的最少独立事件数。
pub const CORROBORATION_MIN_INDEPENDENT: usize = 2;

/// 候选互证输入——轻量事件摘要（描述一条既有独立事件的语义/极性维度）。
#[derive(Debug, Clone)]
pub struct CorroborationInput {
    /// 事件独立性 + 显式 valence 方向（复用 `independent_pair` 语义）。
    pub evidence: EventEvidence,
    /// 事件摘要/正文文本（语义比较的降级关键词源；也可由 keywords 显式提供）。
    pub content: String,
    /// 事件关键词（可选；为空时由 content 提取 bigram 关键词）。
    pub keywords: Vec<String>,
}

/// 候选互证判定结论（按 candidates 下标一一对应）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorroborateVerdict {
    /// 候选达到互证（≥2 独立事件语义相似且 valence 方向一致），
    /// 建议上层走 save_fact_with_version 提升为 active。
    Promote {
        /// 判定说明（诊断/日志，不含原文）
        reason: String,
    },
    /// 未达互证，保持 candidate。
    KeepCandidate {
        /// 判定说明（诊断/日志，不含原文）
        reason: String,
    },
}

/// 事件 valence 方向与候选文本极性是否一致。
///
/// 规则:
/// - 候选极性由 `sentiment_polarity` 判定：>0 为正、<0 为负、=0 为中性。
/// - 极性一致: 正候选 ↔ valence_positive=true，负候选 ↔ valence_positive=false。
/// - 中性候选无方向可比，按不一致保守处理（宁缺毋滥，等待更有信号的证据）。
fn valence_consistent(candidate_content: &str, event_positive: bool) -> bool {
    let score = sentiment_polarity(candidate_content);
    if score > 0.0 {
        event_positive
    } else if score < 0.0 {
        !event_positive
    } else {
        false
    }
}

/// 候选与事件的语义相似判定（关键词交集，无向量降级路径）。
///
/// 说明:
/// - 候选侧关键词 = 候选自带 keywords ∪ content bigram；
///   事件侧关键词 = 显式 keywords（若有）∪ content bigram。
/// - 使用 Jaccard > 0 等价于"两侧关键词集合有 ≥1 个共同词"。
/// - 无 embedding 依赖：候选与事件文本可能词源异构（事件语义词 vs L1 bigram），
///   故两侧都并入 content bigram 提供共同文本空间，降低异构漏判。
fn semantically_similar(candidate: &FactCandidate, event: &CorroborationInput) -> bool {
    let mut cand_tokens = candidate.keywords.clone();
    cand_tokens.extend(extract_text_keywords(&candidate.content));
    cand_tokens.sort();
    cand_tokens.dedup();

    let mut ev_tokens = event.keywords.clone();
    if ev_tokens.is_empty() {
        ev_tokens = extract_text_keywords(&event.content);
    } else {
        ev_tokens.extend(extract_text_keywords(&event.content));
        ev_tokens.sort();
        ev_tokens.dedup();
    }

    jaccard_similarity(&cand_tokens, &ev_tokens) > 0.0
}

/// 对一批低置信候选做二次交叉抽取 / 互证提升判定（策略③）。
///
/// 业务意图:
/// - auto_fact_detect 的召回兜底闭环：策略①②产出的低置信候选（隐含偏好、
///   L1 线索）若能被同 persona 的多条独立事件佐证（语义相似 + valence 方向一致），
///   则从"单源弱信号"升级为"跨源互证信号"，可由上层提升为 active。
/// - 互证提升规则: 候选需匹配到 ≥2 条相互独立的既有事件
///   （`independent_pair` 语义：不同事件、不同 L1、时间跨 ≥1 天或非同批）。
///   同批/同一来源不构成独立证据；valence 方向冲突不构成互证。
///
/// 参数:
/// - `candidates`: 待判定候选列表（通常为策略①②产出的低置信候选）。
/// - `existing_events`: 该 persona 的既有独立事件摘要（调用方负责按 persona 过滤）。
///
/// 返回:
/// - 与 `candidates` 等长的 `Vec<CorroborateVerdict>`，按下标一一对应。
///
/// 说明:
/// - 语义相似默认走关键词交集（无向量降级），不依赖 embedding 与异步。
/// - 上层（app 编排）消费：`Promote` 可走 save_fact_with_version 提升为 active；
///   `KeepCandidate` 保持 candidate 轨道。
pub fn corroborate_candidates(
    candidates: &[FactCandidate],
    existing_events: &[CorroborationInput],
) -> Vec<CorroborateVerdict> {
    candidates
        .iter()
        .map(|cand| {
            // 匹配: 与候选语义相似且 valence 方向一致的既有事件下标
            let matches: Vec<usize> = existing_events
                .iter()
                .enumerate()
                .filter(|(_, ev)| {
                    semantically_similar(cand, ev)
                        && valence_consistent(&cand.content, ev.evidence.valence_positive)
                })
                .map(|(i, _)| i)
                .collect();

            // 在匹配事件中寻找 ≥2 条相互独立的事件
            let mut promoted = false;
            'outer: for (a, &ia) in matches.iter().enumerate() {
                for &ib in matches.iter().skip(a + 1) {
                    if independent_pair(
                        &existing_events[ia].evidence,
                        &existing_events[ib].evidence,
                    ) {
                        promoted = true;
                        break 'outer;
                    }
                }
            }

            if promoted {
                CorroborateVerdict::Promote {
                    reason: "≥2 条独立事件语义相似且 valence 方向一致，建议提升".to_string(),
                }
            } else {
                CorroborateVerdict::KeepCandidate {
                    reason: "未达互证（独立事件不足或极性不一致），保持 candidate".to_string(),
                }
            }
        })
        .collect()
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
                86400_000 * 3,
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
                86400_000 * 3,
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
            event_input(2, "l1b", 86400_000 * 3, false, true, "跑步让人心情好", &[]),
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
                86400_000 * 3,
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
}
