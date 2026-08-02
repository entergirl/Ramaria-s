//! rust/crates/ramaria-cli/src/util.rs - CLI 共享工具函数
//!
//! 设计特点:
//! - 消除跨命令模块的代码重复（format_timestamp / truncate / extract_toml_value）
//! - 所有函数为纯函数，零 I/O、零状态
//! - 供 commands/ 下各命令模块调用
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
/// - 若 `s.chars.count <= max_chars`，返回原字符串的 clone。
/// - 否则截取前 `max_chars - 3` 字符，追加 `"..."`。
///
/// 说明:
/// - 使用 `.chars` 迭代器保证不截断多字节 UTF-8 字符中间。
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

/// 从 TOML 文本中提取形如 `key = value` 的简单键值（不做节检查）。
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

    /// format_timestamp 各输入参数化验证。
    #[test]
    fn format_ts_cases() {
        let cases = [
            (0i64, None),                                              // 0 → None
            (-1, None),                                                // 负数 → None
            (1_718_006_400_000, Some("2024-06-10 08:00".to_string())), // 有效时间
        ];
        for (ts, expected) in cases {
            assert_eq!(format_timestamp(ts), expected, "ts={ts}");
        }
    }

    // ---- truncate ----

    /// truncate 各 (input, max) 参数化验证。
    #[test]
    fn truncate_cases() {
        let cases = [
            ("hello", 10, "hello"), // 未超长原样
            ("hello", 5, "hello"),  // 恰好边界
            ("hello world", 8, "hello..."),
            ("hello", 3, "..."),         // max=3 → 0 chars + "..."
            ("你好世界", 4, "你好世界"), // CJK 1 char/字
            ("你好世界", 3, "..."),
            ("你好世界啊", 4, "你..."), // 5 chars > 4, 取 1 字 + "..."
            ("", 10, ""),               // 空串
        ];
        for (input, max, expected) in cases {
            assert_eq!(truncate(input, max), expected, "input={input:?} max={max}");
        }
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
