//! rust/crates/ramaria-memory/src/event/prompt.rs - 事件提取 Prompt 模板
//!
//! 设计特点:
//! - 事件提取 Prompt: 从多条 L1 摘要中提取结构化事件（11 个推断属性）
//! - 支持注入补充上下文段落（CompositeIndex 检索结果），标注防编造约束
//! - Paraphrase Prompt: 将态度的自然语言去情境化重述
//! - 严格 JSON 数组输出格式约束，引导 LLM 生成合规事件
//! - 字段约束（五档 confidence/salience/valence/share，三选一 presentation）嵌入 Prompt

// =========================================================
// 事件提取 Prompt
// =========================================================

/// 事件提取 Prompt 模板。
///
/// v2.0 重构 (CRAFT 框架):
/// - Context: 明确任务背景——从 L1 摘要提取结构化事件。
/// - Role: 历史学家视角（证据驱动）+ 心理学视角（动机推断）。
/// - Action: 两个子任务——事件提取（11 字段）+ 关系提取（6 种类型）。
/// - Format: 严格 JSON 输出（裸 JSON，不加 markdown 代码块）。
/// - Target: 质量目标（独立可理解、宁缺毋滥、动机不凭空猜测）。
///
/// 用途: 给定同一个人的多条未吸收 L1 摘要，提取结构化离散事件。
///
/// 输出: JSON 对象，含 "events" 数组和 "relations" 数组。
///
/// 关键约束:
/// - confidence < 0.6 的事件不参与后续性格推断
/// - share 不设推断阈值，仅在 RAG 暴露环节过滤（share >= 0.3）
/// - 每个事件必须独立，不跨 L1 合并分歧信息
pub const EVENT_EXTRACTION_PROMPT: &str = r#"# Context（背景）
你是一个事件提取助手。从多条对话摘要（L1）中提取离散的结构化事件，并推断事件之间的因果关系。

# Role（角色定位）
你以严谨的历史学家的视角审视对话摘要：每条事件必须有摘要中的具体内容支撑，不可凭空编造。你同时具备心理学视角，能从对话中推断行为背后的底层动机。

# Action（执行任务）

## 子任务 1：事件提取
从下方 L1 摘要列表中提取离散事件，每条事件包含以下字段：

| 字段 | 类型 | 约束 |
|------|------|------|
| `title` | string | ≤20 字中文标题，概括事件核心 |
| `summary` | string | 2–3 句话，从分析对象的视角描述，用"用户"指代此人 |
| `keywords` | string | 5–8 个中文名词，逗号分隔（含分类标签+地点，禁止英文关键词） |
| `participants` | [string] | 除"用户"外的参与者名称或角色，无则空数组 `[]` |
| `confidence` | float | 0.0–1.0，事实确凿度。确凿=1.0（"用户明确说了..."），推测≈0.5（"可能发生了..."）。<0.6 不参与后续性格推断但事件仍可输出 |
| `salience` | float | 五档离散：0.0 / 0.25 / 0.5 / 0.75 / 1.0（定义同 L1） |
| `valence` | float | 五档离散：-1.0 / -0.5 / 0.0 / 0.5 / 1.0（定义同 L1） |
| `presentation` | string | 三选一："objective"（客观事实）/ "subjective"（主观感受）/ "mixed"（混合） |
| `share` | float | 0.0–1.0，该事件内容是否适合告诉他人。鼓励使用连续值（如 0.45, 0.55, 0.85），避免仅使用 0.3 和 0.7 两个锚点 |
| `attitude` | string | 对此事的态度（自然语言一句话）。即使是碎片化闲聊，也从语气和措辞中推断基本态度倾向（如"平和陈述""略带抱怨""敷衍回应""轻松调侃"）。确无任何态度线索时填"中性交流"，不填空字符串 |
| `motives` | [string] | 底层动机，从以下七类中选择最相关的 1–3 个：自我保护 / 归属 / 地位 / 自主 / 公平 / 养育 / 求知。无法判断时空数组 `[]` |

## 子任务 2：事件关系提取
提取事件间的因果关系，每条关系包含：
- `from_index` / `to_index`：引用 events 数组中的索引（从 0 开始），表示 from → to 的方向
- `kind`：六选一——
  - `"CausedBy"` — from 是 to 的因果前因
  - `"PartOf"` — from 是 to 的子事件
  - `"RelatedTo"` — 一般主题关联
  - `"ContinuedBy"` — from 被 to 延续/发展
  - `"Contradicts"` — from 与 to 矛盾
  - `"Timeline"` — 纯时序先后（无因果关系时使用）
- `weight`：0.0–1.0，关系确信度
- `detail`：1 句话简述关系逻辑
- 无明显关系时空数组 `[]`

## 关键规则

### 角色区分（极其重要——违反此规则将导致性格推断错误）
- 对话可能涉及两方。L1 摘要中嵌入的对话内容如果包含发送者标识（如"[昵称] 消息内容"），请仔细区分谁是"用户"（分析对象）、谁是另一方。
- 互动归属：用户主动 → "用户主动..."；对方发起 → "对方...，用户回应..."
- 参与者列表中包含另一方（如适用）。
- **严禁**出现"用户向用户发送了..."或"用户向自己..."等逻辑错误。

### 合并与去重
- 同一主题分散在多条 L1 中 → 合并为一个事件，同时提高 confidence
- 不要将无关 L1 强行合并为"杂项"事件
- 如果当前 L1 与补充背景中的历史事件高度重合（相同人物、相同主题、相近时间）→ 合并并提高 confidence，而非新建事件

### 数量控制
- 最多提取 5 条事件
- L1 过于琐碎无法提炼事件 → 返回 `{"events":[],"relations":[]}`

# Format（输出格式）
你的整个回复必须是一个裸 JSON 对象，以 { 开头、以 } 结尾，不要添加任何其他内容：

{"events":[{"title":"...","summary":"...","keywords":"...","participants":[],"confidence":0.8,"salience":0.7,"valence":-0.5,"presentation":"subjective","share":0.3,"attitude":"...","motives":["..."]}],"relations":[{"from_index":0,"to_index":1,"kind":"CausedBy","weight":0.8,"detail":"..."}]}

# Target（质量目标）
- 每条事件的 summary 独立可理解（脱离 L1 原文仍有信息量）
- 关系提取宁缺毋滥——无明显关系时空数组比强行编造好
- 动机推断基于对话中的隐含线索（语气、措辞、上下文），不凭空猜测
- attitude 不要填空字符串，至少填"中性交流"

---

# 待分析的 L1 摘要
{l1_summaries}"#;

// =========================================================
// 补充上下文段落模板
// =========================================================

/// 补充上下文段落的标题/说明。
///
/// v2.0 重构:
/// - 从中文自然语言改为与 CRAFT 框架一致的 Markdown 标题格式。
/// - 强化三个约束指令的措辞（"仅供背景参考"→明确标注为补充信息，
///   "不得据此编造"→更直接的口吻，"优先合并"→具体操作约束）。
///
/// 注入到事件提取 Prompt 中，标注以下约束:
/// - "仅供背景参考": 明确告知 LLM 不得将历史事件直接复制为当前事件。
/// - "不得据此编造新事件": 防止 LLM 基于未在当前 L1 中出现的上下文虚构事件。
/// - "事件重合时优先合并": 若当前 L1 与历史事件高度相似，应提高 confidence 而非新建重复事件。
pub const CONTEXT_SECTION_HEADER: &str = "\
### 补充背景（仅供参考——不得据此编造新事件）
以下是与当前对话主题相关的历史记忆。这些是**补充信息**，只能用于：
1. 若当前 L1 中的内容与以下历史事件高度重合（相同人物、相同主题、相近时间），
   应**合并**为同一事件并提高 confidence，而非新建事件。
2. **严禁**将以下历史事件直接复制为新事件——每条事件必须基于当前 L1 摘要。
3. 以下信息中的人物和事件如未在当前 L1 中出现，**不得**据此编造新事件：";

/// 单条上下文文档的格式化模板。
///
/// 格式: `- [层级] 文档摘要`
pub const CONTEXT_ITEM_TEMPLATE: &str = "- [{layer}] {summary}";

// =========================================================
// Paraphrase Prompt
// =========================================================

/// 态度去情境化重述 Prompt。
///
/// v2.0 重构 (CRAFT 框架):
/// - Context: 明确任务背景——态度→通用行为模式。
/// - Role: 个案抽象为模式的心理学研究者视角。
/// - Action: 去情境化重述（剥离实体、保留情感核心）。
/// - Format: 纯文本 ≤30 字，第三人称。
/// - Target: 质量目标+示例。
///
/// 用途: 将态度的自然语言原文（如"被老板批评后很沮丧"）剥离具体实体，
/// 转为通用模式描述（如"面对权威批评时倾向于沮丧"）。
///
/// 结果: 缓存到 `memory_events.paraphrase` 列，避免每次 System Prompt 构建时重调 LLM。
pub const PARAPHRASE_PROMPT: &str = r#"# Context（背景）
你是一个心理分析助手。将具体的态度描述转化为去情境化的通用行为模式表述——剥离具体实体（人名、地名、时间），保留情感核心和反应模式的本质。

# Role（角色定位）
你像一个将个案抽象为模式的心理学研究者：只关心"这个人在什么类型的情境下会产生什么类型的情感反应"，不关心具体是谁、在哪、什么时候。

# Action（执行任务）
将下方的态度原文进行去情境化重述。

# Format（输出格式）
- 第三人称（"此人倾向于..."或"面对...时，会..."）
- 不超过 30 字
- **纯文本，不要 JSON，不要引号，不要 markdown**
- 保留情感色彩和反应模式的核心特征
- 不引入原文中没有的新信息

# Target（质量目标）
- 原文"被老板批评后很沮丧" → "面对权威批评时倾向于沮丧"
- 原文"和朋友出去玩很开心" → "在社交活动中容易获得愉悦感"
- 原文"对项目成功感到自豪" → "在成就被认可时产生强烈自豪感"
- 如果原文已经是通用描述，可以微调但不要过度改写以致丢失原意

---

# 原文态度
{attitude}

# 对话上下文（仅供理解背景，不要直接引用具体细节）
{context}"#;

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

/// 构建事件提取 Prompt，将"用户"替换为 persona 显示名称。
///
/// 用法:
/// - 导入场景中，L2 事件提取 Prompt 的"用'用户'指代"应替换为实际 persona 名称。
/// - 正常对话场景仍可用 `build_event_extraction_prompt`（保持"用户"）。
///
/// 参数:
/// - `l1_formatted`: 同 `build_event_extraction_prompt`。
/// - `persona_name`: persona 显示名称，用于替换 Prompt 中的"用户"。
/// - `other_persona_name`: 对话另一方的名称。`None` 表示未知（单方对话或不确定）。
///
/// 返回:
/// - 完整 prompt 字符串，所有"用户"已替换为 `persona_name`。
pub fn build_event_extraction_prompt_for_persona(
    l1_formatted: &str,
    persona_name: &str,
    other_persona_name: Option<&str>,
) -> String {
    // 先构建基础 prompt (含 L1 文本)，再替换"用户"→实际 persona 名称。
    // 注意：L1 摘要文本（来自导入场景，content 已有 [sender_name] 前缀 + 空前缀格式化）
    // 不应再包含"用户"字样，因此全量替换是安全的。
    let mut prompt = build_event_extraction_prompt(l1_formatted);
    prompt = prompt.replace("用户", persona_name);

    // 注入对话另一方角色提示，帮助 LLM 区分行为归属
    if let Some(other) = other_persona_name {
        let role_hint = format!(
            "\n\n【当前分析对象的对话方：{other}】\
             \n以上 L1 摘要中嵌入的对话内容包含两方：{persona_name}（分析对象）和 {other}（对话另一方）。\
             \n提取事件时，请仔细区分每句话的发送者，确保事件的行为归属正确。\
             \n例如：\"{persona_name} 的消息\"应归属于分析对象，\"{other} 的消息\"应归属于对方。"
        );
        prompt.push_str(&role_hint);
    }

    prompt
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

/// 构建带补充上下文的事件提取 Prompt，
/// 将"用户"替换为 persona 显示名称。
///
/// 参数:
/// - 同 `build_event_extraction_prompt_with_context`。
/// - `persona_name`: persona 显示名称。
/// - `other_persona_name`: 对话另一方的名称。
///
/// 返回:
/// - 完整 prompt 字符串，所有"用户"已替换为 `persona_name`。
pub fn build_event_extraction_prompt_with_context_for_persona(
    l1_formatted: &str,
    context_docs: &[crate::event::context_retriever::ContextDocument],
    persona_name: &str,
    other_persona_name: Option<&str>,
) -> String {
    let mut prompt = build_event_extraction_prompt_with_context(l1_formatted, context_docs);
    prompt = prompt.replace("用户", persona_name);

    if let Some(other) = other_persona_name {
        let role_hint = format!(
            "\n\n【当前分析对象的对话方：{other}】\
             \n以上 L1 摘要中嵌入的对话内容包含两方：{persona_name}（分析对象）和 {other}（对话另一方）。\
             \n提取事件时，请仔细区分每句话的发送者，确保事件的行为归属正确。"
        );
        prompt.push_str(&role_hint);
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
        assert!(prompt.contains("motives"), "prompt 应包含 motives 字段");
        assert!(prompt.contains("自我保护"));
        assert!(prompt.contains("归属"));
        assert!(prompt.contains("求知"));
    }

    #[test]
    fn event_prompt_contains_relations_format() {
        let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
        assert!(prompt.contains("relations"), "prompt 应包含 relations 数组");
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
    fn event_prompt_contains_role_distinction_rules() {
        let prompt = build_event_extraction_prompt("[1] 2025-01-01 测试摘要");
        assert!(
            prompt.contains("角色区分"),
            "v2.0: prompt 应包含角色区分段落"
        );
        assert!(prompt.contains("另一方"));
        assert!(prompt.contains("用户主动"));
        assert!(prompt.contains("逻辑错误"));
    }

    #[test]
    fn persona_prompt_injects_other_party_hint() {
        let prompt =
            build_event_extraction_prompt_for_persona("[1] 2025-01-01 测试", "张三", Some("李四"));
        assert!(prompt.contains("张三"), "应包含 persona 名称");
        assert!(prompt.contains("李四"), "应包含对话另一方名称");
        assert!(prompt.contains("对话方：李四"));
    }

    #[test]
    fn persona_prompt_no_other_party_no_hint() {
        let prompt = build_event_extraction_prompt_for_persona("[1] 2025-01-01 测试", "张三", None);
        assert!(prompt.contains("张三"));
        assert!(!prompt.contains("对话方"), "无另一方时不应注入角色提示");
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
    // build_event_extraction_prompt_with_context 测试
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
        assert!(prompt.contains("仅供参考"));
        assert!(prompt.contains("不得据此编造"));
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
        assert!(prompt.contains("合并"));
        assert!(prompt.contains("而非新建事件"));
        assert!(prompt.contains("直接复制为新事件"));
    }
}
