//! rust/crates/ramaria-memory/src/prompt/builder.rs - 5-Block System Prompt 装配器
//!
//! 设计特点:
//! - Block A: 角色定义 — 从 persona/facts/traits 动态组装
//! - Block B: Few-shot 示例 — 从 selected persona_examples 注入 3-5 对
//! - Block C: 记忆上下文 — 从 RAG 检索结果注入（Persona-Aware 已过滤）
//! - Block D: 知识边界 — 人格的知识范围和不回答的话题
//! - Block E: 当前语境 — 时间、可选的天气/位置
//! - 支持线上记忆注入开关：若关闭且 provider 为线上，跳过 Block C
//! - 所有 block 均支持缺省降级（如无 traits 则省略对应段落）
//!
//! 依赖:
//! - `ramaria_core::types`: Persona, PersonaFact, PersonalityTrait, PersonaExample
//! - `ramaria_memory::rag`: RAG 上下文格式化（由上层传入）

use chrono::Local;
use ramaria_core::types::{
    Persona, PersonaExample, PersonaFact, PersonalityTrait, ProfileField, TraitStatus,
};

// =========================================================
// 5-Block System Prompt 配置
// =========================================================

/// System Prompt 装配配置。
///
/// 字段约定:
/// - `max_examples`: Block B 最大示例对数。默认 5。
/// - `max_traits_per_layer`: 每层最多展示的性格标签数。默认 3。
/// - `include_traits`: 是否包含性格标签（Block A）。默认 true。
/// - `include_facts`: 是否包含事实信息（Block A）。默认 true。
/// - `include_examples`: 是否包含 Few-shot 示例（Block B）。默认 true。
/// - `include_knowledge_boundary`: 是否包含知识边界（Block D）。默认 true。
/// - `current_time_str`: 当前时间的格式化字符串（Block E）。空则自动使用 now_ms。
#[derive(Debug, Clone)]
pub struct PromptConfig {
    /// Block B 最大示例对数
    pub max_examples: usize,
    /// 每层最多展示的性格标签数
    pub max_traits_per_layer: usize,
    /// 是否包含性格标签
    pub include_traits: bool,
    /// 是否包含事实信息
    pub include_facts: bool,
    /// 是否包含 Few-shot 示例
    pub include_examples: bool,
    /// 是否包含知识边界
    pub include_knowledge_boundary: bool,
    /// 当前时间字符串（Block E）
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

/// 5-Block 装配所需的全部数据。
///
/// 职责:
/// - 将分散的 persona 数据聚合为一次 System Prompt 构建的输入。
/// - 所有字段均可选：缺失时对应 Block 自动省略。
///
/// 跨 session 上下文:
/// - `recent_session_summaries`: 无条件注入的近期 L1 摘要（最近 1-3 条），
///   解决"新 session 发'你好'时 LLM 完全不知道上次聊了什么"的问题。
/// - `last_active_at`: 该 persona 最后活跃时间，供 LLM 判断对话连续性。
#[derive(Debug, Clone, Default)]
pub struct PromptContext {
    /// 人格基本信息
    pub persona: Option<Persona>,

    /// 事实信息（Block A）
    pub facts: Vec<PersonaFact>,

    /// 性格标签（Block A）
    pub traits: Vec<PersonalityTrait>,

    /// Few-shot 示例（Block B）
    pub examples: Vec<PersonaExample>,

    /// RAG 记忆上下文文本（Block C 子段 [相关记忆]，由检索结果格式化传入）
    pub memory_context: Option<String>,

    /// 近期 session 摘要（Block C 子段 [近期对话摘要]，无条件注入）
    ///
    /// 字段约定:
    /// - 按时间降序排列（最近在前）。
    /// - 每条为格式化好的摘要文本（含时间段和氛围）。
    /// - 为空时 [近期对话摘要] 显示"（这是你与用户的首次对话）"。
    pub recent_session_summaries: Vec<String>,

    /// 该 persona 最近一次活跃时间（Block E 可选扩展）
    pub last_active_at: Option<String>,

    /// 知识边界描述（Block D）
    pub knowledge_boundary: Option<String>,

    /// 当前时间字符串（Block E，空则使用 now_ms）
    pub current_time_str: Option<String>,

    /// 天气信息（Block E 可选）
    pub weather: Option<String>,
}

// =========================================================
// 装配函数
// =========================================================

/// 装配完整的 5-Block System Prompt。
///
/// 参数:
/// - `context`: 装配上下文（persona/facts/traits/examples 等）。
/// - `config`: 装配配置（控制哪些 block 启用及数量上限）。
///
/// 返回:
/// - 完整的 System Prompt 字符串，可直接作为 `ChatRequest.system_prompt` 使用。
///
/// 降级策略:
/// - 无 persona 时使用默认助手的身份描述。
/// - 无 traits 时省略性格段。
/// - 无 facts 时省略事实段。
/// - 无 examples 时省略 Block B。
/// - Block C 拆分为两层: [近期对话摘要] (无条件) + [相关记忆] (RAG 结果)。
pub fn assemble_prompt(context: &PromptContext, config: &PromptConfig) -> String {
    let mut blocks: Vec<String> = Vec::with_capacity(6); // +1 for split Block C

    // ---- Block A: 角色定义 ----
    blocks.push(build_block_a(context, config));

    // ---- Block B: Few-shot 示例 ----
    if config.include_examples && !context.examples.is_empty() {
        let block_b = build_block_b(&context.examples, config.max_examples);
        if !block_b.is_empty() {
            blocks.push(block_b);
        }
    }

    // ---- Block C: 记忆上下文（拆分为两层） ----
    blocks.push(build_block_c(context));

    // ---- Block D: 知识边界 ----
    if config.include_knowledge_boundary {
        blocks.push(build_block_d(context));
    }

    // ---- Block E: 当前语境 ----
    blocks.push(build_block_e(context));

    // 拼接：双换行分隔各 Block
    blocks.join("\n\n")
}

// =========================================================
// Block A: 角色定义
// =========================================================

/// 组装 Block A：角色身份 + 性格标签 + 事实信息。
fn build_block_a(context: &PromptContext, config: &PromptConfig) -> String {
    let mut parts: Vec<String> = Vec::new();

    // A1: 角色身份
    if let Some(ref persona) = context.persona {
        parts.push(format!("你是「{}」，一位 AI 助手。", persona.name));
        // 追加 persona kind 描述
        let kind_desc = match persona.kind {
            ramaria_core::types::PersonaKind::Rama => "你是 Ramaria 助手自身。",
            ramaria_core::types::PersonaKind::User => "你正在以用户的视角思考和回复。",
            ramaria_core::types::PersonaKind::Char => "你正在扮演一个虚构角色。",
            ramaria_core::types::PersonaKind::Anim => "你正在扮演一个动画角色。",
            ramaria_core::types::PersonaKind::Oc => "你正在扮演一个原创角色（OC）。",
            ramaria_core::types::PersonaKind::Hist => "你正在扮演一个历史人物。",
            // PersonaKind 为 #[non_exhaustive]，需处理未来新增类型
            _ => "你正在扮演一个角色。",
        };
        parts.push(kind_desc.to_string());

        // 加载 config JSON 中的额外描述
        if let Some(ref cfg_json) = persona.config
            && let Ok(obj) = serde_json::from_str::<serde_json::Value>(cfg_json)
        {
            if let Some(desc) = obj.get("description").and_then(|v| v.as_str()) {
                parts.push(format!("背景：{desc}"));
            }
            if let Some(style) = obj.get("speaking_style").and_then(|v| v.as_str()) {
                parts.push(format!("说话风格：{style}"));
            }
        }
    } else {
        parts.push(
            "你是 Ramaria，一个具有记忆能力、善解人意的 AI 助手。\n\
             你可以记住与用户的对话历史，并在后续对话中引用这些记忆。"
                .to_string(),
        );
    }

    // A2: 性格标签（按 layer 分组）
    if config.include_traits && !context.traits.is_empty() {
        let trait_text = format_traits_for_prompt(&context.traits, config.max_traits_per_layer);
        if !trait_text.is_empty() {
            parts.push(format!("性格特征：\n{trait_text}"));
        }
    }

    // A3: 事实信息
    if config.include_facts && !context.facts.is_empty() {
        let fact_text = format_facts_for_prompt(&context.facts);
        if !fact_text.is_empty() {
            parts.push(format!("已知事实：\n{fact_text}"));
        }
    }

    parts.join("\n\n")
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
            // TraitLayer 为 #[non_exhaustive]，需处理未来新增层次
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
            // ProfileField 为 #[non_exhaustive]，需处理未来新增字段
            _ => "其他",
        };
        lines.push(format!("  [{field_label}] {}", fact.content));
    }

    lines.join("\n")
}

// =========================================================
// Block B: Few-shot 示例
// =========================================================

/// 组装 Block B：Few-shot 对话示例。
fn build_block_b(examples: &[PersonaExample], max_examples: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("[回复示例]\n以下是你在此人格下的一些回复示例，请参考其风格：".to_string());

    for (i, ex) in examples.iter().take(max_examples).enumerate() {
        lines.push(format!("\n示例 {}：", i + 1));

        // 前文语境
        if let Some(ref ctx) = ex.context
            && !ctx.trim().is_empty()
        {
            // 前文最多展示 3 条
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
// Block C: 记忆上下文（两层结构）
// =========================================================

/// 组装 Block C：近期对话摘要 + 相关记忆。
///
/// 两层结构:
/// 1. [近期对话摘要] — 无条件注入最近 1-3 条 L1 摘要，即使与当前查询不匹配。
///
/// 解决"新 session 发'你好'时 LLM 完全不知道上次聊了什么"的问题。
/// 2. [相关记忆] — RAG 检索结果，按关键词/向量/图谱匹配。
///
/// 为空时显示"（暂无与当前话题直接相关的历史记忆）"。
///
/// 跨 session 叙事:
/// - 若 `recent_session_summaries` 非空，调用 `build_cross_session_narrative` 生成引导句。
/// - 该引导句告诉 LLM 此前对话的总体脉络，便于自然地衔接。
fn build_block_c(context: &PromptContext) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(2);

    // C1: [近期对话摘要] — 无条件注入
    if context.recent_session_summaries.is_empty() {
        parts.push("[近期对话摘要]\n（这是你与用户的首次对话）".to_string());
    } else {
        let mut lines = Vec::with_capacity(context.recent_session_summaries.len() + 1);
        lines.push("[近期对话摘要]".to_string());

        // 生成跨 session 叙事引导句
        let narrative = build_cross_session_narrative(&context.recent_session_summaries);
        lines.push(narrative);
        lines.push(String::new()); // 空行分隔

        // 逐条列出近期对话摘要
        for (i, summary) in context.recent_session_summaries.iter().enumerate() {
            // 截断过长摘要到 120 字符（与 RAG 格式化保持一致）
            let display: String = if summary.chars().count() > 120 {
                summary.chars().take(120).collect::<String>() + "…"
            } else {
                summary.clone()
            };
            lines.push(format!("  {}. {}", i + 1, display));
        }

        parts.push(lines.join("\n"));
    }

    // C2: [相关记忆] — RAG 检索结果（条件注入）
    match &context.memory_context {
        Some(ctx) if !ctx.trim().is_empty() => {
            parts.push(format!(
                "[相关记忆]\n\
                 以下是与当前话题相关的历史记忆，请结合这些信息回复：\n\
                 {ctx}"
            ));
        }
        _ => {
            parts.push("[相关记忆]\n（暂无与当前话题直接相关的历史记忆）".to_string());
        }
    }

    parts.join("\n\n")
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
/// - 叙事引导句字符串，如:
///   "你此前与用户进行了 3 次对话：讨论了 Python 学习计划、完成了 FastAPI 项目、\
///   探讨了 Rust 异步编程。最近一次对话发生在不久前。"
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
            // 去除末尾可能的不完整字符
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
// Block D: 知识边界
// =========================================================

/// 组装 Block D：知识边界。
fn build_block_d(context: &PromptContext) -> String {
    if let Some(ref boundary) = context.knowledge_boundary
        && !boundary.trim().is_empty()
    {
        return format!("[知识边界]\n{boundary}");
    }

    // 默认知识边界
    concat!(
        "[知识边界]\n",
        "- 你是 AI 助手，你的知识截止于训练数据。\n",
        "- 不要编造你不知道的事实或日期。如果不确定，请诚实说明。\n",
        "- 不提供医疗、法律或金融建议。\n",
        "- 不生成有害、违法或不道德的内容。"
    )
    .to_string()
}

// =========================================================
// Block E: 当前语境
// =========================================================

/// 组装 Block E：当前时间 + 可选天气 + 可选最后活跃时间。
///
/// 时间格式：
/// - 若 `context.current_time_str` 有值，直接使用（由上层 App 传入格式化字符串）。
/// - 否则使用 `chrono::Local::now` 生成可读日期时间（`%Y-%m-%d %H:%M`）。
///
/// 跨 session 提示：
/// - 若 `context.last_active_at` 有值，追加"上次对话时间"行，
///   帮助 LLM 判断对话间隔（如"两周前"→ 需要寒暄；"几分钟前"→ 无缝继续）。
fn build_block_e(context: &PromptContext) -> String {
    let time_str = context
        .current_time_str
        .clone()
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d %H:%M").to_string());

    let mut lines = vec![format!("[当前语境]\n当前时间：{time_str}")];

    if let Some(ref weather) = context.weather
        && !weather.trim().is_empty()
    {
        lines.push(format!("天气：{weather}"));
    }

    // 跨 session 上下文: 告知 LLM 上次对话是什么时候
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

    // ---- Block A 测试 ----

    #[test]
    fn block_a_with_persona() {
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
        // Block C 现在拆分为两层：C1 近期对话摘要 + C2 相关记忆
        assert!(result.contains("[近期对话摘要]"));
        assert!(result.contains("首次对话")); // 无近期摘要时显示首次对话提示
        assert!(result.contains("[相关记忆]"));
        assert!(result.contains("[知识边界]"));
        assert!(result.contains("[当前语境]"));
    }

    #[test]
    fn block_a_without_persona_uses_default() {
        let ctx = PromptContext::default();
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);
        assert!(result.contains("Ramaria"));
        assert!(result.contains("AI 助手"));
    }

    #[test]
    fn block_a_with_traits() {
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
    fn block_a_traits_disabled() {
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
    fn block_a_with_facts() {
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

    // ---- Block B 测试 ----

    #[test]
    fn block_b_with_examples() {
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

        assert!(result.contains("[回复示例]"));
        assert!(result.contains("你好呀"));
        assert!(result.contains("😊"));
        assert!(result.contains("Rust"));
    }

    #[test]
    fn block_b_disabled() {
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
        assert!(!result.contains("[回复示例]"));
    }

    #[test]
    fn block_b_max_examples_limit() {
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

    // ---- Block C 测试 ----

    #[test]
    fn block_c_with_memory_only() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            memory_context: Some("用户之前提到喜欢猫。".into()),
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        // C1: 无近期摘要 → 首次对话提示
        assert!(result.contains("首次对话"));
        // C2: 有 RAG 结果
        assert!(result.contains("喜欢猫"));
        assert!(result.contains("[相关记忆]"));
    }

    #[test]
    fn block_c_without_memory_shows_placeholder() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            memory_context: None,
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        // C1: 首次对话
        assert!(result.contains("首次对话"));
        // C2: RAG 无结果占位
        assert!(result.contains("暂无与当前话题直接相关的历史记忆"));
    }

    #[test]
    fn block_c_with_recent_summaries_and_memory() {
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

        // C1: 近期对话摘要应出现
        assert!(result.contains("[近期对话摘要]"));
        assert!(result.contains("Python异步编程"));
        assert!(result.contains("Rust项目"));
        // C2: RAG 相关记忆
        assert!(result.contains("[相关记忆]"));
        assert!(result.contains("橘猫"));
        // 跨 session 叙事引导句应出现
        assert!(result.contains("此前与用户进行了"));
    }

    #[test]
    fn block_c_single_recent_summary() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            recent_session_summaries: vec!["讨论了天气和出行计划，决定周末去爬山".to_string()],
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("[近期对话摘要]"));
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

    // ---- Block D 测试 ----

    #[test]
    fn block_d_custom_boundary() {
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
    fn block_d_disabled() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            ..Default::default()
        };
        let config = PromptConfig {
            include_knowledge_boundary: false,
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &config);

        assert!(!result.contains("[知识边界]"));
    }

    // ---- Block E 测试 ----

    #[test]
    fn block_e_with_weather() {
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
    fn block_e_defaults_to_readable_time() {
        // 未提供 current_time_str 时，应生成可读时间而非 Unix 毫秒时间戳
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            ..Default::default()
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        assert!(result.contains("当前时间"));
        // 不应包含 Unix 毫秒时间戳（13位数字）
        let after_time = result.split("当前时间：").nth(1).unwrap_or("");
        let time_part = after_time.lines().next().unwrap_or("");
        assert!(
            time_part.chars().filter(|c| c.is_ascii_digit()).count() <= 16,
            "时间部分不应是长整数时间戳: {time_part}"
        );
        // 应为 YYYY-MM-DD HH:MM 格式（含连字符和冒号）
        assert!(time_part.contains('-'), "应包含日期连字符: {time_part}");
        assert!(time_part.contains(':'), "应包含时间冒号: {time_part}");
    }

    #[test]
    fn block_e_with_last_active() {
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
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        // 所有 Block 都应存在（Block C 拆为两层）
        assert!(result.contains("小明"), "Block A 缺失");
        assert!(result.contains("[回复示例]"), "Block B 缺失");
        assert!(result.contains("[近期对话摘要]"), "Block C1 缺失");
        assert!(result.contains("[相关记忆]"), "Block C2 缺失");
        assert!(result.contains("[知识边界]"), "Block D 缺失");
        assert!(result.contains("[当前语境]"), "Block E 缺失");
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
        assert!(result.contains("首次对话")); // C1 首次对话提示
    }
}
