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
// 知识层按需检索（对话注入用例编排）
// =========================================================

/// 从存储加载 persona 的 active 事实并做判定器命中判断（对话注入用例编排）。
///
/// 职责:
/// - 对话轮次知识层"兜底注入"的完整编排：读 active 事实 → 判定器命中判断
///   → 时效重排召回（零新增 LLM 调用，纯规则）。
/// - 在线管线与服务层 recall 用例共用同一份实现，避免两处口径漂移。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `config`: `[knowledge]` 配置（判定器开关、检索 top_k / 阈值、时效半衰期、渲染预算）。
/// - `persona_uid`: 目标 persona（严格隔离，跨 persona 不可见）。
/// - `user_message`: 用户当前输入（判定器与召回输入）。
///
/// 返回:
/// - 判定器命中且检索有结果 → 匹配的 active facts（按时效排序）。
/// - 未命中 / 关闭 / 检索失败 → 空 Vec（不注入，不阻塞对话主流程）。
pub async fn load_knowledge_facts_for_query(
    storage: &dyn ramaria_core::traits::StorageBackend,
    config: &ramaria_core::config::KnowledgeConfig,
    persona_uid: &str,
    user_message: &str,
) -> Vec<PersonaFact> {
    // 判定器关闭 → 不检索注入
    if !config.detector_enabled {
        return Vec::new();
    }

    // 读取活性事实（失败 → 降级为空，不阻塞）
    let active = match storage.list_active_facts_by_persona(persona_uid).await {
        Ok(facts) => facts,
        Err(e) => {
            tracing::warn!(
                persona_uid,
                error = %e,
                "知识层检索：读取 active 事实失败，本次不注入"
            );
            return Vec::new();
        }
    };
    if active.is_empty() {
        return Vec::new();
    }

    // 判定器命中判断（未命中 → 不注入）
    if judge_knowledge_query(user_message, &active) == MatchLevel::None {
        return Vec::new();
    }

    // 命中 → 召回（active 事实按时效排序；top_k / 阈值走 [knowledge] 独立检索参数，
    // 0 / 0.0 = 不截断 / 不过滤；渲染预算由调用方裁剪）
    let query = KnowledgeQuery {
        user_message: user_message.to_string(),
        facts: active,
        budget_chars: config.injection_budget_chars,
    };
    let options = KnowledgeRetrievalOptions {
        top_k: config.retrieve_top_k as usize,
        threshold: config.retrieve_threshold,
    };
    let now = ramaria_core::types::now_ms();
    let retrieval =
        retrieve_knowledge_with_options(&query, now, config.volatile_halflife_days, options);
    if retrieval.matched.is_empty() {
        Vec::new()
    } else {
        retrieval.matched
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
