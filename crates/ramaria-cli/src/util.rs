//! rust/crates/ramaria-cli/src/util.rs - CLI 共享工具函数
//!
//! 设计特点:
//! - 消除跨命令模块的代码重复（format_timestamp / truncate / extract_toml_value）
//! - 所有函数为纯函数，零 I/O、零状态
//! - 供 commands/ 下各模块和 ui.rs 调用
//! - 按字符边界操作，正确处理 UTF-8 与 CJK

use chrono::TimeZone;

// =========================================================
// 时间格式化
// =========================================================

/// 将 Unix 毫秒时间戳格式化为可读字符串。
///
/// 参数:
/// - `ms`: Unix 毫秒时间戳。
///
/// 返回:
/// - `Some("2024-06-10 08:00")`: 有效时间戳。
/// - `None`: ms ≤ 0（无效时间戳）。
///
/// 说明:
/// - 输出格式 `%Y-%m-%d %H:%M`，不含秒数以减少终端宽度占用。
/// - 使用 UTC 时区以保证跨平台一致性。
pub fn format_timestamp(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let secs = ms / 1000;
    chrono::Utc
        .timestamp_opt(secs, ((ms % 1000) * 1_000_000) as u32)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
}

// =========================================================
// 字符串截断
// =========================================================

/// 按字符（而非字节）截断字符串到指定长度。
///
/// 参数:
/// - `s`: 原始字符串（可含 CJK 等多字节字符）。
/// - `max_chars`: 最大字符数。
///
/// 返回:
/// - 若 `s.chars().count() <= max_chars`，返回原字符串的 clone。
/// - 否则截取前 `max_chars - 3` 字符，追加 `"..."`。
///
/// 说明:
/// - 使用 `.chars()` 迭代器保证不截断多字节 UTF-8 字符中间。
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

// =========================================================
// TOML 简单解析
// =========================================================

/// 从 TOML 文本中提取 `[identity]` 节下的简单键值。
///
/// 参数:
/// - `content`: 完整 TOML 文本。
/// - `key`: 要查找的键名（如 "assistant_name"）。
///
/// 返回:
/// - `Some(value)`: 找到匹配的键值（去除引号）。
/// - `None`: 未找到或格式不符合预期。
///
/// 说明:
/// - 支持双引号 `"..."` 和单引号 `'...'` 两种值格式。
/// - 仅支持单行键值对，不支持多行字符串或嵌套表。
/// - 这是轻量级解析器，不依赖 `toml` crate——避免为简单场景引入额外依赖。
pub fn extract_toml_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(&format!("{key} =")) {
            // 双引号格式
            if let Some(start) = trimmed.find('"')
                && let Some(end) = trimmed[start + 1..].find('"')
            {
                return Some(trimmed[start + 1..start + 1 + end].to_string());
            }
            // 单引号格式
            if let Some(start) = trimmed.find('\'')
                && let Some(end) = trimmed[start + 1..].find('\'')
            {
                return Some(trimmed[start + 1..start + 1 + end].to_string());
            }
        }
    }
    None
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- format_timestamp ----

    #[test]
    fn format_ts_zero_returns_none() {
        assert_eq!(format_timestamp(0), None);
    }

    #[test]
    fn format_ts_negative_returns_none() {
        assert_eq!(format_timestamp(-1), None);
    }

    #[test]
    fn format_ts_valid() {
        // 2024-06-10T08:00:00 UTC = 1718006400000 ms
        let result = format_timestamp(1_718_006_400_000);
        assert_eq!(result, Some("2024-06-10 08:00".to_string()));
    }

    // ---- truncate ----

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn truncate_very_short_max() {
        // max_chars = 3 → 0 chars + "..."
        assert_eq!(truncate("hello", 3), "...");
    }

    #[test]
    fn truncate_cjk() {
        // 中文字符也是 1 char
        // "你好世界" = 4 chars，不超过 max 时原样返回
        assert_eq!(truncate("你好世界", 4), "你好世界");
        assert_eq!(truncate("你好世界", 3), "..."); // 3-3=0 chars + "..."
        // 用更长的 CJK 字符串验证截断
        assert_eq!(truncate("你好世界啊", 4), "你..."); // "你好世界啊"=5 chars > 4, 取1字+"..."
    }

    #[test]
    fn truncate_empty() {
        assert_eq!(truncate("", 10), "");
    }

    // ---- extract_toml_value ----

    #[test]
    fn toml_value_double_quotes() {
        let toml = "[identity]\nassistant_name = \"黎杋枫\"\nuser_name = \"用户\"";
        assert_eq!(
            extract_toml_value(toml, "assistant_name"),
            Some("黎杋枫".to_string())
        );
        assert_eq!(
            extract_toml_value(toml, "user_name"),
            Some("用户".to_string())
        );
    }

    #[test]
    fn toml_value_single_quotes() {
        let toml = "[identity]\nassistant_name = '测试'";
        assert_eq!(
            extract_toml_value(toml, "assistant_name"),
            Some("测试".to_string())
        );
    }

    #[test]
    fn toml_value_not_found() {
        assert_eq!(
            extract_toml_value("[identity]\nkey = \"val\"", "nonexistent"),
            None
        );
    }

    #[test]
    fn toml_value_empty_content() {
        assert_eq!(extract_toml_value("", "key"), None);
    }

    #[test]
    fn toml_value_no_quotes() {
        // 无引号的值不被识别
        assert_eq!(extract_toml_value("key = value", "key"), None);
    }
}
