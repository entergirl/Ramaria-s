//! crates/ramaria-eval/src/report/format.rs - Ramaria 评测报告数值格式
//!
//! 设计特点:
//! - 空值与缺失统一渲染为 `-`，不写 0 也不写 N/A
//! - 定点小数与正负号口径固定，同一数值在 JSON 与 markdown 中读法一致
//! - 负零统一收敛为正零，避免产物出现 `-0.000`
//! - p 值小于 0.001 时写 `<0.001`，不写科学计数法
//! - 百分比保留一位小数，只用于比例类指标

/// 定点小数；`None` 渲染为 `-`。
pub(crate) fn fixed(value: Option<f64>, digits: usize) -> String {
    match value {
        Some(value) => normalize(format!("{value:.digits$}")),
        None => "-".to_string(),
    }
}

/// 带符号的差值；`None` 渲染为 `-`。
pub(crate) fn delta(value: Option<f64>) -> String {
    match value {
        Some(value) => normalize_sign(format!("{value:+.3}")),
        None => "-".to_string(),
    }
}

/// p 值；`None` 渲染为 `-`，小于 0.001 写作 `<0.001`。
pub(crate) fn p_value(value: Option<f64>) -> String {
    match value {
        Some(value) if value < 0.001 => "<0.001".to_string(),
        Some(value) => normalize(format!("{value:.3}")),
        None => "-".to_string(),
    }
}

/// 带标签的校正后 p 值（BH-FDR）。
pub(crate) fn p_adj_label(value: Option<f64>) -> String {
    match value {
        Some(value) if value < 0.001 => "p_adj<0.001".to_string(),
        Some(value) => format!("p_adj={}", normalize(format!("{value:.3}"))),
        None => "p_adj 未产出".to_string(),
    }
}

/// 比例 → 百分比（一位小数）；`None` 渲染为 `-`。
pub(crate) fn percent(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{:.1}%", value * 100.0),
        None => "-".to_string(),
    }
}

/// 负零收敛为正零。
fn normalize(text: String) -> String {
    match text.strip_prefix("-") {
        Some(rest) if rest.chars().all(|ch| ch == '0' || ch == '.') => rest.to_string(),
        _ => text,
    }
}

/// 显式正号的正零收敛为无符号零。
fn normalize_sign(text: String) -> String {
    match text.strip_prefix('+') {
        Some(rest) if rest.chars().all(|ch| ch == '0' || ch == '.') => rest.to_string(),
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空值与负零的写法固定。
    #[test]
    fn none_and_negative_zero_are_readable() {
        assert_eq!(fixed(None, 3), "-");
        assert_eq!(fixed(Some(-0.0), 3), "0.000");
        assert_eq!(delta(Some(0.0)), "0.000");
        assert_eq!(delta(Some(-0.05)), "-0.050");
        assert_eq!(p_value(Some(0.0001)), "<0.001");
        assert_eq!(percent(Some(0.123_45)), "12.3%");
    }
}
