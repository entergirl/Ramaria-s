//! crates/ramaria-memory/src/prompt/builder/context.rs - 当前语境块构建
//!
//! 设计特点:
//! - 组装 `# 当前时间` 段：时间 + 可选天气 + 可选上次活跃时间 + 可选近期状态行
//! - 时间优先取 `context.current_time_str`，否则用 `chrono::Local::now`
//! - 天气/上次活跃/近期状态为空时跳过对应行
//! - 纯字符串拼接，无 I/O

use chrono::Local;

use crate::chat::RecentMood;

use super::PromptContext;

// =========================================================
// 当前语境块
// =========================================================

/// 组装当前时间块（`# 当前时间`）：时间 + 可选天气 + 可选上次活跃时间 + 可选近期状态。
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

    if let Some(mood) = context.recent_mood {
        lines.push(recent_mood_line(mood).to_string());
    }

    lines.join("\n")
}

/// 近期状态行文案（有方向时追加在当前语境块末尾）。
///
/// 参数:
/// - `mood`: 近期心情方向。
///
/// 返回:
/// - 该方向的静态语气引导行。
fn recent_mood_line(mood: RecentMood) -> &'static str {
    match mood {
        RecentMood::Positive => "近期状态：最近心情不错，语气可以轻快一些。",
        RecentMood::Negative => "近期状态：最近心情有些低落，语气放缓一些，别太跳脱。",
    }
}
