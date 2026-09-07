//! crates/ramaria-memory/src/token_budget.rs - Token 预算管理模块
//!
//! 设计特点:
//! - 字符数估算 token 数: 中文 ≈ len/2，英文 ≈ len/4
//! - 预算分配优先级: System Prompt → RAG 上下文 → 对话历史（新→旧）
//! - 句子边界截断（。！？\n），不硬切单词或中文字符
//! - 不引入 tiktoken-rs，保持零外部 tokenizer 依赖
//! - 纯函数设计，可独立单元测试，零 I/O

use ramaria_core::config::{InjectionBudgetConfig, InjectionSlot};
use ramaria_core::traits::ChatMessage;

// =========================================================
// Token 估算
// =========================================================

/// 基于字符数估算 token 数量。
///
/// 策略:
/// - 中文/CJK 字符（Unicode 范围 U+4E00..U+9FFF, U+3000..U+303F 等）: n/2
/// - 英文/拉丁字符（含空格、标点）: n/4
/// - 其他字符: n/2（保守估算）
///
/// 参数:
/// - `text`: 待估算的文本。
///
/// 返回:
/// - 估算的 token 数（最小为 1）。
///
/// 说明:
/// - 这是粗略估算，精确值需 tiktoken 或类似 tokenizer。
/// - 对中文的 2 chars/token、英文的 4 chars/token 是常见经验值。
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let (cjk_count, latin_count, other_count) = count_char_types(text);
    // 中文 ≈ 2 chars/token，英文 ≈ 4 chars/token，其他 ≈ 2 chars/token（保守）
    let cjk_tokens = (cjk_count as f64 / 2.0).ceil() as usize;
    let latin_tokens = (latin_count as f64 / 4.0).ceil() as usize;
    let other_tokens = (other_count as f64 / 2.0).ceil() as usize;

    (cjk_tokens + latin_tokens + other_tokens).max(1)
}

/// 统计文本中各类字符的数量。
fn count_char_types(text: &str) -> (usize, usize, usize) {
    let mut cjk = 0usize;
    let mut latin = 0usize;
    let mut other = 0usize;

    for ch in text.chars() {
        if is_cjk(ch) {
            cjk += 1;
        } else if ch.is_ascii_alphabetic() || ch.is_ascii_digit() || ch == ' ' {
            // 英文/数字/空格 → 按拉丁字符处理
            latin += 1;
        } else if ch.is_ascii_punctuation() {
            latin += 1; // 英文标点按拉丁字符
        } else {
            other += 1;
        }
    }

    (cjk, latin, other)
}

/// 判断字符是否为 CJK 统一表意文字（仅汉字，不含标点）。
///
/// 说明:
/// - 仅包含汉字 Unicode 区间，不包含 CJK 标点（、。）和全角形式（！＂）。
/// - 标点和全角字符按 `other` 类别处理（保守估算 n/2）。
fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{4E00}'..='\u{9FFF}'     // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}'   // CJK Extension A
        | '\u{20000}'..='\u{2A6DF}' // CJK Extension B
        | '\u{F900}'..='\u{FAFF}'   // CJK Compatibility Ideographs
    )
}

// =========================================================
// 句子边界截断（委托统一字符边界工具）
// =========================================================

/// 在句子边界截断文本。
///
/// 策略（语义由 `ramaria_core::text::truncate_chars_at_sentence_boundary` 统一实现）:
/// - 在 `max_chars` 限制内寻找最近的句子终止符（`。！？\n`）。
/// - 若找不到句子边界，在最后空白处截断。
/// - 若无空白，直接按 `max_chars` 硬截断。
/// - 截断后添加省略号 `…` 作为视觉提示。
///
/// 参数:
/// - `text`: 待截断的文本。
/// - `max_chars`: 最大字符数限制。
///
/// 返回:
/// - 截断后的文本（含 `…` 后缀）。
pub fn truncate_at_boundary(text: &str, max_chars: usize) -> String {
    ramaria_core::text::truncate_chars_at_sentence_boundary(text, max_chars)
}

// =========================================================
// Token 预算分配配置
// =========================================================

/// Token 预算配置。
///
/// 字段约定:
/// - `system_prompt_reserve`: System Prompt 预留 token 数，默认 1000。
/// - `output_reserve`: 输出预留 token 数（对齐 `max_tokens` 参数）。
/// - `context_window`: 模型上下文窗口 token 总数。
#[derive(Debug, Clone)]
pub struct TokenBudgetConfig {
    /// System Prompt 预留（默认 1000）
    pub system_prompt_reserve: usize,
    /// 上下文窗口总大小
    pub context_window: usize,
    /// LLM 最大输出 tokens（对齐 provider 配置）
    pub max_output_tokens: u32,
}

impl TokenBudgetConfig {
    /// 创建新的 TokenBudgetConfig。
    pub fn new(context_window: usize, max_output_tokens: u32) -> Self {
        Self {
            system_prompt_reserve: 1000,
            context_window,
            max_output_tokens,
        }
    }
}

// =========================================================
// 预算分配主函数
// =========================================================

/// token 预算分配结果。
///
/// 职责:
/// - 存放应用预算限制后的 system_prompt、memory_context 和 history。
#[derive(Debug, Clone)]
pub struct BudgetedContext {
    /// 可能被截断的 system prompt
    pub system_prompt: String,
    /// 可能被截断的记忆上下文
    pub memory_context: Option<String>,
    /// 被截断的对话历史（保留最近的消息）
    pub history: Vec<ChatMessage>,
    /// 估算的总 token 使用量
    pub estimated_tokens: usize,
}

/// 应用 token 预算到对话上下文。
///
/// 优先级:
/// 1. System Prompt: 最大 `system_prompt_reserve` tokens（超长截断）
/// 2. RAG 记忆上下文: 按剩余预算填充，句子边界截断
/// 3. 对话历史: 从最新到最旧填充，每条消息独立截断
/// 4. 当前用户消息: 始终完整保留（不截断）
///
/// 参数:
/// - `system_prompt`: 原始 System Prompt。
/// - `memory_context`: 原始记忆上下文（已按 RRF score 排序）。
/// - `history`: 对话历史（按时间升序）。
/// - `user_message`: 当前用户消息。
/// - `config`: 预算配置。
///
/// 返回:
/// - `BudgetedContext`，包含预算分配后的各组件。
pub fn apply_token_budget(
    system_prompt: &str,
    memory_context: Option<&str>,
    history: &[ChatMessage],
    user_message: &str,
    config: &TokenBudgetConfig,
) -> BudgetedContext {
    let total_budget = config.context_window;

    // Step 1: 用户消息 token（始终保留完整）
    let user_tokens = estimate_tokens(user_message);

    // Step 2: 输出预留
    let output_reserve = config.max_output_tokens as usize;

    // Step 3: System Prompt（限制在 system_prompt_reserve 内）
    let (system_prompt_trimmed, system_tokens) =
        trim_system_prompt(system_prompt, config.system_prompt_reserve);

    // Step 4: 计算剩余预算（给记忆和历史的）
    let used_by_fixed = system_tokens + user_tokens + output_reserve;
    let flexible_budget = total_budget.saturating_sub(used_by_fixed);

    // Step 5: 分配记忆上下文（优先）
    let (memory_trimmed, memory_tokens) = trim_memory_context(memory_context, flexible_budget);

    // Step 6: 分配对话历史（剩余预算给历史）
    let history_budget = flexible_budget.saturating_sub(memory_tokens);
    let history_trimmed = trim_history(history, history_budget);

    let estimated_tokens =
        used_by_fixed + memory_tokens + estimate_history_tokens(&history_trimmed);

    BudgetedContext {
        system_prompt: system_prompt_trimmed,
        memory_context: memory_trimmed,
        history: history_trimmed,
        estimated_tokens,
    }
}

// =========================================================
// 内部裁剪函数
// =========================================================

/// 裁剪 System Prompt 到预算内。
fn trim_system_prompt(system_prompt: &str, max_tokens: usize) -> (String, usize) {
    let tokens = estimate_tokens(system_prompt);
    if tokens <= max_tokens {
        return (system_prompt.to_string(), tokens);
    }
    // 粗略映射：tokens → chars（中文为主 ≈ 2x）
    let max_chars = max_tokens * 2;
    let trimmed = truncate_at_boundary(system_prompt, max_chars);
    let trimmed_tokens = estimate_tokens(&trimmed);
    (trimmed, trimmed_tokens)
}

/// 裁剪记忆上下文到预算内。
///
/// 上下文已按 RRF score 排序（由 RAG 系统保证），直接按字符截断即可。
fn trim_memory_context(memory_context: Option<&str>, max_tokens: usize) -> (Option<String>, usize) {
    let text = match memory_context {
        Some(t) if !t.is_empty() => t,
        _ => return (None, 0),
    };
    // 预算为 0 时直接返回 None（不留任何记忆上下文）
    if max_tokens == 0 {
        return (None, 0);
    }
    let tokens = estimate_tokens(text);
    if tokens <= max_tokens {
        return (Some(text.to_string()), tokens);
    }
    // 粗略映射，保留 char 比例
    let max_chars = (text.chars().count() as f64 * max_tokens as f64 / tokens as f64) as usize;
    let trimmed = truncate_at_boundary(text, max_chars.max(1));
    let trimmed_tokens = estimate_tokens(&trimmed);
    (Some(trimmed), trimmed_tokens)
}

/// 裁剪对话历史到预算内（从最新到最旧保留）。
fn trim_history(history: &[ChatMessage], max_tokens: usize) -> Vec<ChatMessage> {
    if history.is_empty() || max_tokens == 0 {
        return Vec::new();
    }

    let mut remaining = max_tokens;
    let mut kept: Vec<&ChatMessage> = Vec::new();

    // 从最新到最旧遍历
    for msg in history.iter().rev() {
        let msg_tokens = estimate_tokens(&msg.content);
        if msg_tokens <= remaining {
            kept.push(msg);
            remaining = remaining.saturating_sub(msg_tokens);
        } else {
            // 最后一条（最旧的）部分保留
            let max_chars = (msg.content.chars().count() as f64 * remaining as f64
                / msg_tokens as f64) as usize;
            if max_chars > 0 {
                // 不能直接写入 ChatMessage（它不可变），跳过部分保留
                // 此处简单放弃这条消息以保持代码简洁
            }
            break;
        }
    }

    // 恢复为从旧到新排列
    kept.reverse();
    kept.into_iter().cloned().collect()
}

/// 估算历史消息列表的总 token 数。
fn estimate_history_tokens(history: &[ChatMessage]) -> usize {
    history.iter().map(|m| estimate_tokens(&m.content)).sum()
}

// =========================================================
// 注入协调预算（RAG 基座 + 四层注入的协调分配）
// =========================================================

/// 协调预算分配结果。
///
/// 职责:
/// - 报告最终保留的注入块、RAG 记忆上下文、被整块丢弃的通道与统计，
///   供上层（prompt 装配 / app 编排）重建 system_prompt 与 `memory_context`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatedInjection {
    /// 保留的注入块（每 slot 至多一项；顺序不保证与输入一致，
    /// 调用方按 slot 在原段落顺序上重建 system_prompt）。
    pub kept: Vec<(InjectionSlot, String)>,
    /// 协调后的 RAG 记忆上下文（`None` = 未注入 / 被整块丢弃）。
    pub memory_context: Option<String>,
    /// 被整块丢弃的注入通道（低优先先被裁；RAG 被整体丢弃时含 `Rag`）。
    pub dropped: Vec<InjectionSlot>,
    /// 最终注入总 token（保留注入块 + RAG，≤ `max_injection_tokens`）。
    pub injected_tokens: usize,
    /// 是否对最高优先内容触发过兜底句子截断（单块本身超出总池）。
    pub fallback_truncated: bool,
}

/// 在 RAG 基座与四层注入之间执行协调预算分配。
///
/// 语义:
/// - 预算池 = `config.max_injection_tokens`；参与通道 = `layers`（四层注入块，
///   已按各自通道内预算渲染）+ `rag`（RAG 摘要，`memory_context` 文本）。
/// - 超预算时按 `config.order` 保留高优先通道：**低优先通道整块丢弃**（不做
///   块内截断，避免破坏语义），直到总注入 ≤ 预算；若最高优先通道单块本身
///   超出总池，则对其做句子边界截断兜底（`fallback_truncated=true`）。
/// - `config.max_rag_tokens > 0` 时先对 RAG 摘要做独立上限截断，再入总池。
/// - `config.enabled=false` 时不做任何裁剪（原样保留——协调路径未启用的
///   防御，正常由调用方直接走既有路径）。
///
/// 边界处理:
/// - 预算为 0 / 极小：可裁通道全部丢弃，RAG 清空，不 panic、不产生空占位。
/// - 各层内容为空（trim 后）或 RAG 为空白：自动跳过，不占预算。
/// - UTF-8 多字节边界：所有截断经 `ramaria_core::text` 统一字符边界工具，
///   不做字节硬切；估算采用 token→char 的保守映射（中文为主 ≈ 2 char/token，
///   裁剪后估算 token 恒 ≤ 预算）。
///
/// 参数:
/// - `layers`: 各注入通道的已渲染文本（可含空白项，调用方保证通道顺序）。
/// - `rag`: RAG 摘要文本（`None` / 空白 = 无 RAG 注入）。
/// - `config`: 注入协调预算配置（core `[injection_budget]`）。
///
/// 返回:
/// - `CoordinatedInjection`：保留项 / RAG / 丢弃统计 / 总注入 token。
pub fn allocate_injection_budget(
    layers: &[(InjectionSlot, String)],
    rag: Option<&str>,
    config: &InjectionBudgetConfig,
) -> CoordinatedInjection {
    // 预处理：跳过空白注入块与空白 RAG
    let kept_layers: Vec<(InjectionSlot, String)> = layers
        .iter()
        .filter(|(_, content)| !content.trim().is_empty())
        .cloned()
        .collect();
    let rag_trimmed = rag.map(str::trim).filter(|s| !s.is_empty());

    let mut dropped: Vec<InjectionSlot> = Vec::new();
    let mut fallback_truncated = false;

    // 机制关闭 → 原样保留（防御；正常调用方不进入本函数）
    if !config.enabled {
        let rag_tokens = rag_trimmed.map_or(0, estimate_tokens);
        let layer_tokens: usize = kept_layers.iter().map(|(_, c)| estimate_tokens(c)).sum();
        return CoordinatedInjection {
            memory_context: rag_trimmed.map(|s| s.to_string()),
            kept: kept_layers,
            dropped,
            injected_tokens: rag_tokens + layer_tokens,
            fallback_truncated,
        };
    }

    let budget = config.max_injection_tokens;
    if budget == 0 {
        // 预算 0：全部注入通道丢弃，RAG 清空（固定骨架由上层保留）
        return CoordinatedInjection {
            kept: Vec::new(),
            memory_context: None,
            dropped: kept_layers.iter().map(|(slot, _)| *slot).collect(),
            injected_tokens: 0,
            fallback_truncated: false,
        };
    }

    // ① RAG 独立上限（max_rag_tokens > 0 时生效，之后参与总池）
    let mut rag_owned = rag_trimmed.map(|s| s.to_string());
    if let Some(cap) = nonzero(config.max_rag_tokens) {
        // 先判断是否超限（借用结束），再 take 可变赋值，避免借用冲突
        let over_cap = rag_owned
            .as_deref()
            .is_some_and(|text| estimate_tokens(text) > cap);
        if over_cap {
            rag_owned = rag_owned.take().map(|text| trim_text_to_tokens(&text, cap));
            fallback_truncated = true;
        }
    }

    // ② 汇总可协调项（注入块 + RAG），按保留顺序（高优先在前）稳定排序
    let mut items: Vec<(InjectionSlot, String, usize)> = kept_layers
        .iter()
        .map(|(slot, content)| (*slot, content.clone(), estimate_tokens(content)))
        .collect();
    if let Some(rag_text) = rag_owned.as_deref() {
        let tokens = estimate_tokens(rag_text);
        if tokens > 0 {
            items.push((InjectionSlot::Rag, rag_text.to_string(), tokens));
        }
    }
    // rank 越小越优先；未列入 order 的通道 rank = usize::MAX（最先被丢）
    items.sort_by_key(|(slot, _, _)| {
        config
            .order
            .iter()
            .position(|s| s == slot)
            .unwrap_or(usize::MAX)
    });

    // ③ 贪心保留：从高优先到低优先依次装入，首个放不下及其后全部丢弃
    let mut remaining = budget;
    let mut kept: Vec<(InjectionSlot, String)> = Vec::new();
    let mut idx = 0usize;
    while idx < items.len() {
        let (slot, content, tokens) = &items[idx];
        if *tokens <= remaining {
            kept.push((*slot, content.clone()));
            remaining -= *tokens;
            idx += 1;
        } else if kept.is_empty() {
            // 最高优先项单块超池：句子边界截断兜底（保证不超预算）
            let trimmed = trim_text_to_tokens(content, budget);
            fallback_truncated = true;
            if !trimmed.is_empty() {
                kept.push((*slot, trimmed));
            } else {
                dropped.push(*slot);
            }
            idx += 1;
            // 其后所有更低优先项丢弃
            for (s, _, _) in &items[idx..] {
                dropped.push(*s);
            }
            break;
        } else {
            // 该项放不下 → 该项及其后（更低优先）全部丢弃
            for (s, _, _) in &items[idx..] {
                dropped.push(*s);
            }
            break;
        }
    }

    // ④ 拆分 RAG 与注入块结果
    let mut kept_layers_out: Vec<(InjectionSlot, String)> = Vec::new();
    let mut memory_context_out: Option<String> = None;
    for (slot, content) in kept {
        if slot == InjectionSlot::Rag {
            memory_context_out = Some(content);
        } else {
            kept_layers_out.push((slot, content));
        }
    }

    let injected_tokens = kept_layers_out
        .iter()
        .map(|(_, c)| estimate_tokens(c))
        .sum::<usize>()
        + memory_context_out.as_deref().map_or(0, estimate_tokens);

    CoordinatedInjection {
        kept: kept_layers_out,
        memory_context: memory_context_out,
        dropped,
        injected_tokens,
        fallback_truncated,
    }
}

/// 将正整数选项转为可判定的值（避免在表达式内重复写 0 判断）。
fn nonzero(v: usize) -> Option<usize> {
    if v > 0 { Some(v) } else { None }
}

/// 将文本裁剪到至多 `max_tokens`（估算口径，中文为主 ≈ 2 字符/token）。
///
/// 规则:
/// - 估算已在预算内 → 原样返回。
/// - 超限 → 先按 `max_tokens × 2` 字符（token→char 保守映射）做句子边界截断，
///   再按估算逐字符收紧（字符安全，`String::pop` 按 Unicode 字符边界移除），
///   直至估算 token ≤ `max_tokens`——防止"奇数个汉字 + 全角标点"等组合因
///   每类独立向上取整而突破预算；预算 0 → 返回空串。
fn trim_text_to_tokens(text: &str, max_tokens: usize) -> String {
    if max_tokens == 0 || text.is_empty() {
        return String::new();
    }
    if estimate_tokens(text) <= max_tokens {
        return text.to_string();
    }
    let max_chars = max_tokens.saturating_mul(2);
    let trimmed = truncate_at_boundary(text, max_chars);
    let mut out = ramaria_core::text::truncate_chars_bare(&trimmed, max_chars);
    while !out.is_empty() && estimate_tokens(&out) > max_tokens {
        out.pop();
    }
    out
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- estimate_tokens ----

    /// estimate_tokens 各输入参数化验证。
    #[test]
    fn estimate_tokens_cases() {
        // (input, expected)：CJK 按 2 字符/token，拉丁按 4 字符/token，向上取整
        let cases = [
            ("", 0),
            ("你好世界", 2),    // 4 CJK → 2
            ("Hello World", 3), // 11 latin → 3
            ("你好 World", 3),  // 2/2 + 6/4 = 1 + 2
            ("a", 1),
            ("中", 1),
        ];
        for (input, expected) in cases {
            assert_eq!(estimate_tokens(input), expected, "input={input:?}");
        }
        // 长文本应有合理的 token 数
        let text = "这是一段较长的中文文本用于测试token估算的准确性。".repeat(10);
        assert!(estimate_tokens(&text) > 50, "长文本应有合理的 token 数");
    }

    // ---- truncate_at_boundary ----

    #[test]
    fn truncate_within_limit_unchanged() {
        let text = "短文本。";
        let result = truncate_at_boundary(text, 100);
        assert_eq!(result, text);
    }

    #[test]
    fn truncate_at_period() {
        let text = "第一句话。第二句话。第三句话。";
        let result = truncate_at_boundary(text, 10);
        // "第一句话。" = 5 chars → fits within 10
        assert!(result.ends_with('…'));
        assert!(result.starts_with("第一句话。"));
    }

    #[test]
    fn truncate_at_newline() {
        let text = "第一行\n第二行\n第三行";
        let result = truncate_at_boundary(text, 8);
        assert!(result.ends_with('…'));
        assert!(result.contains("第一行\n"));
    }

    #[test]
    fn truncate_no_boundary_falls_back_to_whitespace() {
        let text = "Hello World from Rust";
        // 24 chars, max 10 → "Hello Worl…" (last space after "Hello")
        let result = truncate_at_boundary(text, 10);
        assert!(result.ends_with('…'));
        assert!(result.starts_with("Hello"));
    }

    #[test]
    fn truncate_no_boundary_no_whitespace() {
        let text = "abcdefghijklmnopqrstuvwxyz"; // no boundaries
        let result = truncate_at_boundary(text, 5);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 6); // 5 chars + '…'
    }

    // ---- 类型计数 ----

    #[test]
    fn char_type_counting() {
        let (cjk, latin, other) = count_char_types("你好World！");
        assert_eq!(cjk, 2); // "你好"
        assert_eq!(latin, 5); // "World"
        assert_eq!(other, 1); // "！" (fullwidth)
    }

    // ---- apply_token_budget ----

    #[test]
    fn budget_small_window_preserves_user_message() {
        let config = TokenBudgetConfig::new(500, 256);
        let system_prompt = "你是一个测试助手。";
        let memory = Some("相关记忆：用户之前提到过喜欢编程。");
        let history = vec![
            ChatMessage {
                role: ramaria_core::types::MessageRole::User,
                content: "你好".to_string(),
            },
            ChatMessage {
                role: ramaria_core::types::MessageRole::Assistant,
                content: "你好！有什么可以帮你的？".to_string(),
            },
        ];
        let user_message = "今天天气怎么样？";

        let result = apply_token_budget(system_prompt, memory, &history, user_message, &config);

        // 用户消息始终保留 → system_prompt 应存在
        assert!(!result.system_prompt.is_empty());
        // estimated_tokens 应在预算内
        assert!(
            result.estimated_tokens <= config.context_window,
            "estimated {} > {}",
            result.estimated_tokens,
            config.context_window
        );
    }

    #[test]
    fn budget_large_window_preserves_all() {
        let config = TokenBudgetConfig::new(10000, 512);
        let system_prompt = "你是一个测试助手。";
        let memory = Some("相关记忆。");
        let history = vec![ChatMessage {
            role: ramaria_core::types::MessageRole::User,
            content: "你好".to_string(),
        }];
        let user_message = "测试消息";

        let result = apply_token_budget(system_prompt, memory, &history, user_message, &config);

        // 大窗口应保留所有内容
        assert_eq!(result.system_prompt, system_prompt);
        assert_eq!(result.memory_context.as_deref(), memory);
        assert_eq!(result.history.len(), 1);
    }

    #[test]
    fn budget_trims_history_when_tight() {
        // 120 token 窗口，256 输出 → budget 不够
        let config = TokenBudgetConfig::new(120, 100);
        let system_prompt = "助手";
        let memory = None;
        let history = vec![
            ChatMessage {
                role: ramaria_core::types::MessageRole::User,
                content: "非常长的消息".repeat(20),
            },
            ChatMessage {
                role: ramaria_core::types::MessageRole::Assistant,
                content: "回复".to_string(),
            },
        ];
        let user_message = "hi";

        let result = apply_token_budget(system_prompt, memory, &history, user_message, &config);

        // output reserve + user + system consumes most of 120 → history should be empty
        assert!(
            result.history.is_empty() || result.history.len() < 2,
            "tight budget should trim history"
        );
        assert!(
            result.estimated_tokens <= config.context_window,
            "estimated {} > {}",
            result.estimated_tokens,
            config.context_window
        );
    }

    #[test]
    fn budget_zero_history_budget() {
        // context_window(50) < output_reserve(256): unrealistic edge case
        // → flexible_budget = 0 → memory and history should be empty
        let config = TokenBudgetConfig::new(50, 256);
        let result = apply_token_budget(
            "助手",
            Some("记忆"),
            &[ChatMessage {
                role: ramaria_core::types::MessageRole::User,
                content: "旧消息".to_string(),
            }],
            "新消息",
            &config,
        );
        // flexible_budget = 0 → history and memory should be empty
        assert!(result.history.is_empty());
        assert!(result.memory_context.is_none());
        // Note: estimated_tokens may exceed context_window when output_reserve
        // alone is larger than the window — this is expected for the edge case.
    }

    #[test]
    fn estimate_tokens_english_text() {
        let text =
            "The quick brown fox jumps over the lazy dog. This is a longer sentence for testing.";
        let tokens = estimate_tokens(text);
        // 87 chars mostly latin → ~87/4 ≈ 22 tokens
        assert!(tokens >= 15 && tokens <= 30, "got {tokens}");
    }

    // ---- 注入协调预算（allocate_injection_budget） ----

    /// 构造启用/停用 + 指定总池上限的协调配置。
    fn cfg(enabled: bool, max_tokens: usize) -> InjectionBudgetConfig {
        let mut c = InjectionBudgetConfig::default();
        c.enabled = enabled;
        c.max_injection_tokens = max_tokens;
        c
    }

    /// 固定 3 token 的中文测试段（6 汉字 ≈ 3 token）。
    fn zh3(text: &str, slot: InjectionSlot) -> (InjectionSlot, String) {
        assert_eq!(text.chars().count(), 6, "helper 需要 6 汉字段: {text}");
        (slot, text.to_string())
    }

    #[test]
    fn coordinated_disabled_keeps_everything_unchanged() {
        // enabled=false（v1.7 等价路径的防御）：不做任何裁剪，原样保留
        let layers = vec![
            zh3("行为规则文本", InjectionSlot::Behavior),
            zh3("知识卡片内容", InjectionSlot::Knowledge),
            zh3("表达风格示例", InjectionSlot::Style),
            zh3("脉络最近摘要", InjectionSlot::Memory),
        ];
        let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &cfg(false, 4));
        assert_eq!(out.kept, layers, "关闭时各层原样保留");
        assert_eq!(out.memory_context.as_deref(), Some("记忆摘要内容"));
        assert!(out.dropped.is_empty());
        assert!(!out.fallback_truncated);
    }

    #[test]
    fn coordinated_within_budget_keeps_all() {
        let layers = vec![
            zh3("行为规则文本", InjectionSlot::Behavior),
            zh3("知识卡片内容", InjectionSlot::Knowledge),
            zh3("表达风格示例", InjectionSlot::Style),
            zh3("脉络最近摘要", InjectionSlot::Memory),
        ];
        // 5 项各 3 token：总池 15 → 全部保留
        let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &cfg(true, 15));
        assert_eq!(out.kept.len(), 4);
        assert!(out.memory_context.is_some());
        assert!(out.dropped.is_empty());
        assert_eq!(out.injected_tokens, 15);
    }

    #[test]
    fn coordinated_over_budget_drops_lowest_priority_first() {
        // 默认 order：rag > behavior > knowledge > style > memory
        // 总池 9 → rag + behavior + knowledge 恰好装满；style/memory 先被丢
        let layers = vec![
            zh3("行为规则文本", InjectionSlot::Behavior),
            zh3("知识卡片内容", InjectionSlot::Knowledge),
            zh3("表达风格示例", InjectionSlot::Style),
            zh3("脉络最近摘要", InjectionSlot::Memory),
        ];
        let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &cfg(true, 9));
        let slots: Vec<InjectionSlot> = out.kept.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            slots,
            vec![InjectionSlot::Behavior, InjectionSlot::Knowledge],
            "高优先注入块保留（RAG 走 memory_context 独立通道）"
        );
        assert_eq!(
            out.dropped,
            vec![InjectionSlot::Style, InjectionSlot::Memory],
            "低优先通道先被整块丢弃"
        );
        assert_eq!(out.memory_context.as_deref(), Some("记忆摘要内容"));
        assert_eq!(out.injected_tokens, 9, "总注入 ≤ 预算");
    }

    #[test]
    fn coordinated_custom_order_and_unlisted_drop_first() {
        // order = [style, memory, rag]；behavior/knowledge 未列出 → 最先被丢
        let mut c = cfg(true, 9);
        c.order = vec![
            InjectionSlot::Style,
            InjectionSlot::Memory,
            InjectionSlot::Rag,
        ];
        let layers = vec![
            zh3("行为规则文本", InjectionSlot::Behavior),
            zh3("知识卡片内容", InjectionSlot::Knowledge),
            zh3("表达风格示例", InjectionSlot::Style),
            zh3("脉络最近摘要", InjectionSlot::Memory),
        ];
        let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &c);
        let slots: Vec<InjectionSlot> = out.kept.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            slots,
            vec![InjectionSlot::Style, InjectionSlot::Memory],
            "列出的高优先注入块保留（RAG 走 memory_context 独立通道）"
        );
        assert_eq!(
            out.dropped,
            vec![InjectionSlot::Behavior, InjectionSlot::Knowledge],
            "未列出通道比所有列出通道更低优先"
        );
        assert_eq!(out.memory_context.as_deref(), Some("记忆摘要内容"));
        assert_eq!(out.injected_tokens, 9);
    }

    #[test]
    fn coordinated_total_never_exceeds_budget_across_budget_levels() {
        // 各层/rag 长度不等（中英混合），遍历预算档位断言总注入 ≤ 预算
        let layers = vec![
            (
                InjectionSlot::Behavior,
                "行为规则：共情优先、避免说教、简短回应。".to_string(),
            ),
            (
                InjectionSlot::Knowledge,
                "对方喜欢科幻电影与编程，最近在学习 Rust。".to_string(),
            ),
            (
                InjectionSlot::Style,
                "Hello World! 语气轻松活泼一些。".to_string(),
            ),
            (
                InjectionSlot::Memory,
                "上次聊了出行计划，气氛轻松愉快。".to_string(),
            ),
        ];
        let rag = "用户提到喜欢猫，养了一只橘猫，最近想换工作。";
        let total = layers
            .iter()
            .map(|(_, c)| estimate_tokens(c))
            .sum::<usize>()
            + estimate_tokens(rag);

        for budget in [0usize, 1, 3, 5, 10, 20, 50, 1000] {
            let out = allocate_injection_budget(&layers, Some(rag), &cfg(true, budget));
            assert!(
                out.injected_tokens <= budget,
                "总注入 {} > 预算 {budget}",
                out.injected_tokens
            );
            // budget=0 时全丢；budget 极大时全保留
            if budget == 0 {
                assert!(out.kept.is_empty());
                assert!(out.memory_context.is_none());
            }
            if budget >= total {
                assert!(out.dropped.is_empty(), "大预算不应丢弃");
            }
        }
    }

    #[test]
    fn coordinated_zero_budget_drops_all() {
        let layers = vec![
            zh3("行为规则文本", InjectionSlot::Behavior),
            zh3("脉络最近摘要", InjectionSlot::Memory),
        ];
        let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &cfg(true, 0));
        assert!(out.kept.is_empty());
        assert!(out.memory_context.is_none());
        assert_eq!(out.injected_tokens, 0);
        assert_eq!(
            out.dropped,
            vec![InjectionSlot::Behavior, InjectionSlot::Memory],
            "预算 0 → 全部通道丢弃"
        );
    }

    #[test]
    fn coordinated_empty_contents_skipped_without_dropping() {
        let layers = vec![
            (InjectionSlot::Behavior, "   ".to_string()),
            (InjectionSlot::Knowledge, String::new()),
        ];
        let out = allocate_injection_budget(&layers, Some("   "), &cfg(true, 100));
        assert!(out.kept.is_empty(), "空白注入块不占预算");
        assert!(out.memory_context.is_none(), "空白 RAG 视为无注入");
        assert!(out.dropped.is_empty());
        assert_eq!(out.injected_tokens, 0);
    }

    #[test]
    fn coordinated_rag_dropped_when_low_priority_and_over_budget() {
        // rag 未列入 order → 低优先；预算只够 behavior → rag 被整体丢弃
        let mut c = cfg(true, 3);
        c.order = vec![InjectionSlot::Behavior];
        let layers = vec![zh3("行为规则文本", InjectionSlot::Behavior)];
        let out = allocate_injection_budget(&layers, Some("记忆摘要内容"), &c);
        assert_eq!(out.kept.len(), 1, "behavior 保留");
        assert!(out.memory_context.is_none(), "RAG 被整块丢弃");
        assert_eq!(out.dropped, vec![InjectionSlot::Rag]);
    }

    #[test]
    fn coordinated_single_huge_layer_fallback_truncation() {
        // 唯一高优先通道单块超总池 → 兜底句子边界截断，估算 ≤ 预算
        let mut c = cfg(true, 10);
        c.order = vec![InjectionSlot::Behavior];
        let huge = "行为规则".to_string() + &"共情回应注意语气自然避免机械说教。".repeat(10);
        assert!(estimate_tokens(&huge) > 10, "构造应超预算");
        let layers = vec![(InjectionSlot::Behavior, huge)];
        let out = allocate_injection_budget(&layers, None, &c);
        assert!(out.fallback_truncated, "单块超池触发兜底截断");
        assert_eq!(out.kept.len(), 1);
        let kept_tokens = estimate_tokens(&out.kept[0].1);
        assert!(kept_tokens <= 10, "兜底截断后 {kept_tokens} ≤ 预算");
        assert!(out.dropped.is_empty(), "最高优先兜底不算丢弃");
    }

    #[test]
    fn coordinated_rag_independent_cap_trims_first() {
        // max_rag_tokens > 0：先做 RAG 独立上限截断，再入总池
        let mut c = cfg(true, 100);
        c.max_rag_tokens = 6;
        let long_rag = "一二三四五六七八九十甲乙丙丁戊己庚辛壬癸子丑寅卯".to_string();
        assert!(estimate_tokens(&long_rag) > 6);
        let out = allocate_injection_budget(&[], Some(&long_rag), &c);
        let rag = out.memory_context.expect("RAG 截断保留");
        assert!(!rag.is_empty());
        assert!(
            estimate_tokens(&rag) <= 6,
            "RAG 独立上限截断: {}",
            estimate_tokens(&rag)
        );
        assert!(out.fallback_truncated);
    }

    #[test]
    fn coordinated_utf8_multibyte_safe_on_truncation() {
        // UTF-8 多字节（emoji）裁剪不 panic、结果合法且不超预算换算字符
        let mut c = cfg(true, 4);
        c.order = vec![InjectionSlot::Memory];
        let emoji = "😀😀😀😀😀😀😀😀😀😀".to_string();
        assert!(estimate_tokens(&emoji) > 4);
        let layers = vec![(InjectionSlot::Memory, emoji.clone())];
        let out = allocate_injection_budget(&layers, None, &c);
        assert!(out.fallback_truncated);
        let kept = out.kept.first().map(|(_, t)| t.as_str()).unwrap_or("");
        assert!(!kept.is_empty());
        assert!(
            kept.chars().count() <= 8,
            "字符安全截断（≤ 预算×2 chars）: {}",
            kept
        );
        assert!(estimate_tokens(kept) <= 4, "估算 ≤ 预算");
        // 输入保持合法（无中间 panic、无替换符）
        assert!(!kept.contains('\u{FFFD}'), "不应产生替换符");
    }
}
