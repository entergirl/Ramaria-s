//! crates/ramaria-memory/src/prompt/builder/assemble.rs - 装配入口与体量度量
//!
//! 设计特点:
//! - 结构化装配：固定骨架 + 四层注入块，空块在拼接时跳过
//! - 单次渲染复用：协调预算按块裁剪后与普通装配共用同一部件序列
//! - 体量度量：字符数 + token 估算，供注入体量对照

use super::*;

// =========================================================
// 装配函数
// =========================================================

/// Prompt 部件类别：固定骨架或某注入通道。
///
/// 字段约定:
/// - `Fixed`: 系统提示骨架（角色层含底线/当前时间），不参与注入预算裁剪。
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
/// 角色层(Fixed，含底线) → 行为层(Inject Behavior) →
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
/// - 无 traits 时省略角色层中的性格段。
/// - 无 facts 时省略角色层中的关于对方段。
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
/// - 固定骨架（角色层含底线/当前时间）不参与裁剪，始终完整保留。
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
