//! crates/ramaria-memory/src/prompt/layer_guard/tests.rs - //! crates/ramaria-memory/src/prompt/layer_guard.rs - 注入装配前层间证据去重与冲突仲裁单元测试
//!
//! 设计特点:
//! - 位于 prompt::layer_guard 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 layer_guard.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
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
