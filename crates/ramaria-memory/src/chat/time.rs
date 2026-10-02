//! crates/ramaria-memory/src/chat/time.rs - 对话层共享时间格式化
//!
//! 设计特点:
//! - 统一本地时区 `YYYY-MM-DD HH:MM` 格式化口径
//! - 供消息时间戳与 System Prompt 当前时间等消费方复用
//! - 纯函数，无 I/O 与副作用

// =========================================================
// 共享时间格式化
// =========================================================

/// 返回当前时间的 `YYYY-MM-DD HH:MM` 字符串（本地时区）。
///
/// 用途: 消息时间戳、System Prompt 当前时间等共享格式化。
pub fn now_timestamp_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}
