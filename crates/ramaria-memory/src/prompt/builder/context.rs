//! crates/ramaria-memory/src/prompt/builder/context.rs - 当前语境块构建
//!
//! 设计特点:
//! - 组装 `# 当前时间` 段：时间 + 可选天气 + 可选上次活跃时间
//! - 时间优先取 `context.current_time_str`，否则用 `chrono::Local::now`
//! - 天气/上次活跃为空时跳过对应行
//! - 纯字符串拼接，无 I/O

use chrono::Local;

use super::PromptContext;

// =========================================================
// 当前语境块
// =========================================================

/// 组装当前时间块（`# 当前时间`）：时间 + 可选天气 + 可选上次活跃时间。
///
/// 时间格式：
/// - 若 `context.current_time_str` 有值，直接使用。
/// - 否则使用 `chrono::Local::now` 生成可读日期时间（`%Y-%m-%d %H:%M`）。
pub(super) fn build_context_block(context: &PromptContext) -> String {
    let time_str = context
        .current_time_str
        .clone()
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d %H:%M").to_string());

    let mut lines = vec![format!(
        "# 当前时间\n\
         当前时间：{time_str}"
    )];

    if let Some(ref weather) = context.weather
        && !weather.trim().is_empty()
    {
        lines.push(format!("天气：{weather}"));
    }

    if let Some(ref last_active) = context.last_active_at
        && !last_active.is_empty()
    {
        lines.push(format!("上次对话时间：{last_active}"));
    }

    lines.join("\n")
}
