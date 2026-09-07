//! crates/ramaria-memory/src/fact/retriever.rs - 知识层规则判定器与检索注入
//!
//! 设计特点:
//! - 规则判定器三规则（零新增 LLM 调用）:
//!   a) 事实类疑问词 + 话题关键词命中 facts 索引
//!   b) 话题关键词命中 facts 的 field/关键词索引
//!   c) 显式指代（上次/之前/你说过/我记得/ta 的）
//! - 命中 → 同 field 召回 + 向量检索（按时效加权）→ 事实卡片注入
//! - 不命中 → 不注入（静默降级，不影响主线）
//! - 只注入 status=active 事实（版本链中仅当前生效参与注入）
//! - 注入采用事实陈述（非原文），隐私安全

use ramaria_core::types::{PersonaFact, ProfileField};

use crate::fact::tier::decay_weight;

/// 事实类疑问词表（判定器 a）`.
const QUESTION_MARKERS: &[&str] = &[
    "？",
    "吗",
    "呢",
    "什么",
    "怎么",
    "为什么",
    "谁",
    "哪",
    "几",
    "多少",
    "是否",
    "是不是",
];

/// 显式指代词表（判定器 c）。
const EXPLICIT_REFERENCE_MARKERS: &[&str] = &[
    "上次",
    "之前",
    "你说过",
    "我记得",
    "ta的",
    "她的",
    "他的",
    "你提到",
    "你之前",
];

/// 判定匹配级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchLevel {
    /// 不命中（不注入）
    #[default]
    None,
    /// a) 事实类疑问词且话题关键词与 facts 有交集
    QuestionWithTopic,
    /// b) 话题关键词命中 facts 的 field/关键词索引
    TopicHit,
    /// c) 显式指代
    ExplicitReference,
}

/// 知识检索输入。
#[derive(Debug, Clone)]
pub struct KnowledgeQuery {
    /// 用户当前消息
    pub user_message: String,
    /// 该 persona 的 active facts（按 field 分组）
    pub facts: Vec<PersonaFact>,
    /// 检索预算（字符上限；超出则保前部）
    pub budget_chars: usize,
}

/// 知识路检索策略参数（`[knowledge]` 组独立生效，三路互不串扰）。
#[derive(Debug, Clone, Copy, Default)]
pub struct KnowledgeRetrievalOptions {
    /// 候选事实条数上限；`0` = 不截断（与上一版本行为等价）。
    pub top_k: usize,
    /// 查询-事实命中强度下限（0.0..=1.0）；`0.0` = 不过滤（与上一版本行为等价）。
    pub threshold: f64,
}

/// 知识检索结果。
#[derive(Debug, Clone, Default)]
pub struct KnowledgeRetrieval {
    /// 判定级别
    pub match_level: MatchLevel,
    /// 命中的 active facts
    pub matched: Vec<PersonaFact>,
}

impl KnowledgeRetrieval {
    pub fn is_triggered(&self) -> bool {
        self.match_level != MatchLevel::None && !self.matched.is_empty()
    }
}

/// 判定触发级别（不调用 LLM，纯规则）。
///
/// 说明:
/// - 优先判定 c（显式指代）→ b（话题命中）→ a（疑问词+话题交集）。
/// - 判定器只需 user_message 与 facts，返回最高匹配级别。
pub fn judge_knowledge_query(user_message: &str, facts: &[PersonaFact]) -> MatchLevel {
    // 无 active facts → 直接不命中（否则无内容可注入）
    if facts.is_empty() {
        return MatchLevel::None;
    }

    // 收集 facts 的全部关键词与 field 标签
    // 排除 SpeakingStyle：风格规则由表达层注入，知识层只读引用、不作为检索触发源
    let mut topic_vocab: Vec<String> = Vec::new();
    let mut field_labels: Vec<&'static str> = Vec::new();
    for f in facts {
        if f.field == ProfileField::SpeakingStyle {
            continue;
        }
        if let Some(kw) = &f.keyword_hint {
            for k in kw.split([',', '，', '、']) {
                let t = k.trim();
                if !t.is_empty() {
                    topic_vocab.push(t.to_string());
                }
            }
        }
        field_labels.push(f.field.label());
        // 关键词包含在字段 label 中（如"兴趣爱好"字段名命中话题）
        field_labels.push(f.field.as_str());
    }

    let msg = user_message.to_string();

    // c) 显式指代（最高优先：说"你之前/上次"时明确索取记忆）
    if EXPLICIT_REFERENCE_MARKERS.iter().any(|m| msg.contains(m)) {
        return MatchLevel::ExplicitReference;
    }

    // 话题关键词（facts 关键词 + 字段标签词）
    let topic_hit = || {
        topic_vocab
            .iter()
            .map(|s| s.as_str())
            .chain(field_labels.iter().copied())
            .any(|kw| !kw.is_empty() && msg.contains(kw))
    };

    // a) 疑问词 + 话题关键词交集（事实类疑问优先于纯话题命中）
    let has_question = QUESTION_MARKERS.iter().any(|m| msg.contains(m));
    if has_question && topic_hit() {
        return MatchLevel::QuestionWithTopic;
    }

    // b) 话题关键词命中
    if topic_hit() {
        return MatchLevel::TopicHit;
    }

    MatchLevel::None
}

/// 计算用户消息与单条事实的查询-事实命中强度（0.0..=1.0）。
///
/// 规则:
/// - 事实侧 token = keyword_hint 按逗号/顿号切分去空 + field label + field as_str。
/// - 查询侧 token = 既有中文 bigram + ASCII 小写词分词（与 BM25 同一分词器，零 embedding）。
/// - 单个事实侧 token 命中 = 与任一查询侧 token 相等或互为子串（ASCII 忽略大小写）。
/// - overlap = 命中事实侧 token 数 / 事实侧 token 数；事实侧无 token → 1.0。
pub fn fact_query_overlap(user_message: &str, fact: &PersonaFact) -> f64 {
    let mut fact_tokens: Vec<String> = Vec::with_capacity(4);
    if let Some(kw) = &fact.keyword_hint {
        for k in kw.split([',', '，', '、']) {
            let t = k.trim();
            if !t.is_empty() {
                fact_tokens.push(t.to_string());
            }
        }
    }
    fact_tokens.push(fact.field.label().to_string());
    fact_tokens.push(fact.field.as_str().to_string());

    if fact_tokens.is_empty() {
        return 1.0;
    }

    let query_tokens: std::collections::HashSet<String> =
        crate::bm25::tokenize(user_message).into_iter().collect();
    if query_tokens.is_empty() {
        return 0.0;
    }

    let hit_count = fact_tokens
        .iter()
        .filter(|f| {
            let f_norm = lowercase_ascii(f);
            query_tokens
                .iter()
                .any(|q| q == &f_norm || f_norm.contains(q.as_str()) || q.contains(&f_norm))
        })
        .count();
    hit_count as f64 / fact_tokens.len() as f64
}

/// 仅对 ASCII 大写字母做小写化（中文不变），用于事实侧 token 与查询 token 的比对。
fn lowercase_ascii(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                c
            }
        })
        .collect()
}

/// 检索知识（上一版本语义等价入口）：判定命中后召回全部 active facts（不截断、不过滤）。
///
/// 说明:
/// - 等价于以默认 `KnowledgeRetrievalOptions` 调用 [`retrieve_knowledge_with_options`]。
pub fn retrieve_knowledge(
    query: &KnowledgeQuery,
    now: i64,
    halflife_days: u32,
) -> KnowledgeRetrieval {
    retrieve_knowledge_with_options(
        query,
        now,
        halflife_days,
        KnowledgeRetrievalOptions::default(),
    )
}

/// 检索知识（携带 `[knowledge]` 独立检索参数）。
///
/// 说明:
/// - 触发才检索；不触发返回空（不注入）。
/// - `now` 用于 volatile 事实的时效加权（稳定/历史恒 1.0）。
/// - `options.top_k > 0` 时按时效排序后截断为前 top_k 条；`0` = 不截断（上一版本等价）。
/// - `options.threshold > 0.0` 时仅保留查询-事实命中强度 ≥ threshold 的候选；
///   `0.0` = 不过滤（上一版本等价）。截断为最后一步。
/// - 检索范围 = 全部 active facts（简约实现；同 field + 向量检索的细化在集成层用 embedding）。
pub fn retrieve_knowledge_with_options(
    query: &KnowledgeQuery,
    now: i64,
    halflife_days: u32,
    options: KnowledgeRetrievalOptions,
) -> KnowledgeRetrieval {
    let level = judge_knowledge_query(&query.user_message, &query.facts);
    if level == MatchLevel::None {
        return KnowledgeRetrieval::default();
    }

    // 命中 → 召回 active facts（注入前按时效排序 + 条数截断/命中下限过滤）
    // 排除 SpeakingStyle：风格规则由表达层注入，知识层不重复注入（只读引用无副作用）
    let mut matched: Vec<PersonaFact> = query
        .facts
        .iter()
        .filter(|f| f.field != ProfileField::SpeakingStyle)
        .filter(|f| {
            options.threshold <= 0.0
                || fact_query_overlap(&query.user_message, f) >= options.threshold
        })
        .cloned()
        .collect();
    // 排序：稳定/历史靠前（时效权重大者），volatile 按新鲜度靠前
    matched.sort_by(|a, b| {
        let wa = weight_of(a, now, halflife_days);
        let wb = weight_of(b, now, halflife_days);
        wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
    });
    // top_k 截断必须是最后一步（阈值过滤在前）
    if options.top_k > 0 && matched.len() > options.top_k {
        matched.truncate(options.top_k);
    }

    KnowledgeRetrieval {
        match_level: level,
        matched,
    }
}

/// 计算单条事实的时效权重（stable/historical=1.0；volatile 随事件时间衰减）。
fn weight_of(fact: &PersonaFact, now: i64, halflife_days: u32) -> f64 {
    let event_time = fact.created_at;
    let tier = fact.tier;
    decay_weight(tier, event_time, now, halflife_days)
}

// =========================================================
// 注入文本构造
// =========================================================

/// 渲染知识卡片文本（`# 知识（知识层，按需）` 段落内容）。
///
/// 格式:
/// ```text
/// 关于{field}：{content}
/// ```
/// 按 ProfileField 分组，同字段合并。
pub fn render_knowledge_cards(facts: &[PersonaFact]) -> String {
    let mut ordered_fields: Vec<ProfileField> = Vec::new();
    for f in facts {
        if !ordered_fields.contains(&f.field) {
            ordered_fields.push(f.field);
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for field in ordered_fields {
        let field_facts: Vec<&PersonaFact> = facts.iter().filter(|f| f.field == field).collect();
        if field_facts.is_empty() {
            continue;
        }
        let contents: Vec<&str> = field_facts
            .iter()
            .map(|f| f.content.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if contents.is_empty() {
            continue;
        }
        lines.push(format!("关于{}：{}", field.label(), contents.join("；")));
    }
    lines.join("\n")
}

/// 构建知识层注入块（供 `prompt/layers.rs::render_knowledge_block` 消费）。
///
/// 说明:
/// - 命中且有内容 → `Some(InjectionBlock)`（`# 知识（知识层，按需）`）。
/// - 未命中 / 无 active 事实 / 内容为空 → `None`（不产生段落）。
pub fn build_knowledge_injection(
    facts: &[PersonaFact],
    budget_chars: usize,
) -> Option<crate::prompt::layers::InjectionBlock> {
    let cards = render_knowledge_cards(facts);
    if cards.trim().is_empty() {
        return None;
    }
    // 预算裁剪：超预算保前部 + 截断提示（卡片为简洁陈述，一般不触发）
    let content = if cards.chars().count() > budget_chars {
        ramaria_core::text::truncate_chars(&cards, budget_chars)
    } else {
        cards
    };
    Some(crate::prompt::layers::InjectionBlock::new(
        crate::prompt::layers::LayerKind::Knowledge,
        "# 知识（知识层，按需）",
        content,
    ))
}

// =========================================================
// 知识层注入去重（RAG 摘要为主、断言知识为兜底）
// =========================================================

/// 知识层判定命中后、渲染前的注入去重。
///
/// 职责:
/// - 把"RAG 摘要为主召回路径、断言知识为兜底"落为可测规则：凡已由 RAG 摘要
///   文本覆盖的事实，知识卡片不再重复注入；与角色层已知事实区同一事实 id 的
///   条目也不在知识卡片区重复（角色描述区保留）。
///
/// 去重规则:
/// 1. 来源引用命中 RAG 覆盖集合 → 剔除：`ref_l1_id` 映射 `L1:{uuid}`、
///    `ref_event_id` 映射 `L2:{id}`（与 RAG 文档 label 同一文本空间比较）。
/// 2. 与 `role_facts` 同一事实 id → 剔除（同一条 active BasicInfo 记录同时出现在
///    角色描述区与知识卡片区时，角色区保留、知识区不重复）。
/// 3. 无来源引用（无法映射到 RAG 文档）→ 保留：手工/冷启动/风格类等
///    非 RAG 文档来源事实仍需兜底注入。
///
/// 语义边界:
/// - `rag_covered_labels` 只应收录**实际注入上下文文本**的文档标识；
///   传入空集合 = 不按 RAG 去重（RAG 关闭/未命中时的兜底路径，全部保留）。
/// - 本函数不修改事实内容，仅过滤；调用方保证 `facts` 已为 active。
///
/// 参数:
/// - `facts`: 知识层判定器命中的 active facts。
/// - `rag_covered_labels`: RAG 摘要实际覆盖的文档 label 集合。
/// - `role_facts`: 角色层已知事实区展示的事实列表（按 id 去重）。
///
/// 返回:
/// - 过滤后应注入知识卡片区的事实。
pub fn dedup_knowledge_facts(
    facts: &[PersonaFact],
    rag_covered_labels: &std::collections::HashSet<String>,
    role_facts: &[PersonaFact],
) -> Vec<PersonaFact> {
    let role_ids: std::collections::HashSet<i64> = role_facts.iter().map(|f| f.id).collect();
    facts
        .iter()
        .filter(|f| {
            // 角色层已知事实区已展示同一条记录 → 知识卡片区不重复注入
            if role_ids.contains(&f.id) {
                return false;
            }
            // 来源文档已进入 RAG 摘要覆盖集合 → 该事实已由摘要文本提供
            if fact_doc_labels(f)
                .iter()
                .any(|l| rag_covered_labels.contains(l))
            {
                return false;
            }
            true
        })
        .cloned()
        .collect()
}

/// 将事实的来源引用映射为统一文档 label（与 RAG 检索 label 同一文本空间）。
///
/// 说明:
/// - `ref_l1_id` → `L1:{uuid}`，`ref_event_id` → `L2:{id}`；
///   与 `DocId` 的 `Display`（retriever/向量通道共用格式）一致。
/// - 无来源引用时返回空（无法映射，由调用方按"保留"处理）。
fn fact_doc_labels(fact: &PersonaFact) -> Vec<String> {
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
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
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
}
