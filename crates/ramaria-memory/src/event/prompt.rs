//! rust/crates/ramaria-memory/src/event/prompt.rs - 事件提取 Prompt 模板
//!
//! 设计特点:
//! - 事件提取 Prompt: 从多条 L1 摘要中提取结构化事件（11 个推断属性）
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
pub const EVENT_EXTRACTION_PROMPT: &str = r#"你是一个事件提取助手。请根据下面多条对话摘要（L1），提取其中的离散事件。

【输出格式要求】
严格按照以下 JSON 格式输出，输出一个 JSON 数组，每个元素是一条事件：

[
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
    "attitude": "该人物对此事的态度（自然语言描述），如无则填空字符串"
  }
]

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

【提取规则】
1. 每条事件必须基于下方 L1 摘要中的具体内容，不可凭空编造
2. 同一主题可能分散在多条 L1 中，应合并为一个事件（同时提高 confidence）
3. 避免将无关 L1 强行合并为一个"杂项"事件——降级时由系统自动处理
4. 如果某条 L1 无法归入任何事件，可以忽略（系统已有降级兜底）
5. 最多提取 5 条事件。如果 L1 非常琐碎无法提炼事件，返回空数组 []

【待分析的 L1 摘要列表】
{l1_summaries}

请输出 JSON 数组："""#;

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
}
