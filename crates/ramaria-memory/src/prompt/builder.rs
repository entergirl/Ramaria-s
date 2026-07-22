//! rust/crates/ramaria-memory/src/prompt/builder.rs - CRISPE System Prompt 装配器
//!
//! v2.0 重构 (CRISPE 框架):
//! - Capacity: 能力边界（AI 助手核心能力 + 安全边界）
//! - Role: 角色身份（persona 名称/类型/背景）
//! - Memory: 记忆上下文——最高优先级（近期对话脉络 + RAG 相关记忆）
//! - Insight: 行为洞察（性格标签 + 已知事实）
//! - Statement: 声明示例（Few-shot 对话示例）
//! - Personality: 性格约束（说话风格）
//! - Experiment: 回复规范（chat style rules + 记忆引用规则）
//! - 当前语境: 时间/天气/上次活跃
//!
//! 设计特点:
//! - Memory 块从 Block C 的从属位置提升至 Role 之后，明确"最高优先级"
//! - 支持线上记忆注入开关：若关闭且 provider 为线上，Memory 块仅显示近期摘要
//! - 所有块均支持缺省降级（如无 traits 则省略 Insight 中的性格段）
//! - 记忆引用规则精确定义"主动回溯 vs 被动响应"的边界
//!
//! 依赖:
//! - `ramaria_core::types`: Persona, PersonaFact, PersonalityTrait, PersonaExample
//! - `ramaria_memory::rag`: RAG 上下文格式化（由上层传入）

use chrono::Local;
use ramaria_core::types::{
    Persona, PersonaExample, PersonaFact, PersonalityTrait, ProfileField, TraitStatus,
};

// =========================================================
// System Prompt 装配配置
// =========================================================

/// System Prompt 装配配置。
///
/// v2.0 字段:
/// - `max_examples`: Statement 块最大示例对数。默认 5。
/// - `max_traits_per_layer`: Insight 块每层最多展示的性格标签数。默认 3。
/// - `include_traits`: 是否包含性格标签（Insight 块）。默认 true。
/// - `include_facts`: 是否包含事实信息（Insight 块）。默认 true。
/// - `include_examples`: 是否包含 Few-shot 示例（Statement 块）。默认 true。
/// - `include_knowledge_boundary`: 是否包含知识边界（Capacity 块末尾）。默认 true。
/// - `current_time_str`: 当前时间的格式化字符串。空则自动使用 chrono::Local::now()。
#[derive(Debug, Clone)]
pub struct PromptConfig {
    /// Statement 块最大示例对数
    pub max_examples: usize,
    /// Insight 块每层最多展示的性格标签数
    pub max_traits_per_layer: usize,
    /// 是否包含性格标签
    pub include_traits: bool,
    /// 是否包含事实信息
    pub include_facts: bool,
    /// 是否包含 Few-shot 示例
    pub include_examples: bool,
    /// 是否包含知识边界
    pub include_knowledge_boundary: bool,
    /// 当前时间字符串（空则使用 chrono::Local::now()）
    pub current_time_str: String,
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            max_examples: 5,
            max_traits_per_layer: 3,
            include_traits: true,
            include_facts: true,
            include_examples: true,
            include_knowledge_boundary: true,
            current_time_str: String::new(),
        }
    }
}

// =========================================================
// 装配上下文
// =========================================================

/// CRISPE System Prompt 装配所需的全部数据。
///
/// v2.0 新增字段:
/// - `chat_style_rules`: 回复规则文本（Experiment 块）。由 Stage 6 的 `resolve_chat_style_rules` 提供。
///   若为空则使用最小化默认规则。
///
/// 职责:
/// - 将分散的 persona 数据聚合为一次 System Prompt 构建的输入。
/// - 所有字段均可选：缺失时对应段自动降级。
///
/// 跨 session 上下文:
/// - `recent_session_summaries`: 无条件注入的近期 L1 摘要（最近 1-3 条），
///   解决"新 session 发'你好'时 LLM 完全不知道上次聊了什么"的问题。
/// - `last_active_at`: 该 persona 最后活跃时间，供 LLM 判断对话连续性。
#[derive(Debug, Clone, Default)]
pub struct PromptContext {
    /// 人格基本信息
    pub persona: Option<Persona>,

    /// 事实信息（Insight 块）
    pub facts: Vec<PersonaFact>,

    /// 性格标签（Insight 块）
    pub traits: Vec<PersonalityTrait>,

    /// Few-shot 示例（Statement 块）
    pub examples: Vec<PersonaExample>,

    /// RAG 记忆上下文文本（Memory 块 [相关历史记忆]，由检索结果格式化传入）
    pub memory_context: Option<String>,

    /// 近期 session 摘要（Memory 块 [近期对话脉络]，无条件注入）
    ///
    /// 字段约定:
    /// - 按时间降序排列（最近在前）。
    /// - 每条为格式化好的摘要文本（含时间段和氛围）。
    /// - 为空时显示"（这是你与用户的首次对话）"。
    pub recent_session_summaries: Vec<String>,

    /// 该 persona 最近一次活跃时间（当前语境块）
    pub last_active_at: Option<String>,

    /// 知识边界描述（Capacity 块末尾）
    pub knowledge_boundary: Option<String>,

    /// 当前时间字符串（空则使用 chrono::Local::now()）
    pub current_time_str: Option<String>,

    /// 天气信息（当前语境块可选）
    pub weather: Option<String>,

    /// v2.0 新增: 回复规则文本（Experiment 块）
    ///
    /// 由 Stage 6 的 `resolve_chat_style_rules` 提供。
    /// 若为空则使用最小化默认规则。
    pub chat_style_rules: Option<String>,
}

// =========================================================
// CRISPE System Prompt 模板
// =========================================================

/// CRISPE 框架 System Prompt 模板。
///
/// 占位符说明:
/// - `{capacity}` → Capacity 块（能力边界 + 知识边界）
/// - `{role}` → Role 块（角色身份/类型/背景）
/// - `{memory}` → Memory 块（近期对话脉络 + 相关历史记忆）
/// - `{insight}` → Insight 块（性格标签 + 已知事实）
/// - `{statement}` → Statement 块（Few-shot 示例）
/// - `{personality}` → Personality 块（说话风格）
/// - `{experiment}` → Experiment 块（回复规则 + 记忆引用规则）
/// - `{context_block}` → 当前语境块（时间/天气/上次活跃）
const CRISPE_TEMPLATE: &str = "\
{capacity}

{role}

{memory}

{insight}

{statement}

{personality}

{experiment}

{context_block}";

// =========================================================
// 装配函数
// =========================================================

/// 装配完整的 CRISPE System Prompt。
///
/// v2.0: 从 5-Block 格式重构为 CRISPE 七段式。
/// Memory 块前移至 Role 之后，标注"最高优先级"。
///
/// 参数:
/// - `context`: 装配上下文（persona/facts/traits/examples 等）。
/// - `config`: 装配配置（控制哪些块启用及数量上限）。
///
/// 返回:
/// - 完整的 System Prompt 字符串，可直接作为 `ChatRequest.system_prompt` 使用。
///
/// 降级策略:
/// - 无 persona 时使用默认 Ramaria 身份描述。
/// - 无 traits 时省略 Insight 中的性格段。
/// - 无 facts 时省略 Insight 中的事实段。
/// - 无 examples 时省略 Statement 块。
/// - 无 chat_style_rules 时使用最小化默认规则。
pub fn assemble_prompt(context: &PromptContext, config: &PromptConfig) -> String {
    let capacity = build_capacity(config, context);
    let role = build_role(context);
    let memory = build_memory(context);
    let insight = build_insight(context, config);
    let statement = build_statement(context, config);
    let personality = build_personality(context);
    let experiment = build_experiment(context, config);
    let context_block = build_context_block(context);

    CRISPE_TEMPLATE
        .replace("{capacity}", &capacity)
        .replace("{role}", &role)
        .replace("{memory}", &memory)
        .replace("{insight}", &insight)
        .replace("{statement}", &statement)
        .replace("{personality}", &personality)
        .replace("{experiment}", &experiment)
        .replace("{context_block}", &context_block)
}

// =========================================================
// Capacity 块: 能力边界
// =========================================================

/// 组装 Capacity 块：能力边界 + 知识边界。
fn build_capacity(config: &PromptConfig, context: &PromptContext) -> String {
    let mut parts = vec![
        "# Capacity（能力边界）\n\
         你是 Ramaria 记忆系统驱动的 AI 助手。你的核心能力是**记住与用户的对话历史，并在合适的时机自然引用**。\
         你的知识截止于训练数据，不知道的事情不编造，不确定的信息会说明。\
         你不提供医疗/法律/金融建议，不生成有害内容。"
            .to_string(),
    ];

    // 知识边界（可选）
    if config.include_knowledge_boundary {
        if let Some(ref boundary) = context.knowledge_boundary
            && !boundary.trim().is_empty()
        {
            parts.push(format!("\n\n## 知识边界\n{boundary}"));
        } else {
            parts.push(
                "\n\n## 知识边界\n\
                 - 不要编造你不知道的事实或日期。如果不确定，请诚实说明。\n\
                 - 不提供医疗、法律或金融建议。\n\
                 - 不生成有害、违法或不道德的内容。"
                    .to_string(),
            );
        }
    }

    parts.join("")
}

// =========================================================
// Role 块: 角色身份
// =========================================================

/// 组装 Role 块：角色身份 + persona 类型 + 背景描述 + 说话风格。
fn build_role(context: &PromptContext) -> String {
    if let Some(ref persona) = context.persona {
        let mut parts = vec![format!(
            "# Role（角色身份）\n你是「{}」，一位 AI 助手。",
            persona.name
        )];

        // persona kind 描述
        let kind_desc = match persona.kind {
            ramaria_core::types::PersonaKind::Rama => "你是 Ramaria 助手自身。",
            ramaria_core::types::PersonaKind::User => "你正在以用户的视角思考和回复。",
            ramaria_core::types::PersonaKind::Char => "你正在扮演一个虚构角色。",
            ramaria_core::types::PersonaKind::Anim => "你正在扮演一个动画角色。",
            ramaria_core::types::PersonaKind::Oc => "你正在扮演一个原创角色（OC）。",
            ramaria_core::types::PersonaKind::Hist => "你正在扮演一个历史人物。",
            _ => "你正在扮演一个角色。",
        };
        parts.push(kind_desc.to_string());

        // config JSON 中的额外描述
        if let Some(ref cfg_json) = persona.config
            && let Ok(obj) = serde_json::from_str::<serde_json::Value>(cfg_json)
            && let Some(desc) = obj.get("description").and_then(|v| v.as_str())
        {
            parts.push(format!("背景：{desc}"));
        }

        parts.join("\n")
    } else {
        "# Role（角色身份）\n\
         你是 Ramaria，一个具有记忆能力、善解人意的 AI 助手。\n\
         你可以记住与用户的对话历史，并在后续对话中引用这些记忆。"
            .to_string()
    }
}

// =========================================================
// Memory 块: 记忆上下文（最高优先级）
// =========================================================

/// 组装 Memory 块：近期对话脉络 + 相关历史记忆。
///
/// v2.0: 从 Block C 从属位置提升为独立 # Memory 块，标注"最高优先级"。
///
/// 两层结构:
/// 1. [近期对话脉络] — 无条件注入最近 1-3 条 L1 摘要的叙事引导句
/// 2. [相关历史记忆] — RAG 检索结果（条件注入，由 injection_guard 控制）
fn build_memory(context: &PromptContext) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(2);

    parts.push(
        "# Memory（记忆上下文——最高优先级）\n\
               以下是你的记忆系统检索到的相关信息。你对用户的了解完全来源于此。\
               请仔细阅读，在对话中自然地运用这些信息——但只在话题相关或用户主动提及时引用，\
               不强行插入无关记忆。"
            .to_string(),
    );

    // 近期对话脉络
    if context.recent_session_summaries.is_empty() {
        parts.push("\n\n## 近期对话脉络\n（这是你与用户的首次对话）".to_string());
    } else {
        let narrative = build_cross_session_narrative(&context.recent_session_summaries);
        let mut lines = vec!["\n\n## 近期对话脉络".to_string(), narrative];

        // 逐条列出近期摘要（截断到 120 字符）
        for (i, summary) in context.recent_session_summaries.iter().enumerate() {
            let display: String = if summary.chars().count() > 120 {
                summary.chars().take(120).collect::<String>() + "…"
            } else {
                summary.clone()
            };
            lines.push(format!("  {}. {}", i + 1, display));
        }

        parts.push(lines.join("\n"));
    }

    // 相关历史记忆（RAG 结果）
    match &context.memory_context {
        Some(ctx) if !ctx.trim().is_empty() => {
            parts.push(format!(
                "\n\n## 相关历史记忆\n\
                 以下是与当前话题相关的历史记忆，请结合这些信息回复：\n\
                 {ctx}"
            ));
        }
        _ => {
            parts.push("\n\n## 相关历史记忆\n（暂无与当前话题直接相关的历史记忆）".to_string());
        }
    }

    parts.join("")
}

/// 从近期 L1 摘要构建跨 session 叙事引导句。
///
/// 职责:
/// - 将孤立的 L1 摘要串联为连贯的叙事脉络，告知 LLM"此前对话的总体进展"。
/// - 使 LLM 能自然地引用此前对话，而非每次从零开始。
///
/// 算法:
/// - 取最近 3 条摘要，提取前 30 字符作为话题锚点。
/// - 按时间顺序串联为"你此前与用户讨论了 A、B、C 等话题"格式。
/// - 添加时间提示（"最近一次对话发生在 XX"）。
///
/// 参数:
/// - `summaries`: 按时间降序排列的 L1 摘要文本列表。
///
/// 返回:
/// - 叙事引导句字符串。
pub fn build_cross_session_narrative(summaries: &[String]) -> String {
    if summaries.is_empty() {
        return String::new();
    }

    // 取最近 3 条
    let recent: Vec<&String> = summaries.iter().take(3).collect();

    // 提取每条摘要的前 30 字符作为话题锚点
    let topics: Vec<String> = recent
        .iter()
        .map(|s| {
            let anchor: String = s.chars().take(30).collect();
            anchor.trim().to_string()
        })
        .collect();

    // 反转为主题时间线（最早→最近）
    let mut timeline = topics.clone();
    timeline.reverse();

    let count = timeline.len();
    let topic_list = timeline.join("、");

    // 生成引导句
    let narrative = if count == 1 {
        format!("你此前与用户讨论过「{topic_list}」。")
    } else {
        format!("你此前与用户进行了 {count} 次对话：讨论了「{topic_list}」。")
    };

    // 追加时间提示
    let time_hint = if count >= 2 {
        " 最近一次对话发生在不久前，用户可能希望继续之前的话题。"
    } else {
        " 用户可能希望继续之前的话题。"
    };

    narrative + time_hint
}

// =========================================================
// Insight 块: 行为洞察
// =========================================================

/// 组装 Insight 块：性格标签 + 已知事实。
fn build_insight(context: &PromptContext, config: &PromptConfig) -> String {
    let mut parts = vec![
        "# Insight（行为洞察）\n\
         以下是从历史对话中推断出的用户性格特征。记忆上下文提供了「用户做了什么」，\
         而这里提供了「用户是什么样的人」。请结合两者来调整回复的深度和角度："
            .to_string(),
    ];

    // 性格标签（按 layer 分组）
    if config.include_traits && !context.traits.is_empty() {
        let trait_text = format_traits_for_prompt(&context.traits, config.max_traits_per_layer);
        if !trait_text.is_empty() {
            parts.push(format!("\n\n## 性格特征\n{trait_text}"));
        }
    }

    // 事实信息
    if config.include_facts && !context.facts.is_empty() {
        let fact_text = format_facts_for_prompt(&context.facts);
        if !fact_text.is_empty() {
            parts.push(format!("\n\n## 已知事实\n{fact_text}"));
        }
    }

    parts.join("")
}

/// 将性格标签格式化为 prompt 文本，按 layer 分组。
fn format_traits_for_prompt(traits: &[PersonalityTrait], max_per_layer: usize) -> String {
    use ramaria_core::types::TraitLayer;
    use std::collections::BTreeMap;

    // 按 layer 分组，只取 active 的
    let mut by_layer: BTreeMap<&str, Vec<&PersonalityTrait>> = BTreeMap::new();
    for t in traits {
        if t.status != TraitStatus::Active {
            continue;
        }
        let layer_name = match t.layer {
            TraitLayer::Base => "基础性格",
            TraitLayer::Primary => "主要特征",
            TraitLayer::Accent => "次要特征",
            _ => "其他特征",
        };
        by_layer.entry(layer_name).or_default().push(t);
    }

    if by_layer.is_empty() {
        return String::new();
    }

    let mut lines = Vec::new();
    for (layer_name, layer_traits) in &by_layer {
        lines.push(format!("【{layer_name}】"));
        for t in layer_traits.iter().take(max_per_layer) {
            let mut desc = format!("  - {}", t.trait_label);
            if !t.meaning.is_empty() {
                desc.push_str(&format!("（{}）", t.meaning));
            }
            lines.push(desc);
        }
    }

    lines.join("\n")
}

/// 将事实信息格式化为 prompt 文本，按 ProfileField 分组。
fn format_facts_for_prompt(facts: &[PersonaFact]) -> String {
    if facts.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    for fact in facts {
        let field_label = match fact.field {
            ProfileField::BasicInfo => "基础信息",
            ProfileField::PersonalStatus => "近期状态",
            ProfileField::Interests => "兴趣爱好",
            ProfileField::Social => "社交情况",
            ProfileField::History => "历史事件",
            ProfileField::RecentContext => "近期背景",
            ProfileField::SpeakingStyle => "说话风格",
            _ => "其他",
        };
        lines.push(format!("  [{field_label}] {}", fact.content));
    }

    lines.join("\n")
}

// =========================================================
// Statement 块: Few-shot 示例
// =========================================================

/// 组装 Statement 块：Few-shot 对话示例。
fn build_statement(context: &PromptContext, config: &PromptConfig) -> String {
    if !config.include_examples || context.examples.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    lines.push(
        "# Statement（声明示例）\n\
         以下是你在此人格下的对话示例，请参考其风格和节奏："
            .to_string(),
    );

    for (i, ex) in context
        .examples
        .iter()
        .take(config.max_examples)
        .enumerate()
    {
        lines.push(format!("\n示例 {}：", i + 1));

        // 前文语境
        if let Some(ref ctx) = ex.context
            && !ctx.trim().is_empty()
        {
            let ctx_lines: Vec<&str> = ctx.lines().take(3).collect();
            for cl in ctx_lines {
                lines.push(format!("  前文：{cl}"));
            }
        }

        lines.push(format!("  对方：{}", ex.partner));
        lines.push(format!("  你：{}", ex.reply));
    }

    lines.join("\n")
}

// =========================================================
// Personality 块: 性格约束
// =========================================================

/// 组装 Personality 块：说话风格。
///
/// v2.0: 从 Block A 中独立出来，作为 CRISPE 框架的独立段。
/// 从 persona.config JSON 的 `speaking_style` 字段提取。
fn build_personality(context: &PromptContext) -> String {
    if let Some(ref persona) = context.persona
        && let Some(ref cfg_json) = persona.config
        && let Ok(obj) = serde_json::from_str::<serde_json::Value>(cfg_json)
        && let Some(style) = obj.get("speaking_style").and_then(|v| v.as_str())
        && !style.trim().is_empty()
    {
        format!("# Personality（性格约束）\n{style}")
    } else {
        String::new()
    }
}

// =========================================================
// Experiment 块: 回复规范
// =========================================================

/// 组装 Experiment 块：回复规则 + 记忆引用规则。
///
/// v2.0: 合并原 SHARED_CHAT_STYLE_RULES（回复规则）和新增的记忆引用规则。
/// 记忆引用规则精确定义"主动回溯 vs 被动响应"的边界。
fn build_experiment(context: &PromptContext, config: &PromptConfig) -> String {
    let mut parts: Vec<String> = Vec::new();

    parts.push("# Experiment（回复规范）".to_string());

    // 回复规则
    if let Some(ref rules) = context.chat_style_rules
        && !rules.trim().is_empty()
    {
        parts.push(format!("\n## 核心规则\n{rules}"));
    } else {
        // 最小化默认规则
        parts.push(
            "\n## 核心规则\n\
             - 用自然、友好的语气回复。\n\
             - 回复简洁明了，不冗长。\n\
             - 不确定的事情请诚实说明。"
                .to_string(),
        );
    }

    // 记忆引用规则（v2.0 新增）
    if config.include_knowledge_boundary {
        parts.push(
            "\n## 记忆引用规则（极其重要）\n\
             1. **时机判断**：仅当用户当前消息与记忆上下文明确相关时，才引用历史记忆。\
             如果用户只是打招呼或开启全新话题，不要强行插入「上次我们聊到……」.\n\
             2. **自然衔接**：引用记忆时用「记得你之前……」或类似自然表达，\
             而不是「根据系统记录……」等机械措辞。\n\
             3. **主动回溯 vs 被动响应**：如果用户说「你还记得我之前说的吗？」，\
             这意味着用户主动邀请你回溯——此时可以自由引用记忆。如果用户没有提及，\
             只在话题自然相关时引用。\n\
             4. **跨 Session 连续性**：如果近期对话脉络显示你们之前聊过某个话题\
             且对话间隔很短（几小时内），用户可能希望继续之前的话题——在回复中自然衔接。\
             如果间隔较长（几天以上），先寒暄再观察用户是否主动延续。"
                .to_string(),
        );
    }

    parts.join("")
}

// =========================================================
// 当前语境块
// =========================================================

/// 组装当前语境块：时间 + 可选天气 + 可选上次活跃时间。
///
/// 时间格式：
/// - 若 `context.current_time_str` 有值，直接使用。
/// - 否则使用 `chrono::Local::now` 生成可读日期时间（`%Y-%m-%d %H:%M`）。
fn build_context_block(context: &PromptContext) -> String {
    let time_str = context
        .current_time_str
        .clone()
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d %H:%M").to_string());

    let mut lines = vec![format!(
        "# 当前语境\n\
         当前时间：{time_str}"
    )];

    if let Some(ref weather) = context.weather
        && !weather.trim().is_empty()
    {
        lines.push(format!("天气：{weather}"));
    }

    if let Some(ref last_active) = context.last_active_at
        && !last_active.is_empty()
    {
        lines.push(format!("上次对话时间：{last_active}"));
    }

    lines.join("\n")
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{PersonaKind, TraitLayer, TraitSource, TraitStatus};

    // ---- 辅助构造器 ----

    fn make_test_persona() -> Persona {
        Persona {
            id: 1,
            uid: "char-0001".into(),
            name: "小明".into(),
            kind: PersonaKind::Char,
            seq: 1,
            source: "manual".into(),
            ref_id: None,
            avatar: None,
            active: true,
            config: Some(
                r#"{"description":"一个喜欢编程的大学生","speaking_style":"热情活泼，喜欢用emoji"}"#
                    .into(),
            ),
            description: None,
            created_at: 1000,
            updated_at: 1000,
        }
    }

    fn make_test_trait(label: &str, layer: TraitLayer, meaning: &str) -> PersonalityTrait {
        PersonalityTrait {
            id: 1,
            persona_uid: "char-0001".into(),
            layer,
            trait_label: label.into(),
            meaning: meaning.into(),
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq: 1,
            source: TraitSource::Manual,
            ref_event_id: None,
            ref_l1_id: None,
            confidence: 0.9,
            evidence: 0.8,
            consistency: 0.7,
            status: TraitStatus::Active,
            created_at: 1000,
            updated_at: 1000,
        }
    }

    fn make_test_example(partner: &str, reply: &str) -> PersonaExample {
        PersonaExample {
            id: 1,
            persona_uid: "char-0001".into(),
            partner: partner.into(),
            reply: reply.into(),
            session_id: None,
            context: None,
            valence: 0.5,
            tags: None,
            selected: true,
            length: reply.chars().count() as i32,
            created_at: 1000,
        }
    }

    // ---- Role 块测试 ----

    #[test]
    fn role_with_persona() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("小明"));
        assert!(result.contains("虚构角色"));
        assert!(result.contains("编程"));
        assert!(result.contains("emoji"));
        // CRISPE 框架 markers
        assert!(result.contains("# Role"));
        assert!(result.contains("# Memory"));
        assert!(result.contains("# Insight"));
        assert!(result.contains("# 当前语境"));
    }

    #[test]
    fn role_without_persona_uses_default() {
        let ctx = PromptContext::default();
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);
        assert!(result.contains("Ramaria"));
        assert!(result.contains("AI 助手"));
    }

    // ---- Insight 块测试 ----

    #[test]
    fn insight_with_traits() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            traits: vec![
                make_test_trait("乐观", TraitLayer::Base, "总是看到积极的一面"),
                make_test_trait("好奇心强", TraitLayer::Primary, "对新鲜事物充满兴趣"),
            ],
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("基础性格"));
        assert!(result.contains("乐观"));
        assert!(result.contains("好奇心强"));
    }

    #[test]
    fn insight_traits_disabled() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            traits: vec![make_test_trait("乐观", TraitLayer::Base, "积极")],
            ..Default::default()
        };
        let config = PromptConfig {
            include_traits: false,
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &config);
        assert!(!result.contains("乐观"));
    }

    #[test]
    fn insight_with_facts() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            facts: vec![PersonaFact {
                id: 1,
                persona_uid: "char-0001".into(),
                field: ProfileField::Interests,
                content: "喜欢编程、阅读科幻小说".into(),
                source: ramaria_core::types::FactSource::Manual,
                ref_event_id: None,
                ref_l1_id: None,
                created_at: 1000,
                updated_at: 1000,
            }],
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("已知事实"));
        assert!(result.contains("科幻小说"));
    }

    // ---- Statement 块测试 ----

    #[test]
    fn statement_with_examples() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            examples: vec![
                make_test_example("你好呀", "嗨！今天想聊点什么呢？😊"),
                make_test_example("你会编程吗？", "当然啦！Python 和 Rust 我都会～"),
            ],
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("# Statement"));
        assert!(result.contains("你好呀"));
        assert!(result.contains("😊"));
        assert!(result.contains("Rust"));
    }

    #[test]
    fn statement_disabled() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            examples: vec![make_test_example("你好", "嗨")],
            ..Default::default()
        };
        let config = PromptConfig {
            include_examples: false,
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &config);
        assert!(!result.contains("# Statement"));
    }

    #[test]
    fn statement_max_examples_limit() {
        let examples: Vec<PersonaExample> = (0..10)
            .map(|i| make_test_example(&format!("test{i}"), &format!("reply{i}")))
            .collect();
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            examples,
            ..Default::default()
        };
        let config = PromptConfig {
            max_examples: 3,
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("示例 1"));
        assert!(result.contains("示例 2"));
        assert!(result.contains("示例 3"));
        assert!(!result.contains("示例 4"));
    }

    // ---- Memory 块测试 ----

    #[test]
    fn memory_with_rag_only() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            memory_context: Some("用户之前提到喜欢猫。".into()),
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        // 无近期摘要 → 首次对话提示
        assert!(result.contains("首次对话"));
        // RAG 结果
        assert!(result.contains("喜欢猫"));
        assert!(result.contains("Memory（记忆上下文"));
    }

    #[test]
    fn memory_without_rag_shows_placeholder() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            memory_context: None,
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("首次对话"));
        assert!(result.contains("暂无与当前话题直接相关的历史记忆"));
    }

    #[test]
    fn memory_with_recent_summaries_and_rag() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            recent_session_summaries: vec![
                "讨论了Python异步编程和FastAPI的使用".to_string(),
                "完成了Rust项目的第一个crate发布".to_string(),
                "探讨了AI助手的记忆系统设计".to_string(),
            ],
            memory_context: Some("用户：喜欢猫，养了一只橘猫".into()),
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("近期对话脉络"));
        assert!(result.contains("Python异步编程"));
        assert!(result.contains("Rust项目"));
        assert!(result.contains("相关历史记忆"));
        assert!(result.contains("橘猫"));
        // 跨 session 叙事引导句
        assert!(result.contains("此前与用户进行了"));
    }

    #[test]
    fn memory_single_recent_summary() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            recent_session_summaries: vec!["讨论了天气和出行计划，决定周末去爬山".to_string()],
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("近期对话脉络"));
        assert!(result.contains("你此前与用户讨论过"));
        assert!(result.contains("爬山"));
    }

    // ---- 跨 session 叙事引导句测试 ----

    #[test]
    fn cross_session_narrative_single() {
        let summaries = vec!["用户今天学习了Rust编程语言".to_string()];
        let narrative = build_cross_session_narrative(&summaries);
        assert!(narrative.contains("你此前与用户讨论过"));
        assert!(narrative.contains("Rust编程"));
        assert!(!narrative.contains("次对话"));
    }

    #[test]
    fn cross_session_narrative_multiple() {
        let summaries = vec![
            "探讨了AI助手的记忆系统".to_string(),
            "完成了Rust项目发布".to_string(),
            "讨论了Python异步编程".to_string(),
        ];
        let narrative = build_cross_session_narrative(&summaries);
        assert!(narrative.contains("你此前与用户进行了 3 次对话"));
        assert!(narrative.contains("不久前"));
    }

    #[test]
    fn cross_session_narrative_empty() {
        let summaries: Vec<String> = vec![];
        let narrative = build_cross_session_narrative(&summaries);
        assert!(narrative.is_empty());
    }

    // ---- Capacity 块测试 ----

    #[test]
    fn capacity_custom_boundary() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            knowledge_boundary: Some("你是一个精通 Rust 的专家，但不了解 Python。".into()),
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("精通 Rust"));
    }

    #[test]
    fn capacity_disabled() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            ..Default::default()
        };
        let config = PromptConfig {
            include_knowledge_boundary: false,
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &config);

        assert!(!result.contains("知识边界"));
    }

    // ---- 当前语境块测试 ----

    #[test]
    fn context_with_weather() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            current_time_str: Some("2026-06-10".into()),
            weather: Some("晴，25°C".into()),
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("2026-06-10"));
        assert!(result.contains("当前时间"));
        assert!(result.contains("晴"));
    }

    #[test]
    fn context_defaults_to_readable_time() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("当前时间"));
        let after_time = result.split("当前时间：").nth(1).unwrap_or("");
        let time_part = after_time.lines().next().unwrap_or("");
        assert!(
            time_part.chars().filter(|c| c.is_ascii_digit()).count() <= 16,
            "时间部分不应是长整数时间戳: {time_part}"
        );
        assert!(time_part.contains('-'), "应包含日期连字符: {time_part}");
        assert!(time_part.contains(':'), "应包含时间冒号: {time_part}");
    }

    #[test]
    fn context_with_last_active() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            current_time_str: Some("2026-06-16".into()),
            last_active_at: Some("2026-06-13 14:30".into()),
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("上次对话时间"));
        assert!(result.contains("2026-06-13"));
    }

    // ---- 完整装配 ----

    #[test]
    fn full_prompt_all_blocks() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            facts: vec![PersonaFact {
                id: 1,
                persona_uid: "char-0001".into(),
                field: ProfileField::Interests,
                content: "编程".into(),
                source: ramaria_core::types::FactSource::Manual,
                ref_event_id: None,
                ref_l1_id: None,
                created_at: 1000,
                updated_at: 1000,
            }],
            traits: vec![make_test_trait("乐观", TraitLayer::Base, "积极")],
            examples: vec![make_test_example("你好", "嗨！")],
            recent_session_summaries: vec!["之前讨论了Rust编程".to_string()],
            memory_context: Some("用户：喜欢猫".into()),
            knowledge_boundary: Some("知识边界测试".into()),
            current_time_str: Some("2026-06-10".into()),
            last_active_at: Some("2026-06-08".into()),
            weather: Some("晴".into()),
            chat_style_rules: Some("测试回复规则".into()),
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        // CRISPE 框架所有块
        assert!(result.contains("小明"), "Role 块缺失");
        assert!(result.contains("# Statement"), "Statement 块缺失");
        assert!(result.contains("近期对话脉络"), "Memory 近期对话脉络缺失");
        assert!(result.contains("相关历史记忆"), "Memory 相关历史记忆缺失");
        assert!(result.contains("知识边界"), "Capacity 知识边界缺失");
        assert!(result.contains("当前时间"), "语境块缺失");
        assert!(result.contains("测试回复规则"), "Experiment 块缺失");
        // 跨 session 上下文
        assert!(result.contains("Rust编程"), "近期摘要未注入");
        assert!(result.contains("上次对话时间"), "最后活跃时间未注入");
    }

    #[test]
    fn empty_context_produces_valid_prompt() {
        let ctx = PromptContext::default();
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(!result.is_empty());
        assert!(result.contains("Ramaria"));
        assert!(result.contains("首次对话"));
    }
}
