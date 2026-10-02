//! crates/ramaria-memory/src/prompt/layers.rs - 四层注入结构与预算分配器
//!
//! 驱动环装配：四层融合为一次生成：
//!
//! | 层 | 内容 | 状态 |
//! |----|------|------|
//! | 行为层（Behavior） | 情境-反应规则 | 已填充（情境路由命中注入） |
//! | 知识层（Knowledge） | 事实卡片 | 已填充（判定器命中注入 active 事实） |
//! | 表达层（Style） | utt 原文块 + 风格特征规则 | 已注入（原文片段/示例/说话风格） |
//! | 脉络层（Memory） | L1 近期脉络 + 相关历史记忆 + 原文片段 + 桥接 | 已注入 |
//!
//! 优先级：行为 > 知识 > 表达 > 脉络。
//!
//! 预算规则:
//! - 行为控制块固定小比例、始终保底（默认 400 字符，`PromptConfig.behavior_block_max_chars` 可调）。
//! - 脉络独立预算（约 30% 上限，相对 system prompt 预留 token）。
//! - 超限裁剪顺序：原文块（按相似度从低到高丢整块）→ 桥接（从头部截断、保最近）
//!   → 相关历史记忆（句子边界截断）→ 脉络摘要（保最近，丢最旧）。
//!
//! 安全约束：
//! - 原文级内容（utt/桥接）在此模块仅做预算裁剪，不做内容改写；
//!   白名单过滤在检索/加载层完成（`ramaria-service`），本模块不感知 persona 类型。
//! - 本模块为纯函数，零 I/O，不写日志（原文内容不落日志由上层保证）。
//! - 行为块只消费路由决策（规则文本/参数/avoid），不接触事件原文与对话原文。

use crate::behavior::MergedDecision;
use crate::prompt::builder::{PromptConfig, PromptContext};
use crate::token_budget::truncate_at_boundary;
use ramaria_core::behavior::BehaviorParams;

// =========================================================
// 四层注入结构
// =========================================================

/// 四层注入的层类型（按注入流程顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    /// 行为层：情境-反应规则
    Behavior,
    /// 知识层：事实卡片
    Knowledge,
    /// 表达层：说话风格 + 对话示例 + 原文片段
    Style,
    /// 脉络层：近期脉络 + 相关记忆 + 桥接
    Memory,
}

impl LayerKind {
    /// 层的显示名称（与 prompt 段落标题对应）。
    pub fn as_str(self) -> &'static str {
        match self {
            LayerKind::Behavior => "行为",
            LayerKind::Knowledge => "知识",
            LayerKind::Style => "表达",
            LayerKind::Memory => "脉络",
        }
    }

    /// 注入优先级（数值越小越优先保留：行为 > 知识 > 表达 > 脉络）。
    pub fn priority(self) -> u8 {
        match self {
            LayerKind::Behavior => 1,
            LayerKind::Knowledge => 2,
            LayerKind::Style => 3,
            LayerKind::Memory => 4,
        }
    }
}

/// 统一注入块：一次生成中的所有注入内容单元。
///
/// 职责:
/// - 将各层注入内容统一为一个可枚举、可排序、可裁剪的单元。
/// - 为行为规则、知识卡片等提供统一的挂载点，避免重构 prompt 装配器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectionBlock {
    /// 所属层
    pub layer: LayerKind,
    /// 段落标题（如 `# 角色（行为层）`），渲染时为空则不产生段落
    pub title: &'static str,
    /// 注入内容（已渲染文本；空内容表示该块不参与装配）
    pub content: String,
}

impl InjectionBlock {
    /// 创建注入块。
    pub fn new(layer: LayerKind, title: &'static str, content: String) -> Self {
        Self {
            layer,
            title,
            content,
        }
    }

    /// 内容为空（或全空白）时该块不产生段落。
    pub fn is_empty(&self) -> bool {
        self.content.trim().is_empty()
    }
}

// =========================================================
// 脉络层预算配置
// =========================================================

/// 脉络层预算配置（独立预算约 30% 上限）。
///
/// 与 `token_budget::TokenBudgetConfig` 的关系：
/// - `system_prompt_reserve_tokens` 对齐该结构的同名默认值（1000）。
/// - 字符预算 = `system_prompt_reserve_tokens × memory_layer_ratio × 2`
///   （token→char 映射，中文为主 ≈ 2 字符/token，与 token_budget 的估算一致）。
#[derive(Debug, Clone, Copy)]
pub struct LayerBudgetConfig {
    /// System Prompt 预留 token 数（默认 1000，对齐 token_budget）
    pub system_prompt_reserve_tokens: usize,
    /// 脉络层预算占比上限（默认 0.30）
    pub memory_layer_ratio: f64,
}

impl Default for LayerBudgetConfig {
    fn default() -> Self {
        Self {
            system_prompt_reserve_tokens: 1000,
            memory_layer_ratio: 0.30,
        }
    }
}

impl LayerBudgetConfig {
    /// 计算脉络层字符预算。
    ///
    /// 公式: `reserve_tokens × ratio × 2`（向下取整，至少 1 字符）。
    pub fn budget_chars(&self) -> usize {
        let chars = self.system_prompt_reserve_tokens as f64 * self.memory_layer_ratio * 2.0;
        (chars as usize).max(1)
    }
}

// =========================================================
// 脉络层预算分配
// =========================================================

/// 脉络层预算分配结果（各注入源在预算内裁剪后的最终形态）。
///
/// 字段约定:
/// - `utt`: 原文片段（整块保留/丢弃，不做块内截断——原文整体引用）。
/// - `bridge`: 桥接内容（从头部截断、保最近）。
/// - `rag`: 相关历史记忆（句子边界截断、保最相关前部）。
/// - `summaries`: 近期脉络摘要（保最近，丢弃最旧；仍按时间降序）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryLayerBudget {
    /// 原文片段（None = 预算不足或输入为空，不注入）
    pub utt: Option<String>,
    /// 桥接内容（None = 预算不足或输入为空，不注入）
    pub bridge: Option<String>,
    /// 相关历史记忆（None = 预算不足或输入为空）
    pub rag: Option<String>,
    /// 近期脉络摘要（按时间降序，最近在前）
    pub summaries: Vec<String>,
}

/// 块间分隔符（与 `builder::render_utt_context` 的输出约定一致）。
const BLOCK_SEPARATOR: &str = "\n\n";

/// 脉络层预算分配器。
///
/// 保留优先级（从高到低）：
/// 1. 脉络摘要（保最近）
/// 2. 相关历史记忆（句子边界截断）
/// 3. 桥接（截头部、保最近）
/// 4. 原文块（整块保留/丢弃，低分先丢）
///
/// 即超限裁剪顺序：**原文块 → 桥接头部 → 相关记忆 → 脉络保最近**。
/// 预算耗尽后剩余源全部不注入（None / 空 Vec）。
///
/// 参数:
/// - `utt`: 已按相似度降序渲染的原文片段（多块以空行分隔；调用方保证块序）。
/// - `bridge`: 已按 `[bridge].max_chars` 预截断的桥接文本（最近在尾部）。
/// - `summaries`: 近期 L1 摘要（按时间降序，最近在前；调用方保证排序）。
/// - `rag`: 相关历史记忆文本（按相关度排序，最相关在前）。
/// - `budget_chars`: 脉络层字符预算（`LayerBudgetConfig::budget_chars`）。
///
/// 返回:
/// - 预算分配结果；全部输入为空时返回全空结果（不产生空段落）。
pub fn allocate_memory_layer_budget(
    utt: Option<&str>,
    bridge: Option<&str>,
    summaries: &[String],
    rag: Option<&str>,
    budget_chars: usize,
) -> MemoryLayerBudget {
    let mut out = MemoryLayerBudget::default();
    let mut used = 0usize;

    // ---- ① 脉络摘要（优先级最高：保最近，丢最旧） ----
    // summaries 按时间降序（最近在前），从头累加；预算不足时该条及更旧的丢弃。
    for s in summaries {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        let chars = t.chars().count();
        if used + chars > budget_chars {
            break;
        }
        out.summaries.push(t.to_string());
        used += chars;
    }

    // ---- ② 相关历史记忆（句子边界截断，保最相关前部） ----
    if let Some(rag_text) = rag.map(str::trim).filter(|s| !s.is_empty()) {
        let chars = rag_text.chars().count();
        if used + chars <= budget_chars {
            out.rag = Some(rag_text.to_string());
            used += chars;
        } else if used < budget_chars {
            let remaining = budget_chars - used;
            // `truncate_at_boundary` 在句子边界恰在窗口末尾时可能返回 max+1 字符
            // （含省略号），此处 clamp 保证预算不超支（防御）。
            let trimmed = truncate_at_boundary(rag_text, remaining);
            out.rag = Some(ramaria_core::text::truncate_chars_bare(&trimmed, remaining));
            used += out.rag.as_ref().map_or(0, |s| s.chars().count());
        }
    }

    // ---- ③ 桥接（截头部、保最近尾部） ----
    if let Some(bridge_text) = bridge.map(str::trim).filter(|s| !s.is_empty()) {
        let chars = bridge_text.chars().count();
        if used + chars <= budget_chars {
            out.bridge = Some(bridge_text.to_string());
            used += chars;
        } else if used < budget_chars {
            let remaining = budget_chars - used;
            out.bridge = Some(take_tail(bridge_text, remaining));
            used += out.bridge.as_ref().map_or(0, |s| s.chars().count());
        }
    }

    // ---- ④ 原文块（整块保留/丢弃，低分先丢；块序 = 相似度降序） ----
    if let Some(utt_text) = utt.map(str::trim).filter(|s| !s.is_empty()) {
        let chars = utt_text.chars().count();
        if used + chars <= budget_chars {
            out.utt = Some(utt_text.to_string());
        } else if used < budget_chars {
            let remaining = budget_chars - used;
            out.utt = Some(keep_high_score_blocks(utt_text, remaining));
        }
    }

    out
}

/// 从已渲染的原文片段中保留高分块（块按相似度降序排列，头部为最高分）。
///
/// 规则（与 `builder::render_utt_context` 一致）:
/// - 以空行（`\n\n`）切块，从头部（高分）整块累加。
/// - 首个超预算的块及其后全部丢弃（不做块内截断）。
/// - 块间分隔符（`\n\n`）计入预算，保证输出总长 ≤ `max_chars`。
///
/// 参数:
/// - `text`: 已渲染的原文片段（块间以空行分隔，降序）。
/// - `max_chars`: 剩余字符预算。
///
/// 返回:
/// - 预算内的块文本（块间空行分隔）；首块即超预算时返回空字符串。
fn keep_high_score_blocks(text: &str, max_chars: usize) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;

    for block in text.split(BLOCK_SEPARATOR) {
        let b = block.trim();
        if b.is_empty() {
            continue;
        }
        let chars = b.chars().count();
        // 分隔符计费：非首块需额外 2 字符（`\n\n`），保证输出总长 ≤ 预算
        let sep_cost = if kept.is_empty() { 0 } else { 2 };
        if used + sep_cost + chars > max_chars {
            break;
        }
        used += sep_cost + chars;
        kept.push(b);
    }

    kept.join(BLOCK_SEPARATOR)
}

/// 从文本尾部（最近内容）截取最多 `max_chars` 字符。
///
/// 规则:
/// - 内容不足预算时原样返回。
/// - 超预算时从尾部（最近）按整行累积保留，直至预算放不下下一行；
///   以 `…` 前缀提示截断，结果总长 ≤ `max_chars`。
/// - 单行即超出预算时，硬截取尾部 `max_chars - 1` 字符并加 `…` 前缀。
///
/// 参数:
/// - `text`: 桥接文本（最近内容在尾部）。
/// - `max_chars`: 字符预算。
///
/// 返回:
/// - 尾部截取文本（带 `…` 前缀）。
fn take_tail(text: &str, max_chars: usize) -> String {
    // 防御：预算为 0 时直接返回空（不产生 `…` 占位）
    if max_chars == 0 {
        return String::new();
    }
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }

    // 从尾部（最近）按整行累积，保留尽可能多的最近行
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for line in text.lines().rev() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let chars = l.chars().count();
        // 每行额外预留 1 字符（换行或省略号）
        if used + chars + 1 > max_chars {
            break;
        }
        kept.push(l);
        used += chars + 1;
    }

    if kept.is_empty() {
        // 单行也放不下 → 硬截取尾部（防御路径）
        let tail: String = text
            .chars()
            .skip(total.saturating_sub(max_chars.saturating_sub(1)))
            .collect();
        return format!("…{tail}");
    }

    kept.reverse();
    format!("…{}", kept.join("\n"))
}

// =========================================================
// 行为层槽位（情境-反应规则注入）
// =========================================================

/// 行为控制块默认字符预算（固定小比例、始终保底）。
const BEHAVIOR_BLOCK_DEFAULT_MAX_CHARS: usize = 400;
/// 行为块最小字符预算（低于标题+引导行长度时渲染残缺段落，防御性返回 None）。
const BEHAVIOR_BLOCK_MIN_CHARS: usize = 24;

/// 表达倾向程度词的中性区间下界（参数低于该值 → 输出低档程度词）。
const BEHAVIOR_TENDENCY_LOW: f64 = 0.4;
/// 表达倾向程度词的中性区间上界（参数高于该值 → 输出高档程度词）。
const BEHAVIOR_TENDENCY_HIGH: f64 = 0.6;

/// 行为层注入块渲染。
///
/// 消费 `PromptContext.behavior_decision`（情境路由合并结果，由 `ramaria-service`
/// 在生成用例中注入）：
/// - `None`（未命中 / 行为关闭 / 路由失败降级）→ 返回 `None`，不产生段落。
/// - `Some(decision)` → 渲染 `## 行为规则` 小节（reaction + 表达倾向程度词 + avoid），
///   段落置于 `# 角色（行为层）` 之后，语义上归属角色段。
///
/// 预算:
/// - 行为控制块固定小比例（默认 400 字符，`PromptConfig.behavior_block_max_chars`
///   可调），超限从头部截断保规则文本并加 `…`（规则文本为主、程度词为辅）。
///
/// 参数:
/// - `context`: 装配上下文（含行为路由决策）。
/// - `config`: 装配配置（行为块预算）。
///
/// 返回:
/// - 命中时返回行为层注入块；未命中/决策为空时返回 `None`。
pub fn render_behavior_block(
    context: &PromptContext,
    config: &PromptConfig,
) -> Option<InjectionBlock> {
    let decision = context.behavior_decision.as_ref()?;
    let max_chars = config
        .behavior_block_max_chars
        .unwrap_or(BEHAVIOR_BLOCK_DEFAULT_MAX_CHARS);

    let content = render_behavior_decision(decision, max_chars)?;
    Some(InjectionBlock::new(
        LayerKind::Behavior,
        "## 行为规则",
        content,
    ))
}

/// 将合并后的路由决策渲染为行为规则小节文本。
///
/// 输出格式（规则文本为主、程度词为辅）:
/// ```text
/// ## 行为规则
/// 聊到「加班」「累」等话题时：{reaction}
/// - 表达倾向：语气偏冷 · 更主动一点 · 说细一点
/// - 避免：深夜打扰、说教
/// ```
///
/// 降级:
/// - 候选规则（reaction 为空）→ 以"按表达倾向调整回应"占位（程度词与避免行照常注入）。
/// - 表达倾向：只渲染偏离中性（<0.4 或 >0.6）的维度；全部中性时不输出该行。
/// - avoid 为空 → 不输出避免行。
/// - 超预算 → 从头部截断保规则文本（reaction 优先），追加 `…`，总长 ≤ `max_chars`。
///
/// 参数:
/// - `decision`: 路由合并决策（主规则 + 合并 avoid/params）。
/// - `max_chars`: 行为块字符预算。
///
/// 返回:
/// - 渲染文本；截断后为空时返回 `None`（不产生空段落）。
fn render_behavior_decision(decision: &MergedDecision, max_chars: usize) -> Option<String> {
    // 预算不足最小可读长度（标题+引导行）：不渲染残缺段落（防御，与预算 0 一致）
    if max_chars < BEHAVIOR_BLOCK_MIN_CHARS {
        return None;
    }
    let keywords = &decision.primary_rule.situation.keywords;
    let kw_text = if keywords.is_empty() {
        "相关话题".to_string()
    } else {
        let quoted: Vec<String> = keywords.iter().map(|k| format!("「{k}」")).collect();
        quoted.join("、")
    };

    let reaction_line = match decision.primary_rule.reaction.as_deref() {
        Some(reaction) if !reaction.trim().is_empty() => reaction.trim().to_string(),
        _ => "（候选规则，按表达倾向调整回应）".to_string(),
    };

    let mut lines = vec![
        "## 行为规则".to_string(),
        format!("聊到{kw_text}等话题时：{reaction_line}"),
    ];
    if let Some(tendency_line) = format_behavior_tendency(&decision.merged_params) {
        lines.push(tendency_line);
    }
    if !decision.merged_avoid.is_empty() {
        lines.push(format!("- 避免：{}", decision.merged_avoid.join("、")));
    }

    let mut content = lines.join("\n");
    let total = content.chars().count();
    if total > max_chars {
        // 行为控制块固定小比例：超限保前部（规则文本优先），截断提示 `…`
        content = content
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>()
            + "…";
    }
    if content.trim().is_empty() {
        return None;
    }
    Some(content)
}

/// 将行为参数映射为表达倾向程度词行。
///
/// 映射（阈值 0.4 / 0.6；区间内视为中性、不输出）:
/// - 情感强度：<0.4 → 语气偏冷；>0.6 → 语气偏热
/// - 主动程度：<0.4 → 安静一点；>0.6 → 更主动一点
/// - 详细度：<0.4 → 说简一点；>0.6 → 说细一点
/// - 正式度：<0.4 → 更随意；>0.6 → 偏正式
///
/// 返回:
/// - 命中维度按上表顺序以 ` · ` 连接、加 `- 表达倾向：` 前缀；全部中性时返回 `None`。
fn format_behavior_tendency(params: &BehaviorParams) -> Option<String> {
    let mut terms: Vec<&str> = Vec::new();

    if params.emotional_intensity < BEHAVIOR_TENDENCY_LOW {
        terms.push("语气偏冷");
    } else if params.emotional_intensity > BEHAVIOR_TENDENCY_HIGH {
        terms.push("语气偏热");
    }
    if params.proactiveness < BEHAVIOR_TENDENCY_LOW {
        terms.push("安静一点");
    } else if params.proactiveness > BEHAVIOR_TENDENCY_HIGH {
        terms.push("更主动一点");
    }
    if params.detail_level < BEHAVIOR_TENDENCY_LOW {
        terms.push("说简一点");
    } else if params.detail_level > BEHAVIOR_TENDENCY_HIGH {
        terms.push("说细一点");
    }
    if params.formality < BEHAVIOR_TENDENCY_LOW {
        terms.push("更随意");
    } else if params.formality > BEHAVIOR_TENDENCY_HIGH {
        terms.push("偏正式");
    }

    if terms.is_empty() {
        return None;
    }
    Some(format!("- 表达倾向：{}", terms.join(" · ")))
}

/// 知识层注入块渲染（事实卡片）。
///
/// 消费 `PromptContext.knowledge_facts`（active 事实，由 `ramaria-service` 检索/判定后装配）。
/// 渲染 `# 知识（知识层，按需）` 段落；空集 → `None`（不产生空段落）。
///
/// 预算:
/// - 经 `config.knowledge_block_max_chars` 显式注入（与 `[knowledge].injection_budget_chars`
///   对齐）；`None` 时回退本层默认预算 [`MAX_KNOWLEDGE_CHARS`]（800，行为等价）。
pub fn render_knowledge_block(
    context: &PromptContext,
    config: &PromptConfig,
) -> Option<InjectionBlock> {
    if context.knowledge_facts.is_empty() {
        return None;
    }
    let budget = config
        .knowledge_block_max_chars
        .unwrap_or(MAX_KNOWLEDGE_CHARS);
    crate::fact::retriever::build_knowledge_injection(&context.knowledge_facts, budget)
}

/// 知识层注入预算（字符上限；对齐脉络层预算思路，固定小占比）。
const MAX_KNOWLEDGE_CHARS: usize = 800;

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
