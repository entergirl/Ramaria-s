//! crates/ramaria-memory/src/l1/summarizer/helpers.rs - 摘要管线自由辅助函数
//!
//! 设计特点:
//! - format_messages: L0 消息列表 → 对话文本（仅 User/Assistant 参与摘要）。
//! - is_progressive_triggered: 渐进式摘要触发判断（消息数 / 时间跨度双条件）。
//! - build_prior_context: 上一块上文构建（短块注入原文 / 长块注入上一 L1 + 线索 / 降级截断）。
//! - validate_continuation / validate_evidence_notes / normalize_optional_slot: LLM 输出字段校验与规范化。
//! - parse_keywords: 关键词字符串 → (存储串, KeywordToken 列表)。
//! - 全部为纯函数/无 I/O；隐私红线：日志只记长度与字段，不记录原文。

use ramaria_core::MemoryL1;
use ramaria_core::keyword::KeywordToken;
use ramaria_core::types::{EvidenceNote, MessageRole};
use tracing::{debug, warn};
use uuid::Uuid;

use super::L1SummarizerConfig;

// =========================================================
// 消息格式化（自由函数，供块级生成与上文构建复用）
// =========================================================

/// 将消息列表格式化为对话文本（供 L1 摘要 prompt 使用）。
///
/// 格式:
/// - User 消息: `{user_prefix}{content}`
/// - Assistant 消息: `{assistant_prefix}{content}`
/// - System/Tool 消息: 跳过（不参与摘要）
pub(super) fn format_messages(
    messages: &[ramaria_core::types::Message],
    user_prefix: &str,
    assistant_prefix: &str,
) -> String {
    let mut lines = Vec::with_capacity(messages.len());
    for msg in messages {
        let prefix = match msg.role {
            MessageRole::User => user_prefix,
            MessageRole::Assistant => assistant_prefix,
            // System/Tool 消息不进入摘要上下文
            _ => continue,
        };
        lines.push(format!("{prefix}{}", msg.content));
    }
    lines.join("\n")
}

// =========================================================
// B3 渐进式摘要：触发判断（v1.7）
// =========================================================

/// 判断会话是否触发渐进式摘要。
///
/// 规则（决策 D-V17-005）:
/// - 消息数 > `cfg.msg_threshold`（默认 100），或
/// - 首末消息时间跨度 > `cfg.span_hours`（默认 24 小时）。
///
/// 说明:
/// - 消息列表需按时间升序（调用方保证：`list_messages` 返回升序）。
/// - 单条消息跨度视为 0（不触发时间条件）。
pub(super) fn is_progressive_triggered(
    messages: &[ramaria_core::types::Message],
    cfg: &ramaria_core::config::L1ProgressiveConfig,
) -> bool {
    if messages.len() as u32 > cfg.msg_threshold {
        return true;
    }
    if let (Some(first), Some(last)) = (messages.first(), messages.last()) {
        let span_hours = (last.created_at.saturating_sub(first.created_at)) as f64 / 3_600_000.0;
        if span_hours > cfg.span_hours as f64 {
            return true;
        }
    }
    false
}

// =========================================================
// B2 上下文感知：上一块上文构建（v1.5，§6.3 混合形态）
// =========================================================

/// 构建上一块的上文文本（只注入最近 1 块，不链式）。
///
/// 混合形态（§6.3）:
/// - 上一块消息数 ≤ `prior_context_threshold`（默认 20）→ 注入 L0 原文。
/// - 长块（消息数 > 阈值）→ 注入上一 L1 的摘要 + 结构化线索
///   （`evidence_notes`，含 time/who/cause 槽位）。
/// - 长块但上一 L1 不可用（生成失败降级）→ 回退注入上一块原文并截断到
///   `prior_context_max_chars`（默认 1500 字符），防止超长上文挤占输出预算。
///
/// 隐私: 原文仅作为 LLM prompt 上下文（与摘要生成同链路），不落日志。
pub(super) fn build_prior_context(
    prev_chunk: &crate::utt::UttChunk,
    prev_l1: Option<&MemoryL1>,
    config: &L1SummarizerConfig,
    user_prefix: &str,
    assistant_prefix: &str,
) -> String {
    let is_long = (prev_chunk.msg_count as usize) > config.prior_context_threshold;

    // 短块 → 直接注入 L0 原文（原文信息量最大，无需 L1）
    if !is_long {
        return format_messages(&prev_chunk.messages, user_prefix, assistant_prefix);
    }

    // 长块 → 优先注入上一 L1 摘要 + 结构化线索
    if let Some(prev) = prev_l1 {
        let mut ctx = format!("[上一块摘要] {}", prev.summary);
        if let Some(notes) = prev.evidence_notes.as_ref().filter(|n| !n.is_empty()) {
            let lines: Vec<String> = notes
                .iter()
                .filter_map(|n| {
                    if n.text.trim().is_empty() {
                        None
                    } else {
                        Some(format!("- {}{}", n.text.trim(), slot_suffix(n)))
                    }
                })
                .collect();
            if !lines.is_empty() {
                ctx.push_str("\n[上一块线索]");
                ctx.push_str(&format!("\n{}", lines.join("\n")));
            }
        }
        return ctx;
    }

    // 长块且上一 L1 缺失（降级）→ 注入上一块原文并截断
    let raw = format_messages(&prev_chunk.messages, user_prefix, assistant_prefix);
    ramaria_core::text::truncate_chars(&raw, config.prior_context_max_chars)
}

/// 构造 evidence 线索的可选槽位后缀（`（时间：... · 人物：... · 原因：...）`）。
pub(super) fn slot_suffix(note: &ramaria_core::types::EvidenceNote) -> String {
    let mut parts = Vec::new();
    if let Some(t) = note.time.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("时间：{}", t.trim()));
    }
    if let Some(w) = note.who.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("人物：{}", w.trim()));
    }
    if let Some(c) = note.cause.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("原因：{}", c.trim()));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("（{}）", parts.join(" · "))
    }
}

/// 校验 LLM 输出的 continuation 枚举值。
///
/// 规则（§6.3）:
/// - 合法值三选一：`延续` / `转折` / `无关`，返回校验后的值。
/// - 缺失（None）→ None（正常：LLM 未输出或 prompt 无该字段）。
/// - 非法值 → 置 None 并记 warn（不阻塞生成）。
pub(super) fn validate_continuation(raw: Option<&str>, session_id: Uuid) -> Option<String> {
    match raw.map(str::trim) {
        Some("延续") | Some("转折") | Some("无关") => raw.map(str::trim).map(str::to_string),
        Some(other) if !other.is_empty() => {
            warn!(%session_id, value = %other, "非法的 continuation 值，置为 None");
            None
        }
        _ => None,
    }
}

/// 校验 evidence_notes 字段（v1.4 结构化格式）。
///
/// 校验规则:
/// 1. 输入为 None 或空数组 → 降级为空数组，记 warn
/// 2. 每条 evidence 的 `text` trim 后 < 5 字符 → 丢弃该条，记 debug
/// 3. 可选槽位 `time`/`who`/`cause` 规范化：trim 后为空字符串视为缺失 → 置 None
///    （v3.1 §6.3：cause 缺失时槽位置空，不阻塞生成）
/// 4. 丢弃后数组为空 → 降级为空数组，记 warn
///
/// 返回:
/// - `Vec<EvidenceNote>`：经过滤的有效 evidence 列表（可能为空）
pub(super) fn validate_evidence_notes(
    raw: Option<Vec<EvidenceNote>>,
    session_id: Uuid,
) -> Vec<EvidenceNote> {
    let raw_list = match raw {
        Some(list) if !list.is_empty() => list,
        _ => {
            warn!(%session_id, "LLM 未产出 evidence_notes 或为空数组，降级为空");
            return vec![];
        }
    };

    // 过滤过短条目（校验 text 槽位）+ 规范化可选槽位（空白视为缺失）
    let valid: Vec<EvidenceNote> = raw_list
        .into_iter()
        .map(|mut note| {
            // text 必填槽位：trim 后参与长度校验
            note.text = note.text.trim().to_string();
            // 可选槽位：trim 后为空字符串 → 置 None（保持"缺省即无"的语义，
            // 避免下游把空字符串误当作有效槽位值）
            note.time = normalize_optional_slot(note.time);
            note.who = normalize_optional_slot(note.who);
            note.cause = normalize_optional_slot(note.cause);
            note
        })
        .filter(|note| {
            let ok = note.text.chars().count() >= 5;
            if !ok {
                // 隐私红线：evidence_notes 承载原文级信息，日志只记长度不记内容
                debug!(%session_id, len = note.text.chars().count(), "evidence 过短（<5 字符），丢弃");
            }
            ok
        })
        .collect();

    if valid.is_empty() {
        warn!(%session_id, "所有 evidence_notes 条目均不满足最小长度要求，降级为空");
    }

    valid
}

/// 规范化可选槽位值：trim 后为空字符串（或仅空白）视为缺失 → None。
///
/// 说明:
/// - LLM 可能输出 `"time": ""` 或 `"cause": "  "` 这类空值，
///   与缺失槽位（JSON 省略该键）语义等价，统一归一为 None。
/// - 非空值保留 trim 后的内容，避免首尾空白污染下游消费。
pub(super) fn normalize_optional_slot(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// =========================================================
// 纯函数辅助
// =========================================================

/// 解析关键词字符串为 `(存储用的逗号分隔字符串, 标准化关键词列表)`。
///
/// 如果输入为空或仅含空白字符，返回 `(None, vec![])`。
/// 返回 `Vec<KeywordToken>` 替代裸 `String`。
pub(super) fn parse_keywords(raw: Option<&str>) -> (Option<String>, Vec<KeywordToken>) {
    let cleaned = raw.map(|s| s.trim()).filter(|s| !s.is_empty());
    match cleaned {
        None => (None, vec![]),
        Some(s) => {
            let list: Vec<KeywordToken> = s
                .split(',')
                .map(|k| k.trim())
                .filter(|k| !k.is_empty())
                .filter_map(KeywordToken::new)
                .collect();
            // 存储时使用逗号分隔字符串
            (Some(s.to_string()), list)
        }
    }
}
