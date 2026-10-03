//! crates/ramaria-desktop/src/webview.rs - WebView2 远程调试端口清理
//!
//! 设计特点:
//! - release 构建启动时剥离 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` 中的
//!   远程调试端口参数，阻断经环境变量注入打开调试端口
//! - debug 构建不做任何改动，保留本地开发调试能力
//! - 参数解析为纯函数（可单测），环境变量读写集中在启动早期单线程阶段

/// 从 WebView2 附加启动参数中剥离远程调试端口参数。
///
/// 职责:
/// - 按空白分词，去除所有 `--remote-debugging-port` 形式的 token
///   （独立 token 或 `--remote-debugging-port=<port>`）；
///   独立 token 后紧跟的纯数字端口 token 一并去除（空格分隔形态）
/// - 其余 token 原样保留，以单空格重组
///
/// 参数:
/// - `args`: WebView2 附加启动参数字符串
///
/// 返回:
/// - 未包含目标参数时返回 `None`（表示无需修改）
/// - 包含目标参数时返回重组结果（可能为空字符串）
// debug 构建不启用清理逻辑，本函数仅被单测引用
#[cfg_attr(debug_assertions, allow(dead_code))]
fn strip_remote_debugging_port(args: &str) -> Option<String> {
    let mut found = false;
    let mut skip_next_port = false;
    let mut kept: Vec<&str> = Vec::new();

    for token in args.split_whitespace() {
        if skip_next_port {
            skip_next_port = false;
            // 空格分隔形态的端口数字（如 `--remote-debugging-port 9222`）一并剥离
            if token.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
        }
        if token == "--remote-debugging-port" {
            found = true;
            skip_next_port = true;
        } else if token.starts_with("--remote-debugging-port=") {
            found = true;
        } else {
            kept.push(token);
        }
    }

    if !found {
        return None;
    }
    Some(kept.join(" "))
}

/// 启动时清理 WebView2 附加启动参数中的远程调试端口。
///
/// 说明:
/// - 未设置 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` 或不含目标参数时静默返回
/// - 清理后参数为空时移除环境变量，否则回写剩余参数
/// - 日志只记录"已清理"事实，不记录参数原值
/// - 仅在 release 构建由 `run` 调用，且位于任何线程创建之前
#[cfg(not(debug_assertions))]
pub(crate) fn sanitize_webview2_debug_args() {
    const KEY: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

    let Ok(current) = std::env::var(KEY) else {
        return;
    };
    let Some(cleaned) = strip_remote_debugging_port(&current) else {
        return;
    };

    // 启动早期单线程阶段（任何线程创建之前），无并发读者
    unsafe {
        if cleaned.is_empty() {
            std::env::remove_var(KEY);
        } else {
            std::env::set_var(KEY, &cleaned);
        }
    }

    tracing::warn!("已清理 WebView2 附加启动参数中的远程调试端口");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_removes_equals_form() {
        assert_eq!(
            strip_remote_debugging_port("--remote-debugging-port=9222"),
            Some(String::new())
        );
    }

    #[test]
    fn strip_removes_bare_flag_token() {
        assert_eq!(
            strip_remote_debugging_port("--remote-debugging-port"),
            Some(String::new())
        );
    }

    #[test]
    fn strip_keeps_other_args() {
        assert_eq!(
            strip_remote_debugging_port("--disable-gpu --remote-debugging-port=9222 --lang=zh-CN"),
            Some(String::from("--disable-gpu --lang=zh-CN"))
        );
    }

    #[test]
    fn strip_returns_none_without_target() {
        assert_eq!(
            strip_remote_debugging_port("--disable-gpu --lang=zh-CN"),
            None
        );
        assert_eq!(strip_remote_debugging_port(""), None);
    }

    #[test]
    fn strip_handles_target_at_edges() {
        // 目标参数位于末尾
        assert_eq!(
            strip_remote_debugging_port("--disable-gpu --remote-debugging-port=9222"),
            Some(String::from("--disable-gpu"))
        );
        // 目标参数位于行中
        assert_eq!(
            strip_remote_debugging_port("--remote-debugging-port=9222 --disable-gpu"),
            Some(String::from("--disable-gpu"))
        );
    }

    #[test]
    fn strip_normalizes_whitespace() {
        assert_eq!(
            strip_remote_debugging_port("  --disable-gpu   --remote-debugging-port=9222  "),
            Some(String::from("--disable-gpu"))
        );
    }

    #[test]
    fn strip_removes_space_separated_port() {
        assert_eq!(
            strip_remote_debugging_port("--disable-gpu --remote-debugging-port 9222 --lang=zh-CN"),
            Some(String::from("--disable-gpu --lang=zh-CN"))
        );
    }

    #[test]
    fn strip_keeps_non_numeric_token_after_flag() {
        // 跟随 token 非端口数字时保留（只剥 flag 本身）
        assert_eq!(
            strip_remote_debugging_port("--remote-debugging-port foo"),
            Some(String::from("foo"))
        );
    }
}
