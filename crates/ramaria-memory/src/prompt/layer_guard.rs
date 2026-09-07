//! crates/ramaria-memory/src/prompt/layer_guard.rs - 注入装配前层间证据去重与冲突仲裁
//!
//! 设计特点:
//! - 同一事实跨层只注入一次：知识卡片为唯一"可剔除"通道，角色区/RAG/行为规则作为保留方。
//! - 引用级去重复用 `fact::retriever::dedup_knowledge_facts`（同 id / RAG 文档 label 覆盖），
//!   内容级判重以既有中文 bigram 分词（`bm25::tokenize`）的短侧 token 覆盖率为判据，
//!   不引入 embedding/外部依赖，纯内存纯函数、零 I/O。
//! - 冲突优先级与 fact 版本链口径一致（manual > 事件有引用 > 无引用，同权新者胜）；
//!   层通道保留语义沿用 `prompt::layers::LayerKind::priority`（行为>知识>表达>脉络）
//!   与协调预算的 RAG 基座保留语义：角色区/RAG/行为规则为保留方，知识卡片是可剔除通道。
//! - 证据可追溯：每剔除一条事实产出一条 `DedupTrace`（removed id + 保留方引用 +
//!   内容摘要 hash），日志不含原文全文。
//!
//! 语义边界（刻意不做，防误删）:
//! - 角色区已知事实（Role）恒保留：与 RAG 摘要/知识卡片重复时不剔除角色区
//!   （角色区是稳定身份声明，RAG 是话题相关记忆转述，属结构性冗余而非同一注入通道）。
//! - 行为规则 / 表达风格 / 原文（utt/桥接）文本不互为判重对象：规则、风格描述、
//!   原话语录与"事实陈述"语义不同构；其中行为规则文本可作为"保留参照"参与
//!   知识卡片的内容级判重（行为层注入优先级最高，规则文本若与知识卡片重复则剔除知识卡片）。
//! - 本模块只裁决"重复"，不裁决"矛盾"：同一 ProfileField 的矛盾裁决已由写库侧
//!   版本链仲裁完成（active 集合自身无矛盾）；注入侧仅保证呈现不重复。

use std::collections::HashSet;

use ramaria_core::types::{FactSource, PersonaFact};

use crate::bm25::tokenize;
use crate::fact::retriever::dedup_knowledge_facts;

// =========================================================
// 判据常量
// =========================================================

/// 参与内容级判重的最短断言长度（字符）。低于此长度的断言 bigram 过少，
/// 覆盖判定不稳定，宁可保留（避免误杀）。
pub const MIN_CLAIM_CHARS: usize = 6;

/// 短侧 token 覆盖率阈值：claim 侧 token 中 ≥ 该比例出现在 context 侧即判"已被覆盖"。
///
/// 机制起点值（阶段一不定稿参数，实际定稿在 M8 数据 Gate 之后）；
/// 默认关闭开关下不参与 prompt 输出，阈值仅影响显式开启时的裁决。
pub const CONTENT_COVERAGE_THRESHOLD: f64 = 0.7;

// =========================================================
// 保留参照（内容级判重的"保留方"文本）
// =========================================================

/// 保留参照来源类别。
///
/// 变体:
/// - `RagSummary`: RAG 摘要文本（`memory_context`），与 D-V20-008"RAG 摘要为主"对齐。
/// - `BehaviorRule`: 行为层命中的规则文本（reaction），行为注入优先级最高。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionKind {
    /// RAG 摘要文本
    RagSummary,
    /// 行为层规则文本
    BehaviorRule,
}

impl RetentionKind {
    /// 返回简短标识（用于保留方引用与日志）。
    pub fn as_str(self) -> &'static str {
        match self {
            RetentionKind::RagSummary => "rag",
            RetentionKind::BehaviorRule => "behavior",
        }
    }
}

/// 内容级判重的保留参照文本（作为 context 侧，只保留、永不剔除）。
///
/// 字段约定:
/// - `text`: 参照全文（RAG 摘要 / 行为规则 reaction）。
/// - 隐私约束: `text` 仅在本模块内部做 token 化比较，不写日志。
#[derive(Debug, Clone)]
pub struct RetentionReference<'a> {
    /// 参照类别
    pub kind: RetentionKind,
    /// 参照文本
    pub text: &'a str,
}

// =========================================================
// 仲裁输入/输出
// =========================================================

/// 层间证据去重与冲突仲裁输入。
///
/// 字段约定:
/// - `knowledge`: 知识层判定器命中的 active facts（仲裁后可能被剔除，唯一可剔除通道）。
/// - `role`: 角色层已知事实区展示的事实（仅消费，不剔除；按 content 做重复对照）。
/// - `references`: 保留参照文本（RAG 摘要 / 行为规则文本）；可空。
/// - `rag_covered_labels`: RAG 实际注入文本覆盖的文档 label 集合（`L1:{uuid}`/`L2:{id}`）；
///   空集合 = RAG 未注入 → 引用级 RAG 去重不生效（知识兜底路径保留）。
#[derive(Debug, Clone)]
pub struct LayerGuardInput<'a> {
    /// 知识层候选事实（待仲裁）
    pub knowledge: &'a [PersonaFact],
    /// 角色层已知事实（保留方）
    pub role: &'a [PersonaFact],
    /// 保留参照文本（RAG/行为规则）
    pub references: &'a [RetentionReference<'a>],
    /// RAG 覆盖文档 label 集合
    pub rag_covered_labels: &'a HashSet<String>,
}

/// 剔除原因（证据追溯用，不含原文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupReason {
    /// 与角色层同一记录（同 id）
    RoleSameRecord,
    /// 内容级：与角色层已知事实重复
    RoleContentDuplicate,
    /// 引用级：来源文档已进入 RAG 覆盖集合
    RagRefCovered,
    /// 内容级：断言文本已被保留参照（RAG 摘要/行为规则）覆盖
    ReferenceContentCovered,
    /// 内容级：知识卡片内部重复，保留来源权威更高者
    InnerLowerAuthority,
}

impl DedupReason {
    /// 返回简短标识（日志用）。
    pub fn as_str(self) -> &'static str {
        match self {
            DedupReason::RoleSameRecord => "role_same_record",
            DedupReason::RoleContentDuplicate => "role_content_duplicate",
            DedupReason::RagRefCovered => "rag_ref_covered",
            DedupReason::ReferenceContentCovered => "reference_content_covered",
            DedupReason::InnerLowerAuthority => "inner_lower_authority",
        }
    }
}

/// 单条去重/冲突仲裁记录（证据可追溯）。
///
/// 隐私约束: 本记录不含原文全文，`content_digest` 仅用于日志侧对账。
#[derive(Debug, Clone)]
pub struct DedupTrace {
    /// 被剔除的事实 id
    pub removed_fact_id: i64,
    /// 剔除原因
    pub reason: DedupReason,
    /// 保留方引用（`role_fact:{role_id}` / `rag:{label}` / `behavior` / `knowledge:{id}`）
    pub kept_ref: String,
    /// 被剔除事实内容的摘要 hash（DefaultHasher，非安全用途）
    pub content_digest: u64,
}

/// 仲裁结果。
///
/// 字段约定:
/// - `knowledge_facts`: 仲裁后应注入知识卡片区的事实（保持输入相对顺序）。
/// - `role_facts`: 角色区事实原样返回（角色区不因本仲裁剔除）。
/// - `traces`: 每条被剔除事实的裁决说明（空 = 未剔除任何事实）。
#[derive(Debug, Clone)]
pub struct LayerGuardOutcome {
    /// 仲裁后知识卡片事实
    pub knowledge_facts: Vec<PersonaFact>,
    /// 角色区事实（原样）
    pub role_facts: Vec<PersonaFact>,
    /// 裁决说明
    pub traces: Vec<DedupTrace>,
}

// =========================================================
// 内容级判重工具
// =========================================================

/// 将文本分词为 token 集合（中文 bigram + 英文词，与 BM25 同一分词器）。
fn token_set(text: &str) -> HashSet<String> {
    tokenize(text).into_iter().collect()
}

/// 计算 claim 侧 token 出现在 context 侧集合中的覆盖率（0.0..=1.0）。
///
/// 说明:
/// - claim 侧无 token（纯 emoji/单字/空）或 context 为空 → 0.0（信息不足不判重）。
fn token_coverage(claim_tokens: &HashSet<String>, context_tokens: &HashSet<String>) -> f64 {
    if claim_tokens.is_empty() || context_tokens.is_empty() {
        return 0.0;
    }
    let hits = claim_tokens
        .iter()
        .filter(|t| context_tokens.contains(t.as_str()))
        .count();
    hits as f64 / claim_tokens.len() as f64
}

/// 两段文本的"较短侧是否被较长侧覆盖"判定。
///
/// 规则:
/// - 短侧字符数 < [`MIN_CLAIM_CHARS`] → `None`（信息不足，保守不判）。
/// - 否则返回短侧 token 在长侧集合中的覆盖率。
fn short_side_coverage(a: &str, b: &str) -> Option<f64> {
    let (short, long) = if a.chars().count() <= b.chars().count() {
        (a, b)
    } else {
        (b, a)
    };
    if short.chars().count() < MIN_CLAIM_CHARS {
        return None;
    }
    let short_tokens = token_set(short);
    let long_tokens = token_set(long);
    Some(token_coverage(&short_tokens, &long_tokens))
}

/// 事实来源权威级（对齐版本链 manual > 事件互证 > 单事件口径的注入侧代理）。
///
/// 说明:
/// - `Manual` = 3（人工事实最高）。
/// - `Event`/`L1` 且带来源引用（ref_event_id/ref_l1_id） = 2（有事件支撑）。
/// - 其余 = 1（无引用单源；真正的互证计数在写库版本链侧，注入侧以引用存在为代理）。
fn fact_authority(fact: &PersonaFact) -> u8 {
    match fact.source {
        FactSource::Manual => 3,
        FactSource::Event | FactSource::L1
            if fact.ref_event_id.is_some() || fact.ref_l1_id.is_some() =>
        {
            2
        }
        FactSource::Event | FactSource::L1 => 1,
        // FactSource 为 non_exhaustive：未来新增来源保守视为低权威（不高于既有档）
        _ => 1,
    }
}

/// 内容摘要 hash（DefaultHasher，非安全用途；供日志对账且不含原文）。
fn content_digest(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut h);
    h.finish()
}

/// 事实来源引用映射为 RAG label（与 `fact/retriever.rs` 的 label 文本空间一致）。
fn doc_labels_of_fact(fact: &PersonaFact) -> Vec<String> {
    let mut labels = Vec::with_capacity(2);
    if let Some(id) = fact.ref_l1_id {
        labels.push(format!("L1:{id}"));
    }
    if let Some(id) = fact.ref_event_id {
        labels.push(format!("L2:{id}"));
    }
    labels
}

// =========================================================
// 仲裁主入口
// =========================================================

/// 执行注入装配前的层间证据去重与冲突仲裁。
///
/// 规则（按执行顺序）:
/// 1. 引用级：调用 `fact::retriever::dedup_knowledge_facts`——与角色层同 id、
///    或来源文档已进 RAG 覆盖集合的知识卡片剔除（既有权威语义）。
/// 2. 内容级角色重复：知识卡片与角色区某事实文本高覆盖（短侧 ≥ 6 字符、覆盖率 ≥ 0.7）
///    → 剔除知识卡片（角色区保留）。
/// 3. 内容级保留参照：知识卡片断言被 RAG 摘要 / 行为规则文本覆盖 → 剔除知识卡片
///    （RAG 摘要为主、行为规则最高优先）。
/// 4. 知识卡片内部重复：两两内容高覆盖 → 保留来源权威更高者
///    （manual > 有引用事件 > 无引用；同权取更新时间新者）。
///
/// 降级:
/// - `enabled=false` → 原样返回（与既有注入路径逐字段等价，回退 v1.7）。
/// - 任一判重步骤无输入（knowledge 为空 / role 为空 / 无参照）→ 对应步骤跳过。
///
/// 参数:
/// - `input`: 仲裁输入。
/// - `enabled`: 层间去重开关（默认关闭；关闭 = 回退既有引用级去重路径）。
///
/// 返回:
/// - `LayerGuardOutcome`：仲裁后的知识卡片/角色区事实与裁决说明。
pub fn arbitrate_fact_layers(input: &LayerGuardInput<'_>, enabled: bool) -> LayerGuardOutcome {
    if !enabled {
        return LayerGuardOutcome {
            knowledge_facts: input.knowledge.to_vec(),
            role_facts: input.role.to_vec(),
            traces: Vec::new(),
        };
    }

    let mut traces: Vec<DedupTrace> = Vec::new();
    let role_ids: HashSet<i64> = input.role.iter().map(|f| f.id).collect();

    // ---- ① 引用级去重（复用既有权威实现） ----
    let ref_kept = dedup_knowledge_facts(input.knowledge, input.rag_covered_labels, input.role);
    let ref_kept_ids: HashSet<i64> = ref_kept.iter().map(|k| k.id).collect();
    for fact in input.knowledge {
        if ref_kept_ids.contains(&fact.id) {
            continue;
        }
        if role_ids.contains(&fact.id) {
            // 与角色层同一条记录（role 保留，knowledge 剔除；保留方 id 与被剔除 id 相同）
            traces.push(DedupTrace {
                removed_fact_id: fact.id,
                reason: DedupReason::RoleSameRecord,
                kept_ref: format!("role_fact:{}", fact.id),
                content_digest: content_digest(&fact.content),
            });
            continue;
        }
        // 引用级 RAG 覆盖：保留方为实际命中的文档 label
        let hit_label = doc_labels_of_fact(fact)
            .iter()
            .find(|l| input.rag_covered_labels.contains(*l))
            .cloned()
            .unwrap_or_else(|| "rag".to_string());
        traces.push(DedupTrace {
            removed_fact_id: fact.id,
            reason: DedupReason::RagRefCovered,
            kept_ref: hit_label,
            content_digest: content_digest(&fact.content),
        });
    }

    // ---- ② 内容级角色重复 ----
    let mut kept: Vec<PersonaFact> = Vec::with_capacity(ref_kept.len());
    for fact in ref_kept {
        let covered_role_id = input.role.iter().find(|r| {
            short_side_coverage(&fact.content, &r.content)
                .map(|c| c >= CONTENT_COVERAGE_THRESHOLD)
                .unwrap_or(false)
        });
        if let Some(role_fact) = covered_role_id {
            traces.push(DedupTrace {
                removed_fact_id: fact.id,
                reason: DedupReason::RoleContentDuplicate,
                kept_ref: format!("role_fact:{}", role_fact.id),
                content_digest: content_digest(&fact.content),
            });
        } else {
            kept.push(fact);
        }
    }

    // ---- ③ 内容级保留参照（RAG 摘要 / 行为规则文本） ----
    let mut kept2: Vec<PersonaFact> = Vec::with_capacity(kept.len());
    for fact in kept {
        let covered_kind = input.references.iter().find_map(|r| {
            if fact.content.chars().count() < MIN_CLAIM_CHARS {
                return None;
            }
            let claim_tokens = token_set(&fact.content);
            let context_tokens = token_set(r.text);
            (token_coverage(&claim_tokens, &context_tokens) >= CONTENT_COVERAGE_THRESHOLD)
                .then_some(r.kind)
        });
        if let Some(kind) = covered_kind {
            traces.push(DedupTrace {
                removed_fact_id: fact.id,
                reason: DedupReason::ReferenceContentCovered,
                kept_ref: kind.as_str().to_string(),
                content_digest: content_digest(&fact.content),
            });
        } else {
            kept2.push(fact);
        }
    }

    // ---- ④ 知识卡片内部重复（保留权威更高者） ----
    let mut dropped: HashSet<usize> = HashSet::new();
    for i in 0..kept2.len() {
        if dropped.contains(&i) {
            continue;
        }
        for j in (i + 1)..kept2.len() {
            if dropped.contains(&j) {
                continue;
            }
            let dup = short_side_coverage(&kept2[i].content, &kept2[j].content)
                .map(|c| c >= CONTENT_COVERAGE_THRESHOLD)
                .unwrap_or(false);
            if !dup {
                continue;
            }
            // 重复 → 保留权威更高者；同权威取更新时间新者（对齐"时间新者胜"）
            let (ai, aj) = (fact_authority(&kept2[i]), fact_authority(&kept2[j]));
            let keep_i = if ai != aj {
                ai > aj
            } else {
                kept2[i].updated_at >= kept2[j].updated_at
            };
            let (winner, loser) = if keep_i { (i, j) } else { (j, i) };
            dropped.insert(loser);
            traces.push(DedupTrace {
                removed_fact_id: kept2[loser].id,
                reason: DedupReason::InnerLowerAuthority,
                kept_ref: format!("knowledge:{}", kept2[winner].id),
                content_digest: content_digest(&kept2[loser].content),
            });
        }
    }
    let final_kept: Vec<PersonaFact> = kept2
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !dropped.contains(i))
        .map(|(_, f)| f)
        .collect();

    LayerGuardOutcome {
        knowledge_facts: final_kept,
        role_facts: input.role.to_vec(),
        traces,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{FactStatus, FactTier, ProfileField};

    fn fact(
        id: i64,
        field: ProfileField,
        content: &str,
        source: FactSource,
        ref_l1: Option<uuid::Uuid>,
        ref_event: Option<i64>,
    ) -> PersonaFact {
        let mut f = PersonaFact::new("char-0001".into(), field, content.into(), source);
        f.id = id;
        f.status = FactStatus::Active;
        f.tier = FactTier::Stable;
        f.ref_l1_id = ref_l1;
        f.ref_event_id = ref_event;
        f
    }

    fn empty_labels() -> HashSet<String> {
        HashSet::new()
    }

    fn outcome_of(
        knowledge: Vec<PersonaFact>,
        role: Vec<PersonaFact>,
        refs: Vec<RetentionReference<'_>>,
        labels: &HashSet<String>,
    ) -> LayerGuardOutcome {
        let input = LayerGuardInput {
            knowledge: &knowledge,
            role: &role,
            references: &refs,
            rag_covered_labels: labels,
        };
        arbitrate_fact_layers(&input, true)
    }

    /// 关闭开关 → 原样返回（回退既有路径的 identity 保证）。
    #[test]
    fn disabled_returns_input_unchanged() {
        let k = vec![
            fact(
                1,
                ProfileField::Interests,
                "用户喜欢科幻电影",
                FactSource::Event,
                None,
                None,
            ),
            fact(
                2,
                ProfileField::Social,
                "有一个朋友叫小李",
                FactSource::Manual,
                None,
                None,
            ),
        ];
        let r = vec![fact(
            9,
            ProfileField::BasicInfo,
            "出生于上海",
            FactSource::Manual,
            None,
            None,
        )];
        let labels = empty_labels();
        let input = LayerGuardInput {
            knowledge: &k,
            role: &r,
            references: &[],
            rag_covered_labels: &labels,
        };
        let out = arbitrate_fact_layers(&input, false);
        assert_eq!(out.knowledge_facts.len(), 2, "关闭时知识卡片不剔除");
        assert_eq!(out.role_facts.len(), 1);
        assert!(out.traces.is_empty(), "关闭时不产裁决说明");
    }

    /// 内容级角色重复：role 短句被 knowledge 长句完整包含 → 剔除 knowledge（角色区保留）。
    #[test]
    fn role_content_duplicate_removes_knowledge() {
        // role BasicInfo（Manual）与 knowledge Interests（Event）描述同一事实但 id 不同
        let role = vec![fact(
            5,
            ProfileField::BasicInfo,
            "喜欢科幻电影",
            FactSource::Manual,
            None,
            None,
        )];
        let knowledge = vec![fact(
            9,
            ProfileField::Interests,
            "用户喜欢科幻电影",
            FactSource::Event,
            None,
            None,
        )];
        let out = outcome_of(knowledge, role, vec![], &empty_labels());
        assert!(
            out.knowledge_facts.is_empty(),
            "与角色区重复的知识卡片应剔除"
        );
        assert_eq!(out.role_facts.len(), 1, "角色区保留");
        let trace = &out.traces[0];
        assert_eq!(trace.removed_fact_id, 9);
        assert_eq!(trace.reason, DedupReason::RoleContentDuplicate);
        assert_eq!(trace.kept_ref, "role_fact:5", "保留方为角色区事实 id");
        assert_ne!(trace.content_digest, 0, "内容摘要存在即可对账（不含原文）");
    }

    /// 引用级角色重复：同一条记录（同 id）在角色区与知识卡片 → 剔除 knowledge。
    #[test]
    fn role_same_id_removes_knowledge() {
        let same = fact(
            7,
            ProfileField::BasicInfo,
            "出生于上海",
            FactSource::Manual,
            None,
            None,
        );
        let role = vec![same.clone()];
        let knowledge = vec![
            same,
            fact(
                8,
                ProfileField::Interests,
                "喜欢科幻电影",
                FactSource::Manual,
                None,
                None,
            ),
        ];
        let out = outcome_of(knowledge, role, vec![], &empty_labels());
        assert_eq!(
            out.knowledge_facts.len(),
            1,
            "同 id 的 BasicInfo 记录被剔除，其余保留"
        );
        assert_eq!(out.knowledge_facts[0].id, 8);
        assert_eq!(out.traces[0].reason, DedupReason::RoleSameRecord);
        assert_eq!(
            out.traces[0].kept_ref, "role_fact:7",
            "同 id 记录保留方为角色区该记录"
        );
    }

    /// 引用级 RAG 覆盖：来源文档（L2 事件）已在 RAG 覆盖集合 → 剔除 knowledge。
    #[test]
    fn rag_ref_covered_removes_knowledge() {
        let labels = HashSet::from(["L2:42".to_string()]);
        let knowledge = vec![fact(
            3,
            ProfileField::Social,
            "有一个朋友叫小李",
            FactSource::Event,
            None,
            Some(42),
        )];
        let out = outcome_of(knowledge, vec![], vec![], &labels);
        assert!(out.knowledge_facts.is_empty());
        assert_eq!(out.traces[0].reason, DedupReason::RagRefCovered);
        assert_eq!(
            out.traces[0].kept_ref, "L2:42",
            "保留方引用精确到命中文档 label"
        );
    }

    /// 内容级 RAG 参照覆盖：manual 无引用事实但文本已被 RAG 摘要覆盖 → 剔除。
    #[test]
    fn rag_reference_content_covered_removes_knowledge() {
        let knowledge = vec![fact(
            4,
            ProfileField::Interests,
            "喜欢看科幻电影",
            FactSource::Manual,
            None,
            None,
        )];
        let rag_text = "[相关记忆]\n1. (L1) 用户喜欢看科幻电影，尤其星际穿越 [score=0.9]";
        let refs = vec![RetentionReference {
            kind: RetentionKind::RagSummary,
            text: rag_text,
        }];
        let out = outcome_of(knowledge, vec![], refs, &empty_labels());
        assert!(
            out.knowledge_facts.is_empty(),
            "manual 事实文本已被 RAG 摘要覆盖 → 知识卡片不重复注入"
        );
        assert_eq!(out.traces[0].reason, DedupReason::ReferenceContentCovered);
        assert_eq!(out.traces[0].kept_ref, "rag");
    }

    /// 内容级行为规则参照覆盖：知识卡片文本与行为规则 reaction 高覆盖 → 剔除（行为优先）。
    #[test]
    fn behavior_reference_covered_removes_knowledge() {
        let knowledge = vec![fact(
            6,
            ProfileField::RecentContext,
            "加班到很晚需要先休息",
            FactSource::Event,
            None,
            None,
        )];
        let behavior_text = "当聊到加班话题时：加班到很晚需要先休息，不要劝继续工作";
        let refs = vec![RetentionReference {
            kind: RetentionKind::BehaviorRule,
            text: behavior_text,
        }];
        let out = outcome_of(knowledge, vec![], refs, &empty_labels());
        assert!(
            out.knowledge_facts.is_empty(),
            "行为规则文本已覆盖该断言 → 知识卡片剔除"
        );
        assert_eq!(out.traces[0].reason, DedupReason::ReferenceContentCovered);
        assert_eq!(out.traces[0].kept_ref, "behavior");
    }

    /// 冲突优先级：manual 事实 vs 无引用事件事实内部重复 → 保留 manual。
    #[test]
    fn inner_conflict_keeps_manual_higher_authority() {
        let manual = fact(
            11,
            ProfileField::Interests,
            "用户喜欢看科幻电影",
            FactSource::Manual,
            None,
            None,
        );
        let event = fact(
            12,
            ProfileField::Interests,
            "用户喜欢看科幻电影",
            FactSource::Event,
            None,
            None,
        );
        let out = outcome_of(
            vec![event.clone(), manual.clone()],
            vec![],
            vec![],
            &empty_labels(),
        );
        assert_eq!(out.knowledge_facts.len(), 1, "内部重复只保留一条");
        assert_eq!(out.knowledge_facts[0].id, 11, "manual 权威更高保留");
        assert_eq!(out.traces[0].removed_fact_id, 12);
        assert_eq!(out.traces[0].reason, DedupReason::InnerLowerAuthority);
        assert_eq!(out.traces[0].kept_ref, "knowledge:11");
    }

    /// 冲突优先级 tie-break：同权威内部重复 → 保留更新时间新者（对齐时间新者胜）。
    #[test]
    fn inner_conflict_keeps_newer_when_equal_authority() {
        let mut older = fact(
            21,
            ProfileField::Interests,
            "用户喜欢看科幻电影",
            FactSource::Event,
            Some(uuid::Uuid::new_v4()),
            None,
        );
        older.updated_at = 1000;
        let mut newer = fact(
            22,
            ProfileField::Interests,
            "用户喜欢看科幻电影",
            FactSource::Event,
            Some(uuid::Uuid::new_v4()),
            None,
        );
        newer.updated_at = 2000;
        let out = outcome_of(vec![older, newer], vec![], vec![], &empty_labels());
        assert_eq!(out.knowledge_facts.len(), 1);
        assert_eq!(out.knowledge_facts[0].id, 22, "同权威取新者");
    }

    /// 不误杀：同话题但不同措辞（覆盖率低于阈值）→ 两条都保留。
    #[test]
    fn similar_but_distinct_contents_are_kept() {
        let a = fact(
            31,
            ProfileField::Interests,
            "喜欢看科幻电影",
            FactSource::Event,
            None,
            None,
        );
        let b = fact(
            32,
            ProfileField::Interests,
            "平时爱读科幻小说",
            FactSource::Event,
            None,
            None,
        );
        let out = outcome_of(vec![a, b], vec![], vec![], &empty_labels());
        assert_eq!(out.knowledge_facts.len(), 2, "同话题不同内容不判重");
        assert!(out.traces.is_empty());
    }

    /// 空输入/单层输入不 panic，输出为空或原样。
    #[test]
    fn empty_and_single_layer_inputs_are_safe() {
        // 全空
        let out = outcome_of(vec![], vec![], vec![], &empty_labels());
        assert!(out.knowledge_facts.is_empty());
        assert!(out.traces.is_empty());
        // 仅角色区（无知识卡片）→ 角色区原样
        let role = vec![fact(
            1,
            ProfileField::BasicInfo,
            "出生于上海",
            FactSource::Manual,
            None,
            None,
        )];
        let out = outcome_of(vec![], role.clone(), vec![], &empty_labels());
        assert_eq!(out.role_facts.len(), 1);
        // 仅知识卡片（无 role / 无参照）
        let k = vec![fact(
            2,
            ProfileField::Interests,
            "喜欢看科幻电影",
            FactSource::Manual,
            None,
            None,
        )];
        let out = outcome_of(k, vec![], vec![], &empty_labels());
        assert_eq!(out.knowledge_facts.len(), 1, "无对照时知识卡片保留");
    }

    /// 中文/emoji/边界：emoji 或过短内容不 panic 且不误判。
    #[test]
    fn chinese_emoji_and_short_boundaries() {
        // 中文正常参与判重（角色短句被长句包含 → 剔除）
        let role = vec![fact(
            1,
            ProfileField::BasicInfo,
            "喜欢科幻电影",
            FactSource::Manual,
            None,
            None,
        )];
        let knowledge = vec![fact(
            2,
            ProfileField::Interests,
            "用户喜欢科幻电影很多年",
            FactSource::Event,
            None,
            None,
        )];
        let out = outcome_of(knowledge, role, vec![], &empty_labels());
        assert!(
            out.knowledge_facts.is_empty(),
            "中文长句包含角色短句 → 剔除"
        );

        // 全 emoji content → token 为空，不与任何对照判重（不 panic）
        let emoji_fact = fact(
            3,
            ProfileField::Interests,
            "😀😁😂",
            FactSource::Event,
            None,
            None,
        );
        let rag_text = "😀😁😂 用户心情很好";
        let refs = vec![RetentionReference {
            kind: RetentionKind::RagSummary,
            text: rag_text,
        }];
        let out = outcome_of(vec![emoji_fact], vec![], refs, &empty_labels());
        assert_eq!(out.knowledge_facts.len(), 1, "emoji 内容不参与内容级判重");
    }

    /// 过短（< MIN_CLAIM_CHARS）不参与内容级判重：即使完全一致也保留（宁缺毋滥）。
    #[test]
    fn too_short_contents_are_not_arbitrated() {
        let a = fact(
            41,
            ProfileField::Interests,
            "喜欢猫",
            FactSource::Manual,
            None,
            None,
        );
        let b = fact(
            42,
            ProfileField::Interests,
            "喜欢猫",
            FactSource::Event,
            None,
            None,
        );
        let out = outcome_of(vec![a, b], vec![], vec![], &empty_labels());
        assert_eq!(out.knowledge_facts.len(), 2, "短文本（<6 字符）不判重");
        assert!(out.traces.is_empty());
    }

    /// 证据链保留：role 与 knowledge 内容重复被剔除后，outcome.role_facts 仍完整保留原事实。
    #[test]
    fn evidence_kept_for_role_retained() {
        let role_fact = fact(
            51,
            ProfileField::BasicInfo,
            "用户是程序员",
            FactSource::Manual,
            None,
            None,
        );
        let knowledge = vec![fact(
            52,
            ProfileField::Interests,
            "用户是程序员喜欢写代码",
            FactSource::Event,
            None,
            None,
        )];
        let out = outcome_of(knowledge, vec![role_fact.clone()], vec![], &empty_labels());
        assert!(out.knowledge_facts.is_empty());
        assert_eq!(out.role_facts[0].id, 51);
        assert_eq!(
            out.role_facts[0].content, "用户是程序员",
            "角色区事实内容原样保留"
        );
        // trace 可追溯到被剔除 id
        assert_eq!(out.traces[0].removed_fact_id, 52);
        assert_eq!(out.traces[0].kept_ref, "role_fact:51");
    }
}
