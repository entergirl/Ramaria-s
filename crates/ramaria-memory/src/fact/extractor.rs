//! crates/ramaria-memory/src/fact/extractor.rs - 知识层事实抽取
//!
//! 设计特点:
//! - 从 MemoryEvent 抽取事实卡片（content + ProfileField 归属 + 关键词 + 置信度）
//! - 触发条件: 事件 confidence ≥ 0.6 且 presentation = objective/mixed（客观/混合）
//! - 主观事件（subjective）额外抽取隐含事实（conf=0.5 入 candidate 轨道待互证）
//! - auto_fact_detect 增强（召回兜底，非主力）：
//!   - 策略① 隐含事实按字段补全：主观/低置信事件按 classify_event_field
//!     产对应字段语义前缀的隐含候选（偏好/关系/经历/近况/说话风格）
//!   - 策略② 线索→断言覆盖：将 L1 保真线索（EvidenceNote）提升为断言级候选，
//!     来源标记 FactSource::L1、置信 0.55（仍 <0.6 走 candidate 轨道）
//! - 规则兜底（LLM 不可用时的关键词/模板提取，纯函数可测）
//! - LLM 抽取封装 `FactExtractor`（依赖 LlmProvider，mock 友好）；`build_extract_prompt` 生成模板
//!
//! 分层归属:
//! - ProfileField 映射见 `tier.rs`: BasicInfo/Interests/Social/SpeakingStyle → stable，
//!   PersonalStatus/RecentContext → volatile, History → historical

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider};
use ramaria_core::types::{
    EvidenceNote, FactSource, FactTier, MemoryEvent, MemoryL1, Presentation, ProfileField,
};

use crate::fact::tier::tier_for_field;
use crate::keyword::normalizer::{BigramNormalizer, KeywordNormalizer};
use uuid::Uuid;

/// 事件触达知识抽取的置信度门槛。
pub const EXTRACT_CONFIDENCE_THRESHOLD: f64 = 0.6;
/// 主观隐含事实置信度（低置信，入 candidate 轨道待互证）。
pub const SUBJECTIVE_IMPLIED_CONFIDENCE: f64 = 0.5;
/// L1 保真线索提升为断言候选的最小文本长度（字符，与 L1 摘要 evidence 校验口径一致）。
pub const L1_EVIDENCE_MIN_CHARS: usize = 5;
/// L1 保真线索断言候选内容的最大字符数（截断防超长句入库）。
pub const L1_EVIDENCE_MAX_CONTENT_CHARS: usize = 120;
/// L1 保真线索断言置信度（比规则主观推断略高：线索来自 LLM 摘要的保真断言，
/// 但仍低于常规门槛，入 candidate 轨道待互证后才提升）。
pub const L1_EVIDENCE_CONFIDENCE: f64 = 0.55;
/// 从自由文本提取关键词的最大数量（bigram 过多时截断，控制候选关键词噪声）。
pub const MAX_TEXT_KEYWORDS: usize = 16;

/// 事实候选（抽取中间产物）。
#[derive(Debug, Clone)]
pub struct FactCandidate {
    /// 事实内容
    pub content: String,
    /// 字段归属
    pub field: ProfileField,
    /// 分层
    pub tier: FactTier,
    /// 关键词（逗号分隔）
    pub keywords: Vec<String>,
    /// 置信度
    pub confidence: f64,
    /// 来源类型
    pub source: FactSource,
    /// 来源事件 id（None = 非事件来源，如 L1 保真线索）
    pub ref_event_id: Option<i64>,
    /// 是否主观隐含事实（conf=0.5）
    pub subjective_implied: bool,
    /// 来源 L1 id（事件溯源，可选）
    pub ref_l1_id: Option<Uuid>,
}

// =========================================================
// 字段归属heuristic（规则兜底）
// =========================================================

/// 依据事件标题/摘要/关键词推断 ProfileField。
///
/// 说明:
/// - 用关键词黑名单做低开销启发式；无法判定时回退 `RecentContext`（近期背景最通用）。
/// - 关键词为逗号分隔集合，包含标题/摘要/关键词字段的拼接。
pub fn classify_event_field(title: &str, summary: &str, keywords: Option<&str>) -> ProfileField {
    let haystack = format!("{title} {summary} {}", keywords.unwrap_or(""));
    let h = haystack.to_lowercase();

    let hits = |list: &[&str]| list.iter().any(|k| h.contains(k));

    // 兴趣爱好（显式提示词）
    if hits(&["喜欢", "爱好", "喜欢看", "爱读", "热衷", "兴趣", "每天会"]) {
        return ProfileField::Interests;
    }
    // 历史事件（过去时 / 明确发生过了）
    if hits(&[
        "曾经",
        "过去",
        "以前",
        "当时",
        "已经",
        "上年",
        "上个月",
        "毕业后",
    ]) {
        return ProfileField::History;
    }
    // 社交（人际/家人/朋友/社交）
    if hits(&[
        "朋友",
        "家人",
        "同事",
        "同学",
        "社交",
        "恋爱",
        "对象",
        "朋友聚会",
    ]) {
        return ProfileField::Social;
    }
    // 说话风格（语气/风格/口头禅）
    if hits(&["口头禅", "说话", "语气", "爱说", "习惯说"]) {
        return ProfileField::SpeakingStyle;
    }
    // 从分类标签关键词
    if hits(&["工作", "项目", "加班", "上班", "离职", "入职"]) {
        return ProfileField::PersonalStatus;
    }
    // 默认近期背景
    ProfileField::RecentContext
}

/// 判断事件是否满足知识抽取触发条件（confidence ≥ 0.6 且客观/混合）。
pub fn should_extract(event: &MemoryEvent) -> bool {
    event.confidence >= EXTRACT_CONFIDENCE_THRESHOLD
        && event.presentation != Presentation::Subjective
}

// =========================================================
// 规则兜底抽取（纯函数）
// =========================================================

/// 规则抽取器（无 LLM 依赖，降级路径与确定性测试用）。
pub struct RuleExtractor;

impl RuleExtractor {
    /// 从单个事件抽取候选事实（客观/混合轨道）。
    ///
    /// 说明:
    /// - 由事件的 paraphrase（去情境化重述）或关键词派生 content。
    /// - 客观/混合事件若满足触发条件产出常规事实；主观事件额外产出隐含偏好事实（conf=0.5）。
    ///
    /// 返回:
    /// - 0 个或多个 `FactCandidate`（可能同时含常规 + 隐含偏好）。
    pub fn extract_from_event(event: &MemoryEvent) -> Vec<FactCandidate> {
        let field = classify_event_field(&event.title, &event.summary, event.keywords.as_deref());
        let tier = tier_for_field(field);
        let keywords = split_keywords(event.keywords.as_deref());

        // 事实内容：优先 paraphrase（态度去情境化），否则用标题
        let base_content = event
            .paraphrase
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&event.title)
            .trim()
            .to_string();
        if base_content.is_empty() {
            return vec![];
        }

        let mut out = Vec::new();

        // 客观/混合事件：常规事实
        if should_extract(event) {
            out.push(FactCandidate {
                content: base_content.clone(),
                field,
                tier,
                keywords: keywords.clone(),
                confidence: event.confidence,
                source: FactSource::Event,
                ref_event_id: Some(event.id),
                subjective_implied: false,
                ref_l1_id: None,
            });
        }

        // 主观事件（或不满足客观门槛时）额外产出隐含偏好事实（conf=0.5）
        if event.presentation == Presentation::Subjective
            || event.confidence < EXTRACT_CONFIDENCE_THRESHOLD
        {
            // 隐含偏好事实：以 paraphrase/标题为"偏好内容"，标注 confidence 0.5
            out.push(FactCandidate {
                content: format!("偏好：{base_content}"),
                field: ProfileField::Interests,
                tier: FactTier::Stable,
                keywords,
                confidence: SUBJECTIVE_IMPLIED_CONFIDENCE,
                source: FactSource::Event,
                ref_event_id: Some(event.id),
                subjective_implied: true,
                ref_l1_id: None,
            });
        }

        out
    }
}

// =========================================================
// 策略① 主观隐含事实补全（字段感知，auto_fact_detect 增强）
// =========================================================

/// 关键词归一化 helper：逗号/顿号分隔，去空白，保留非空段。
///
/// 说明:
/// - 供常规抽取与增强抽取共用同一解析口径，行为与历史实现等价。
fn split_keywords(raw: Option<&str>) -> Vec<String> {
    raw.map(|s| {
        s.split([',', '，', '、'])
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// 字段对应的隐含事实语义前缀。
///
/// 说明:
/// - 主观/低置信事件的表达往往无法断言"是什么"，只能以弱化语气提示方向；
///   前缀用于让 content 可读且按字段区分，不改变"隐含、待互证"的置信语义。
/// - `ProfileField::BasicInfo` 无合适弱化前缀（基础信息本就该客观高置信），返回 None。
fn implied_prefix(field: ProfileField) -> Option<&'static str> {
    match field {
        ProfileField::Interests => Some("偏好："),
        ProfileField::Social => Some("关系："),
        ProfileField::History => Some("经历："),
        ProfileField::PersonalStatus | ProfileField::RecentContext => Some("近况："),
        ProfileField::SpeakingStyle => Some("说话风格："),
        // BasicInfo（以及未来新增字段）无合适弱化前缀：基础信息须客观高置信
        _ => None,
    }
}

/// 从主观/低置信事件产出字段感知的隐含事实候选（策略①增强层）。
///
/// 用法:
/// - 调用方在 auto_fact_detect 增强开启时，对"主观事件或 conf<0.6 事件"
///   替代 `extract_from_event` 的固定 Interests 隐含分支；关闭时回退 v1.7 旧函数。
///
/// 参数:
/// - `event`: 待抽取事件。
///
/// 返回:
/// - `Some(FactCandidate)`: 隐含事实，field 由 classify_event_field 决定，
///   content = 字段前缀 + 事件 paraphrase/标题，confidence=0.5，
///   subjective_implied=true（供仲裁走 candidate 轨道）。
/// - `None`: 事件不满足隐含条件（客观/混合且 conf≥0.6，走常规轨道）、
///   文本为空或分类字段不支持弱化（BasicInfo）。
pub fn extract_implied_fact_from_event(event: &MemoryEvent) -> Option<FactCandidate> {
    // 常规轨道（客观且达标）不在此函数职责内
    if should_extract(event) {
        return None;
    }
    let field = classify_event_field(&event.title, &event.summary, event.keywords.as_deref());
    let prefix = implied_prefix(field)?;
    let base_content = event
        .paraphrase
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&event.title)
        .trim()
        .to_string();
    if base_content.is_empty() {
        return None;
    }
    Some(FactCandidate {
        content: format!("{prefix}{base_content}"),
        field,
        tier: tier_for_field(field),
        keywords: split_keywords(event.keywords.as_deref()),
        confidence: SUBJECTIVE_IMPLIED_CONFIDENCE,
        source: FactSource::Event,
        ref_event_id: Some(event.id),
        subjective_implied: true,
        ref_l1_id: None,
    })
}

// =========================================================
// 策略② 线索→断言覆盖（auto_fact_detect 增强）
// =========================================================

/// 从自由文本提取关键词（bigram + 英文切分，排序去重，限量）。
///
/// 说明:
/// - 复用 `keyword::normalizer::BigramNormalizer`（M3 统一的中文 bigram /
///   英文按词切分实现），结果排序去重后截断到 `MAX_TEXT_KEYWORDS`，
///   供 L1 线索等无结构化 keywords 的文本抽取候选关键词。
pub fn extract_text_keywords(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = BigramNormalizer
        .normalize(text)
        .into_iter()
        .map(|t| t.as_str().to_string())
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens.truncate(MAX_TEXT_KEYWORDS);
    tokens
}

/// 将 L1 保真线索提升为断言级候选（策略②增强层）。
///
/// 业务意图:
/// - L1 摘要的 evidence_notes 是 LLM 从原文提炼的保真断言（"每周都要加班到很晚"），
///   当前只作为 L2 抽取 prompt 背景；本策略将其提升为低置信事实候选，
///   补上"事件抽取未覆盖但摘要已断言"的漏报兜底，仍走 candidate 轨道待互证。
///
/// 参数:
/// - `persona_uid`: 目标 persona；若 L1 已标注 persona 且与目标不一致则拒绝（隔离红线）。
/// - `l1`: 线索所属 L1（取其 id 作为事实溯源；persona_uid 供隔离校验）。
/// - `note`: L1 保真线索。
///
/// 返回:
/// - `Some(FactCandidate)`: content=线索 text（截断到 `L1_EVIDENCE_MAX_CONTENT_CHARS`），
///   field 由 classify_event_field 推断、tier 按 field，keywords 从 text 提取，
///   confidence=`L1_EVIDENCE_CONFIDENCE`(0.55，略高于规则主观推断但仍 <0.6 入 candidate)、
///   source=`FactSource::L1`、ref_l1_id=Some(l1.id)、ref_event_id=None、subjective_implied=false。
/// - `None`: text trim 后字符数 < `L1_EVIDENCE_MIN_CHARS`（对齐既有 L1 evidence 校验），
///   或 L1 persona 已标注且与 `persona_uid` 不一致。
///
/// 说明:
/// - 时间/who 槽位仅作证据元数据，不进 content（避免喧宾夺主）。
pub fn extract_from_l1_evidence(
    persona_uid: &str,
    l1: &MemoryL1,
    note: &EvidenceNote,
) -> Option<FactCandidate> {
    // persona 隔离：L1 已归属 persona 时必须与目标一致，避免跨 persona 泄漏
    if let Some(actual) = l1.persona_uid.as_deref()
        && actual != persona_uid
    {
        return None;
    }
    let text = note.text.trim();
    if text.chars().count() < L1_EVIDENCE_MIN_CHARS {
        return None;
    }
    // 截断到合理上限（按 Unicode 字符边界，不切开多字节字符）
    let content = ramaria_core::text::truncate_chars_bare(text, L1_EVIDENCE_MAX_CONTENT_CHARS);
    if content.is_empty() {
        return None;
    }
    let field = classify_event_field(text, text, None);
    Some(FactCandidate {
        content,
        field,
        tier: tier_for_field(field),
        keywords: extract_text_keywords(text),
        confidence: L1_EVIDENCE_CONFIDENCE,
        source: FactSource::L1,
        ref_event_id: None,
        subjective_implied: false,
        ref_l1_id: Some(l1.id),
    })
}

// =========================================================
// LLM 抽取
// =========================================================

/// 构造事实抽取 prompt（LLM 路径）。
///
/// 说明:
/// - 输入事件文本，要求 LLM 输出结构化 JSON 列表（content/field/keywords）。
/// - 隐私: prompt 仅含事件 paraphrase/摘要（结构化事实），不含原始对话全文。
pub fn build_extract_prompt(event_text: &str, persona_name: &str) -> String {
    format!(
        "请从以下关于 {persona_name} 的事件中提取可作为长期记住的人物事实。\n\
         输出 JSON 数组，每项字段:\n\
         - content: 事实陈述（简短、去情境化，用'TA'代称）\n\
         - field: 归属字段，取值之一 [basic_info, personal_status, interests, social, history, recent_context, speaking_style]\n\
         - keywords: 字符串数组（3-6 个关键词，用于判重和检索）\n\
         - confidence: 0.0-1.0 事实确凿度\n\
         只输出合法 JSON 数组，不要输出其他文字。\n\n\
         事件: {event_text}"
    )
}

/// 事实抽取器（依赖 LlmProvider，mock 友好）。
///
/// 职责:
/// - 承载 LLM 抽取的信息收集与 JSON 解析。
/// - LLM 调用失败时静默降级为规则兜底（`RuleExtractor`），不阻塞主流程。
pub struct FactExtractor<'a> {
    llm: &'a dyn LlmProvider,
    temperature: f64,
    max_tokens: u32,
}

impl<'a> FactExtractor<'a> {
    pub fn new(llm: &'a dyn LlmProvider, temperature: f64, max_tokens: u32) -> Self {
        Self {
            llm,
            temperature,
            max_tokens,
        }
    }

    /// 用 LLM 抽取事实 JSON 文本。
    ///
    /// 参数:
    /// - `event_text`: 去情境化事件文本（paraphrase/摘要）。
    /// - `persona_name`: 人物名。
    ///
    /// 返回:
    /// - 原始响应（由调用方解析入库；原始响应不落日志，见隐私红线）。
    pub async fn extract(&self, event_text: &str, persona_name: &str) -> RamariaResult<String> {
        let prompt = build_extract_prompt(event_text, persona_name);
        let request = ChatRequest {
            system_prompt: String::new(),
            memory_context: None,
            history: vec![],
            user_message: prompt,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            request_id: Uuid::new_v4(),
            template_version: crate::prompt::PROMPT_TEMPLATE_VERSION.to_string(),
        };
        self.llm.chat(&request).await
    }
}

/// 抽取输入（供编排层批量喂入）。
#[derive(Debug, Clone)]
pub struct ExtractInput {
    pub events: Vec<MemoryEvent>,
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::MemoryEvent;

    fn event(id: i64, title: &str, summary: &str, kw: &str, conf: f64) -> MemoryEvent {
        let mut e = MemoryEvent::new("char-0001".into(), title.into(), summary.into(), 0, 1000);
        e.id = id;
        e.keywords = Some(kw.to_string());
        e.confidence = conf;
        e.presentation = Presentation::Mixed;
        e
    }

    #[test]
    fn objective_fact_extracts_regular_candidate() {
        let ev = event(1, "加入开源项目", "正在参与项目开发", "工作,项目,开源", 0.9);
        let candidates = RuleExtractor::extract_from_event(&ev);
        assert!(!candidates.is_empty());
        let regular = candidates.iter().find(|c| !c.subjective_implied).unwrap();
        assert_eq!(regular.field, ProfileField::PersonalStatus);
        assert!(regular.confidence >= EXTRACT_CONFIDENCE_THRESHOLD);
        assert_eq!(regular.source, FactSource::Event);
    }

    #[test]
    fn subjective_event_yields_implied_fact() {
        let mut ev = event(2, "情绪低落", "最近压力大", "压力,情绪", 0.7);
        ev.presentation = Presentation::Subjective;
        let candidates = RuleExtractor::extract_from_event(&ev);
        let implied = candidates.iter().find(|c| c.subjective_implied);
        assert!(implied.is_some(), "主观事件应产出隐含偏好事实");
        let imp = implied.unwrap();
        assert_eq!(imp.confidence, SUBJECTIVE_IMPLIED_CONFIDENCE);
        assert!(imp.content.contains("偏好"));
    }

    #[test]
    fn below_threshold_event_only_implied() {
        // conf < 0.6 → 不触发常规事实，但可进入隐含偏好轨道
        let ev = event(3, "小事", "一般", "日常", 0.4);
        let candidates = RuleExtractor::extract_from_event(&ev);
        assert!(!candidates.is_empty());
        assert!(candidates.iter().all(|c| c.subjective_implied));
    }

    #[test]
    fn would_outline_classify_interests() {
        let ev = event(4, "喜欢看科幻电影", "经常看", "喜欢,科幻", 0.9);
        let candidates = RuleExtractor::extract_from_event(&ev);
        let regular = candidates.iter().find(|c| !c.subjective_implied).unwrap();
        assert_eq!(regular.field, ProfileField::Interests);
        assert!(!regular.keywords.is_empty());
    }

    #[test]
    fn empty_content_skips() {
        let ev = event(5, "", "", "无", 0.9);
        let candidates = RuleExtractor::extract_from_event(&ev);
        // 标题为空且无 paraphrase → 无候选
        assert!(candidates.is_empty() || candidates.iter().all(|c| c.confidence < 0.6));
    }

    #[test]
    fn prompt_builds_structure() {
        let prompt = build_extract_prompt("事件描述", "小明");
        assert!(prompt.contains("小明"));
        assert!(prompt.contains("content"));
        assert!(prompt.contains("JSON"));
    }

    // =========================================================
    // 策略① 主观隐含事实补全（字段感知增强层）
    // =========================================================

    /// 主观 Interests 事件 → "偏好：" 前缀 + Interests/Stable/conf=0.5。
    #[test]
    fn implied_fact_maps_interests_prefix() {
        let mut ev = event(
            10,
            "好喜欢看科幻电影",
            "提到喜欢科幻和悬疑",
            "喜欢,科幻",
            0.7,
        );
        ev.presentation = Presentation::Subjective;
        let cand = extract_implied_fact_from_event(&ev).expect("主观兴趣事件应产出隐含事实");
        assert_eq!(cand.field, ProfileField::Interests);
        assert_eq!(cand.tier, FactTier::Stable);
        assert!(
            cand.content.starts_with("偏好："),
            "content={}",
            cand.content
        );
        assert_eq!(cand.confidence, SUBJECTIVE_IMPLIED_CONFIDENCE);
        assert!(cand.subjective_implied);
        assert_eq!(cand.ref_event_id, Some(10));
        assert!(cand.ref_l1_id.is_none());
    }

    /// 主观 Social 事件 → "关系：" 前缀 + Social 字段（不只 Interests）。
    #[test]
    fn implied_fact_maps_social_prefix() {
        let mut ev = event(
            11,
            "说很看重和朋友的关系",
            "觉得朋友很重要",
            "朋友,关系",
            0.7,
        );
        ev.presentation = Presentation::Subjective;
        let cand = extract_implied_fact_from_event(&ev).expect("主观社交事件应产出隐含事实");
        assert_eq!(cand.field, ProfileField::Social);
        assert!(
            cand.content.starts_with("关系："),
            "content={}",
            cand.content
        );
        assert_eq!(cand.tier, FactTier::Stable);
    }

    /// 主观 History 事件 → "经历：" 前缀 + History 字段。
    #[test]
    fn implied_fact_maps_history_prefix() {
        let mut ev = event(12, "回想曾经养过猫的日子", "以前养过一只猫", "猫,曾经", 0.7);
        ev.presentation = Presentation::Subjective;
        let cand = extract_implied_fact_from_event(&ev).expect("主观历史事件应产出隐含事实");
        assert_eq!(cand.field, ProfileField::History);
        assert!(
            cand.content.starts_with("经历："),
            "content={}",
            cand.content
        );
        assert_eq!(cand.tier, FactTier::Historical);
    }

    /// 低置信（conf<0.6）客观事件 → "近况：" 前缀，field 按 PersonalStatus 关键词命中。
    #[test]
    fn implied_fact_maps_personal_status_for_low_confidence() {
        let ev = event(13, "最近加班多", "最近工作压力大", "工作,加班", 0.4);
        let cand = extract_implied_fact_from_event(&ev).expect("低置信事件应产出隐含事实");
        assert_eq!(cand.field, ProfileField::PersonalStatus);
        assert_eq!(cand.tier, FactTier::Volatile);
        assert!(
            cand.content.starts_with("近况："),
            "content={}",
            cand.content
        );
    }

    /// 常规轨道（客观且 conf≥0.6）不产隐含事实——增强函数只服务隐含轨道。
    #[test]
    fn implied_fact_skips_regular_track() {
        let ev = event(
            14,
            "加入开源项目",
            "正在参与项目开发",
            "工作,项目,开源",
            0.9,
        );
        assert!(
            extract_implied_fact_from_event(&ev).is_none(),
            "常规轨道事件不应产出隐含事实"
        );
    }

    // =========================================================
    // 策略② 线索→断言覆盖
    // =========================================================

    /// 构造带 id 与 persona 的 L1 夹具。
    fn l1_fixture(l1_id: Uuid, persona: &str) -> MemoryL1 {
        let mut l1 = MemoryL1::new(Uuid::new_v4(), "摘要".to_string(), None);
        l1.id = l1_id;
        l1.persona_uid = Some(persona.to_string());
        l1
    }

    /// L1 保真线索 → 断言级候选：source=L1、ref_l1_id 溯源、conf=0.55。
    #[test]
    fn l1_evidence_lifts_to_assertion_candidate() {
        let l1_id = Uuid::new_v4();
        let l1 = l1_fixture(l1_id, "char-0001");
        // text 承载事实断言；time/who/cause 元数据放在槽位（验证不进 content）
        let note = EvidenceNote::with_slots(
            "每周都要加班到很晚",
            Some("最近一个月".to_string()),
            Some("用户".to_string()),
            Some("项目上线压力大".to_string()),
        );
        let cand = extract_from_l1_evidence("char-0001", &l1, &note).expect("有效线索应产出候选");
        assert_eq!(cand.source, FactSource::L1);
        assert_eq!(cand.ref_event_id, None);
        assert_eq!(cand.ref_l1_id, Some(l1_id));
        assert!(!cand.subjective_implied);
        assert_eq!(cand.confidence, L1_EVIDENCE_CONFIDENCE);
        // 时间/who/cause 仅作元数据，不进 content（避免喧宾夺主）
        assert!(
            !cand.content.contains("最近一个月"),
            "content={}",
            cand.content
        );
        assert!(!cand.content.contains("用户"), "content={}", cand.content);
        assert!(cand.content.contains("加班"), "content={}", cand.content);
        // 关键词从 text 提取（bigram 命中"加班"）
        assert!(
            cand.keywords.iter().any(|k| k == "加班"),
            "keywords={:?}",
            cand.keywords
        );
        assert!(!cand.keywords.is_empty());
    }

    /// 过短 text（<5 字符）→ None，对齐 L1 evidence 校验口径。
    #[test]
    fn l1_evidence_short_text_is_none() {
        let l1 = l1_fixture(Uuid::new_v4(), "char-0001");
        let note = EvidenceNote::new("短");
        assert!(
            extract_from_l1_evidence("char-0001", &l1, &note).is_none(),
            "过短线索不应提升为断言候选"
        );
    }

    /// 空白 text → None。
    #[test]
    fn l1_evidence_blank_text_is_none() {
        let l1 = l1_fixture(Uuid::new_v4(), "char-0001");
        let note = EvidenceNote::new("   ");
        assert!(extract_from_l1_evidence("char-0001", &l1, &note).is_none());
    }

    /// 超长 text 截断到字符上限且不切多字节。
    #[test]
    fn l1_evidence_long_text_truncated() {
        let l1 = l1_fixture(Uuid::new_v4(), "char-0001");
        let long = format!("用户喜欢{}", "长句内容".repeat(30));
        let note = EvidenceNote::new(&long);
        let cand = extract_from_l1_evidence("char-0001", &l1, &note)
            .expect("超长线索仍应产出候选（截断）");
        assert!(cand.content.chars().count() <= L1_EVIDENCE_MAX_CONTENT_CHARS);
        assert!(
            cand.content.starts_with("用户喜欢"),
            "content={}",
            cand.content
        );
    }

    /// L1 persona 已归属他人 → 隔离拒绝（不跨 persona 泄漏）。
    #[test]
    fn l1_evidence_persona_mismatch_is_none() {
        let l1 = l1_fixture(Uuid::new_v4(), "char-other");
        let note = EvidenceNote::new("每周都要加班到很晚");
        assert!(
            extract_from_l1_evidence("char-0001", &l1, &note).is_none(),
            "persona 不一致应拒绝"
        );
    }
}
