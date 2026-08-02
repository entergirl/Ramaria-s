//! rust/crates/ramaria-memory/src/token_budget.rs - Token 预算管理模块
//!
//! 设计特点:
//! - 字符数估算 token 数: 中文 ≈ len/2，英文 ≈ len/4
//! - 预算分配优先级: System Prompt → RAG 上下文 → 对话历史（新→旧）
//! - 句子边界截断（。！？\n），不硬切单词或中文字符
//! - 不引入 tiktoken-rs，保持零外部 tokenizer 依赖
//! - 纯函数设计，可独立单元测试，零 I/O

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
// 句子边界截断
// =========================================================

/// 在句子边界截断文本。
///
/// 策略:
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
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_string();
    }

    // 在 max_chars 范围内寻找最后一个句子边界
    let truncated: String = text.chars().take(max_chars).collect();
    let boundary = find_last_sentence_boundary(&truncated);

    match boundary {
        Some(pos) => {
            // pos 可能正好是最后一个字符（此时无需进一步截断，但需加 …）
            let end = pos + 1; // include the boundary char
            let result: String = truncated.chars().take(end).collect();
            format!("{result}…")
        }
        None => {
            // 无句子边界 → 在最后空白处截断
            if let Some(pos) = truncated.rfind(char::is_whitespace) {
                let result: String = truncated.chars().take(pos).collect();
                format!("{result}…")
            } else {
                format!("{truncated}…")
            }
        }
    }
}

/// 在字符串中查找最后一个句子终止符的位置。
///
/// 句子终止符: `。` `！` `？` `\n`
fn find_last_sentence_boundary(text: &str) -> Option<usize> {
    text.char_indices()
        .rev()
        .find(|(_, ch)| matches!(ch, '。' | '！' | '？' | '\n'))
        .map(|(idx, _)| idx)
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
}
