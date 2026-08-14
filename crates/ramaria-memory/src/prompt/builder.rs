//! rust/crates/ramaria-memory/src/prompt/builder.rs - 四层 System Prompt 装配器
//!
//! v1.4 M6（T-V14-6-001）模板精简：由 v2.0 的 CRISPE 七段式对齐算法说明书
//! v3.1 §8.2 的四层注入结构。段落命名与结构映射表（`TEMPLATE_LAYER_MAP`）：
//!
//! | v1.4 M6 段落（v3.1 §8.2） | 内容来源 | v1.3 CRISPE 对应块 |
//! |--------------------------|----------|--------------------|
//! | `# 能力边界` | 安全边界（保留，非四层） | Capacity |
//! | `# 角色（行为层）` | 角色身份 + 性格特征 + 已知事实 + 回复规范 | Role + Insight + Experiment |
//! | `## 行为规则`（行为层，v1.5） | 情境-反应规则（`render_behavior_block`，命中注入） | v1.4 空槽位已填充 |
//! | `# 说话风格（表达层）` | 说话风格 + 对话示例 | Personality + Statement |
//! | `# 知识（知识层，按需）` | 事实卡片（`render_knowledge_block`，v1.6） | 新增预留 |
//! | `# 记忆（脉络层）` | 近期对话脉络 + 相关记忆 + 原文片段 + 桥接 | Memory + utt/桥接（v1.4） |
//! | `# 当前时间` | 时间/天气/上次活跃 | 当前语境 |
//!
//! 设计特点:
//! - 空块自动跳过：行为槽位 v1.5 已填充（未命中/关闭不产生段落），知识槽位仍为空实现
//!   （T-V14-6-003），不产生空段落。
//! - 脉络层独立预算（v3.1 §8.3，≤ 30%），超限裁剪顺序：原文块 → 桥接头部
//!   → 相关记忆 → 脉络保最近（预算分配器见 `layers.rs`）。
//! - 回归红线：助手类 persona（原文白名单外）不注入原文/桥接，输出与 v1.3 语义等价。
//!
//! 依赖:
//! - `ramaria_core::types`: Persona, PersonaFact, PersonalityTrait, PersonaExample
//! - `ramaria_memory::rag`: RAG 上下文格式化（由上层传入）
//! - `prompt::layers`: 四层注入结构与预算分配器（T-V14-6-002）

use crate::prompt::layers::{
    LayerBudgetConfig, allocate_memory_layer_budget, render_behavior_block, render_knowledge_block,
};
use crate::retriever::UttHit;
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
/// - `include_knowledge_boundary`: 是否包含知识边界（能力边界块末尾）。默认 true。
/// - `current_time_str`: 当前时间的格式化字符串。空则自动使用 chrono::Local::now()。
/// - `memory_layer_budget_chars`: 脉络层字符预算上限（v1.4 M6）。
///   `None` 时使用默认预算：`LayerBudgetConfig`（1000 tokens × 30% × 2 = 600 字符）。
#[derive(Debug, Clone)]
pub struct PromptConfig {
    /// 对话示例最大条数
    pub max_examples: usize,
    /// 性格标签每层最多展示数
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
    /// 脉络层字符预算上限（v1.4 M6；None = 默认 600 字符）
    pub memory_layer_budget_chars: Option<usize>,
    /// 行为控制块字符预算上限（v1.5 M6；None = 默认 400 字符，§8.3 固定小比例）
    pub behavior_block_max_chars: Option<usize>,
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
            memory_layer_budget_chars: None,
            behavior_block_max_chars: None,
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

    /// v1.4 新增: utt 原文片段（Memory 块 [原文片段] 小节，已按预算裁剪渲染）
    ///
    /// 安全约束:
    /// - 仅角色类 persona（白名单内）由检索层填充；白名单外为 None（行为等同 v1.3）。
    /// - 原文内容不写日志。
    pub utt_context: Option<String>,

    /// v1.4 M5 新增: 桥接内容（Memory 块 [桥接（上一会话尾部）] 小节，
    /// 已按预算从头部截断、保最近；None 表示不注入，等同 v1.3）
    ///
    /// 安全约束（与 utt_context 一致）:
    /// - 承载原文级信息，仅白名单内 persona 由桥接层填充。
    /// - 内容不写日志。
    pub bridge_context: Option<String>,

    /// v1.5 M6 新增: 行为层路由决策（情境路由命中合并结果，`behavior/routing.rs`）。
    ///
    /// 字段约定:
    /// - `None` = 行为关闭 / 未命中 / 路由失败降级 → 不注入行为块，
    ///   prompt 与 v1.4 语义等价（回归红线，由 `ramaria-app` 保证）。
    /// - `Some(decision)` = 主规则 + 合并 avoid/params，由
    ///   `render_behavior_block` 渲染 `## 行为规则` 小节。
    pub behavior_decision: Option<crate::behavior::MergedDecision>,
}

// =========================================================
// 四层 System Prompt 模板（v1.4 M6，对齐 v3.1 §8.2）
// =========================================================

/// 四层模板结构（v3.1 §8.2）。
///
/// 占位符说明:
/// - `{capacity}` → `# 能力边界`（安全边界，非四层，前置保留）
/// - `{role_layer}` → `# 角色（行为层）`（角色身份/性格特征/已知事实/回复规范）
/// - `{behavior}` → 行为层槽位（v1.5 情境-反应规则；当前为空实现）
/// - `{style_layer}` → `# 说话风格（表达层）`（说话风格 + 对话示例）
/// - `{knowledge}` → `# 知识（知识层，按需）`（v1.6 事实卡片；当前为空实现）
/// - `{memory}` → `# 记忆（脉络层）`（近期脉络/相关记忆/原文片段/桥接）
/// - `{context_block}` → `# 当前时间`（时间/天气/上次活跃）
///
/// 装配语义: 空块自动跳过（不产生空段落），由 `assemble_prompt` 按序拼接。
pub const LAYER_TEMPLATE: &str = "\
{capacity}

{role_layer}

{behavior}

{style_layer}

{knowledge}

{memory}

{context_block}";

/// 段落结构映射表（v1.4 M6 文档化产物，T-V14-6-001）。
///
/// 每项 `(段落标题, 内容来源, v1.3 CRISPE 对应块)`：
/// 记录四层模板与 v1.3 七段式的对应关系，供回归核对与后续版本参考。
pub const TEMPLATE_LAYER_MAP: &[(&str, &str, &str)] = &[
    (
        "# 能力边界",
        "安全边界（AI 助手核心能力 + 知识边界）",
        "Capacity（v1.3 保留）",
    ),
    (
        "# 角色（行为层）",
        "角色身份 + 性格特征 + 已知事实 + 回复规范",
        "Role + Insight + Experiment",
    ),
    (
        "# 说话风格（表达层）",
        "说话风格（speaking_style）+ 对话示例（Few-shot）",
        "Personality + Statement",
    ),
    (
        "# 知识（知识层，按需）",
        "事实卡片（v1.6 填充，当前槽位为空）",
        "新增预留（T-V14-6-003）",
    ),
    (
        "# 记忆（脉络层）",
        "近期对话脉络 + 相关历史记忆 + 原文片段 + 桥接",
        "Memory（v1.3）+ utt/桥接（v1.4）",
    ),
    ("# 当前时间", "时间 / 天气 / 上次活跃", "当前语境（v1.3）"),
];

// =========================================================
// 装配函数
// =========================================================

/// 装配完整的四层 System Prompt。
///
/// v2.0: 从 5-Block 格式重构为 CRISPE 七段式。
/// v1.4 M6: 对齐 v3.1 §8.2 精简为四层结构（段落映射见 `TEMPLATE_LAYER_MAP`），
/// 空块自动跳过（行为/知识槽位当前为空，不产生空段落）。
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
/// - 无 traits 时省略角色层中的性格特征段。
/// - 无 facts 时省略角色层中的已知事实段。
/// - 无 examples 时省略表达层中的对话示例段。
/// - 无 chat_style_rules 时使用最小化默认规则。
/// - 行为层未命中/关闭（`behavior_decision=None`）→ 不产生段落（等同 v1.4）；
///   知识层槽位为空 → 不产生段落（v1.6 填充后自动生效）。
pub fn assemble_prompt(context: &PromptContext, config: &PromptConfig) -> String {
    let mut blocks: Vec<String> = Vec::with_capacity(7);
    for block in [
        build_capacity(config, context),
        build_role_layer(context, config),
        // 行为层（v1.5 M6 已填充；None → 不产生段落，等同 v1.4）
        render_behavior_block(context, config).map_or(String::new(), |b| b.content),
        build_style_layer(context, config),
        // 知识层槽位（v1.6 填充；当前 None → 不产生段落）
        render_knowledge_block(context).map_or(String::new(), |b| b.content),
        build_memory(context, config),
        build_context_block(context),
    ] {
        if !block.trim().is_empty() {
            blocks.push(block);
        }
    }
    blocks.join("\n\n")
}

// =========================================================
// Capacity 块: 能力边界
// =========================================================

/// 组装能力边界块：AI 助手核心能力 + 知识边界（安全红线，非四层，前置保留）。
fn build_capacity(config: &PromptConfig, context: &PromptContext) -> String {
    let mut parts = vec![
        "# 能力边界\n\
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

/// 组装角色身份段：角色身份 + persona 类型 + 背景描述（`# 角色（行为层）` 的头部）。
fn build_role(context: &PromptContext) -> String {
    if let Some(ref persona) = context.persona {
        let mut parts = vec![format!(
            "# 角色（行为层）\n你是「{}」，一位 AI 助手。",
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
        "# 角色（行为层）\n\
         你是 Ramaria，一个具有记忆能力、善解人意的 AI 助手。\n\
         你可以记住与用户的对话历史，并在后续对话中引用这些记忆。"
            .to_string()
    }
}

// =========================================================
// Memory 块: 记忆上下文（最高优先级）
// =========================================================

/// 组装记忆层块：近期对话脉络 + 相关历史记忆 + 原文片段 + 桥接（`# 记忆（脉络层）`）。
///
/// v2.0: 从 Block C 从属位置提升为独立 Memory 块。
/// v1.4 M6（T-V14-6-002）: 接入脉络层预算分配器（v3.1 §8.3）——
/// 独立预算 ≤ 30%，超限裁剪顺序：原文块 → 桥接头部 → 相关记忆 → 脉络保最近。
///
/// 子段落结构（v3.1 §8.2）:
/// 1. `## 近期对话脉络` — 最近 1-3 条 L1 摘要的叙事引导句（预算内保最近）
/// 2. `## 相关历史记忆` — RAG 检索结果（条件注入）
/// 3. `## 原文片段` — utt 话语块（v1.4，白名单外为 None 不产生段落）
/// 4. `## 桥接（上一会话尾部）` — 上一会话尾部原文（v1.4 M5）
fn build_memory(context: &PromptContext, config: &PromptConfig) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(2);

    parts.push(
        "# 记忆（脉络层）\n\
               以下是你的记忆系统检索到的相关信息。你对用户的了解完全来源于此。\
               请仔细阅读，在对话中自然地运用这些信息——但只在话题相关或用户主动提及时引用，\
               不强行插入无关记忆。"
            .to_string(),
    );

    // 脉络层预算分配（v3.1 §8.3：独立预算，默认 1000 tokens × 30% × 2 = 600 字符）
    let budget = config
        .memory_layer_budget_chars
        .unwrap_or_else(|| LayerBudgetConfig::default().budget_chars());
    let alloc = allocate_memory_layer_budget(
        context.utt_context.as_deref(),
        context.bridge_context.as_deref(),
        &context.recent_session_summaries,
        context.memory_context.as_deref(),
        budget,
    );

    // 近期对话脉络（预算内保最近；预算不足时显示"首次对话"）
    if alloc.summaries.is_empty() {
        parts.push("\n\n## 近期对话脉络\n（这是你与用户的首次对话）".to_string());
    } else {
        let narrative = build_cross_session_narrative(&alloc.summaries);
        let mut lines = vec!["\n\n## 近期对话脉络".to_string(), narrative];

        // 逐条列出近期摘要（截断到 120 字符）
        for (i, summary) in alloc.summaries.iter().enumerate() {
            let display: String = if summary.chars().count() > 120 {
                summary.chars().take(120).collect::<String>() + "…"
            } else {
                summary.clone()
            };
            lines.push(format!("  {}. {}", i + 1, display));
        }

        parts.push(lines.join("\n"));
    }

    // 相关历史记忆（RAG 结果；预算内句子边界截断）
    match &alloc.rag {
        Some(rag) if !rag.trim().is_empty() => {
            parts.push(format!(
                "\n\n## 相关历史记忆\n\
                 以下是与当前话题相关的历史记忆，请结合这些信息回复：\n\
                 {rag}"
            ));
        }
        _ => {
            parts.push("\n\n## 相关历史记忆\n（暂无与当前话题直接相关的历史记忆）".to_string());
        }
    }

    // 原文片段（v1.4 utt 话语块；预算不足/白名单外为 None → 不产生段落，等同 v1.3）
    if let Some(utt) = &alloc.utt
        && !utt.trim().is_empty()
    {
        parts.push(format!(
            "\n\n## 原文片段\n\
             以下是目标角色说过的原话（完整引用，不要逐字抄袭，仅学习其语气、用词与口癖）：\n\
             {utt}"
        ));
    }

    // 桥接（v1.4 M5 上一会话尾部；预算不足/开关关闭/白名单外为 None → 不产生段落）
    if let Some(bridge) = &alloc.bridge
        && !bridge.trim().is_empty()
    {
        parts.push(format!(
            "\n\n## 桥接（上一会话尾部）\n\
             以下是你上一段对话结尾的原文，用于保持对话连贯性（仅供衔接参考，\
             不要逐字引用，也不要编造其中未提及的内容）：\n\
             {bridge}"
        ));
    }

    parts.join("")
}

// =========================================================
// utt 原文片段渲染与预算裁剪（v1.4）
// =========================================================

/// 按预算渲染【原文片段】段落内容（整块保留/丢弃，超预算按相似度从低到高丢整块）。
///
/// 规则（v3.1 §8.3）:
/// - `hits` 必须按得分降序传入（检索侧保证）。
/// - 从高分到低分整块累加：未超预算的块全部保留，首个超预算的块及其后全部丢弃
///   （不做块内截断——原文是整体引用的，截断会破坏语义）。
///
/// 参数:
/// - `hits`: 检索命中（按得分降序）。
/// - `max_block_chars`: 全部块合计的字符预算上限（`[utt].max_block_chars`）。
///
/// 返回:
/// - 块文本序列（块间空行分隔）；`hits` 为空或首块即超预算时返回空字符串。
pub fn render_utt_context(hits: &[UttHit], max_block_chars: usize) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;

    for hit in hits {
        let text = hit.doc.block_text.trim();
        if text.is_empty() {
            continue;
        }
        let chars = text.chars().count();
        if used + chars > max_block_chars {
            // 超预算：丢整块（含其后所有块——已按相似度降序，剩余相似度更低）
            break;
        }
        kept.push(text.to_string());
        used += chars;
    }

    kept.join("\n\n")
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
// 角色层（行为层）: 角色身份 + 性格特征 + 已知事实 + 回复规范
// =========================================================

/// 组装角色层块（`# 角色（行为层）`）：角色身份 + 性格特征 + 已知事实 + 回复规范。
///
/// 对应 v1.3 的 Role + Insight + Experiment 三块（映射见 `TEMPLATE_LAYER_MAP`）；
/// 行为规则槽位（v1.5 情境-反应规则）由 `render_behavior_block` 在装配时挂载。
fn build_role_layer(context: &PromptContext, config: &PromptConfig) -> String {
    let mut parts: Vec<String> = vec![build_role(context)];

    // 性格标签（按 layer 分组；无 traits 时省略，v1.3 降级语义）
    if config.include_traits && !context.traits.is_empty() {
        let trait_text = format_traits_for_prompt(&context.traits, config.max_traits_per_layer);
        if !trait_text.is_empty() {
            parts.push(format!("\n\n## 性格特征\n{trait_text}"));
        }
    }

    // 已知事实（无 facts 时省略）
    if config.include_facts && !context.facts.is_empty() {
        let fact_text = format_facts_for_prompt(&context.facts);
        if !fact_text.is_empty() {
            parts.push(format!("\n\n## 已知事实\n{fact_text}"));
        }
    }

    // 回复规范（核心规则 + 记忆引用规则；无自定义规则时使用最小化默认）
    parts.push(build_experiment_section(context, config));

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
// 表达层（说话风格）: 说话风格 + 对话示例
// =========================================================

/// 组装表达层块（`# 说话风格（表达层）`）：说话风格 + 对话示例。
///
/// 对应 v1.3 的 Personality + Statement 两块（映射见 `TEMPLATE_LAYER_MAP`）；
/// 两子段皆缺省时整体不产生段落（v1.3 降级语义）。
fn build_style_layer(context: &PromptContext, config: &PromptConfig) -> String {
    let mut sub: Vec<String> = Vec::with_capacity(2);

    // 说话风格（persona.config 的 speaking_style）
    let style = build_personality(context);
    if !style.is_empty() {
        sub.push(style);
    }

    // 对话示例（Few-shot）
    let statement = build_statement(context, config);
    if !statement.is_empty() {
        sub.push(statement);
    }

    if sub.is_empty() {
        return String::new();
    }
    format!("# 说话风格（表达层）\n{}", sub.join("\n\n"))
}

/// 组装说话风格子段（`## 说话风格`）。
///
/// v2.0: 从 Block A 中独立出来，作为独立段。
/// v1.4 M6: 并入表达层作为子段；无 speaking_style 时返回空（不产生段落）。
/// 从 persona.config JSON 的 `speaking_style` 字段提取。
fn build_personality(context: &PromptContext) -> String {
    if let Some(ref persona) = context.persona
        && let Some(ref cfg_json) = persona.config
        && let Ok(obj) = serde_json::from_str::<serde_json::Value>(cfg_json)
        && let Some(style) = obj.get("speaking_style").and_then(|v| v.as_str())
        && !style.trim().is_empty()
    {
        format!("## 说话风格\n{style}")
    } else {
        String::new()
    }
}

/// 组装对话示例子段（`## 对话示例`）：Few-shot 对话示例。
///
/// v1.4 M6: 从独立 Statement 块并入表达层；无示例时返回空（不产生段落）。
fn build_statement(context: &PromptContext, config: &PromptConfig) -> String {
    if !config.include_examples || context.examples.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    lines.push(
        "## 对话示例\n\
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
// 回复规范子段（角色层内）
// =========================================================

/// 组装回复规范子段（`## 回复规范`）：回复规则 + 记忆引用规则。
///
/// v2.0: 合并原 SHARED_CHAT_STYLE_RULES（回复规则）和新增的记忆引用规则。
/// v1.4 M6: 从独立 Experiment 块并入角色层；
/// 记忆引用规则精确定义"主动回溯 vs 被动响应"的边界。
fn build_experiment_section(context: &PromptContext, config: &PromptConfig) -> String {
    let mut parts: Vec<String> = vec!["## 回复规范".to_string()];

    // 核心回复规则
    if let Some(ref rules) = context.chat_style_rules
        && !rules.trim().is_empty()
    {
        parts.push(format!("\n### 核心规则\n{rules}"));
    } else {
        // 最小化默认规则
        parts.push(
            "\n### 核心规则\n\
             - 用自然、友好的语气回复。\n\
             - 回复简洁明了，不冗长。\n\
             - 不确定的事情请诚实说明。"
                .to_string(),
        );
    }

    // 记忆引用规则（v2.0 新增）
    if config.include_knowledge_boundary {
        parts.push(
            "\n### 记忆引用规则（极其重要）\n\
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

/// 组装当前时间块（`# 当前时间`）：时间 + 可选天气 + 可选上次活跃时间。
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
        "# 当前时间\n\
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
        // 四层模板 markers（v1.4 M6，对齐 v3.1 §8.2）
        assert!(result.contains("# 角色（行为层）"));
        assert!(result.contains("# 记忆（脉络层）"));
        assert!(result.contains("## 说话风格"));
        assert!(result.contains("# 当前时间"));
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

        assert!(result.contains("## 对话示例"));
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
        assert!(!result.contains("## 对话示例"));
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
        assert!(result.contains("记忆（脉络层）"));
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
            utt_context: None,       // 默认无原文片段（v1.4）
            bridge_context: None,    // 默认无桥接内容（v1.4 M5）
            behavior_decision: None, // v1.5 M6：默认无行为路由决策
        };
        let config = PromptConfig::default();
        let result = assemble_prompt(&ctx, &config);

        // CRISPE 框架所有块
        assert!(result.contains("小明"), "Role 块缺失");
        assert!(result.contains("## 对话示例"), "对话示例子段缺失");
        assert!(result.contains("近期对话脉络"), "Memory 近期对话脉络缺失");
        assert!(result.contains("相关历史记忆"), "Memory 相关历史记忆缺失");
        assert!(result.contains("知识边界"), "Capacity 知识边界缺失");
        assert!(result.contains("当前时间"), "语境块缺失");
        assert!(result.contains("测试回复规则"), "Experiment 块缺失");
        // 跨 session 上下文
        assert!(result.contains("Rust编程"), "近期摘要未注入");
        assert!(result.contains("上次对话时间"), "最后活跃时间未注入");
    }

    // =========================================================
    // 行为层装配（v1.5 M6）
    // =========================================================

    /// 构造行为路由合并决策（与 layers.rs 测试同构）。
    fn make_behavior_decision() -> crate::behavior::MergedDecision {
        use ramaria_core::behavior::{BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource};
        let mut rule = BehaviorRule::new(
            "char-0001",
            BehaviorSituation {
                keywords: vec!["加班".to_string()],
                centroid: None,
                response_centroid: None,
                valence_mean: -0.5,
                valence_std: 0.2,
                sample_count: 6,
                presentation_dist: Vec::new(),
                situation_strength_mean: 3.0,
                time_span_days: 10.0,
                trait_refs: Vec::new(),
            },
            Some("先共情再给建议，语气疲惫但温和".to_string()),
            BehaviorParams::default(),
            RuleSource::Auto,
        );
        rule.id = 1;
        crate::behavior::MergedDecision {
            primary_rule: rule,
            merged_avoid: vec!["深夜打扰".to_string()],
            merged_params: BehaviorParams {
                emotional_intensity: -0.4,
                proactiveness: 0.7,
                detail_level: 0.6,
                formality: 0.3,
            },
        }
    }

    #[test]
    fn behavior_block_injected_between_role_and_style() {
        // 命中：行为块注入，位置在角色段与说话风格段之间（注入优先级 行为 > 表达）
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            behavior_decision: Some(make_behavior_decision()),
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &PromptConfig::default());
        assert!(result.contains("## 行为规则"), "行为块缺失: {result}");
        assert!(result.contains("先共情再给建议"), "reaction 缺失");
        assert!(result.contains("深夜打扰"), "avoid 缺失");
        let role_pos = result.find("# 角色（行为层）").expect("角色段存在");
        let behavior_pos = result.find("## 行为规则").expect("行为块存在");
        let style_pos = result.find("# 说话风格（表达层）").expect("表达段存在");
        assert!(
            role_pos < behavior_pos && behavior_pos < style_pos,
            "行为块应位于角色段之后、表达段之前（行为 > 表达）"
        );
    }

    #[test]
    fn behavior_block_absent_without_decision_equals_v1_4() {
        // 未命中/关闭（decision=None）→ 无行为块，输出与 v1.4 语义等价
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            behavior_decision: None,
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &PromptConfig::default());
        assert!(!result.contains("## 行为规则"), "未命中不产生行为块");
        assert!(!result.contains("先共情再给建议"), "规则文本不泄漏");
    }

    #[test]
    fn behavior_block_budget_applied_in_assemble() {
        // 极紧预算：行为块被截断到预算内（§8.3 固定小比例）
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            behavior_decision: Some(make_behavior_decision()),
            ..Default::default()
        };
        let config = PromptConfig {
            behavior_block_max_chars: Some(24),
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &config);
        let behavior_pos = result.find("## 行为规则").expect("行为块存在");
        // 截取行为块文本（到下一个段落标题或结尾）
        let tail = &result[behavior_pos..];
        let block_len = tail.find("\n\n# ").map(|p| p).unwrap_or(tail.len());
        let block = &tail[..block_len];
        assert!(block.chars().count() <= 24, "行为块 ≤ 预算: {block}");
        assert!(block.ends_with('…'), "截断提示: {block}");
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

    // =========================================================
    // utt 原文片段（v1.4）
    // =========================================================

    fn utt_hit(id: i64, persona: &str, text: &str, score: f64) -> crate::retriever::UttHit {
        crate::retriever::UttHit {
            doc: crate::retriever::UttDocView {
                id,
                persona_uid: persona.to_string(),
                session_id: uuid::Uuid::new_v4(),
                block_text: text.to_string(),
                msg_count: 2,
                created_at: 1000,
            },
            score,
            channel: "vector",
        }
    }

    #[test]
    fn render_utt_context_keeps_all_within_budget() {
        let hits = vec![
            utt_hit(1, "char-0001", "今天天气真好", 0.9),
            utt_hit(2, "char-0001", "晚上吃火锅", 0.8),
        ];
        let out = render_utt_context(&hits, 500);
        assert!(out.contains("今天天气真好"));
        assert!(out.contains("晚上吃火锅"));
        assert!(
            out.contains(
                "

"
            ),
            "块间空行分隔"
        );
    }

    #[test]
    fn render_utt_context_trims_by_budget_keeping_high_score() {
        // 预算只够一块：高分的保留，低分的整块丢弃
        // 块1 9 字符 ≤ 预算 10；块1+块2 = 9+5+2(空行) > 10 → 块2 被丢
        let hits = vec![
            utt_hit(1, "char-0001", "第一块内容很长很长", 0.9),
            utt_hit(2, "char-0001", "第二块内容", 0.8),
        ];
        let out = render_utt_context(&hits, 10);
        assert!(out.contains("第一块"), "高分块保留");
        assert!(!out.contains("第二块"), "超预算整块丢弃");
    }

    #[test]
    fn render_utt_context_first_block_over_budget_yields_empty() {
        let hits = vec![utt_hit(1, "char-0001", "超长块内容", 0.9)];
        let out = render_utt_context(&hits, 2);
        assert!(out.is_empty(), "首块即超预算 → 不注入");
    }

    #[test]
    fn render_utt_context_empty_hits_yields_empty() {
        assert!(render_utt_context(&[], 100).is_empty());
    }

    #[test]
    fn render_utt_context_skips_blank_blocks() {
        let hits = vec![
            utt_hit(1, "char-0001", "   ", 0.9),
            utt_hit(2, "char-0001", "有效内容", 0.8),
        ];
        let out = render_utt_context(&hits, 100);
        assert!(out.contains("有效内容"));
        assert!(
            !out.contains(
                "

"
            ),
            "空白块被跳过不产生空段"
        );
    }

    #[test]
    fn assemble_prompt_includes_utt_section_only_when_present() {
        // 回归红线：无原文片段 → prompt 不含【原文片段】段落（白名单外/未命中 = v1.3）
        let ctx = PromptContext::default();
        let result = assemble_prompt(&ctx, &PromptConfig::default());
        assert!(!result.contains("原文片段"), "无原文时不产生段落");

        // 有原文片段 → 段落出现
        let ctx2 = PromptContext {
            utt_context: Some("这是角色原话内容".to_string()),
            ..Default::default()
        };
        let result2 = assemble_prompt(&ctx2, &PromptConfig::default());
        assert!(result2.contains("## 原文片段"), "原文片段段落应出现");
        assert!(result2.contains("这是角色原话内容"));
    }

    /// v1.4 M5（T-V14-5-003）：桥接内容存在时产生【桥接（上一会话尾部）】段落；
    /// 缺失/空白时不产生段落（回归红线：白名单外 = 与 v1.3 语义等价）。
    #[test]
    fn assemble_prompt_includes_bridge_section_only_when_present() {
        // 无桥接内容 → 不产生段落
        let ctx = PromptContext::default();
        let result = assemble_prompt(&ctx, &PromptConfig::default());
        assert!(
            !result.contains("桥接（上一会话尾部）"),
            "无桥接时不产生段落"
        );

        // 空白内容 → 不产生段落（防御）
        let ctx_blank = PromptContext {
            bridge_context: Some("   ".to_string()),
            ..Default::default()
        };
        let result_blank = assemble_prompt(&ctx_blank, &PromptConfig::default());
        assert!(
            !result_blank.contains("## 桥接"),
            "空白桥接内容不应产生段落"
        );

        // 有桥接内容 → 段落出现，含衔接说明与原文
        let ctx2 = PromptContext {
            bridge_context: Some("[2026-08-01 20:00] 角色: 上次聊到这里".to_string()),
            ..Default::default()
        };
        let result2 = assemble_prompt(&ctx2, &PromptConfig::default());
        assert!(
            result2.contains("## 桥接（上一会话尾部）"),
            "桥接段落应出现"
        );
        assert!(result2.contains("保持对话连贯性"), "应含衔接用途说明");
        assert!(result2.contains("上次聊到这里"), "应含桥接原文内容");
    }

    /// v1.4 M5（T-V14-5-003）：桥接与原文片段并存时两个段落都渲染（互不覆盖）。
    #[test]
    fn assemble_prompt_bridge_and_utt_coexist() {
        let ctx = PromptContext {
            utt_context: Some("原文片段内容".to_string()),
            bridge_context: Some("桥接内容".to_string()),
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &PromptConfig::default());
        assert!(result.contains("## 原文片段"), "原文片段段落应保留");
        assert!(result.contains("## 桥接（上一会话尾部）"), "桥接段落应出现");
        assert!(result.contains("原文片段内容") && result.contains("桥接内容"));
    }

    // =========================================================
    // v1.4 M6（T-V14-6-001）：模板精简与语义等价回归
    // =========================================================

    /// 模板结构映射表（`TEMPLATE_LAYER_MAP`）与四层模板常量一致（文档化核对）。
    #[test]
    fn template_layer_map_matches_rendered_paragraphs() {
        // 映射表覆盖 v3.1 §8.2 的四层 + 能力边界 + 当前时间
        let titles: Vec<&str> = TEMPLATE_LAYER_MAP.iter().map(|(t, _, _)| *t).collect();
        assert!(titles.contains(&"# 能力边界"));
        assert!(titles.contains(&"# 角色（行为层）"));
        assert!(titles.contains(&"# 说话风格（表达层）"));
        assert!(titles.contains(&"# 知识（知识层，按需）"));
        assert!(titles.contains(&"# 记忆（脉络层）"));
        assert!(titles.contains(&"# 当前时间"));

        // 模板常量包含全部占位符（结构可机械核对）
        for (i, placeholder) in [
            "capacity",
            "role_layer",
            "behavior",
            "style_layer",
            "knowledge",
            "memory",
            "context_block",
        ]
        .iter()
        .enumerate()
        {
            let _ = i;
            assert!(
                LAYER_TEMPLATE.contains(&format!("{{{placeholder}}}")),
                "模板缺少占位符 {{{placeholder}}}"
            );
        }
    }

    /// 回归红线：助手类 persona（原文白名单外）的输出与 v1.3 语义等价——
    /// 全部关键语义元素（能力边界/角色身份/性格/事实/示例/回复规则/记忆引用规则/
    /// 近期脉络/相关记忆/当前时间）保留，且不产生原文/桥接段落。
    ///
    /// 说明: 白名单闸门在检索/桥接加载层（`retrieve_memory.rs` / `bridge.rs`，
    /// 已分别有断言测试），本测试验证装配层对 `None` 注入源不产生段落。
    #[test]
    fn rama_persona_prompt_semantically_equivalent_to_v13() {
        let ctx = PromptContext {
            persona: Some(Persona {
                id: 1,
                uid: "rama-0001".into(),
                name: "Ramaria".into(),
                kind: PersonaKind::Rama, // 助手类：原文白名单外
                seq: 1,
                source: "system".into(),
                ref_id: None,
                avatar: None,
                active: true,
                config: Some(r#"{"description":"系统助手"}"#.into()),
                description: None,
                created_at: 1000,
                updated_at: 1000,
            }),
            facts: vec![PersonaFact {
                id: 1,
                persona_uid: "rama-0001".into(),
                field: ProfileField::Interests,
                content: "用户喜欢编程".into(),
                source: ramaria_core::types::FactSource::Manual,
                ref_event_id: None,
                ref_l1_id: None,
                created_at: 1000,
                updated_at: 1000,
            }],
            traits: vec![make_test_trait("严谨", TraitLayer::Base, "做事认真")],
            examples: vec![make_test_example("你好", "你好呀")],
            recent_session_summaries: vec!["昨天讨论了项目排期".to_string()],
            memory_context: Some("用户：最近在学 Rust".into()),
            chat_style_rules: Some("回复简洁，用口语化表达".into()),
            current_time_str: Some("2026-08-08 10:00".into()),
            // 白名单外：注入源为 None（检索/桥接层闸门保证），不产生段落
            utt_context: None,
            bridge_context: None,
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &PromptConfig::default());

        // ---- 语义元素齐全（与 v1.3 语义等价） ----
        let semantic_elements = [
            "# 能力边界",           // Capacity 安全边界
            "记住与用户的对话历史", // 核心能力
            "# 角色（行为层）",     // 角色身份
            "Ramaria",
            "## 性格特征", // Insight traits
            "严谨",
            "## 已知事实", // Insight facts
            "用户喜欢编程",
            "## 回复规范", // Experiment 回复规则
            "回复简洁，用口语化表达",
            "记忆引用规则", // 记忆引用规则保留
            "## 对话示例",  // Statement Few-shot
            "你好呀",
            "## 近期对话脉络", // Memory 脉络
            "项目排期",
            "## 相关历史记忆", // RAG
            "Rust",
            "# 当前时间", // 语境
            "2026-08-08",
        ];
        for elem in semantic_elements {
            assert!(result.contains(elem), "助手类 prompt 缺少语义元素: {elem}");
        }

        // ---- 原文/桥接不注入（隐私红线，装配层对 None 源不产生段落） ----
        assert!(
            !result.contains("## 原文片段"),
            "白名单外不得产生原文片段段落"
        );
        assert!(
            !result.contains("## 桥接（上一会话尾部）"),
            "白名单外不得产生桥接段落"
        );
    }

    /// v1.4 M6：行为/知识槽位为空时不产生空段落（T-V14-6-003 验收点）。
    #[test]
    fn empty_slots_do_not_produce_blank_paragraphs() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &PromptConfig::default());

        // 槽位标题不出现（空实现）
        assert!(
            !result.contains("# 知识（知识层，按需）"),
            "知识槽位为空不产生段落"
        );
        // 无三连空行（join 拼接，空块已跳过）
        assert!(!result.contains("\n\n\n\n"), "不应出现多空行");
        // 段落顺序：能力边界在最前，当前时间在最后
        let capacity_pos = result.find("# 能力边界").expect("能力边界应存在");
        let time_pos = result.rfind("# 当前时间").expect("当前时间应存在");
        assert!(capacity_pos < time_pos, "能力边界应前置，当前时间应置尾");
    }

    /// v1.4 M6（T-V14-6-002）：脉络层预算——超限时原文/桥接被裁剪，
    /// 脉络摘要保最近（装配层集成验证，分配器单测在 layers.rs）。
    #[test]
    fn memory_layer_budget_applied_in_assemble() {
        let ctx = PromptContext {
            persona: Some(make_test_persona()),
            recent_session_summaries: vec![
                "最近的摘要内容".to_string(),
                "较旧的摘要内容".to_string(),
            ],
            utt_context: Some("第一块原文内容\n\n第二块原文内容".to_string()),
            bridge_context: Some("第一行桥接内容\n第二行桥接内容".to_string()),
            ..Default::default()
        };
        // 极紧预算：只够脉络摘要
        let config = PromptConfig {
            memory_layer_budget_chars: Some(8),
            ..Default::default()
        };
        let result = assemble_prompt(&ctx, &config);
        assert!(result.contains("最近的摘要内容"), "脉络摘要保最近");
        assert!(!result.contains("较旧的摘要内容"), "最旧摘要被裁");
        assert!(!result.contains("原文内容"), "原文块被裁");
        assert!(!result.contains("桥接内容"), "桥接被裁");
    }
}
