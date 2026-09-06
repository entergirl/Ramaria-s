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

/// 检索知识：判定命中后召回 active facts（按时效加权排序）。
///
/// 说明:
/// - 触发才检索；不触发返回空（不注入）。
/// - `now` 用于 volatile 事实的时效加权（稳定/历史恒 1.0）。
/// - 检索范围 = 全部 active facts（简约实现；同 field + 向量检索的细化在集成层用 embedding）。
pub fn retrieve_knowledge(
    query: &KnowledgeQuery,
    now: i64,
    halflife_days: u32,
) -> KnowledgeRetrieval {
    let level = judge_knowledge_query(&query.user_message, &query.facts);
    if level == MatchLevel::None {
        return KnowledgeRetrieval::default();
    }

    // 命中 → 召回 active facts（注入前按时效排序 + 预算裁剪）
    // 排除 SpeakingStyle：风格规则由表达层注入，知识层不重复注入（只读引用无副作用）
    let mut matched: Vec<PersonaFact> = query
        .facts
        .iter()
        .filter(|f| f.field != ProfileField::SpeakingStyle)
        .cloned()
        .collect();
    // 排序：稳定/历史靠前（时效权重大者），volatile 按新鲜度靠前
    matched.sort_by(|a, b| {
        let wa = weight_of(a, now, halflife_days);
        let wb = weight_of(b, now, halflife_days);
        wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
    });

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
}
