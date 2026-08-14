//! crates/ramaria-memory/src/utils.rs - 记忆模块通用工具函数
//!
//! 设计特点:
//! - 集中管理 L1 摘要和事件提取共用的纯函数
//! - JSON 解析辅助: strip_thinking, extract_first_json_object, extract_first_json_array
//! - 数值钳制: clamp_valence, clamp_salience, clamp_to_nearest (五档网格)
//! - 零 I/O，零 async，不依赖数据库或外部服务

// =========================================================
// JSON 解析辅助
// =========================================================

/// 剥离 `<think>...</think>` 标签（支持 gemma 等思考模型）。
///
/// 说明:
/// - 匹配 `<think>` 到 `</think>` 之间的所有内容并移除。
/// - 支持多行和任意中间内容。
/// - 递归处理多层嵌套（虽然罕见）。
/// - 若无匹配则返回原始文本。
pub fn strip_thinking(text: &str) -> String {
    let start_tag = "<think>";
    let end_tag = "</think>";

    let start = match text.find(start_tag) {
        Some(pos) => pos,
        None => return text.to_string(),
    };

    let after_start = &text[start + start_tag.len()..];
    let end = match after_start.find(end_tag) {
        Some(pos) => pos,
        None => return text.to_string(),
    };

    let before = &text[..start];
    let after = &after_start[end + end_tag.len()..];
    let result = format!("{before}{after}");

    if result.contains(start_tag) {
        strip_thinking(&result)
    } else {
        result
    }
}

/// 从文本中提取第一个合法 JSON 对象 `{...}`。
///
/// 策略:
/// - 查找第一个 `{` 和匹配的 `}`（支持嵌套结构）。
/// - 使用括号计数算法，兼容 LLM 在 JSON 前后附加说明文字的场景。
pub fn extract_first_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let chars: Vec<char> = text[start..].chars().collect();
    let mut depth = 0u32;
    let mut end_idx = 0usize;

    for (i, ch) in chars.iter().enumerate() {
        match ch {
            '{' => depth += 1,
            '}' => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    end_idx = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }

    if depth == 0 && end_idx > 0 {
        Some(chars[..end_idx].iter().collect())
    } else {
        None
    }
}

/// 提取文本中第一个完整 JSON 数组 `[...]`。
///
/// 策略:
/// - 查找第一个 `[` 和匹配的 `]`。
/// - 正确处理字符串内的括号（不参与深度计数）。
/// - 支持嵌套对象 `{...}` 出现在数组内。
pub fn extract_first_json_array(text: &str) -> Option<String> {
    let start = text.find('[')?;
    let chars: Vec<char> = text[start..].chars().collect();
    let mut depth = 0u32;
    let mut in_string = false;
    let mut prev_char = ' ';

    for (i, ch) in chars.iter().enumerate() {
        if in_string {
            if *ch == '"' && prev_char != '\\' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '[' => depth += 1,
                ']' => {
                    if depth == 0 {
                        continue;
                    }
                    depth -= 1;
                    if depth == 0 {
                        return Some(chars[..=i].iter().collect());
                    }
                }
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
        }
        prev_char = *ch;
    }

    None
}

// =========================================================
// 数值钳制 — 五档网格
// =========================================================

/// 合法 salience 值（五档）。
const SALIENCE_GRID: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

/// 合法 valence 值（五档）。
const VALENCE_GRID: [f64; 5] = [-1.0, -0.5, 0.0, 0.5, 1.0];

/// 一天的毫秒数。
pub const MS_PER_DAY: f64 = 86_400_000.0;

/// 将浮点值钳制到候选数组中最接近的值。
///
/// 策略: 将值映射到候选集中绝对值差最小的元素。
/// 中点值取较小者（由 `min_by` 的稳定排序保证）。
fn clamp_to_nearest(value: f64, candidates: &[f64; 5]) -> f64 {
    candidates
        .iter()
        .copied()
        .min_by(|a, b| {
            (*a - value)
                .abs()
                .partial_cmp(&(*b - value).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(value)
}

/// 将 valence 钳制到最近的合法五档值 (-1.0, -0.5, 0.0, 0.5, 1.0)。
pub fn clamp_valence(value: f64) -> f64 {
    clamp_to_nearest(value, &VALENCE_GRID)
}

/// 将 salience 钳制到最近的合法五档值 (0.0, 0.25, 0.5, 0.75, 1.0)。
pub fn clamp_salience(value: f64) -> f64 {
    clamp_to_nearest(value, &SALIENCE_GRID)
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- strip_thinking ----

    #[test]
    fn strip_thinking_cases() {
        // 含 think 标签 → 去除标签保留正文
        let input = "<think>Let me think...</think>\n{\"summary\": \"hello\"}";
        let result = strip_thinking(input);
        assert!(!result.contains("<think>"));
        assert!(result.contains("{\"summary\""));
        // 无标签 → 原样返回
        let input = "{\"summary\": \"hello\"}";
        assert_eq!(strip_thinking(input), input);
        // 多行标签 → 保留正文
        let input = "Some text\n<think>\nreasoning here\n</think>\n{\"summary\": \"test\"}";
        let result = strip_thinking(input);
        assert!(result.contains("Some text"));
        assert!(result.contains("{\"summary\": \"test\"}"));
        assert!(!result.contains("reasoning"));
    }

    // ---- extract_first_json_object ----

    #[test]
    fn extract_first_json_object_cases() {
        // 前缀文本 + 对象 → 提取对象
        let input = "前缀文本 {\"summary\": \"测试\", \"valence\": 0.5} 后缀文本";
        let result = extract_first_json_object(input).unwrap();
        assert!(result.starts_with('{'));
        assert!(result.ends_with('}'));
        assert!(result.contains("\"summary\""));
        // 嵌套对象 → 原样
        let input = r#"{"a": {"b": [1,2,3]}, "c": "d"}"#;
        assert_eq!(extract_first_json_object(input).unwrap(), input);
        // 无 JSON → None
        assert!(extract_first_json_object("纯文本无JSON").is_none());
    }

    // ---- extract_first_json_array ----

    #[test]
    fn extract_first_json_array_cases() {
        // 前缀 + 数组 → 提取数组
        let text = r#"前缀 [{"a":1}, {"b":2}] 后缀"#;
        let result = extract_first_json_array(text).unwrap();
        assert!(result.starts_with('['));
        assert!(result.ends_with(']'));
        // 无括号 → None
        assert!(extract_first_json_array("no array here").is_none());
        // 字符串内含 ] → 正确识别
        let text = r#"result: ["a", "b]c", "d"] end"#;
        let result = extract_first_json_array(text).unwrap();
        assert!(result.contains("b]c"));
    }

    // ---- clamp_valence / clamp_salience ----

    #[test]
    fn clamp_functions_cases() {
        // clamp_valence: 档位 0.0/0.5/±1.0，取最近档位
        assert!((clamp_valence(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((clamp_valence(1.0) - 1.0).abs() < f64::EPSILON);
        assert!((clamp_valence(-1.0) - (-1.0)).abs() < f64::EPSILON);
        assert!((clamp_valence(0.3) - 0.5).abs() < f64::EPSILON);
        assert!((clamp_valence(-0.7) - (-0.5)).abs() < f64::EPSILON);
        assert!((clamp_valence(0.9) - 1.0).abs() < f64::EPSILON);
        assert!((clamp_valence(0.24) - 0.0).abs() < f64::EPSILON);
        // clamp_salience: 档位 0.0/0.25/0.5/0.75/1.0
        assert!((clamp_salience(0.5) - 0.5).abs() < f64::EPSILON);
        assert!((clamp_salience(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((clamp_salience(0.75) - 0.75).abs() < f64::EPSILON);
        assert!((clamp_salience(0.3) - 0.25).abs() < f64::EPSILON);
        assert!((clamp_salience(0.9) - 1.0).abs() < f64::EPSILON);
        assert!((clamp_salience(0.4) - 0.5).abs() < f64::EPSILON);
    }
}
