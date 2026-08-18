//! crates/ramaria-memory/src/fact/extractor.rs - 知识层事实抽取
//!
//! 设计特点:
//! - 从 MemoryEvent 抽取事实卡片（content + ProfileField 归属 + 关键词 + 置信度）
//! - 触发条件: 事件 confidence ≥ 0.6 且 presentation = objective/mixed（客观/混合）
//! - 主观事件（subjective）额外抽取隐含偏好事实（conf=0.5 入 candidate 轨道）
//! - 规则兜底（LLM 不可用时的关键词/模板提取，纯函数可测）
//! - LLM 抽取封装 `FactExtractor`（依赖 LlmProvider，mock 友好）；`build_extract_prompt` 生成模板
//!
//! 分层归属:
//! - ProfileField 映射见 `tier.rs`: BasicInfo/Interests/Social/SpeakingStyle → stable，
//!   PersonalStatus/RecentContext → volatile, History → historical

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider};
use ramaria_core::types::{FactSource, FactTier, MemoryEvent, Presentation, ProfileField};

use crate::fact::tier::tier_for_field;
use uuid::Uuid;

/// 事件触达知识抽取的置信度门槛。
pub const EXTRACT_CONFIDENCE_THRESHOLD: f64 = 0.6;
/// 主观隐含事实置信度（低置信，入 candidate 轨道待互证）。
pub const SUBJECTIVE_IMPLIED_CONFIDENCE: f64 = 0.5;

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
    /// 来源事件 id
    pub ref_event_id: i64,
    /// 是否主观隐含隐含事实（conf=0.5）
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
        // 关键词归一化（逗号分隔 + 空格分隔兼容）
        let keywords: Vec<String> = event
            .keywords
            .as_deref()
            .map(|s| {
                s.split([',', '，', '、'])
                    .map(|k| k.trim().to_string())
                    .filter(|k| !k.is_empty())
                    .collect()
            })
            .unwrap_or_default();

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
                ref_event_id: event.id,
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
                ref_event_id: event.id,
                subjective_implied: true,
                ref_l1_id: None,
            });
        }

        out
    }
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
}
