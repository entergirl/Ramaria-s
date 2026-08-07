//! rust/crates/ramaria-memory/src/prompt/layers.rs - 四层注入结构与预算分配器（v1.4 M6）
//!
//! 对齐算法说明书 v3.1 §8（驱动环装配：四层融合为一次生成）：
//!
//! | 层 | 内容 | 状态 |
//! |----|------|------|
//! | 行为层（Behavior） | 情境-反应规则（v3.1 §4） | v1.5 填充，当前为空实现（槽位预留） |
//! | 知识层（Knowledge） | 事实卡片（v3.1 §5） | v1.6 填充，当前为空实现（槽位预留） |
//! | 表达层（Style） | utt 原文块 + 风格特征规则 | v1.4 已注入（原文片段/示例/说话风格） |
//! | 脉络层（Memory） | L1 近期脉络 + 相关历史记忆 + 原文片段 + 桥接 | v1.4 已注入 |
//!
//! 优先级（v3.1 §8.1）：行为 > 知识 > 表达 > 脉络。
//!
//! 预算规则（v3.1 §8.3 / 计划书 §2.5）：
//! - 脉络独立预算（约 30% 上限，相对 system prompt 预留 token）。
//! - 超限裁剪顺序：原文块（按相似度从低到高丢整块）→ 桥接（从头部截断、保最近）
//!   → 相关历史记忆（句子边界截断）→ 脉络摘要（保最近，丢最旧）。
//!
//! 安全约束：
//! - 原文级内容（utt/桥接）在此模块仅做预算裁剪，不做内容改写；
//!   白名单过滤在检索/加载层完成（`ramaria-app`），本模块不感知 persona 类型。
//! - 本模块为纯函数，零 I/O，不写日志（原文内容不落日志的红线由上层保证）。

use crate::prompt::builder::PromptContext;
use crate::token_budget::truncate_at_boundary;

// =========================================================
// 四层注入结构
// =========================================================

/// 四层注入的层类型（v3.1 §8.1 流程顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    /// 行为层：情境-反应规则（v1.5 填充，当前槽位为空）
    Behavior,
    /// 知识层：事实卡片（v1.6 填充，当前槽位为空）
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

    /// 注入优先级（数值越小越优先保留，v3.1 §8.1：行为 > 知识 > 表达 > 脉络）。
    pub fn priority(self) -> u8 {
        match self {
            LayerKind::Behavior => 1,
            LayerKind::Knowledge => 2,
            LayerKind::Style => 3,
            LayerKind::Memory => 4,
        }
    }
}

/// 统一注入块：一次生成中的所有注入内容单元（v1.4 M6 四层注入结构）。
///
/// 职责:
/// - 将各层注入内容统一为一个可枚举、可排序、可裁剪的单元。
/// - 为 v1.5（行为规则）、v1.6（知识卡片）提供统一的挂载点，
///   避免后续版本重构 prompt 装配器。
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

/// 脉络层预算配置（v3.1 §8.3：独立预算约 30% 上限）。
///
/// 与 `token_budget::TokenBudgetConfig` 的关系：
/// - `system_prompt_reserve_tokens` 对齐该结构的同名默认值（1000）。
/// - 字符预算 = `system_prompt_reserve_tokens × memory_layer_ratio × 2`
///   （token→char 映射，中文为主 ≈ 2 字符/token，与 token_budget 的估算一致）。
#[derive(Debug, Clone, Copy)]
pub struct LayerBudgetConfig {
    /// System Prompt 预留 token 数（默认 1000，对齐 token_budget）
    pub system_prompt_reserve_tokens: usize,
    /// 脉络层预算占比上限（默认 0.30，v3.1 §8.3「约 30% 上限」）
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

/// 脉络层预算分配器（v3.1 §8.3 / 计划书 §2.5）。
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
            out.rag = Some(fit_chars(trimmed, remaining));
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

/// 将文本裁剪到不超过 `max_chars` 字符（超出时取前部，防御性 clamp）。
///
/// 用途: 上游截断函数（如 `truncate_at_boundary`）在边界条件下可能返回
/// `max + 1` 字符，预算分配器统一在此收紧，保证脉络层总长 ≤ 预算。
fn fit_chars(text: String, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text
    } else {
        text.chars().take(max_chars).collect()
    }
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
// 行为/知识层槽位（v1.5 / v1.6 预留，T-V14-6-003）
// =========================================================

/// 行为层注入块渲染（槽位预留，v1.5 填充：情境-反应规则）。
///
/// 当前为空实现：恒返回 `None`，装配器跳过该块（不产生空段落）。
/// v1.5 实现行为层时在此处返回 `Some(InjectionBlock)`，
/// 渲染 `# 角色（行为层）` 下的行为规则子段。
pub fn render_behavior_block(_context: &PromptContext) -> Option<InjectionBlock> {
    None
}

/// 知识层注入块渲染（槽位预留，v1.6 填充：事实卡片）。
///
/// 当前为空实现：恒返回 `None`，装配器跳过该块（不产生空段落）。
/// v1.6 实现知识层时在此处返回 `Some(InjectionBlock)`，
/// 渲染 `# 知识（知识层，按需）` 段落。
pub fn render_knowledge_block(_context: &PromptContext) -> Option<InjectionBlock> {
    None
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- LayerKind / InjectionBlock ----

    #[test]
    fn layer_priority_follows_v31_section_81() {
        // v3.1 §8.1：行为 > 知识 > 表达 > 脉络
        assert!(LayerKind::Behavior.priority() < LayerKind::Knowledge.priority());
        assert!(LayerKind::Knowledge.priority() < LayerKind::Style.priority());
        assert!(LayerKind::Style.priority() < LayerKind::Memory.priority());
    }

    #[test]
    fn injection_block_empty_when_content_blank() {
        let block = InjectionBlock::new(LayerKind::Memory, "# 记忆", "   ".to_string());
        assert!(block.is_empty(), "全空白内容视为空块");
        let block2 = InjectionBlock::new(LayerKind::Memory, "# 记忆", String::new());
        assert!(block2.is_empty());
        let block3 = InjectionBlock::new(LayerKind::Memory, "# 记忆", "内容".to_string());
        assert!(!block3.is_empty());
    }

    // ---- LayerBudgetConfig ----

    #[test]
    fn budget_chars_default_is_600() {
        // 1000 tokens × 30% × 2 chars/token = 600 字符
        let cfg = LayerBudgetConfig::default();
        assert_eq!(cfg.budget_chars(), 600);
    }

    #[test]
    fn budget_chars_scales_with_config() {
        let cfg = LayerBudgetConfig {
            system_prompt_reserve_tokens: 2000,
            memory_layer_ratio: 0.25,
        };
        assert_eq!(cfg.budget_chars(), 1000);
    }

    #[test]
    fn budget_chars_at_least_one() {
        let cfg = LayerBudgetConfig {
            system_prompt_reserve_tokens: 0,
            memory_layer_ratio: 0.0,
        };
        assert_eq!(cfg.budget_chars(), 1, "预算至少 1 字符");
    }

    // ---- allocate_memory_layer_budget ----

    #[test]
    fn all_within_budget_kept_unchanged() {
        let out = allocate_memory_layer_budget(
            Some("块一内容\n\n块二内容"),
            Some("桥接内容"),
            &["摘要一".to_string(), "摘要二".to_string()],
            Some("相关记忆"),
            1000,
        );
        assert_eq!(out.utt.as_deref(), Some("块一内容\n\n块二内容"));
        assert_eq!(out.bridge.as_deref(), Some("桥接内容"));
        assert_eq!(out.summaries, vec!["摘要一", "摘要二"]);
        assert_eq!(out.rag.as_deref(), Some("相关记忆"));
    }

    #[test]
    fn empty_inputs_yield_empty_budget() {
        let out = allocate_memory_layer_budget(None, None, &[], None, 600);
        assert_eq!(out, MemoryLayerBudget::default());
    }

    #[test]
    fn trim_utt_low_score_blocks_first() {
        // 预算只够一块：高分块（头部）保留，低分块（尾部）整块丢弃
        // 预算 12：摘要(3) + 桥接(4) = 7，剩 5 只够原文第一块（5 字符）
        let out = allocate_memory_layer_budget(
            Some("高分块内容\n\n低分块内容"),
            Some("桥接内容"),
            &["摘要一".to_string()],
            None,
            12,
        );
        assert_eq!(out.utt.as_deref(), Some("高分块内容"), "高分块保留");
        assert_eq!(out.bridge.as_deref(), Some("桥接内容"), "桥接保留");
        assert_eq!(out.summaries, vec!["摘要一"]);
    }

    #[test]
    fn trim_bridge_keeps_recent_tail() {
        // 预算紧张：原文被完全丢弃，桥接截头部保尾部
        // 预算 11：摘要(3) + 桥接保留 8（…+第三行 7）→ 原文(4) 放不下整体丢弃
        let out = allocate_memory_layer_budget(
            Some("原文块"),
            Some("第一行桥接内容\n第二行桥接内容\n第三行桥接内容"),
            &["摘要一".to_string()],
            None,
            11,
        );
        assert_eq!(out.utt, None, "预算不足原文整体丢弃");
        let bridge = out.bridge.expect("桥接应保留尾部");
        assert!(bridge.contains("第三行桥接内容"), "桥接保最近: {bridge}");
        assert!(!bridge.contains("第一行桥接内容"), "桥接截头部: {bridge}");
        assert!(bridge.starts_with('…'), "截断带省略号前缀");
    }

    #[test]
    fn trim_summaries_keeps_recent_drops_oldest() {
        // 预算只够一条摘要：最近的（头部）保留，最旧的（尾部）丢弃
        let out = allocate_memory_layer_budget(
            None,
            None,
            &[
                "最近的摘要内容".to_string(),
                "较旧的摘要内容".to_string(),
                "最旧的摘要内容".to_string(),
            ],
            None,
            8,
        );
        assert_eq!(out.summaries, vec!["最近的摘要内容"], "保最近丢最旧");
    }

    #[test]
    fn trim_rag_at_sentence_boundary() {
        // 预算不足以容纳完整 RAG → 句子边界截断（保前部最相关）
        let out = allocate_memory_layer_budget(
            None,
            None,
            &[],
            Some("第一句相关记忆。第二句相关记忆。第三句相关记忆。"),
            10,
        );
        let rag = out.rag.expect("RAG 应截断保留");
        assert!(rag.starts_with("第一句相关记忆。"), "保前部最相关: {rag}");
        assert!(rag.ends_with('…'), "截断带省略号: {rag}");
    }

    #[test]
    fn zero_budget_yields_nothing() {
        let out = allocate_memory_layer_budget(
            Some("原文"),
            Some("桥接"),
            &["摘要".to_string()],
            Some("记忆"),
            0,
        );
        assert_eq!(out, MemoryLayerBudget::default(), "预算 0 全部不注入");
    }

    #[test]
    fn budget_exhausted_by_priority_order() {
        // 极小预算：只保脉络摘要，其余全部不注入（优先级验证）
        let out = allocate_memory_layer_budget(
            Some("原文内容"),
            Some("桥接内容"),
            &["摘要内容".to_string()],
            Some("记忆内容"),
            4,
        );
        assert_eq!(out.summaries, vec!["摘要内容"], "脉络优先级最高");
        assert_eq!(out.rag, None);
        assert_eq!(out.bridge, None);
        assert_eq!(out.utt, None);
    }

    #[test]
    fn blank_inputs_skipped_not_counted() {
        let out = allocate_memory_layer_budget(
            Some("   "),
            None,
            &["  ".to_string(), "有效摘要".to_string()],
            None,
            600,
        );
        assert_eq!(out.utt, None, "空白原文不注入");
        assert_eq!(out.summaries, vec!["有效摘要"], "空白摘要跳过");
    }

    // ---- keep_high_score_blocks ----

    #[test]
    fn keep_blocks_stops_at_first_over_budget() {
        let text = "第一块内容\n\n第二块内容\n\n第三块内容";
        let kept = keep_high_score_blocks(text, 6);
        assert_eq!(kept, "第一块内容", "首块超预算即停");
    }

    #[test]
    fn keep_blocks_charges_separator_to_budget() {
        // 分隔符计费：块各 3 字符，max=7 时块1(3)+分隔(2)+块2(3)=8 > 7 → 只保留块1
        let kept = keep_high_score_blocks("AAA\n\nBBB", 7);
        assert_eq!(kept, "AAA", "分隔符计入预算");
        assert!(kept.chars().count() <= 7, "输出总长 ≤ 预算");
        // max=8 时可容纳两块
        let kept2 = keep_high_score_blocks("AAA\n\nBBB", 8);
        assert_eq!(kept2, "AAA\n\nBBB");
    }

    #[test]
    fn keep_blocks_skips_blank_segments() {
        let text = "块一\n\n\n\n块二";
        let kept = keep_high_score_blocks(text, 100);
        assert_eq!(kept, "块一\n\n块二", "空段折叠");
    }

    // ---- take_tail ----

    #[test]
    fn take_tail_within_budget_unchanged() {
        assert_eq!(take_tail("短内容", 100), "短内容");
    }

    #[test]
    fn take_tail_trims_head_keeps_recent_lines() {
        let text = "第一行\n第二行\n第三行";
        let tail = take_tail(text, 6);
        assert!(tail.contains("第三行"), "保最近行: {tail}");
        assert!(!tail.contains("第一行"), "截头部: {tail}");
        assert!(tail.starts_with('…'), "截断前缀提示");
    }

    #[test]
    fn take_tail_single_line_truncated() {
        let tail = take_tail("没有换行的长文本内容", 5);
        assert!(tail.starts_with('…'));
        assert_eq!(tail.chars().count(), 5, "结果总长 ≤ 预算（含省略号）");
    }

    #[test]
    fn take_tail_zero_budget_returns_empty() {
        assert_eq!(take_tail("任何内容", 0), "", "预算 0 不产生省略号占位");
    }

    #[test]
    fn fit_chars_clamps_over_budget() {
        assert_eq!(fit_chars("短文本".to_string(), 100), "短文本");
        let clamped = fit_chars("超预算文本内容".to_string(), 4);
        assert_eq!(clamped.chars().count(), 4);
        assert_eq!(fit_chars(String::new(), 0), "");
    }

    #[test]
    fn rag_truncation_never_exceeds_budget() {
        // 句子边界恰在窗口末尾：truncate_at_boundary 可能返回 max+1，clamp 后不超支
        let out = allocate_memory_layer_budget(
            Some("原文内容"),
            Some("桥接内容"),
            &["摘要".to_string()],
            Some("第一句。第二句。第三句。"),
            8,
        );
        let rag = out.rag.expect("RAG 应保留");
        assert!(rag.chars().count() <= 5, "RAG 截断不超预算: {rag}");
        // 摘要(2) + RAG(≤5) ≤ 8；桥接/原文因预算耗尽不注入
        assert_eq!(out.summaries, vec!["摘要"]);
    }

    // ---- 槽位预留（T-V14-6-003） ----

    #[test]
    fn behavior_and_knowledge_slots_are_empty_for_now() {
        let ctx = PromptContext::default();
        assert!(render_behavior_block(&ctx).is_none(), "v1.5 前行为槽位为空");
        assert!(
            render_knowledge_block(&ctx).is_none(),
            "v1.6 前知识槽位为空"
        );
    }
}
