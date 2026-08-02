//! rust/crates/ramaria-desktop/src/notification.rs - 桌面通知模块
//!
//! 设计特点:
//! - 封装 tauri-plugin-notification，提供简洁的通知发送接口
//! - 智能通知：仅在主窗口不可见时发送（避免打扰用户）
//! - 通知内容限制：标题 ≤ 60 字符，正文 ≤ 200 字符（防止超大通知）
//! - 通知超时由系统默认管理（Windows 约 7 秒，可配置）
//! - 失败静默降级：通知发送失败仅记录日志，不抛出错误
//! - 与 tray.rs 联动：通过 is_main_window_visible 判断是否需要通知
//!
//! 触发场景（设计决策）:
//! - chat-done: LLM 回复完成且窗口不可见 → 发送通知
//! - 不发送：chat-delta（增量太频繁）、chat-error（应在界面上展示）
//! - 未来扩展：索引完成、后台任务失败等

use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

/// 通知内容长度限制
const MAX_TITLE_LEN: usize = 60;
/// 通知正文字符上限
const MAX_BODY_LEN: usize = 200;

/// 聊天回复通知的标题前缀
const CHAT_NOTIFICATION_TITLE: &str = "Ramaria 回复";

/// 聊天回复通知正文预览长度（从回复中截取前 N 个字符）
const CHAT_PREVIEW_LEN: usize = 80;

// =========================================================
// 公开 API
// =========================================================

/// 发送桌面通知。
///
/// 参数:
/// - `app_handle`: Tauri AppHandle（用于调用 notification plugin）
/// - `title`: 通知标题（超过 MAX_TITLE_LEN 会被截断）
/// - `body`: 通知正文（超过 MAX_BODY_LEN 会被截断并加 "…"）
///
/// 说明:
/// - 如果标题或正文为空，静默跳过（不发送空通知）
/// - 通知发送失败仅记录 warning 日志，不传播错误
/// - 使用 tauri-plugin-notification 的 builder API
pub fn send_notification<R: Runtime>(app_handle: &AppHandle<R>, title: &str, body: &str) {
    // 参数校验
    if title.is_empty() {
        tracing::warn!("通知标题为空，跳过发送");
        return;
    }

    // 截断过长的标题和正文
    let title_trimmed = truncate_str(title, MAX_TITLE_LEN);
    let body_trimmed = if body.is_empty() {
        "" // 允许空正文（仅标题）
    } else {
        truncate_str(body, MAX_BODY_LEN)
    };

    tracing::debug!(
        title = %title_trimmed,
        body_len = body_trimmed.chars().count(),
        "发送桌面通知"
    );

    // 调用 tauri-plugin-notification
    match app_handle
        .notification()
        .builder()
        .title(title_trimmed.to_string())
        .body(body_trimmed.to_string())
        .show()
    {
        Ok(_) => {
            tracing::debug!("桌面通知发送成功");
        }
        Err(e) => {
            // 通知失败不是致命错误，静默降级
            tracing::warn!(
                error = %e,
                "桌面通知发送失败（可能用户禁用了通知权限）"
            );
        }
    }
}

/// 发送聊天回复完成通知。
///
/// 说明:
/// - 仅在主窗口不可见时发送（由 tray::is_main_window_visible 判断）
/// - 通知正文为 LLM 回复的前 N 个字符预览 + "…"
/// - 窗口可见时静默跳过（用户正在看界面，无需通知）
///
/// 参数:
/// - `app_handle`: Tauri AppHandle
/// - `reply_preview`: LLM 回复的前若干字符（用于通知正文预览）
/// - `total_chars`: 总回复字符数（用于日志记录，不显示在通知中）
pub fn send_chat_notification<R: Runtime>(
    app_handle: &AppHandle<R>,
    reply_preview: &str,
    total_chars: usize,
) {
    // 智能判断：窗口可见时不发通知
    if crate::tray::is_main_window_visible(app_handle) {
        tracing::trace!(total_chars = total_chars, "主窗口可见，跳过聊天通知");
        return;
    }

    // 构建通知正文
    let body = if reply_preview.is_empty() {
        format!("收到一条回复（{} 字）", total_chars)
    } else {
        let preview = truncate_str(reply_preview, CHAT_PREVIEW_LEN);
        if preview.len() < reply_preview.chars().count() {
            format!("{}…", preview)
        } else {
            preview.to_string()
        }
    };

    send_notification(app_handle, CHAT_NOTIFICATION_TITLE, &body);
}

// =========================================================
// 辅助函数
// =========================================================

/// 按字符边界安全截断字符串。
///
/// 说明:
/// - 按 Unicode 字符（而非字节）计数，正确处理中文等多字节字符
/// - 超过 max_len 时截断到 max_len，不添加省略号（调用方负责添加）
///
/// 参数:
/// - `s`: 原始字符串
/// - `max_len`: 最大字符数
///
/// 返回:
/// - 截断后的字符串（如果原字符串 ≤ max_len 则返回原样的 &str）
fn truncate_str(s: &str, max_len: usize) -> &str {
    if max_len == 0 {
        return "";
    }
    let char_count = s.chars().count();
    if char_count <= max_len {
        return s;
    }

    // 找到第 max_len 个字符的字节偏移
    let mut byte_pos = 0;
    let mut count = 0;
    for (i, _) in s.char_indices() {
        if count >= max_len {
            break;
        }
        byte_pos = i;
        count += 1;
    }

    // 如果刚好在字符边界结束，byte_pos 指向最后一个字符
    // 需要包含这个字符的长度
    if count == max_len && byte_pos < s.len() {
        // 计算最后一个字符占用的字节数
        let last_char = s[byte_pos..].chars().next().unwrap_or(' ');
        &s[..byte_pos + last_char.len_utf8()]
    } else {
        &s[..byte_pos]
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_str_cases() {
        let cases = [
            ("hello", 10, "hello"),        // 未超长原样
            ("hello world", 5, "hello"),   // ASCII 截断
            ("你好世界", 2, "你好"),       // 中文按字符截断
            ("Hi你好world", 5, "Hi你好w"), // 中英混合
            ("", 5, ""),                   // 空串
            ("abc", 3, "abc"),             // 恰好边界
            ("hello", 0, ""),              // max=0
            ("ひらがな", 2, "ひら"),       // 日文假名
        ];
        for (input, max, expected) in cases {
            assert_eq!(
                truncate_str(input, max),
                expected,
                "input={input:?} max={max}"
            );
        }
    }

    #[test]
    fn constants_are_reasonable() {
        // 确保常量值在合理范围内
        assert!(MAX_TITLE_LEN > 0 && MAX_TITLE_LEN <= 120);
        assert!(MAX_BODY_LEN > 0 && MAX_BODY_LEN <= 500);
        assert!(CHAT_PREVIEW_LEN > 0 && CHAT_PREVIEW_LEN <= 200);
    }
}
