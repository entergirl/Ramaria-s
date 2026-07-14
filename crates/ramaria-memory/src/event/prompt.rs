//! rust/crates/ramaria-memory/src/event/prompt.rs - 事件提取 Prompt 模板
//!
//! 设计特点:
//! - 事件提取 Prompt: 从多条 L1 摘要中提取结构化事件（11 个推断属性）
//! - v1.3 M3: 支持注入补充上下文段落（CompositeIndex 检索结果），标注防编造约束
//! - Paraphrase Prompt: 将态度的自然语言去情境化重述
//! - 严格 JSON 数组输出格式约束，引导 LLM 生成合规事件
//! - 字段约束（五档 confidence/salience/valence/share，三选一 presentation）嵌入 Prompt

// =========================================================
// 事件提取 Prompt
// =========================================================

/// 事件提取 Prompt 模板。
///
/// 用途: 给定同一个人的多条未吸收 L1 摘要，提取结构化离散事件。
///
/// 输出: JSON 数组，每个元素为一条事件，含 11 个推断属性。
///
/// 关键约束:
/// - confidence < 0.6 的事件不参与后续性格推断
/// - share 不设推断阈值，仅在 RAG 暴露环节过滤（share >= 0.3）
/// - 每个事件必须独立，不跨 L1 合并分歧信息
pub const EVENT_EXTRACTION_PROMPT: &str = r#"你是一个事件提取助手。请根据下面多条对话摘要（L1），提取其中的离散事件，并推断事件之间的因果关系和其背后的底层动机。

【输出格式要求】
严格按照以下 JSON 格式输出，包含 "events" 数组和 "relations" 数组：

{
  "events": [
    {
      "title": "≤20字标题，概括事件核心",
      "summary": "2-3句话描述事件经过，用'用户'指代当前分析的人物",
      "keywords": "逗号分隔的名词标签（含分类标签+地点），5-8个",
      "participants": ["其他参与者的名称或角色", "如无则返回空数组"],
      "confidence": 0.8,
      "salience": 0.7,
      "valence": -0.5,
      "presentation": "subjective",
      "share": 0.3,
      "attitude": "该人物对此事的态度（自然语言描述），如无则填空字符串",
      "motives": ["地位维护", "自主性"]
    }
  ],
  "relations": [
    {
      "from_index": 0,
      "to_index": 1,
      "kind": "CausedBy",
      "weight": 0.8,
      "detail": "简述因果关系（1句话）"
    }
  ]
}

【字段说明】
- title: ≤20字，用中文概括事件的核心内容
- summary: 2-3句话，从当前分析的人物的视角描述事件；用"用户"指代此人
- keywords: 5-8个名词标签，用英文逗号分隔；优先使用能描述事件分类和地点的标签
- participants: JSON 字符串数组；列出事件中除"用户"以外的其他参与者角色或名称
- confidence: 事实确凿度，0.0..1.0。确凿=1.0（如"用户明确说了..."），推测=0.5（如"可能发生了..."），<0.6不参与性格推断

- salience: 情感显著性，0.0..1.0。只能从以下五个值中选一个：
  0.0   平淡（纯事务，无情感投入）
  0.25  轻微（有轻微情绪但不重要）
  0.5   中等（正常事件，有情感内容）
  0.75  较高（情绪明显，或对"用户"有重要意义）
  1.0   极高（强烈情绪，或人生重要节点/里程碑）

- valence: 情绪效价，-1.0..1.0。只能从以下五个值中选一个：
  -1.0  非常消极（崩溃、绝望、强烈愤怒）
  -0.5  偏消极（疲惫、担心、轻度低落）
   0.0  中性（平静日常、无明显情绪）
   0.5  偏积极（放松、满意、轻度开心）
   1.0  非常积极（兴奋、强烈成就感、里程碑）

- presentation: 陈述方式。三选一：
  "objective"  - 客观事实（"用户今天去了医院"）
  "subjective" - 主观感受（"用户觉得很难过"）
  "mixed"      - 混合（兼有事实和感受）

- share: 分享意愿 0.0..1.0。该事件内容是否适合告诉他人。
  0.0 = 极度私密（不应与任何人分享）
  0.3 = 可分享给亲密的人
  0.7 = 可一般分享
  1.0 = 完全公开无妨

- attitude: 该人物对此事件的态度（自然语言一句话），例如"感到骄傲""有些遗憾""很平静地接受"
  如果无法判断或事件为纯事实描述，填空字符串 ""

- motives: 字符串数组，该事件背后反映的底层动机。从以下七类中选择最相关的 1-3 个：
  自我保护 - 事件与人身安全、健康、规避威胁相关
  归属 - 事件与寻求接纳、维护人际关系、害怕孤立相关
  地位 - 事件与维护自尊、争取认可、避免轻视相关
  自主 - 事件与争取控制权、抵制约束、要求自主决策相关
  公平 - 事件与追求公平公正、对不公感到愤怒相关
  养育 - 事件与照顾他人、保护弱者、培养后辈相关
  求知 - 事件与探索未知、学习新事物、理解世界相关
  如无法判断，返回空数组 []

【事件关系提取】（relations 数组）
- from_index / to_index: 引用 events 数组中事件的索引（从 0 开始），表示 from → to 的关系方向
- kind: 关系类型，六选一：
  "CausedBy"    - from 是 to 的因果前因
  "PartOf"      - from 是 to 的子事件
  "RelatedTo"   - 一般主题关联
  "ContinuedBy" - from 被 to 延续/发展
  "Contradicts" - from 与 to 矛盾
  "Timeline"    - 纯时序先后（无因果关系时使用）
- weight: 关系确信度，0.0..1.0
- detail: 1句话简述关系逻辑
- 如果事件间无明显关系可提取，返回空数组 []

【提取规则】
1. 每条事件必须基于下方 L1 摘要中的具体内容，不可凭空编造
2. 同一主题可能分散在多条 L1 中，应合并为一个事件（同时提高 confidence）
3. 避免将无关 L1 强行合并为一个"杂项"事件——降级时由系统自动处理
4. 如果某条 L1 无法归入任何事件，可以忽略（系统已有降级兜底）
5. 最多提取 5 条事件。如果 L1 非常琐碎无法提炼事件，返回空 events 数组和空 relations 数组

【待分析的 L1 摘要列表】
{l1_summaries}

请输出上述 JSON 格式（含 events 和 relations）："""#;

// =========================================================
// 补充上下文段落模板（v1.3 M3）
// =========================================================

/// 补充上下文段落的标题/说明。
///
/// 注入到事件提取 Prompt 中，标注以下约束:
/// - "仅供背景参考": 明确告知 LLM 不得将历史事件直接复制为当前事件。
/// - "不得据此编造新事件": 防止 LLM 基于未在当前 L1 中出现的上下文虚构事件。
/// - "事件重合时优先合并": 若当前 L1 与历史事件高度相似，应提高 confidence 而非新建重复事件。
pub const CONTEXT_SECTION_HEADER: &str = "\
【补充背景——仅供背景参考，不得据此编造新事件】
以下是与当前对话主题相关的历史记忆。若当前 L1 中已出现的对话片段与以下历史事件高度重合（\
相同人物、相同主题、相近时间），应优先合并为同一事件并提高 confidence，而非创建新事件。\
严禁将以下历史事件直接复制为新事件：";

/// 单条上下文文档的格式化模板。
///
/// 格式: `- [层级] 文档摘要`
pub const CONTEXT_ITEM_TEMPLATE: &str = "- [{layer}] {summary}";

// =========================================================
// Paraphrase Prompt
// =========================================================

/// 态度去情境化重述 Prompt。
///
/// 用途: 将态度的自然语言原文（如"被老板批评后很沮丧"）剥离具体实体，
/// 转为通用模式描述（如"面对权威批评时倾向于沮丧"）。
///
/// 结果: 缓存到 `memory_events.paraphrase` 列，避免每次 System Prompt 构建时重调 LLM。
pub const PARAPHRASE_PROMPT: &str = r#"你是一个心理分析助手。请将以下态度描述进行去情境化重述。

【任务】
将原文中的具体人物、地点、事件细节剥离，转化为概括性的行为模式描述。
保留情感倾向和反应模式的本质，但移除具体实体名称和时间地点。

【要求】
- 用第三人称描述（"此人倾向于..."或"面对...时，会..."）
- 不超过 30 字
- 保留情感色彩和反应模式的核心特征
- 不要引入原文中没有的新信息

【原文态度】
{attitude}

【对话上下文（供参考，不要直接引用具体细节）】
{context}

请直接输出去情境化后的重述文本（纯文本，不要 JSON，不要引号）："""#;

// =========================================================
// Prompt 构建函数
// =========================================================

/// 构建事件提取 Prompt。
///
/// 参数:
/// - `l1_formatted`: 格式化后的 L1 摘要列表，每条格式为 `[序号] YYYY-MM-DD summary (keywords: kw1, kw2)`
///
/// 返回:
/// - 完整 prompt 字符串。
pub fn build_event_extraction_prompt(l1_formatted: &str) -> String {
    EVENT_EXTRACTION_PROMPT.replace("{l1_summaries}", l1_formatted)
}

/// 构建带补充上下文的事件提取 Prompt。
///
/// 用法:
/// - 在基础 Prompt 尾部追加"补充背景"段落（由 CompositeIndex ContextRetriever 产出）。
/// - 标注"仅供背景参考，不得据此编造新事件"的约束。
/// - 若上下文为空则退化为 `build_event_extraction_prompt`。
///
/// 参数:
/// - `l1_formatted`: 格式化后的 L1 摘要列表。
/// - `context_docs`: 补充上下文文档列表（来自 `ContextRetriever::retrieve_context`）。
///
/// 返回:
/// - 完整 prompt 字符串。
pub fn build_event_extraction_prompt_with_context(
    l1_formatted: &str,
    context_docs: &[crate::event::context_retriever::ContextDocument],
) -> String {
    let mut prompt = build_event_extraction_prompt(l1_formatted);

    if context_docs.is_empty() {
        return prompt;
    }

    // 追加补充上下文段落
    prompt.push('\n');
    prompt.push_str(CONTEXT_SECTION_HEADER);
    for doc in context_docs {
        let layer_label = if doc.layer == "l1" { "L1" } else { "L2" };
        let item = CONTEXT_ITEM_TEMPLATE
            .replace("{layer}", layer_label)
            .replace("{summary}", &doc.summary);
        prompt.push('\n');
        prompt.push_str(&item);
    }

    prompt
}

/// 构建 paraphrase Prompt。
///
/// 参数:
/// - `attitude`: 态度的自然语言原文。
/// - `context`: 对话上下文（事件 summary + keywords），供 LLM 理解但不会直接引用。
///
/// 返回:
/// - 完整 paraphrase prompt 字符串。
pub fn build_paraphrase_prompt(attitude: &str, context: &str) -> String {
    PARAPHRASE_PROMPT
        .replace("{attitude}", attitude)
        .replace("{context}", context)
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_prompt_contains_required_fields() {
        let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
        assert!(prompt.contains("title"));
        assert!(prompt.contains("summary"));
        assert!(prompt.contains("confidence"));
        assert!(prompt.contains("salience"));
        assert!(prompt.contains("valence"));
        assert!(prompt.contains("presentation"));
        assert!(prompt.contains("share"));
        assert!(prompt.contains("attitude"));
        assert!(prompt.contains("participants"));
        assert!(prompt.contains("keywords"));
    }

    #[test]
    fn event_prompt_contains_motives_field() {
        let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
        assert!(
            prompt.contains("motives"),
            "v1.3: prompt 应包含 motives 字段"
        );
        assert!(prompt.contains("自我保护"));
        assert!(prompt.contains("归属"));
        assert!(prompt.contains("求知"));
    }

    #[test]
    fn event_prompt_contains_relations_format() {
        let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
        assert!(
            prompt.contains("relations"),
            "v1.3: prompt 应包含 relations 数组"
        );
        assert!(prompt.contains("from_index"));
        assert!(prompt.contains("to_index"));
        assert!(prompt.contains("CausedBy"));
        assert!(prompt.contains("PartOf"));
        assert!(prompt.contains("RelatedTo"));
        assert!(prompt.contains("ContinuedBy"));
        assert!(prompt.contains("Contradicts"));
        assert!(prompt.contains("Timeline"));
    }

    #[test]
    fn event_prompt_injects_l1_text() {
        let l1_text = "[1] 2025-06-01 用户去看了电影";
        let prompt = build_event_extraction_prompt(l1_text);
        assert!(prompt.contains(l1_text));
    }

    #[test]
    fn event_prompt_contains_valence_options() {
        let prompt = build_event_extraction_prompt("test");
        for val in &["-1.0", "-0.5", "0.0", "0.5", "1.0"] {
            assert!(prompt.contains(val), "prompt should mention {val}");
        }
    }

    #[test]
    fn event_prompt_contains_presentation_options() {
        let prompt = build_event_extraction_prompt("test");
        assert!(prompt.contains("objective"));
        assert!(prompt.contains("subjective"));
        assert!(prompt.contains("mixed"));
    }

    #[test]
    fn paraphrase_prompt_injects_both_fields() {
        let attitude = "被批评后很沮丧";
        let context = "用户在工作汇报后被领导批评了PPT做得不好";
        let prompt = build_paraphrase_prompt(attitude, context);
        assert!(prompt.contains(attitude));
        assert!(prompt.contains(context));
    }

    #[test]
    fn paraphrase_prompt_has_instructions() {
        let prompt = build_paraphrase_prompt("test", "context");
        assert!(prompt.contains("去情境化"));
        assert!(prompt.contains("第三人称"));
        assert!(prompt.contains("不超过 30 字"));
    }

    // =========================================================
    // build_event_extraction_prompt_with_context 测试（v1.3 M3）
    // =========================================================

    use crate::event::context_retriever::ContextDocument;

    #[test]
    fn prompt_with_context_injects_context_section() {
        let context_docs = vec![ContextDocument {
            summary: "历史事件1: 完成Rust项目".to_string(),
            layer: "l2".to_string(),
            source_channel: "exact".to_string(),
            score: 1.0,
        }];
        let prompt =
            build_event_extraction_prompt_with_context("[1] 2025-01-01 测试摘要", &context_docs);
        assert!(prompt.contains("仅供背景参考"));
        assert!(prompt.contains("不得据此编造新事件"));
        assert!(prompt.contains("[L2] 历史事件1"));
    }

    #[test]
    fn prompt_with_context_empty_docs_no_injection() {
        let prompt = build_event_extraction_prompt_with_context("[1] 2025-01-01 测试摘要", &[]);
        // 空上下文 → 不注入补充段落
        assert!(!prompt.contains("仅供背景参考"));
        assert!(prompt.contains("测试摘要"));
    }

    #[test]
    fn prompt_with_context_multiple_docs() {
        let context_docs = vec![
            ContextDocument {
                summary: "历史L1: Rust编程".to_string(),
                layer: "l1".to_string(),
                source_channel: "exact".to_string(),
                score: 1.0,
            },
            ContextDocument {
                summary: "历史L2: 完成Rust项目".to_string(),
                layer: "l2".to_string(),
                source_channel: "substring".to_string(),
                score: 0.5,
            },
        ];
        let prompt =
            build_event_extraction_prompt_with_context("[1] 2025-01-01 测试摘要", &context_docs);
        assert!(prompt.contains("[L1] 历史L1: Rust编程"));
        assert!(prompt.contains("[L2] 历史L2: 完成Rust项目"));
    }

    #[test]
    fn prompt_with_context_contains_merge_instruction() {
        let context_docs = vec![ContextDocument {
            summary: "历史事件".to_string(),
            layer: "l2".to_string(),
            source_channel: "exact".to_string(),
            score: 1.0,
        }];
        let prompt =
            build_event_extraction_prompt_with_context("[1] 2025-01-01 测试摘要", &context_docs);
        // 验证去重约束已注入
        assert!(prompt.contains("优先合并"));
        assert!(prompt.contains("而非创建新事件"));
        assert!(prompt.contains("严禁将以下历史事件直接复制"));
    }
}
