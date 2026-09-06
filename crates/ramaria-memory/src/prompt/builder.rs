//! crates/ramaria-memory/src/prompt/builder.rs - 四层 System Prompt 装配器
//!
//! 模板为四层注入结构。段落命名与结构映射表（`TEMPLATE_LAYER_MAP`）：
//!
//! | 段落 | 内容来源 | 说明 |
//! |------|----------|------|
//! | `# 能力边界` | 安全边界（保留，非四层） | Capacity |
//! | `# 角色（行为层）` | 角色身份 + 性格特征 + 已知事实 + 回复规范 | Role + Insight + Experiment |
//! | `## 行为规则`（行为层） | 情境-反应规则（`render_behavior_block`，命中注入） | 行为槽位 |
//! | `# 说话风格（表达层）` | 说话风格 + 对话示例 | Personality + Statement |
//! | `# 知识（知识层，按需）` | 事实卡片（`render_knowledge_block`） | 知识槽位 |
//! | `# 记忆（脉络层）` | 近期对话脉络 + 相关记忆 + 原文片段 + 桥接 | Memory + utt/桥接 |
//! | `# 当前时间` | 时间/天气/上次活跃 | 当前语境 |
//!
//! 设计特点:
//! - 空块自动跳过：行为槽位（未命中/关闭不产生段落）、知识槽位（无事实不产生段落）。
//! - 脉络层独立预算（≤ 30%），超限裁剪顺序：原文块 → 桥接头部
//!   → 相关记忆 → 脉络保最近（预算分配器见 `layers.rs`）。
//! - 助手类 persona（原文白名单外）不注入原文/桥接。
//!
//! 依赖:
//! - `ramaria_core::types`: Persona, PersonaFact, PersonalityTrait, PersonaExample
//! - `ramaria_memory::rag`: RAG 上下文格式化（由上层传入）
//! - `prompt::layers`: 四层注入结构与预算分配器

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
/// - `memory_layer_budget_chars`: 脉络层字符预算上限。
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
    /// 脉络层字符预算上限（None = 默认 600 字符）
    pub memory_layer_budget_chars: Option<usize>,
    /// 行为控制块字符预算上限（None = 默认 400 字符，固定小比例）
    pub behavior_block_max_chars: Option<usize>,
    /// 是否渲染"说话风格"子段（手工 speaking_style + 自动风格规则，表达层）。
    ///
    /// 探针消融（F3 / B0 / B1 / S_*）用：`false` 时该子段整体不产生，
    /// 不渲染手工 `speaking_style` 与 `## 自动风格规则`。
    pub include_speaking_style: bool,
    /// 是否渲染记忆块中的"近期对话脉络"子段（`## 近期对话脉络`，脉络层）。
    ///
    /// `false` 时不渲染该子段（含"首次对话"占位提示）。
    pub include_narrative: bool,
    /// 是否渲染记忆块中的"相关历史记忆"子段（`## 相关历史记忆`，RAG 摘要通道）。
    ///
    /// 说明: 本系统 RAG 摘要实际经 `ChatRequest.memory_context` 单独注入
    /// （provider 侧以 `<memory_context>` 追加），System Prompt 内该子段在
    /// 无内容时为占位提示；`false` 时不渲染该子段（含占位提示）。
    pub include_memory_rag: bool,
    /// 是否渲染记忆块中的"原文片段"子段（`## 原文片段`，utt 原文样例）。
    pub include_utt: bool,
    /// 是否渲染记忆块中的"桥接"子段（`## 桥接（上一会话尾部）`，脉络层）。
    pub include_bridge: bool,
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
            include_speaking_style: true,
            include_narrative: true,
            include_memory_rag: true,
            include_utt: true,
            include_bridge: true,
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

    /// 新增: utt 原文片段（Memory 块 [原文片段] 小节，已按预算裁剪渲染）
    ///
    /// 安全约束:
    /// - 仅角色类 persona（白名单内）由检索层填充；白名单外为 None（不注入）。
    /// - 原文内容不写日志。
    pub utt_context: Option<String>,
    /// 新增: 桥接内容（Memory 块 [桥接（上一会话尾部）] 小节，
    /// 已按预算从头部截断、保最近；None 表示不注入）
    ///
    /// 安全约束（与 utt_context 一致）:
    /// - 承载原文级信息，仅白名单内 persona 由桥接层填充。
    /// - 内容不写日志。
    pub bridge_context: Option<String>,
    /// 新增: 行为层路由决策（情境路由命中合并结果，`behavior/routing.rs`）。
    ///
    /// 字段约定:
    /// - `None` = 行为关闭 / 未命中 / 路由失败降级 → 不注入行为块，
    ///   prompt 不产生该段落。
    /// - `Some(decision)` = 主规则 + 合并 avoid/params，由
    ///   `render_behavior_block` 渲染 `## 行为规则` 小节。
    pub behavior_decision: Option<crate::behavior::MergedDecision>,
    /// 新增: 知识层 active 事实（事实卡片注入源，`fact/retriever.rs`）。
    ///
    /// 字段约定:
    /// - 空集 = 知识层关闭 / 判定器未命中 / 检索无结果 → 不注入知识块。
    /// - 非空 = 由 `render_knowledge_block` 渲染 `# 知识（知识层，按需）` 段落。
    /// - 只含 status=active 事实（版本链中仅当前生效参与注入）。
    pub knowledge_facts: Vec<PersonaFact>,
    /// 新增: 自动风格规则文本（表达层 A3 统计产出，`style/rule_gen.rs`）。
    ///
    /// 字段约定:
    /// - `None`/空 = 风格关闭或数据不足或无显著项 → 不注入（prompt 与 v1.6 语义等价）。
    /// - `Some(rule)` = 由 `build_style_layer` 渲染 `## 自动风格规则` 子段；
    ///   手工 `speaking_style` 存在时自动规则被覆盖（不注入，手工优先）。
    /// - 只含统计生成的风格描述，不含原文消息文本。
    pub style_rule_text: Option<String>,
}

// =========================================================
// 四层 System Prompt 模板
// =========================================================

/// 四层模板结构。
///
/// 占位符说明:
/// - `{capacity}` → `# 能力边界`（安全边界，非四层，前置保留）
/// - `{role_layer}` → `# 角色（行为层）`（角色身份/性格特征/已知事实/回复规范）
/// - `{behavior}` → 行为层槽位（情境-反应规则）
/// - `{style_layer}` → `# 说话风格（表达层）`（说话风格 + 对话示例）
/// - `{knowledge}` → `# 知识（知识层，按需）`（事实卡片）
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

/// 段落结构映射表（四层模板与历史 CRISPE 段的对应，供回归核对）。
///
/// 每项 `(段落标题, 内容来源, 对应块)`：
/// 记录四层模板的段落结构与内容来源。
pub const TEMPLATE_LAYER_MAP: &[(&str, &str, &str)] = &[
    (
        "# 能力边界",
        "安全边界（AI 助手核心能力 + 知识边界）",
        "Capacity（保留）",
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
    ("# 知识（知识层，按需）", "事实卡片", "知识层槽位"),
    (
        "# 记忆（脉络层）",
        "近期对话脉络 + 相关历史记忆 + 原文片段 + 桥接",
        "Memory + utt/桥接",
    ),
    ("# 当前时间", "时间 / 天气 / 上次活跃", "当前语境"),
];

// =========================================================
// 装配函数
// =========================================================

/// 装配完整的四层 System Prompt。
///
/// v2.0: 从 5-Block 格式重构为 CRISPE 七段式。
/// 精简为四层结构（段落映射见 `TEMPLATE_LAYER_MAP`），
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
/// - 行为层未命中/关闭（`behavior_decision=None`）→ 不产生段落；
///   知识层无事实 → 不产生段落。
pub fn assemble_prompt(context: &PromptContext, config: &PromptConfig) -> String {
    let mut blocks: Vec<String> = Vec::with_capacity(7);
    for block in [
        build_capacity(config, context),
        build_role_layer(context, config),
        // 行为层（None → 不产生段落）
        render_behavior_block(context, config).map_or(String::new(), |b| b.content),
        build_style_layer(context, config),
        // 知识层槽位（无事实 → 不产生段落）
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
/// 接入脉络层预算分配器——
/// 独立预算 ≤ 30%，超限裁剪顺序：原文块 → 桥接头部 → 相关记忆 → 脉络保最近。
///
/// 子段落结构:
/// 1. `## 近期对话脉络` — 最近 1-3 条 L1 摘要的叙事引导句（预算内保最近）
/// 2. `## 相关历史记忆` — RAG 检索结果（条件注入）
/// 3. `## 原文片段` — utt 话语块（白名单外为 None 不产生段落）
/// 4. `## 桥接（上一会话尾部）` — 上一会话尾部原文
///
/// 探针消融（B0/B1/F4/S_*）:
/// - `config.include_narrative` / `include_memory_rag` / `include_utt` /
///   `include_bridge` 逐子段控制渲染；对应子段关闭时连同占位提示一起跳过。
/// - 全部子段关闭（B0 无记忆注入）→ 整个记忆块不产生（返回空串，由装配器跳过）。
fn build_memory(context: &PromptContext, config: &PromptConfig) -> String {
    // B0 无记忆注入：所有记忆子段关闭 → 整块不产生（含头部与占位）。
    if !config.include_narrative
        && !config.include_memory_rag
        && !config.include_utt
        && !config.include_bridge
    {
        return String::new();
    }

    let mut parts: Vec<String> = Vec::with_capacity(2);

    parts.push(
        "# 记忆（脉络层）\n\
               以下是你的记忆系统检索到的相关信息。你对用户的了解完全来源于此。\
               请仔细阅读，在对话中自然地运用这些信息——但只在话题相关或用户主动提及时引用，\
               不强行插入无关记忆。"
            .to_string(),
    );

    // 脉络层预算分配（独立预算，默认 1000 tokens × 30% × 2 = 600 字符）
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
    if config.include_narrative {
        if alloc.summaries.is_empty() {
            parts.push("\n\n## 近期对话脉络\n（这是你与用户的首次对话）".to_string());
        } else {
            let narrative = build_cross_session_narrative(&alloc.summaries);
            let mut lines = vec!["\n\n## 近期对话脉络".to_string(), narrative];

            // 逐条列出近期摘要（截断到 120 字符）
            for (i, summary) in alloc.summaries.iter().enumerate() {
                let display = ramaria_core::text::truncate_chars(summary, 120);
                lines.push(format!("  {}. {}", i + 1, display));
            }

            parts.push(lines.join("\n"));
        }
    }

    // 相关历史记忆（RAG 结果；预算内句子边界截断）
    if config.include_memory_rag {
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
    }

    // 原文片段（utt 话语块；预算不足/白名单外为 None → 不产生段落）
    if config.include_utt
        && let Some(utt) = &alloc.utt
        && !utt.trim().is_empty()
    {
        parts.push(format!(
            "\n\n## 原文片段\n\
             以下是目标角色说过的原话（完整引用，不要逐字抄袭，仅学习其语气、用词与口癖）：\n\
             {utt}"
        ));
    }

    // 桥接（上一会话尾部；预算不足/开关关闭/白名单外为 None → 不产生段落）
    if config.include_bridge
        && let Some(bridge) = &alloc.bridge
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
// utt 原文片段渲染与预算裁剪
// =========================================================

/// 按预算渲染【原文片段】段落内容（整块保留/丢弃，超预算按相似度从低到高丢整块）。
///
/// 规则:
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
            let anchor = ramaria_core::text::truncate_chars_bare(s, 30);
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
/// 对应 Role + Insight + Experiment 三块；
/// 行为规则槽位（情境-反应规则）由 `render_behavior_block` 在装配时挂载。
fn build_role_layer(context: &PromptContext, config: &PromptConfig) -> String {
    let mut parts: Vec<String> = vec![build_role(context)];

    // 性格标签（按 layer 分组；无 traits 时省略）
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

/// 组装表达层块（`# 说话风格（表达层）`）：说话风格 + 自动风格规则 + 对话示例。
///
/// 对应 Personality + Statement 两块 + 自动风格规则子段（A3）。
///
/// 子段组合规则（手工覆盖优先，D-V17-004）:
/// - 手工 `speaking_style`（persona.config）存在 → 只注入手工风格
///   （自动风格规则被覆盖，不注入）。
/// - 手工不存在且 `style_rule_text`（自动规则）非空 → 注入 `## 自动风格规则`。
/// - 两子段皆缺省时整体不产生段落。
///
/// 探针消融（F3 / B0 / B1 / S_*）:
/// - `config.include_speaking_style=false` → 说话风格与自动风格规则均不渲染
///   （表达层关闭；对话示例仍由 `include_examples` 独立控制）。
fn build_style_layer(context: &PromptContext, config: &PromptConfig) -> String {
    let mut sub: Vec<String> = Vec::with_capacity(3);

    // 说话风格（persona.config 的 speaking_style，手工 E_rules 优先）
    let style = if config.include_speaking_style {
        build_personality(context)
    } else {
        String::new()
    };
    if !style.is_empty() {
        sub.push(style);
    } else if config.include_speaking_style {
        // 自动风格规则（A3 统计产出；手工覆盖时不注入）
        if let Some(rule) = context
            .style_rule_text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sub.push(format!("## 自动风格规则\n{rule}"));
        }
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
/// 并入表达层作为子段；无 speaking_style 时返回空（不产生段落）。
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
/// 从独立 Statement 块并入表达层；无示例时返回空（不产生段落）。
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
/// 从独立 Experiment 块并入角色层；
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

    // 记忆引用规则
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
mod tests;
