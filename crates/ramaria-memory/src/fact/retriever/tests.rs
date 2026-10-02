//! crates/ramaria-memory/src/fact/retriever/tests.rs - //! crates/ramaria-memory/src/fact/retriever.rs - 知识层规则判定器与检索注入单元测试
//!
//! 设计特点:
//! - 位于 fact::retriever 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 retriever.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::types::{FactSource, FactStatus, FactTier};

fn fact(field: ProfileField, content: &str, kw: &str, tier: FactTier) -> PersonaFact {
    let mut f = PersonaFact::new("char-0001".into(), field, content.into(), FactSource::Event);
    f.status = FactStatus::Active;
    f.tier = tier;
    f.keyword_hint = Some(kw.to_string());
    f
}

// =========================================================
// 查询-事实命中强度（fact_query_overlap）
// =========================================================

/// 事实侧 token 含 keyword_hint 片段 + field label + as_str；
/// 查询命中关键词片段时 overlap 计数精确可断言。
#[test]
fn fact_query_overlap_counts_keyword_hits() {
    let f = fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        FactTier::Stable,
    );
    // 事实侧 token = ["电影","科幻","兴趣爱好","interests"]，共 4 个；
    // 查询 "你喜欢看什么电影？" 的 bigram 集合含 "电影" → 命中 1 个
    let overlap = fact_query_overlap("你喜欢看什么电影？", &f);
    assert!(
        (overlap - 0.25).abs() < 1e-9,
        "overlap 应为 1/4=0.25，实际 {overlap}"
    );
}

/// 无重叠关键词的事实 overlap 为 0。
#[test]
fn fact_query_overlap_zero_for_unrelated() {
    let f = fact(
        ProfileField::Interests,
        "喜欢跑步健身",
        "跑步,健身",
        FactTier::Stable,
    );
    let overlap = fact_query_overlap("你喜欢看什么电影？", &f);
    assert!((overlap - 0.0).abs() < 1e-9, "无关事实 overlap 应为 0");
}

/// ASCII 事实关键词按忽略大小写命中（查询含 "Rust" 时 "rust" 片段命中）。
#[test]
fn fact_query_overlap_ascii_ignores_case() {
    let f = fact(
        ProfileField::Interests,
        "喜欢 Rust 编程",
        "Rust,编程",
        FactTier::Stable,
    );
    let overlap = fact_query_overlap("跟我聊聊 Rust 吧", &f);
    // token：["Rust","编程","兴趣爱好","interests"]；"Rust"（小写化 rust）命中查询词 rust
    assert!(overlap >= 0.25, "ASCII 命中应计入 overlap，实际 {overlap}");
}

// =========================================================
// 知识路检索策略参数（retrieve_knowledge_with_options）
// =========================================================

/// threshold=0.0（默认）与旧行为一致：判定器命中即注入全部 active facts。
#[test]
fn options_threshold_zero_matches_old_behavior() {
    let facts = vec![
        fact(
            ProfileField::Interests,
            "喜欢科幻电影",
            "电影,科幻",
            FactTier::Stable,
        ),
        fact(
            ProfileField::Interests,
            "喜欢跑步健身",
            "跑步,健身",
            FactTier::Stable,
        ),
    ];
    let query = KnowledgeQuery {
        user_message: "你喜欢看什么电影？".into(),
        facts,
        budget_chars: 500,
    };
    let r = retrieve_knowledge_with_options(
        &query,
        0,
        30,
        KnowledgeRetrievalOptions {
            top_k: 0,
            threshold: 0.0,
        },
    );
    assert!(r.is_triggered());
    assert_eq!(r.matched.len(), 2, "threshold=0.0 不过滤（上一版本等价）");
}

/// threshold>0 只保留高查询-事实重叠的候选：
/// 判定器因"电影"命中触发，但无关事实（跑步）应被过滤。
#[test]
fn options_threshold_filters_low_overlap_facts() {
    let facts = vec![
        fact(
            ProfileField::Interests,
            "喜欢科幻电影",
            "电影,科幻",
            FactTier::Stable,
        ),
        fact(
            ProfileField::Interests,
            "喜欢跑步健身",
            "跑步,健身",
            FactTier::Stable,
        ),
    ];
    let query = KnowledgeQuery {
        user_message: "你喜欢看什么电影？".into(),
        facts,
        budget_chars: 500,
    };
    // A overlap=0.25 ≥ 0.2 保留；B overlap=0 < 0.2 过滤
    let r = retrieve_knowledge_with_options(
        &query,
        0,
        30,
        KnowledgeRetrievalOptions {
            top_k: 0,
            threshold: 0.2,
        },
    );
    assert_eq!(r.matched.len(), 1, "只留高 overlap 事实");
    assert_eq!(r.matched[0].keyword_hint.as_deref(), Some("电影,科幻"));
}

/// top_k=0 不截断；top_k>0 按排序后前 N 条截断（top_k 截断为最后一步）。
#[test]
fn options_top_k_truncates_and_zero_keeps_all() {
    let facts = vec![
        fact(
            ProfileField::Interests,
            "喜欢科幻电影",
            "电影",
            FactTier::Stable,
        ),
        fact(
            ProfileField::Interests,
            "喜欢喜剧电影",
            "电影",
            FactTier::Stable,
        ),
        fact(
            ProfileField::Interests,
            "喜欢动画电影",
            "电影",
            FactTier::Stable,
        ),
    ];
    let query = KnowledgeQuery {
        user_message: "你喜欢看什么电影？".into(),
        facts,
        budget_chars: 500,
    };
    // top_k=0：不截断
    let all = retrieve_knowledge_with_options(
        &query,
        0,
        30,
        KnowledgeRetrievalOptions {
            top_k: 0,
            threshold: 0.0,
        },
    );
    assert_eq!(all.matched.len(), 3, "top_k=0 不截断（上一版本等价）");
    // top_k=2：截断为前 2 条
    let limited = retrieve_knowledge_with_options(
        &query,
        0,
        30,
        KnowledgeRetrievalOptions {
            top_k: 2,
            threshold: 0.0,
        },
    );
    assert_eq!(limited.matched.len(), 2, "top_k>0 应截断");
}

/// judge 未命中仍不注入（既有红线不回归），携带策略参数也一样。
#[test]
fn options_judge_none_still_no_injection() {
    let facts = vec![fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        FactTier::Stable,
    )];
    let query = KnowledgeQuery {
        user_message: "随便聊聊".into(),
        facts,
        budget_chars: 500,
    };
    let r = retrieve_knowledge_with_options(
        &query,
        0,
        30,
        KnowledgeRetrievalOptions {
            top_k: 3,
            threshold: 0.1,
        },
    );
    assert!(!r.is_triggered());
    assert!(r.matched.is_empty());
}

#[test]
fn question_with_topic_marks_qa() {
    let facts = vec![fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        FactTier::Stable,
    )];
    let level = judge_knowledge_query("你喜欢看什么电影？", &facts);
    assert_eq!(level, MatchLevel::QuestionWithTopic);
}

#[test]
fn question_without_topic_is_none() {
    let facts = vec![fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        FactTier::Stable,
    )];
    let level = judge_knowledge_query("今天天气怎么样？", &facts);
    assert_eq!(level, MatchLevel::None);
}

#[test]
fn topic_hit_marks_topic() {
    let facts = vec![fact(
        ProfileField::Interests,
        "喜欢编程",
        "编程,开发",
        FactTier::Stable,
    )];
    let level = judge_knowledge_query("跟我聊聊编程吧", &facts);
    assert_eq!(level, MatchLevel::TopicHit);
}

#[test]
fn explicit_reference_marks_reference() {
    let facts = vec![fact(
        ProfileField::Social,
        "有一个同学叫小李",
        "朋友,同学",
        FactTier::Stable,
    )];
    let level = judge_knowledge_query("你说过的小李怎样了？", &facts);
    assert_eq!(level, MatchLevel::ExplicitReference);
}

#[test]
fn empty_facts_not_triggered() {
    let level = judge_knowledge_query("你喜欢什么电影？", &[]);
    assert_eq!(level, MatchLevel::None);
}

#[test]
fn retrieve_only_returns_active_when_triggered() {
    let facts = vec![fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        FactTier::Stable,
    )];
    let query = KnowledgeQuery {
        user_message: "你喜欢看什么电影？".into(),
        facts,
        budget_chars: 500,
    };
    let r = retrieve_knowledge(&query, 0, 30);
    assert!(r.is_triggered());
    assert_eq!(r.matched.len(), 1);
}

#[test]
fn retrieve_none_when_no_trigger() {
    let facts = vec![fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        FactTier::Stable,
    )];
    let query = KnowledgeQuery {
        user_message: "随便聊聊".into(),
        facts,
        budget_chars: 500,
    };
    let r = retrieve_knowledge(&query, 0, 30);
    assert!(!r.is_triggered());
}

/// SpeakingStyle 不参与知识层检索注入（表达层已注入，知识层只读引用无副作用）。
#[test]
fn speaking_style_excluded_from_knowledge_injection() {
    let mut style = fact(
        ProfileField::SpeakingStyle,
        "你习惯使用口癖词「哇塞」，说话节奏明快。",
        "口癖,节奏",
        FactTier::Stable,
    );
    style.keyword_hint = Some("哇塞,口癖".to_string());
    let interests = fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        FactTier::Stable,
    );
    let facts = vec![style, interests];

    // 判定器不把 SpeakingStyle 字段词作为检索触发源
    let level = judge_knowledge_query("ta 的说话风格是怎样的？", &facts);
    assert_eq!(level, MatchLevel::None, "SpeakingStyle 不触发知识检索");

    // 知识检索命中时召回排除 SpeakingStyle
    let query = KnowledgeQuery {
        user_message: "你喜欢看什么电影？".into(),
        facts,
        budget_chars: 500,
    };
    let r = retrieve_knowledge(&query, 0, 30);
    assert!(r.is_triggered());
    assert_eq!(r.matched.len(), 1, "仅召回 Interests，排除 SpeakingStyle");
    assert_eq!(r.matched[0].field, ProfileField::Interests);
}

#[test]
fn render_cards_groups_by_field() {
    let facts = vec![
        fact(
            ProfileField::Interests,
            "喜欢科幻",
            "科幻",
            FactTier::Stable,
        ),
        fact(
            ProfileField::Interests,
            "喜欢编程",
            "编程",
            FactTier::Stable,
        ),
        fact(ProfileField::Social, "有朋友小李", "朋友", FactTier::Stable),
    ];
    let cards = render_knowledge_cards(&facts);
    assert!(cards.contains("关于兴趣爱好：喜欢科幻；喜欢编程"));
    assert!(cards.contains("关于社交情况：有朋友小李"));
}

#[test]
fn build_injection_empty_for_blank() {
    let b = build_knowledge_injection(&[], 500);
    assert!(b.is_none());
}

#[test]
fn build_injection_budget_truncates() {
    let facts = vec![fact(
        ProfileField::Interests,
        "很喜欢阅读长篇科幻小说",
        "阅读,科幻",
        FactTier::Stable,
    )];
    let b = build_knowledge_injection(&facts, 10);
    assert!(b.is_some());
    assert!(b.unwrap().content.chars().count() <= 11);
}

// =========================================================
// 知识层降级路径测试
// =========================================================

/// 知识检索无 embedding 依赖：同 field/关键词召回在无向量时仍可用（静默降级）。
///
/// 说明:
/// - `retrieve_knowledge`/`judge_knowledge_query` 为纯规则函数，不触碰 embedding。
/// - 即使 embedding 模型不可用（向量通道关闭），判定器命中 → 同 field 召回仍返回 active 事实。
#[test]
fn retrieval_degrades_to_same_field_without_embedding() {
    let facts = vec![fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        FactTier::Stable,
    )];
    let query = KnowledgeQuery {
        user_message: "你喜欢看什么电影？".into(),
        facts,
        budget_chars: 500,
    };
    // 不传入任何向量/embedding 依赖，纯关键词 + 字段标签召回
    let r = retrieve_knowledge(&query, 0, 30);
    assert!(r.is_triggered(), "embedding 不可用 → 同 field 召回仍触发");
    assert_eq!(r.matched.len(), 1);
}

/// 判定器不命中 → 检索为空 → 注入块为 None（全链静默降级，prompt 无知识块）。
#[test]
fn detector_not_hit_chain_degrades_to_no_injection() {
    let facts = vec![fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        FactTier::Stable,
    )];
    let query = KnowledgeQuery {
        user_message: "随便聊聊".into(),
        facts,
        budget_chars: 500,
    };
    let r = retrieve_knowledge(&query, 0, 30);
    assert!(!r.is_triggered(), "不命中 → 不注入");
    assert!(r.matched.is_empty());
    // 空匹配 → 注入块 None（不产生知识段落）
    assert!(
        build_knowledge_injection(&r.matched, 500).is_none(),
        "不命中 → 无知识块（回归红线 2：不阻塞且不加段落）"
    );
}

/// 仅 candidate 事实（无 active）→ 判定器按空 active 集合处理 → 不注入。
///
/// 说明: 检索层只消费 active 事实；若传入非 active 集合，注入仍为空。
#[test]
fn non_active_facts_do_not_inject() {
    let mut f = fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        FactTier::Stable,
    );
    f.status = FactStatus::Candidate; // 待互证，不参与注入
    let b = build_knowledge_injection(&[f], 500);
    // 渲染卡片不区分状态（由上层筛选 active），但空内容/未命中由调用方保证；
    // 此处断言注入文本存在，状态过滤是 app 层 load_knowledge_facts 的职责
    assert!(b.is_some(), "状态过滤在上层，此处仅验证渲染不崩溃");
}

// =========================================================
// 知识层注入去重（RAG 覆盖为主、断言知识兜底）
// =========================================================

/// 构造带来源引用的事实（ref_l1_id / ref_event_id 可控）。
fn sourced_fact(
    field: ProfileField,
    content: &str,
    kw: &str,
    ref_l1: Option<uuid::Uuid>,
    ref_event: Option<i64>,
) -> PersonaFact {
    let mut f = fact(field, content, kw, FactTier::Stable);
    f.ref_l1_id = ref_l1;
    f.ref_event_id = ref_event;
    f
}

/// 构造空集合的便捷辅助。
fn empty_covered() -> std::collections::HashSet<String> {
    std::collections::HashSet::new()
}

/// 来源文档（L1）已在 RAG 覆盖集合 → 事实被去重；无来源引用事实保留。
#[test]
fn dedup_removes_fact_covered_by_l1_in_rag() {
    let l1_id = uuid::Uuid::new_v4();
    let covered = std::collections::HashSet::from([format!("L1:{l1_id}")]);
    let facts = vec![
        sourced_fact(
            ProfileField::Interests,
            "喜欢科幻电影",
            "电影,科幻",
            Some(l1_id),
            None,
        ),
        sourced_fact(ProfileField::Interests, "喜欢跑步健身", "跑步", None, None),
    ];
    let kept = dedup_knowledge_facts(&facts, &covered, &[]);
    assert_eq!(kept.len(), 1, "RAG 已覆盖的 L1 来源事实应去重");
    assert_eq!(kept[0].content, "喜欢跑步健身", "无来源引用事实应保留");
}

/// 来源文档（L2 事件）已在 RAG 覆盖集合 → 事实被去重。
#[test]
fn dedup_removes_fact_covered_by_l2_event_in_rag() {
    let covered = std::collections::HashSet::from(["L2:42".to_string()]);
    let facts = vec![sourced_fact(
        ProfileField::Social,
        "有一个朋友叫小李",
        "朋友,同学",
        None,
        Some(42),
    )];
    let kept = dedup_knowledge_facts(&facts, &covered, &[]);
    assert!(kept.is_empty(), "L2 事件已覆盖的事实应去重");
}

/// 与角色层已知事实区同一 id → 知识卡片区去重（角色区保留）。
#[test]
fn dedup_removes_fact_duplicated_with_role_facts() {
    let f = sourced_fact(ProfileField::BasicInfo, "出生于上海", "上海", None, None);
    // 角色层 facts 返回同一条记录（BasicInfo 字段，同一 id）
    let role = vec![f.clone()];
    let kept = dedup_knowledge_facts(&[f], &empty_covered(), &role);
    assert!(kept.is_empty(), "角色层已展示的同一事实不重复注入");
}

/// 角色层包含其它 id（未在知识命中集中）→ 不影响本批事实注入。
#[test]
fn dedup_role_other_id_keeps_knowledge_fact() {
    let mut other = fact(
        ProfileField::BasicInfo,
        "性格内向",
        "内向",
        FactTier::Stable,
    );
    other.id = 999;
    let f = sourced_fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        None,
        None,
    );
    let kept = dedup_knowledge_facts(&[f], &empty_covered(), &[other]);
    assert_eq!(kept.len(), 1, "角色层其它 id 不误伤知识事实");
}

/// RAG 覆盖集合为空 → 全部保留（RAG 未命中/关闭时的兜底路径，回退既有行为）。
#[test]
fn dedup_empty_covered_keeps_all() {
    let l1_id = uuid::Uuid::new_v4();
    let facts = vec![
        sourced_fact(
            ProfileField::Interests,
            "喜欢科幻电影",
            "电影,科幻",
            Some(l1_id),
            None,
        ),
        sourced_fact(
            ProfileField::Social,
            "有一个朋友叫小李",
            "朋友",
            None,
            Some(7),
        ),
    ];
    let kept = dedup_knowledge_facts(&facts, &empty_covered(), &[]);
    assert_eq!(kept.len(), 2, "RAG 覆盖集合为空时不去重（知识兜底保留）");
}

/// 有来源引用但对应文档未被 RAG 覆盖 → 保留（仍是有效兜底）。
#[test]
fn dedup_ref_not_in_covered_keeps_fact() {
    let covered_l1 = uuid::Uuid::new_v4();
    let other_l1 = uuid::Uuid::new_v4();
    let covered = std::collections::HashSet::from([format!("L1:{covered_l1}")]);
    let facts = vec![sourced_fact(
        ProfileField::Interests,
        "喜欢科幻电影",
        "电影,科幻",
        Some(other_l1),
        None,
    )];
    let kept = dedup_knowledge_facts(&facts, &covered, &[]);
    assert_eq!(kept.len(), 1, "ref 指向未覆盖文档 → 保留（可兜底注入）");
}

/// fact_doc_labels 把来源引用映射到与 RAG label 一致的文本空间。
#[test]
fn fact_doc_labels_match_rag_label_format() {
    let l1_id = uuid::Uuid::new_v4();
    let f = sourced_fact(
        ProfileField::History,
        "用户搬家了",
        "搬家",
        Some(l1_id),
        Some(8),
    );
    let labels = fact_doc_labels(&f);
    assert_eq!(labels.len(), 2);
    assert!(
        labels.contains(&format!("L1:{l1_id}")),
        "L1 label: {labels:?}"
    );
    assert!(labels.contains(&"L2:8".to_string()), "L2 label: {labels:?}");
    // 无 ref 时为空（调用方按保留处理）
    let plain = fact(ProfileField::History, "无引用事实", "x", FactTier::Stable);
    assert!(fact_doc_labels(&plain).is_empty());
}
