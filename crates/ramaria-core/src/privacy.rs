//! crates/ramaria-core/src/privacy.rs - Ramaria 日志隐私脱敏统一工具
//!
//! 设计特点:
//! - 提供个人标识（QQ 号、persona UID、昵称等）的日志展示脱敏 `mask_id`
//! - 脱敏不可逆、纯展示：存储与业务始终使用原值，本模块不参与读写路径
//! - 确定性映射：同一输入恒得同一输出，保证跨日志点可用掩码串接排障归属
//! - 纯函数、零 I/O、零外部依赖（仅 std），完全符合 ramaria-core 零 I/O 约束
//! - 作为 desktop / importer 等 crate 的单一日志脱敏实现来源，避免各 crate 各自实现

// =========================================================
// 个人标识脱敏
// =========================================================

/// 将个人标识 / 昵称等敏感字符串脱敏为日志可展示形态。
///
/// 规则:
/// - 空串 → 返回空串。
/// - 字符数 ≤ 4 → 整体掩为 `****`（短昵称/短 ID 不保留任何可见片段）。
/// - 字符数 > 4 → 保留首 2 与尾 2 个字符，中间以单个 `…` 替代
///   （首尾不相交；如 `123456789` → `12…89`）。
///
/// 参数:
/// - `raw`: 原始个人标识或昵称（可含多字节中文/emoji）。
///
/// 返回:
/// - 脱敏后的字符串；同一 `raw` 恒得同一输出，便于跨日志点串接归属。
///
/// 说明:
/// - 仅用于日志字段与 message 文本的展示，任何场景都不输出可还原的完整 QQ 号/昵称。
/// - 不影响存储与业务：写库、返回给调用方的值仍为原值，调用方不得用本函数结果覆盖原值。
pub fn mask_id(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    if n <= 4 {
        return "****".to_string();
    }
    // n ≥ 5：首 2 + 尾 2 不相交
    let mut out = String::with_capacity(chars.len().min(n));
    out.push(chars[0]);
    out.push(chars[1]);
    out.push('…');
    out.push(chars[n - 2]);
    out.push(chars[n - 1]);
    out
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_id_empty() {
        assert_eq!(mask_id(""), "");
    }

    #[test]
    fn mask_id_short_whole_masked() {
        // ≤4 字符整体掩码（中文短昵称 / 短号）
        assert_eq!(mask_id("张三"), "****");
        assert_eq!(mask_id("张三丰"), "****");
        assert_eq!(mask_id("隔壁老王"), "****");
        assert_eq!(mask_id("1234"), "****");
        assert_eq!(mask_id("abc"), "****");
    }

    #[test]
    fn mask_id_qq_number_keeps_head_tail() {
        // 全数字 QQ 形态：保留首 2 尾 2，中间不可还原
        assert_eq!(mask_id("123456789"), "12…89");
        assert_eq!(mask_id("1234567890"), "12…90");
        // 5 位最短长串形态：首尾不相交
        assert_eq!(mask_id("12345"), "12…45");
    }

    #[test]
    fn mask_id_mixed_chinese_nickname() {
        // 中英混合昵称按字符（非字节）处理，不切多字节
        assert_eq!(mask_id("阿强同学A"), "阿强…学A");
        assert_eq!(mask_id("Tommy"), "To…my");
    }

    #[test]
    fn mask_id_prefix_uid_is_still_not_reversible() {
        // persona UID（含侧前缀，如 user-/char-）整体脱敏，不输出完整 QQ 片段
        let masked = mask_id("user-123456789");
        assert!(masked.starts_with("us"));
        assert!(masked.ends_with("89"));
        assert!(!masked.contains("123456789"));
        assert!(!masked.contains("1234567"));
    }

    #[test]
    fn mask_id_deterministic() {
        // 确定性：同一输入恒同输出（排障串接依赖）
        assert_eq!(mask_id("user-9876543210"), mask_id("user-9876543210"));
        assert_eq!(mask_id("昵称测试"), mask_id("昵称测试"));
    }
}
