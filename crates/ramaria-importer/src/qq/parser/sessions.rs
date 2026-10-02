//! crates/ramaria-importer/src/qq/parser/sessions.rs - 消息流会话切割
//!
//! 设计特点:
//! - 单次遍历 O(n)，按时间阈值切断为新 session，严守 gap_ms 语义
//! - 单调性: 输入已排序时输出 session 时间不重叠
//! - 无回溯: 不跨 session 合并
//! - 空安全: 输入为空返回空 Vec，不 panic

use crate::traits::{ImportedSession, ParsedMessage};

// =========================================================
// Session 切割
// =========================================================

/// 按时间间隔将消息列表切割为若干 session。
///
/// 算法: 单次遍历 O(n)，严守时间阈值语义。
///
/// 关键性质:
/// - **单调性**：输入已排序，输出 session 时间不重叠。
/// - **无回溯**：不跨 session 合并。
/// - **空安全**：输入为空返回空 Vec，不 panic。
///
/// 参数:
/// - `messages`: 已按时间排序的消息列表。
/// - `gap_ms`: 时间间隔阈值（毫秒），超出此间隔即切断为新 session。
///
/// 返回:
/// - 切割后的 session 列表。
pub(super) fn split_into_sessions(messages: &[ParsedMessage], gap_ms: i64) -> Vec<ImportedSession> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut sessions: Vec<ImportedSession> = Vec::new();
    let mut current: Vec<ParsedMessage> = vec![messages[0].clone()];

    for msg in &messages[1..] {
        let last_ts = current.last().map(|m| m.created_at).unwrap_or(0);
        if msg.created_at - last_ts > gap_ms {
            // 时间间隔超出阈值 → 切断为新 session
            let started_at = current.first().map(|m| m.created_at).unwrap_or(0);
            let ended_at = current.last().map(|m| m.created_at).unwrap_or(0);
            sessions.push(ImportedSession {
                messages: std::mem::take(&mut current),
                started_at,
                ended_at,
            });
            current.push(msg.clone());
        } else {
            current.push(msg.clone());
        }
    }

    // flush 最后一个 session
    if !current.is_empty() {
        let started_at = current.first().map(|m| m.created_at).unwrap_or(0);
        let ended_at = current.last().map(|m| m.created_at).unwrap_or(0);
        sessions.push(ImportedSession {
            messages: current,
            started_at,
            ended_at,
        });
    }

    sessions
}
