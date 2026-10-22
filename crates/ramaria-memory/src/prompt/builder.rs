//! crates/ramaria-memory/src/prompt/builder.rs - 四层 System Prompt 装配器
//!
//! 模板为四层注入结构。段落命名与结构映射表（`TEMPLATE_LAYER_MAP`）：
//!
//! | 段落 | 内容来源 | 说明 |
//! |------|----------|------|
//! | `# 角色（行为层）` | 角色身份 + 底线 + 性格 + 关于对方 + 说话方式/规则/格式 | Role + Insight + Experiment |
//! | `## 行为规则`（行为层） | 情境-反应规则（`render_behavior_block`，命中注入） | 行为槽位 |
//! | `# 说话风格（表达层）` | 说话风格 + 对话示例 | Personality + Statement |
//! | `# 知识（知识层，按需）` | 事实卡片（`render_knowledge_block`） | 知识槽位 |
//! | `# 记忆（脉络层）` | 近期对话脉络 + 相关记忆 + 原文片段 + 桥接 | Memory + utt/桥接 |
//! | `# 当前时间` | 时间/天气/上次活跃/近期状态 | 当前语境 |
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
mod proactive;
mod role;
mod style;

use context::build_context_block;
use memory::build_memory;
use role::build_role_layer;
use style::build_style_layer;

pub use memory::{build_cross_session_narrative, render_utt_context};
pub use proactive::ProactivePromptContext;

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

/// 底线子段（角色段内，身份信息之后）：诚实与安全约束（恒常注入）；
/// 自定义知识边界非空时追加为末行（`include_knowledge_boundary` 控制）。
const BOTTOM_LINE: &str = "## 底线\n\
不知道或没把握的内容直接说明，不编造；不虚构对方说过的话、没发生过的事。\n\
不做医疗、法律、金融方面的建议；不生成伤害他人或违反法律的内容。";

/// 角色层无 persona 时的默认身份段（`# 角色（行为层）` 头部：场景行 + 默认身份行）。
const ROLE_DEFAULT_TEXT: &str = "# 角色（行为层）\n\
场景：你在社交软件上和对方即时聊天（「对方」指正在和你聊天的人）。\n\
你是 Ramaria，你有自己的说话习惯。";

/// 记忆层首段引导（记忆内容性质 + 引用时机/表达方式/询问应答三合一）。
const MEMORY_SECTION_INTRO: &str = "# 记忆（脉络层）\n\
以下是你记得的事。只在话题相关时自然提起；打招呼或新话题时不提。说的时候直接说内容（如「记得你说过…」），不使用「根据记录」「系统记录」这类说法。\n\
对方问起「你还记得…吗」时，直接回答记得的内容。";

/// 近期对话脉络无摘要时的占位提示。
const NARRATIVE_PLACEHOLDER: &str = "（无历史对话）";

/// 相关历史记忆占位提示（无 RAG 命中时）。
const RAG_PLACEHOLDER: &str = "（无相关记忆）";

/// 原文片段引导（utt 块；与对话示例差异化为"连续发言"样例，保留防照搬边界）。
const UTT_LEAD: &str =
    "这也是你以前发过的消息（连着发的几条）。模仿它的分条与断句方式；不要照搬内容。\n";

/// 桥接引导（上一会话尾部；保留延续话题/不重复原文/不编造边界）。
const BRIDGE_LEAD: &str =
    "以下是上一段对话的结尾。接着这个话题继续聊；不要重复原文，不要编造没提过的内容。\n";

/// 对话示例引导（Few-shot；保留防照搬边界）。
const STATEMENT_LEAD: &str = "以下是你平时说话的片段。语气、长度和断句照这个来；不要照搬内容。";

/// 说话风格子段引导行（手工/自动风格互斥渲染，共用同一引导；防风格描述被复述）。
///
/// 优先级声明:
/// - 风格描述只承载语气与用词习惯；分行与标点属于产品格式契约（`## 回复格式`），
///   风格文本中若有"爱用逗号串句""用换行分段"这类书写描述，不得覆盖格式契约。
const STYLE_USAGE_LEAD: &str =
    "（以下是你自己的说话习惯。按这个表达；不要复述这段文字；分行与标点以「回复格式」为准。）";

/// 无自定义规则时的中性默认回复规则（单行；兼作陈述档默认）。
const CORE_RULES_DEFAULT: &str = "## 核心规则\n\
- 不确定或不知道的内容直接说明，不编造。";

/// 全局社交平台对话体裁约束（聊天档注入，先于 persona 风格规则生效）。
///
/// 目的:
/// - 对话发生在社交平台即时聊天场景；模型缺乏体裁约束时会回退"附和 + 反问需求 +
///   解释总结"的助手腔，与 persona 真实社交语气偏离（实测 persona 真实句长均值
///   约 15 字，模型回复 60~73 字）。
/// - 该约束对全部 persona 生效：人格风格规则只在其之上做个性化叠加，
///   不替代体裁。放在 `## 核心规则` 之前，体现"先定体裁、再定个性"。
const SOCIAL_CHAT_TONE_RULES: &str = "## 说话方式\n\
对方是熟人，不是客户。你在和熟人聊天，不是在完成服务任务。\n\
\n\
- 长度：一两句即够，不必凑字；一两个字的回复也是正常的。\n\
- 语体：口语化；用语气词（啊/呀/哦/嗯/啦/嘛）和叠字；省略主语；不用书面词。\n\
- 断句：两个短句不要用逗号连成一句；想接着说就分下一段。\n\
- 不解释、不总结、不列点、不给建议；不代替对方说话。\n\
- 说完就停：不追加计划或安排，不用「还需要别的吗」这类追问。\n\
- 对方说话时，先接住对方的最后半句或情绪，再说自己的内容。\n\
- 对方让你做些什么时，先问缘由（咋了/为什么），不要直接问具体怎么做。\n\
- 可以主动说自己这边的事或开玩笑，不必每次都顺着对方回应。";

/// 回复格式子段（分段契约 + 长度数值兜底，贴近生成位置；陈述档不注入）。
///
/// 设计特点:
/// - `||` 为产品级格式契约（桌面多气泡渲染 / 社交通道逐段发送的行为依赖）；
/// - 分段方式按社交平台口径写明：分多次说必须在段间用 `||`，换行不算分条
///   （换行是段内折行，不产生新气泡）；
/// - 长度为示例节奏锚定之外的数值兜底（全篇仅此一处），复杂问题说明不受此限。
const RESPONSE_ANCHOR: &str = "## 回复格式\n\
一次回复分多次说，必须在两次之间写「||」；回复里一律不出现换行，换行不算分条。\n\
如非复杂问题说明，整条不超过 30 字、3 段。";

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
/// - `include_knowledge_boundary`: 是否包含自定义知识边界（追加到底线子段）。默认 true。
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
    /// 是否包含知识边界（自定义知识边界追加到底线段；固定底线恒常注入）
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
    /// 是否注入全局说话方式（`## 说话方式`）与回复格式（`## 回复格式`）。
    ///
    /// 默认 `true`（聊天档）：体裁约束与格式契约对全部 persona 生效；
    /// 关闭（陈述档，可及性轨）时两者均不注入，`## 核心规则` 回退
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

    /// 知识边界描述（自定义时追加到底线子段末尾）
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
    /// 近期状态（最近记忆的效价方向，当前语境块追加行）。
    ///
    /// 字段约定:
    /// - `None` = 未开启 / 无样本 / 中性 → 不产生任何输出；
    /// - `Some(mood)` = 在 `# 当前时间` 块的「上次对话时间」行之后追加一行。
    pub recent_mood: Option<crate::chat::RecentMood>,
    /// 主动生成场景上下文（None = 非主动生成 → Prompt 不产生主动段）。
    ///
    /// 字段约定:
    /// - `Some(..)` = 本次为主动生成，注入「主动开口」指令段与「此刻情境」时段段；
    /// - `None` = 既有对话路径，输出与既有版本逐字节一致。
    pub proactive_context: Option<ProactivePromptContext>,
}

// =========================================================
// 四层 System Prompt 模板
// =========================================================

/// 四层模板结构。
///
/// 占位符说明:
/// - `{role_layer}` → `# 角色（行为层）`（角色身份/底线/性格/关于对方/说话方式）
/// - `{behavior}` → 行为层槽位（情境-反应规则）
/// - `{style_layer}` → `# 说话风格（表达层）`（说话风格 + 对话示例）
/// - `{knowledge}` → `# 知识（知识层，按需）`（事实卡片）
/// - `{memory}` → `# 记忆（脉络层）`（近期脉络/相关记忆/原文片段/桥接）
/// - `{context_block}` → `# 当前时间`（时间/天气/上次活跃/近期状态）
///
/// 装配语义: 空块自动跳过（不产生空段落），由 `assemble_prompt` 按序拼接。
pub const LAYER_TEMPLATE: &str = "\
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
        "# 角色（行为层）",
        "角色身份 + 底线 + 性格 + 关于对方 + 说话方式/规则/格式",
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
    (
        "# 当前时间",
        "时间 / 天气 / 上次活跃 / 近期状态",
        "当前语境",
    ),
];

mod assemble;
pub use assemble::*;

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
