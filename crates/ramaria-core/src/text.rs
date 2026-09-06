//! crates/ramaria-core/src/text.rs - Ramaria 字符边界截断统一工具
//!
//! 设计特点:
//! - 按 Unicode 字符（而非 UTF-8 字节）边界截断，绝不切开多字节字符或 emoji
//! - 提供全库统一基准 `truncate_chars`（头部截断 + 单一 `…` 省略号，结果不超预算）
//! - 提供无省略号的字符边界切片 `truncate_char_boundary` / `truncate_chars_bare`
//!   （显示类硬上限 / 防御性 clamp，省略号由调用方按需追加）
//! - 提供句子边界优先变体 `truncate_chars_at_sentence_boundary`（对齐 token 预算旧能力）
//! - 纯函数、零 I/O、零外部依赖（仅 std），完全符合 ramaria-core 零 I/O 约束
//! - 作为 cli / desktop / app / memory / importer 各 crate 的单一截断实现来源
//!
//! 省略号约定:
//! - 统一使用单个 U+2026 `…`，不用 ASCII `...` 三连点。
//! - `truncate_chars` 在预算内为省略号预留 1 字符，因此截断结果总长 ≤ `max_chars`。

// =========================================================
// 统一基准：头部截断 + 省略号
// =========================================================

/// 按 Unicode 字符边界从头部截断字符串，超长时追加统一省略号 `…`。
///
/// 规则:
/// - 字符数 ≤ `max_chars` → 原样返回（不加省略号）。
/// - 超长 → 保留前 `max_chars - 1` 个字符并追加一个 `…`，结果总长恰为 `max_chars`。
/// - `max_chars == 0` → 返回空串（无省略号占位）。
/// - `max_chars == 1` 且超长 → 只返回 `…`（不保留内容，省略号占满预算）。
///
/// 参数:
/// - `s`: 原始字符串（可含 CJK、emoji、组合字符等多字节内容）。
/// - `max_chars`: 结果最大字符数（含省略号预留）。
///
/// 返回:
/// - 截断后的字符串；未超长时为原字符串的拷贝。
///
/// 说明:
/// - 逐字符迭代，保证不截断多字节 UTF-8 字符或 emoji（不按字节切片）。
/// - 省略号在预算内，因此即使 `max_chars` 很小也不会让结果超过预算。
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    // 为省略号预留 1 字符：保留前 max_chars - 1 个字符
    let mut out = String::with_capacity(s.len().min(max_chars * 4));
    for ch in s.chars().take(max_chars - 1) {
        out.push(ch);
    }
    out.push('…');
    out
}

// =========================================================
// 字符边界切片（无省略号）
// =========================================================

/// 截取字符串前至多 `max_chars` 个字符（按字符边界），返回借用的 `&str` 切片。
///
/// 规则:
/// - 字符数 ≤ `max_chars` → 返回整个原串。
/// - 超长 → 返回第 `max_chars` 个字符边界之前的部分（恰好 `max_chars` 个字符）。
/// - `max_chars == 0` → 返回空切片。
///
/// 参数:
/// - `s`: 原始字符串。
/// - `max_chars`: 最大字符数（不含省略号；本函数不追加任何后缀）。
///
/// 返回:
/// - 不超过 `max_chars` 个字符的借用切片。
///
/// 说明:
/// - 用于显示类硬上限（如系统通知标题/正文长度）与防御性 clamp，省略号由调用方按需追加。
/// - 返回借用切片，不产生分配；仅需 `String` 时由调用方 `to_string()`。
pub fn truncate_char_boundary(s: &str, max_chars: usize) -> &str {
    if max_chars == 0 {
        return "";
    }
    match s.char_indices().nth(max_chars) {
        // 第 max_chars 个字符的起始字节偏移即边界
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// 按 Unicode 字符边界截取字符串前至多 `max_chars` 个字符并返回 `String`。
///
/// 说明:
/// - 语义与 `truncate_char_boundary` 相同，仅返回拥有所有权的 `String`。
/// - 不追加省略号；需要省略号语义时使用 `truncate_chars`。
pub fn truncate_chars_bare(s: &str, max_chars: usize) -> String {
    truncate_char_boundary(s, max_chars).to_string()
}

// =========================================================
// 句子边界优先变体
// =========================================================

/// 在句子边界附近截断文本并追加 `…`（保留原算法语义，结果可能比预算多 1 字符）。
///
/// 规则（沿用原 token 预算行为，供既有管线无回归迁移）:
/// - 字符数 ≤ `max_chars` → 原样返回。
/// - 超长 → 在预算内寻找最近的句子终止符（`。！？\n`）作为截断点；
///   - 找到句子边界 → 保留到该边界（含终止符）再追加 `…`；
///   - 无句子边界 → 在最后空白处截断后追加 `…`；
///   - 无空白 → 直接按 `max_chars` 硬截断后追加 `…`。
///
/// 参数:
/// - `text`: 待截断文本。
/// - `max_chars`: 字符预算（截断内容的上限，不含 `…` 预留）。
///
/// 返回:
/// - 句子边界截断文本（含 `…` 后缀）。
///
/// 说明:
/// - 当句子终止符恰在预算末尾时，为包含该终止符，结果可能为 `max_chars + 1` 个字符；
///   调用方如需严格不超预算，应对结果再做一次 `truncate_char_boundary` 收紧。
pub fn truncate_chars_at_sentence_boundary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    // 在预算内寻找最后一个句子边界
    let head: String = text.chars().take(max_chars).collect();
    match find_last_sentence_boundary(&head) {
        Some(pos) => {
            // 保留边界字符（含终止符），再加省略号
            let end = pos + 1;
            let result: String = head.chars().take(end).collect();
            format!("{result}…")
        }
        None => {
            // 无句子边界 → 在最后空白处截断
            if let Some(pos) = head.rfind(char::is_whitespace) {
                let result: String = head.chars().take(pos).collect();
                format!("{result}…")
            } else {
                format!("{head}…")
            }
        }
    }
}

/// 在字符串中查找最后一个句子终止符的位置（返回**字符**索引）。
///
/// 句子终止符: `。` `！` `？` `\n`
///
/// 说明:
/// - `char_indices` 返回字节偏移，此处换算为字符索引，供调用方按字符截断使用，
///   避免多字节（中文）场景下截断长度超预算。
fn find_last_sentence_boundary(text: &str) -> Option<usize> {
    text.char_indices()
        .rev()
        .find(|(_, ch)| matches!(ch, '。' | '！' | '？' | '\n'))
        .map(|(idx, _)| text[..idx].chars().count())
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- truncate_chars ----

    /// truncate_chars 各输入参数化验证（中文 / emoji / 纯标点 / ASCII 混合）。
    #[test]
    fn truncate_chars_cases() {
        // 未超长原样
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("", 10), "");
        // 恰好边界
        assert_eq!(truncate_chars("hello", 5), "hello");
        assert_eq!(truncate_chars("你好世界", 4), "你好世界");
        // 超长：预算内预留 1 字符给省略号
        assert_eq!(truncate_chars("hello world", 8), "hello w…");
        assert_eq!(truncate_chars("你好世界啊", 4), "你好世…");
        // emoji 不计入错误（不切开 emoji / 组合字符）
        let emoji = "😀😀😀";
        assert_eq!(truncate_chars(emoji, 2), "😀…");
        // 纯标点
        assert_eq!(truncate_chars("！！！", 2), "！…");
        // 预算 0
        assert_eq!(truncate_chars("abc", 0), "");
        // 预算 1 且超长 → 仅省略号
        assert_eq!(truncate_chars("abc", 1), "…");
    }

    #[test]
    fn truncate_chars_never_splits_utf8() {
        // 多字节 + emoji 混合，结果必须是合法 UTF-8 且在字符边界
        let s = "你好，世界😀！这是一段混合中英 emoji 的文本 abc def";
        for n in 0..s.chars().count() + 2 {
            let out = truncate_chars(s, n);
            let out_chars = out.chars().count();
            if s.chars().count() > n {
                // 超长：≤ n 字符，且以单个省略号结尾
                assert!(
                    out_chars <= n,
                    "超长结果不应超过预算 (n={n}, got {out_chars})"
                );
                if n > 0 {
                    assert!(out.ends_with('…'), "超长应带省略号 (n={n})");
                }
            }
            // 结果必须是合法 UTF-8（无切开多字节字符），字符数 ≤ 原串
            assert!(out_chars <= s.chars().count());
        }
    }

    // ---- truncate_char_boundary ----

    #[test]
    fn truncate_char_boundary_cases() {
        assert_eq!(truncate_char_boundary("hello", 10), "hello");
        assert_eq!(truncate_char_boundary("hello world", 5), "hello");
        assert_eq!(truncate_char_boundary("你好世界", 2), "你好");
        assert_eq!(truncate_char_boundary("Hi你好world", 5), "Hi你好w");
        assert_eq!(truncate_char_boundary("", 5), "");
        assert_eq!(truncate_char_boundary("abc", 3), "abc");
        assert_eq!(truncate_char_boundary("hello", 0), "");
        assert_eq!(truncate_char_boundary("ひらがな", 2), "ひら");
        // emoji 在边界内整体保留
        assert_eq!(truncate_char_boundary("😀😀", 1), "😀");
    }

    // ---- truncate_chars_bare ----

    #[test]
    fn truncate_chars_bare_equals_boundary_owned() {
        assert_eq!(truncate_chars_bare("超预算文本内容", 4), "超预算文");
        assert_eq!(truncate_chars_bare("短文本", 100), "短文本");
        assert_eq!(truncate_chars_bare("", 0), "");
    }

    // ---- truncate_chars_at_sentence_boundary ----

    #[test]
    fn sentence_boundary_within_limit_unchanged() {
        assert_eq!(
            truncate_chars_at_sentence_boundary("短文本。", 100),
            "短文本。"
        );
    }

    #[test]
    fn sentence_boundary_truncate_at_period() {
        let text = "第一句话。第二句话。第三句话。";
        let result = truncate_chars_at_sentence_boundary(text, 10);
        assert!(result.ends_with('…'));
        assert!(result.starts_with("第一句话。"));
    }

    #[test]
    fn sentence_boundary_truncate_at_newline() {
        let text = "第一行\n第二行\n第三行";
        let result = truncate_chars_at_sentence_boundary(text, 8);
        assert!(result.ends_with('…'));
        assert!(result.contains("第一行\n"));
    }

    #[test]
    fn sentence_boundary_falls_back_to_whitespace() {
        let result = truncate_chars_at_sentence_boundary("Hello World from Rust", 10);
        assert!(result.ends_with('…'));
        assert!(result.starts_with("Hello"));
    }

    #[test]
    fn sentence_boundary_no_whitespace() {
        let text = "abcdefghijklmnopqrstuvwxyz";
        let result = truncate_chars_at_sentence_boundary(text, 5);
        assert!(result.ends_with('…'));
    }
}
