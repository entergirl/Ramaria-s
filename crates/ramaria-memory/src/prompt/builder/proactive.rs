//! crates/ramaria-memory/src/prompt/builder/proactive.rs - Prompt 主动开口段构建
//!
//! 设计特点:
//! - 条件段落：`proactive_context` 为 None 时不产生任何输出（既有对话路径零变化）
//! - 文案收口：主动指令段与六时段情境文案集中为常量 / 单一函数，函数只引用不内联
//! - 可选行：锚点 / 角度 / 语气各自 trim 后非空才渲染（None 或空白不产生行）
//! - 时段词汇与 L1 摘要 `time_period` 严格对齐（清晨/上午/下午/傍晚/夜间/深夜）
//! - 纯字符串拼接，无 I/O 与 LLM 依赖

use ramaria_core::time_period::TimePeriod;

use super::PromptContext;

// =========================================================
// 主动生成场景上下文
// =========================================================

/// 主动生成场景上下文（供上层构造、提示词装配层透传）。
///
/// 字段约定:
/// - `time_period`: 当前时段（与 L1 摘要 `time_period` 同一词汇体系）；
/// - `anchor`: 候选锚点摘要（None = 无具体话题的轻触达场景）；
/// - `angle`: 开口角度（None = 未提供）；
/// - `tone`: 说话语气（None = 未提供）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProactivePromptContext {
    /// 当前时段（与 L1 摘要 time_period 同一词汇体系）
    pub time_period: TimePeriod,
    /// 候选锚点摘要（None = 无具体话题的轻触达场景）
    pub anchor: Option<String>,
    /// 开口角度（None = 未提供）
    pub angle: Option<String>,
    /// 说话语气（None = 未提供）
    pub tone: Option<String>,
}

// =========================================================
// 主动段文案（指令段 / 时段情境）
// =========================================================

/// 主动开口指令段（场景说明 + 四条要求；文案独立收口，避免散落）。
const PROACTIVE_DIRECTIVE: &str = "# 主动开口\n\
现在不是你在回复对方，而是你想主动和对方说句话——你打开了和对方的聊天窗口。\n\
要求：\n\
1. 像熟人之间随手发起聊天：可以说你想到的事、关心对方近况，或接着之前聊过的内容往下说。\n\
2. 不解释你为什么突然发消息，不用「好久不见」「在吗」这类开场。\n\
3. 保持你平时的说话习惯：整条不超过 30 字；多条用「||」分隔。\n\
4. 不追问，发出去之后对方回不回都可以。";

/// 当前时段的情境提示文案。
///
/// 参数:
/// - `period`: 当前时段。
///
/// 返回:
/// - 该时段的静态情境文案（六选一）。
fn period_scene(period: TimePeriod) -> &'static str {
    match period {
        TimePeriod::EarlyMorning => "现在是清晨，对方可能刚醒来。消息轻一些，别一次说太多。",
        TimePeriod::Morning => "现在是上午，对方可能在忙。消息简短自然就好。",
        TimePeriod::Afternoon => "现在是下午。对方可能在忙，消息简短自然就好。",
        TimePeriod::Evening => "现在是傍晚，对方可能刚忙完一天。适合轻松地聊两句。",
        TimePeriod::Night => "现在是夜间，对方可能在放松。消息轻一些，不用等回复。",
        TimePeriod::LateNight => {
            "现在是深夜，对方可能已经休息。除非你确信对方习惯深夜聊天，消息要更短更轻。"
        }
    }
}

// =========================================================
// 主动段渲染
// =========================================================

/// 渲染主动开口段（`# 主动开口` 指令 + 可选情境行 + `## 此刻情境` 时段段）。
///
/// 语义:
/// - `context.proactive_context` 为 `None` → 返回空串（非主动生成零输出）；
/// - `Some(..)` → 指令段原文；锚点 / 角度 / 语气各自 trim 后非空才追加对应行；
///   末尾以空行分隔追加 `## 此刻情境` 与当前时段文案。
///
/// 参数:
/// - `context`: 装配上下文（读取 `proactive_context` 字段）。
///
/// 返回:
/// - 主动段文本；无主动上下文时为空串。结果不含首尾多余空白。
pub(super) fn build_proactive_block(context: &PromptContext) -> String {
    let proactive = match context.proactive_context.as_ref() {
        Some(proactive) => proactive,
        None => return String::new(),
    };

    let mut lines: Vec<String> = vec![PROACTIVE_DIRECTIVE.to_string()];

    // 可选情境行：各自 trim 后非空才渲染
    if let Some(anchor) = proactive
        .anchor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(format!(
            "本次想到的事：{anchor}（就当作你自己想起来的事去说，不要复述这句话）"
        ));
    }
    if let Some(angle) = proactive
        .angle
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(format!("开口角度：{angle}"));
    }
    if let Some(tone) = proactive
        .tone
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(format!("说话语气：{tone}"));
    }

    // 空行分隔后追加此刻情境（时段文案与 L1 摘要词汇对齐）
    lines.push(String::new());
    lines.push("## 此刻情境".to_string());
    lines.push(period_scene(proactive.time_period).to_string());

    lines.join("\n")
}
