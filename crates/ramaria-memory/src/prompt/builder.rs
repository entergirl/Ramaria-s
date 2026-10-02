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
//! - 各层块构建按职责拆入子模块（role/memory/style/experiment/context），
//!   本文件仅保留常量、配置/上下文类型与装配入口。
//!
//! 依赖:
//! - `ramaria_core::types`: Persona, PersonaFact, PersonalityTrait, PersonaExample
//! - `ramaria_core::config`: InjectionSlot / InjectionBudgetConfig（协调预算）
//! - `ramaria_memory::rag`: RAG 上下文格式化（由上层传入）
//! - `prompt::layers`: 四层注入结构与预算分配器

use crate::prompt::layers::{render_behavior_block, render_knowledge_block};
use ramaria_core::config::InjectionSlot;
use ramaria_core::types::{Persona, PersonaExample, PersonaFact, PersonalityTrait};

mod context;
mod experiment;
mod memory;
mod role;
mod style;

use context::build_context_block;
use memory::build_memory;
use role::{build_capacity, build_role_layer};
use style::build_style_layer;

pub use memory::{build_cross_session_narrative, render_utt_context};

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

/// Capacity 首段引导：记忆 / 未知 / 安全三条约束（身份声明由角色层承担，
/// 不再重复"你是 Ramaria"；原"知识边界"重叠项已去重）。
const CAPACITY_INTRO: &str = "# 能力边界\n\
- 记忆：你记得和对方过往的对话；相关时自然提起，不相关时不提。\n\
- 未知：不确定或不知道的内容直接说明，不编造。\n\
- 安全：不做医疗/法律/金融建议，不生成对他人有害、违法或人身攻击内容。";

/// 默认知识边界子段（无 persona 自定义边界时追加；仅保留原默认三条中
/// "知识时效/不臆测"一条——能力、安全边界已由 [CAPACITY_INTRO] 覆盖）。
const KNOWLEDGE_BOUNDARY_DEFAULT: &str = "\n\n## 知识边界\n\
知识截至训练时间，不臆测未知的事实或日期。";

/// 角色层无 persona 时的默认身份段（`# 角色（行为层）` 头部：场景行 + 默认身份行）。
const ROLE_DEFAULT_TEXT: &str = "# 角色（行为层）\n\
场景：你在社交软件上和对方即时聊天（「对方」指正在和你聊天的人）。\n\
你是 Ramaria，你有自己的说话习惯。";

/// 记忆层首段引导（说明记忆内容性质与引用时机）。
const MEMORY_SECTION_INTRO: &str = "# 记忆（脉络层）\n\
以下是你的记忆内容。要求：仅在话题相关时自然提及；无相关内容时不提及。";

/// 近期对话脉络无摘要时的占位提示。
const NARRATIVE_PLACEHOLDER: &str = "（无历史对话）";

/// 相关历史记忆占位提示（无 RAG 命中时）。
const RAG_PLACEHOLDER: &str = "（无相关记忆）";

/// 原文片段引导（utt 块；保留防照搬边界）。
const UTT_LEAD: &str = "以下是你过去的发言样本。用途：模仿其语气、用词与断句；禁止照搬内容。\n";

/// 桥接引导（上一会话尾部；保留不重复原文/不编造边界）。
const BRIDGE_LEAD: &str =
    "以下是上一段对话的结尾。要求：延续该话题继续对话；不重复原文；不编造未提及的内容。\n";

/// 对话示例引导（Few-shot；保留防照搬边界）。
const STATEMENT_LEAD: &str =
    "以下是你的说话示例。要求：模仿其长度、语气与断句方式；禁止照搬示例内容。";

/// 说话风格子段引导行（手工/自动风格互斥渲染，共用同一引导；防风格描述被复述）。
const STYLE_USAGE_LEAD: &str = "（以下是你的说话习惯。要求：按其表达，不复述该段文字。）";

/// 无自定义规则时的中性默认回复规则（单行；兼作陈述档默认）。
const CORE_RULES_DEFAULT: &str = "\n### 核心规则\n\
- 不确定或不知道的内容直接说明，不编造。";

/// 全局社交平台对话基调（聊天档注入，优先级高于 persona 风格规则）。
///
/// 目的:
/// - 对话发生在社交平台即时聊天场景；模型缺乏强约束时会回退"附和 + 反问需求 +
///   解释总结"的助手腔，与 persona 真实社交语气偏离（实测 persona 真实句长均值
///   约 15 字，模型回复 60~73 字）。
/// - 该基调对全部 persona 生效：人格风格规则只在其之上做个性化叠加，
///   不替代基调。放在 `### 核心规则` 之前，体现"先定体裁、再定个性"。
const SOCIAL_CHAT_TONE_RULES: &str = "\n### 社交对话基调（优先于任何其它规则）\n\
对方是熟人，不是服务对象。\n\
1. 长度：整条回复不超过 30 字；一般 1~2 句，不确定时只回 1 句。\n\
2. 语体：口语化；可使用语气词（啊/呀/哦/嗯/啦/嘛）和叠字；省略主语；不使用书面词与连接词。\n\
3. 禁止：解释、总结、列点、给出方案；括号内动作或神态描写；代替对方发言；结尾反问「需要我帮你…吗」。\n\
4. 顺序：先回应对方的情绪或最后半句话，再表达自己的内容。\n\
5. 取舍：无内容时不硬找话；说错话时直接改口。";

/// 说话锚点（关键约束贴近生成位置的复述，置于 `### 核心规则` 之后）。
///
/// 复述长度（≤30 字）、`||` 多气泡分隔与语体底线（不解释/不列点/不写括号动作），
/// 与 [SOCIAL_CHAT_TONE_RULES] 构成"角色层开头 + 回复规范末尾"各一次的约束；
/// 陈述档（`include_social_tone=false`）不注入。
const RESPONSE_ANCHOR: &str = "\n### 说话锚点\n\
回复要求：整条 ≤30 字；多条用「||」分隔；口语化；不解释、不列点、不写括号动作。";

/// 记忆引用规则段（标题 + 三条可判定约束：引用时机 / 表达方式 / 对方询问时）。
const MEMORY_CITATION_RULES: &str = "\n### 记忆引用规则\n\
1. 相关才引用：仅当记忆内容与当前话题相关时提及；打招呼或新话题不提及。\n\
2. 表达方式：直接说内容（如「记得你说过…」）；禁止「根据记录」「系统记录」等表述。\n\
3. 对方询问「你还记得…吗」时：直接回答记得的内容；对方未询问时按第 1 条执行。";

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
    /// `false` 时不渲染该子段（含"无历史对话"占位提示）。
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
    /// 是否注入全局社交对话基调（`### 社交对话基调`）与说话锚点（`### 说话锚点`）。
    ///
    /// 默认 `true`（聊天档）：基调与锚点对全部 persona 生效；
    /// 关闭（陈述档，可及性轨）时两者均不注入，`### 核心规则` 回退
    /// [CORE_RULES_DEFAULT] 中性默认（不注入 persona 风格规则）。
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
///   聊天档为空时使用中性默认规则；陈述档不注入。
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
    /// - 为空时显示"（无历史对话）"占位提示。
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
    /// 聊天档为空时使用中性默认规则；陈述档（`include_social_tone=false`）不注入。
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
        "安全边界（记忆 / 未知 / 安全三条约束 + 知识边界，无助手身份声明）",
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
/// - 无 chat_style_rules（或陈述档）时使用中性默认规则。
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
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
