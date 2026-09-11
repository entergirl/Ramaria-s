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
//! - 结构化装配：`render_prompt_parts` 暴露固定骨架与四层注入块（`PromptPart`），
//!   `assemble_prompt` 为其薄封装；协调预算开启时经
//!   `assemble_prompt_coordinated` 按可配顺序保留高优先层。
//!
//! 依赖:
//! - `ramaria_core::types`: Persona, PersonaFact, PersonalityTrait, PersonaExample
//! - `ramaria_core::config`: InjectionSlot / InjectionBudgetConfig（协调预算）
//! - `ramaria_memory::rag`: RAG 上下文格式化（由上层传入）
//! - `prompt::layers`: 四层注入结构与预算分配器

use crate::prompt::layers::{
    LayerBudgetConfig, allocate_memory_layer_budget, render_behavior_block, render_knowledge_block,
};
use crate::retriever::UttHit;
use chrono::Local;
use ramaria_core::config::InjectionSlot;
use ramaria_core::types::{
    Persona, PersonaExample, PersonaFact, PersonalityTrait, ProfileField, TraitStatus,
};

// =========================================================
// 样板文案常量（引导句/占位/默认规则集中管理）
// =========================================================
//
// 约束（标签压缩/提示优化）:
// - 本文件所有引导句/占位/默认规则文本统一收口为下方常量，函数只引用不内联，
//   使"样板体积下降/每类引导句长度上限"可被单测直接断言（见 builder/tests.rs）。
// - 语义等价铁律：压缩只精简引导措辞与样板重复，不删减注入内容本身
//   （原文/事实/规则/风格/示例由上层数据源提供，本处仅骨架与引导）。
// - 段落标题（`# 角色（行为层）` 等）为层结构标识，已被模板映射/探针/集成测试
//   锁定，保持稳定；这里仅收纳随内容拼接的引导句与固定段落文本。

/// Capacity 首段引导：身份 + 记忆能力 + 安全边界（含原"知识边界"默认三条
/// 中与能力边界语义重叠的项，去重后不重复出现）。
const CAPACITY_INTRO: &str = "# 能力边界\n\
你是 Ramaria，能记住与用户的对话历史并在合适时机自然引用；\
知识截至训练时间，不编造、不确定就说明；\
不做医疗/法律/金融建议，不生成有害、违法或不道德内容。";

/// 默认知识边界子段（无 persona 自定义边界时追加；仅保留原默认三条中
/// "知识时效/不臆测"一条——能力、安全边界已由 [CAPACITY_INTRO] 覆盖）。
const KNOWLEDGE_BOUNDARY_DEFAULT: &str = "\n\n## 知识边界\n\
知识截至训练时间，不臆测未知的事实或日期。";

/// 角色层无 persona 时的默认身份段（`# 角色（行为层）` 头部）。
const ROLE_DEFAULT_TEXT: &str = "# 角色（行为层）\n\
你是 Ramaria，一个有记忆能力、善解人意的 AI 助手，\
能记住对话历史并在后续对话中自然引用。";

/// 记忆层首段引导（说明记忆来源与引用时机）。
const MEMORY_SECTION_INTRO: &str = "# 记忆（脉络层）\n\
以下记忆信息是你了解用户的依据，仅在话题相关或用户主动提及时自然引用，不强行插入。";

/// 近期对话脉络无摘要时的占位提示。
const NARRATIVE_PLACEHOLDER: &str = "（你们是首次对话）";

/// 相关历史记忆占位提示（无 RAG 命中时）。
const RAG_PLACEHOLDER: &str = "（暂无直接相关的历史记忆）";

/// 原文片段引导（utt 块；保留"勿逐字抄袭"边界）。
const UTT_LEAD: &str = "以下是角色原话，供学习语气、用词与口癖（勿逐字抄袭）：\n";

/// 桥接引导（上一会话尾部；保留"勿逐字引用/勿编造"边界）。
const BRIDGE_LEAD: &str = "上一段对话结尾原文，用于保持连贯（勿逐字引用，勿编造未提及内容）：\n";

/// 对话示例引导（Few-shot）。
const STATEMENT_LEAD: &str = "参考以下示例的风格与节奏：";

/// 无自定义规则时的最小化默认回复规则（两条合并原三条语义）。
const CORE_RULES_DEFAULT: &str = "\n### 核心规则\n\
- 用自然友好的语气回复，简洁不冗长。\n\
- 不确定就如实说明。";

/// 全局社交平台对话基调（无条件注入，优先级高于 persona 风格规则）。
///
/// 目的:
/// - 对话发生在社交平台即时聊天场景；模型缺乏强约束时会回退"附和 + 反问需求 +
///   解释总结"的助手腔，与 persona 真实社交语气偏离（实测 persona 真实句长均值
///   约 15 字，模型回复 60~73 字）。
/// - 该基调对全部 persona 无条件生效：人格风格规则只在其之上做个性化叠加，
///   不替代基调。放在 `### 核心规则` 之前，体现"先定体裁、再定个性"。
const SOCIAL_CHAT_TONE_RULES: &str = "\n### 社交对话基调（优先于任何其它规则）\n\
你是聊天对象，不是助手。像在社交软件上打字一样回复：\n\
1. 篇幅：每轮 1~3 句短句，整条一般不超过 30 字；不确定就只回一句。\n\
2. 口吻：口语化，可用语气词（啊/呀/哦/嗯/啦/嘛）与叠字，可省略主语，不用书面连接词。\n\
3. 禁止助手话术：不解释、不总结、不列点、不分步给建议；不反问「需要我帮你…吗」；不说「我理解你的感受」「希望这些对你有帮助」之类套话。\n\
4. 禁止旁白：不写括号动作/神态（如「（看到你的消息）」），不代替对方说话。\n\
5. 情绪优先：先接住对方的情绪或话题，再决定要不要多说一句。";

/// 记忆引用规则段（标题 + 四条压缩规则；语义与压缩前四条一致：
/// 时机/措辞/主动回溯 vs 被动响应/跨会话间隔策略）。
const MEMORY_CITATION_RULES: &str = "\n### 记忆引用规则\n\
1. **时机**：仅当与记忆明确相关才引用；打招呼或全新话题不硬插「上次我们聊到…」。\n\
2. **措辞**：用「记得你之前…」等自然表达，不用「根据系统记录…」等机械措辞。\n\
3. **主动回溯**：用户问「你还记得…吗」即主动邀请，可自由引用；否则仅在话题自然相关时引用。\n\
4. **跨会话**：间隔短（几小时内）可在回复中自然衔接；间隔长（几天）先寒暄、观察用户是否延续。";

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
    /// 知识层事实卡片字符预算上限（None = 默认 800 字符）。
    ///
    /// 与 `[knowledge].injection_budget_chars` 对齐：显式设置时真实生效，
    /// `None` 时回退 `layers` 知识块默认预算（与既有行为等价）。
    pub knowledge_block_max_chars: Option<usize>,
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
    /// 是否注入全局社交对话基调（`### 社交对话基调`）。
    ///
    /// 默认 `true`：该基调是对全部 persona 无条件生效的体裁约束，
    /// 关闭时行为回退到"仅有 persona 风格规则"的旧口径（供对照/回退）。
    pub include_social_tone: bool,
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
            knowledge_block_max_chars: None,
            include_speaking_style: true,
            include_narrative: true,
            include_memory_rag: true,
            include_utt: true,
            include_bridge: true,
            include_social_tone: true,
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
    /// - 为空时显示"（你们是首次对话）"占位提示。
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

/// Prompt 部件类别：固定骨架或某注入通道。
///
/// 字段约定:
/// - `Fixed`: 系统提示骨架（能力边界/角色层/当前时间），不参与注入预算裁剪。
/// - `Injection(slot)`: 可协调注入块（行为/知识/表达/脉络四层之一），超预算时
///   低优先通道可被整块丢弃（由协调预算机制处理）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptPartKind {
    /// 固定骨架（始终保留）
    Fixed,
    /// 注入通道块
    Injection(InjectionSlot),
}

/// System Prompt 的可组成单元（固定骨架或注入块）。
///
/// 职责:
/// - 承载结构化装配中间产物，使上层能在块粒度执行注入预算协调后重组。
/// - `content` 为已渲染文本；内容为空（或仅空白）时 join 自动跳过。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptPart {
    /// 部件类别（决定是否参与注入预算裁剪）
    pub kind: PromptPartKind,
    /// 渲染内容（trim 为空表示该块不产生段落）
    pub content: String,
}

/// 按四层模板顺序渲染全部 Prompt 部件。
///
/// 段落顺序（与 `assemble_prompt` 输出一致）:
/// 能力边界(Fixed) → 角色层(Fixed) → 行为层(Inject Behavior) →
/// 表达层(Inject Style) → 知识层(Inject Knowledge) → 脉络层(Inject Memory)
/// → 当前时间(Fixed)。
///
/// 说明:
/// - 空块（行为未命中/知识无事实/表达无内容）仍产生 `content=""` 的部件，
///   join 时自动跳过；`assemble_prompt` 与其输出等价（行为等价重构）。
pub fn render_prompt_parts(context: &PromptContext, config: &PromptConfig) -> Vec<PromptPart> {
    vec![
        PromptPart {
            kind: PromptPartKind::Fixed,
            content: build_capacity(config, context),
        },
        PromptPart {
            kind: PromptPartKind::Fixed,
            content: build_role_layer(context, config),
        },
        PromptPart {
            kind: PromptPartKind::Injection(InjectionSlot::Behavior),
            content: render_behavior_block(context, config).map_or(String::new(), |b| b.content),
        },
        PromptPart {
            kind: PromptPartKind::Injection(InjectionSlot::Style),
            content: build_style_layer(context, config),
        },
        PromptPart {
            kind: PromptPartKind::Injection(InjectionSlot::Knowledge),
            content: render_knowledge_block(context, config).map_or(String::new(), |b| b.content),
        },
        PromptPart {
            kind: PromptPartKind::Injection(InjectionSlot::Memory),
            content: build_memory(context, config),
        },
        PromptPart {
            kind: PromptPartKind::Fixed,
            content: build_context_block(context),
        },
    ]
}

/// 将 Prompt 部件序列拼接为完整 System Prompt（空块跳过，块间空行分隔）。
pub fn join_prompt_parts(parts: &[PromptPart]) -> String {
    parts
        .iter()
        .filter(|p| !p.content.trim().is_empty())
        .map(|p| p.content.as_str())
        .collect::<Vec<&str>>()
        .join("\n\n")
}

// =========================================================
// Prompt 体量度量（注入体量—回复长度结构对照）
// =========================================================

/// Prompt 文本体量（字符数 + token 估算）。
///
/// 职责:
/// - 纯函数统计一段 prompt 的字符数与 token 估算（复用
///   [`crate::token_budget::estimate_tokens`]），零 I/O。
/// - 供"注入体量—回复长度对照"使用：对同一请求度量其 system_prompt 与各注入块
///   体量，即可在真实回复长度数据上做结构对照（效果定论在 M8 高情感数据阶段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptVolume {
    /// UTF-8 字符数
    pub chars: usize,
    /// token 估算（中文 ≈ 2 chars/token、英文 ≈ 4 chars/token）
    pub tokens: usize,
}

/// 统计 prompt 文本体量。
///
/// 参数:
/// - `text`: 待统计文本（整条 system_prompt、注入块或骨架均可）。
///
/// 返回:
/// - 字符数与 token 估算。
pub fn measure_prompt_volume(text: &str) -> PromptVolume {
    PromptVolume {
        chars: text.chars().count(),
        tokens: crate::token_budget::estimate_tokens(text),
    }
}

/// 装配完整的四层 System Prompt。
///
/// v2.0: 从 5-Block 格式重构为 CRISPE 七段式。
/// 精简为四层结构（段落映射见 `TEMPLATE_LAYER_MAP`），
/// 空块自动跳过（行为/知识槽位当前为空，不产生空段落）。
/// 本函数为 `render_prompt_parts` + `join_prompt_parts` 的薄封装，
/// 行为与既有装配完全一致（行为等价重构）。
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
    join_prompt_parts(&render_prompt_parts(context, config))
}

/// 协调装配结果：裁剪后的 system_prompt + RAG 记忆上下文 + 统计。
///
/// 职责:
/// - 供 app 编排层在 `[injection_budget].enabled=true` 时直接消费，
///   替代"整条 system_prompt + memory_context 各占一摊"的旧式预算路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatedPrompt {
    /// 协调后的完整 system_prompt（固定骨架 + 保留的注入块）
    pub system_prompt: String,
    /// 协调后的 RAG 记忆上下文（`None` = 未注入 / 被整块丢弃）
    pub memory_context: Option<String>,
    /// 被整块丢弃的注入通道（低优先先被裁；RAG 整体丢弃时含 `Rag`）
    pub dropped: Vec<InjectionSlot>,
    /// 最终注入总 token（保留注入块 + RAG，≤ 协调预算）
    pub injected_tokens: usize,
    /// 是否触发过兜底句子截断（最高优先内容单块本身超总池）
    pub fallback_truncated: bool,
}

/// 协调装配：RAG 基座与四层注入在统一 token 池内按优先级分配。
///
/// 语义（详见 `token_budget::allocate_injection_budget`）:
/// - `budget` 为协调预算配置（core `[injection_budget]`）；`enabled=false` 时
///   本函数退化为普通装配（`system_prompt` 与 `assemble_prompt` 等价，
///   `memory_context` 原样保留——防御，正常调用方不进入）。
/// - 固定骨架（能力边界/角色层/当前时间）不参与裁剪，始终完整保留。
///
/// 参数:
/// - `context`: 装配上下文。
/// - `config`: 装配配置（各层通道内渲染预算仍生效）。
/// - `budget`: 注入协调预算配置。
/// - `rag`: RAG 摘要文本（`None` = 无 RAG 注入）。
pub fn assemble_prompt_coordinated(
    context: &PromptContext,
    config: &PromptConfig,
    budget: &ramaria_core::config::InjectionBudgetConfig,
    rag: Option<&str>,
) -> CoordinatedPrompt {
    let parts = render_prompt_parts(context, config);
    let layers: Vec<(InjectionSlot, String)> = parts
        .iter()
        .filter_map(|p| match p.kind {
            PromptPartKind::Injection(slot) => Some((slot, p.content.clone())),
            PromptPartKind::Fixed => None,
        })
        .collect();
    let alloc = crate::token_budget::allocate_injection_budget(&layers, rag, budget);

    let dropped_set: std::collections::HashSet<InjectionSlot> =
        alloc.dropped.iter().copied().collect();
    let filtered: Vec<PromptPart> = parts
        .into_iter()
        .filter(|p| match p.kind {
            PromptPartKind::Fixed => true,
            PromptPartKind::Injection(slot) => !dropped_set.contains(&slot),
        })
        .collect();
    let system_prompt = join_prompt_parts(&filtered);

    CoordinatedPrompt {
        system_prompt,
        memory_context: alloc.memory_context,
        dropped: alloc.dropped,
        injected_tokens: alloc.injected_tokens,
        fallback_truncated: alloc.fallback_truncated,
    }
}

// =========================================================
// Capacity 块: 能力边界
// =========================================================

/// 组装能力边界块：AI 助手核心能力 + 知识边界（安全红线，非四层，前置保留）。
fn build_capacity(config: &PromptConfig, context: &PromptContext) -> String {
    let mut parts = vec![CAPACITY_INTRO.to_string()];

    // 知识边界（可选）
    if config.include_knowledge_boundary {
        if let Some(ref boundary) = context.knowledge_boundary
            && !boundary.trim().is_empty()
        {
            parts.push(format!("\n\n## 知识边界\n{boundary}"));
        } else {
            parts.push(KNOWLEDGE_BOUNDARY_DEFAULT.to_string());
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

        // persona kind 描述（一行式角色类型说明）
        let kind_desc = match persona.kind {
            ramaria_core::types::PersonaKind::Rama => "你是 Ramaria 助手自身。",
            ramaria_core::types::PersonaKind::User => "以用户的视角思考与回复。",
            ramaria_core::types::PersonaKind::Char => "你扮演一个虚构角色。",
            ramaria_core::types::PersonaKind::Anim => "你扮演一个动画角色。",
            ramaria_core::types::PersonaKind::Oc => "你扮演一个原创角色（OC）。",
            ramaria_core::types::PersonaKind::Hist => "你扮演一个历史人物。",
            _ => "你扮演一个角色。",
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
        ROLE_DEFAULT_TEXT.to_string()
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

    parts.push(MEMORY_SECTION_INTRO.to_string());

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
            parts.push(format!("\n\n## 近期对话脉络\n{}", NARRATIVE_PLACEHOLDER));
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
                // 段落标题已表明内容性质；引用时机由记忆层首段引导统一约束
                parts.push(format!("\n\n## 相关历史记忆\n{rag}"));
            }
            _ => {
                parts.push(format!("\n\n## 相关历史记忆\n{RAG_PLACEHOLDER}"));
            }
        }
    }

    // 原文片段（utt 话语块；预算不足/白名单外为 None → 不产生段落）
    if config.include_utt
        && let Some(utt) = &alloc.utt
        && !utt.trim().is_empty()
    {
        parts.push(format!("\n\n## 原文片段\n{UTT_LEAD}{utt}"));
    }

    // 桥接（上一会话尾部；预算不足/开关关闭/白名单外为 None → 不产生段落）
    if config.include_bridge
        && let Some(bridge) = &alloc.bridge
        && !bridge.trim().is_empty()
    {
        parts.push(format!(
            "\n\n## 桥接（上一会话尾部）\n{}{}",
            BRIDGE_LEAD, bridge
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

    // 回复规范（社交基调 + 核心规则 + 记忆引用规则；无自定义规则时使用最小化默认）
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
    lines.push(format!("## 对话示例\n{STATEMENT_LEAD}"));

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

/// 组装回复规范子段（`## 回复规范`）：社交对话基调 + 回复规则 + 记忆引用规则。
///
/// v2.0: 合并原 SHARED_CHAT_STYLE_RULES（回复规则）和新增的记忆引用规则。
/// 从独立 Experiment 块并入角色层；
/// 记忆引用规则精确定义"主动回溯 vs 被动响应"的边界。
///
/// 子段顺序（决定"体裁约束 > 个性化 > 记忆引用边界"的效力层级）:
/// 1. `### 社交对话基调` — 全局社交平台体裁约束（`include_social_tone` 控制），
///    对全部 persona 无条件注入；人格规则只在其之上做个性化。
/// 2. `### 核心规则` — persona 风格规则（`chat_style_rules`），缺失时用最小化默认。
/// 3. `### 记忆引用规则` — 何时/如何引用记忆（随知识边界开关）。
///
/// 说明: 基调放在 persona 规则之前，使模型先定体裁、再定个性；
/// persona 已有个性化规则时基调同样在场（兜底型默认规则替代不了全局约束）。
fn build_experiment_section(context: &PromptContext, config: &PromptConfig) -> String {
    let mut parts: Vec<String> = vec!["## 回复规范".to_string()];

    // 全局社交对话基调（无条件注入，先于 persona 风格规则；体现"先定体裁、再定个性"）
    if config.include_social_tone {
        parts.push(SOCIAL_CHAT_TONE_RULES.to_string());
    }

    // 核心回复规则（persona 个性化规则；无则用最小化默认）
    if let Some(ref rules) = context.chat_style_rules
        && !rules.trim().is_empty()
    {
        parts.push(format!("\n### 核心规则\n{rules}"));
    } else {
        // 最小化默认规则
        parts.push(CORE_RULES_DEFAULT.to_string());
    }

    // 记忆引用规则（含主动回溯 vs 被动响应的边界）
    if config.include_knowledge_boundary {
        parts.push(MEMORY_CITATION_RULES.to_string());
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
