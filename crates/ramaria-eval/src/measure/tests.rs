//! crates/ramaria-eval/src/measure/tests.rs - 测量原语单元测试
//!
//! 设计特点:
//! - token 估算对已知输入断言产品口径
//! - 注入规模按条目逐条累计（条目数 / 字符数 / token 数）
//! - 耗时为真实等待的下界
//! - RSS 按平台给值或 None

use super::*;

/// token 估算：空串为 0，中文按 2 字符/token，英文按 4 字符/token。
#[test]
fn estimate_tokens_known_values() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("hello"), 2);
    assert_eq!(estimate_tokens("你好"), 1);
    assert_eq!(estimate_tokens("你好世界"), 2);
}

/// 注入规模按条目数、字符数、逐条 token 之和累计。
#[test]
fn measure_injection_sums_per_item() {
    let measure = measure_injection(&["你好", "hello"]);
    assert_eq!(measure.items, 2);
    assert_eq!(measure.chars, 7, "2 个中文字符 + 5 个拉丁字符");
    assert_eq!(measure.tokens, 3, "逐条估算 1 + 2");
}

/// 耗时计入真实等待，且以毫秒返回。
#[test]
fn elapsed_ms_tracks_wall_clock() {
    let started = std::time::Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(elapsed_ms(started) >= 10, "20ms 等待后不应只得到个位数毫秒");
}

/// RSS：Windows 可取到正值；其他平台明确返回 None。
#[test]
fn current_rss_bytes_matches_platform() {
    #[cfg(windows)]
    {
        let rss = current_rss_bytes().expect("Windows 应可读取进程工作集");
        assert!(rss > 0, "工作集应为正，实际 {rss}");
    }
    #[cfg(not(windows))]
    {
        assert!(current_rss_bytes().is_none(), "非 Windows 平台不提供 RSS");
    }
}
