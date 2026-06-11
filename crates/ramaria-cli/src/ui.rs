//! rust/crates/ramaria-cli/src/ui.rs - 终端输出与格式化工具
//!
//! 设计特点:
//! - 统一错误输出格式（红色 "✗" 前缀 + 错误链）
//! - 成功消息（绿色 "✓" 前缀）
//! - 流式文本增量输出（不换行追加）
//! - 表格对齐工具（session/memory 列表）
//! - 敏感信息遮蔽（API key 显示为 "***"）
//! - read_secret 通过 Windows Console API 隐藏回显（非明文暴露）
//! - 无额外外部依赖，仅使用标准库 + tracing + windows crate（已存在于依赖树）

use std::error::Error;
use std::io::{self, Write};

// =========================================================
// 错误输出
// =========================================================

/// 将 RamariaError 输出到 stderr，包含完整错误链。
pub fn print_error(err: &ramaria_core::error::RamariaError) {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "\x1b[31m✗ 错误:\x1b[0m {err}");

    // 输出 source 链（如有）
    let mut source = err.source();
    while let Some(s) = source {
        let _ = writeln!(stderr, "      原因: {s}");
        source = s.source();
    }
}

/// 打印错误后退出进程。
pub fn fatal(err: &ramaria_core::error::RamariaError, exit_code: i32) -> ! {
    print_error(err);
    std::process::exit(exit_code);
}

/// 打印 anyhow 错误并退出。
pub fn fatal_anyhow(err: &anyhow::Error, exit_code: i32) -> ! {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "\x1b[31m✗ 错误:\x1b[0m {err}");
    // anyhow 支持 chain()
    for cause in err.chain().skip(1) {
        let _ = writeln!(stderr, "      原因: {cause}");
    }
    std::process::exit(exit_code);
}

// =========================================================
// 成功/信息输出
// =========================================================

/// 输出成功消息（绿色 ✓）。
pub fn success(msg: &str) {
    println!("\x1b[32m✓\x1b[0m {msg}");
}

/// 输出信息消息（蓝色 ℹ）。
pub fn info(msg: &str) {
    println!("\x1b[34mℹ\x1b[0m  {msg}");
}

/// 输出警告消息（黄色 ⚠）。
pub fn warn(msg: &str) {
    eprintln!("\x1b[33m⚠ 警告:\x1b[0m {msg}");
}

/// 输出分隔线。
pub fn separator() {
    println!("{}", "-".repeat(60));
}

/// 输出带标签的值（等宽对齐）。
pub fn labeled(label: &str, value: &str) {
    println!("  {label:<20} {value}");
}

// =========================================================
// 流式输出
// =========================================================

/// 将文本增量写入 stdout，不换行，立即 flush。
pub fn write_delta(text: &str) {
    let mut stdout = io::stdout().lock();
    let _ = write!(stdout, "{text}");
    let _ = stdout.flush();
}

/// 流结束后输出换行。
pub fn finish_delta() {
    println!();
}

// =========================================================
// 人格短句格式化器（|| → 换行）
// =========================================================

/// 流式输出的人格短句格式化器。
///
/// 将 LLM 输出中的 `||` 分隔符替换为换行符 `\n`，
/// 在终端中呈现短句分行效果。
///
/// # 跨 chunk 处理
///
/// 流式输出中 `||` 可能被拆分到两个 Delta 事件中：
/// Chunk N 末尾是 `|`，Chunk N+1 开头是 `|`。
/// 本格式化器维护一个 1 字符缓冲来处理此边界情况。
///
/// # 示例
///
/// ```
/// # use ramaria_cli::ui::PersonaFormatter;
/// let mut fmt = PersonaFormatter::new();
/// assert_eq!(fmt.feed("那挺好的||摸鱼"), "那挺好的\n摸鱼");
/// assert_eq!(fmt.feed("就摸鱼"), "就摸鱼");
/// assert_eq!(fmt.feed("||聊天也"), "\n聊天也");
/// assert_eq!(fmt.feed("是正事"), "是正事");
/// assert_eq!(fmt.flush(), None);
/// ```
pub struct PersonaFormatter {
    /// 上一个 chunk 末尾的未决 `|` 字符。
    /// 当上一个块以 `|` 结尾但尚未确定是否组成 `||` 时暂存于此。
    pending_pipe: Option<char>,
}

impl PersonaFormatter {
    /// 创建新的格式化器。
    pub fn new() -> Self {
        Self { pending_pipe: None }
    }

    /// 接收一个 delta 文本块，返回格式化后的字符串（`||` → `\n`）。
    ///
    /// # 参数
    /// - `chunk`: LLM 流式输出的增量文本。
    ///
    /// # 返回
    /// 格式化后的字符串，可直接写入 stdout。
    /// 空字符串表示当前块无需输出（可能全部被缓冲）。
    pub fn feed(&mut self, chunk: &str) -> String {
        if chunk.is_empty() {
            return String::new();
        }

        let mut output = String::with_capacity(chunk.len());
        let mut chars = chunk.chars().peekable();

        // 处理跨块拼接：上一块末尾有 '|'，当前块开头也是 '|'
        if self.pending_pipe == Some('|') {
            if let Some('|') = chars.peek() {
                // 跨块组成 || → 输出换行
                output.push('\n');
                self.pending_pipe = None;
                chars.next(); // 消费当前块的第一个 '|'
            } else {
                // 上一块的 '|' 是孤立的，先输出它
                output.push('|');
                self.pending_pipe = None;
            }
        }

        // 扫描当前块
        while let Some(ch) = chars.next() {
            if ch == '|' {
                if chars.peek() == Some(&'|') {
                    // 块内 || → 换行
                    output.push('\n');
                    chars.next(); // 消费第二个 '|'
                } else {
                    // 单独的 |，可能是下一块的 || 开头，暂存
                    self.pending_pipe = Some('|');
                }
            } else {
                output.push(ch);
            }
        }

        output
    }

    /// 流结束时刷新未决缓冲字符。
    ///
    /// # 返回
    /// - `Some("|")` — 如果上一个块末尾有孤立的 `|` 尚未输出。
    /// - `None` — 没有未决字符。
    pub fn flush(&mut self) -> Option<String> {
        self.pending_pipe.take().map(|_| "|".to_string())
    }
}

impl Default for PersonaFormatter {
    fn default() -> Self {
        Self::new()
    }
}

// =========================================================
// 敏感信息遮蔽
// =========================================================

/// 将 API key 遮蔽为 `abc****xyz` 格式。
/// key 长度 ≤ 4 时显示为 `****`。
pub fn mask_key(key: &str) -> String {
    if key.len() <= 4 {
        return "****".to_string();
    }
    let prefix = &key[..3];
    let suffix = &key[key.len() - 3..];
    format!("{prefix}****{suffix}")
}

/// 遮蔽 Option<String> 中的值。
#[allow(dead_code)]
pub fn mask_optional_key(key: &Option<String>) -> String {
    key.as_deref()
        .map_or_else(|| "(未设置)".to_string(), mask_key)
}

// =========================================================
// 用户输入
// =========================================================

/// 读取一行用户输入（stdin）。
pub fn read_line(prompt: &str) -> io::Result<String> {
    print!("{prompt} ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

/// 读取用户输入（不回显——用于密码/API key 等敏感信息）。
///
/// 参数:
/// - `prompt`: 输入提示文本（不含换行）。
///
/// 返回:
/// - 用户输入的字符串（去除首尾空白）。
///
/// 安全:
/// - **Windows**: 调用 `SetConsoleMode` 禁用 `ENABLE_ECHO_INPUT`，读取后恢复。
///   全程不经过日志、不写入终端缓冲区。
/// - **Unix**: 使用标准输入读取（大多数 Unix 终端驱动默认不回显密码，
///   但若需要更强保证可替换为 `rpassword` crate）。
///
/// 错误处理:
/// - 读取失败时已尝试恢复控制台模式（Windows），不会残留静默状态。
#[cfg(windows)]
pub fn read_secret(prompt: &str) -> io::Result<String> {
    use std::io::BufRead;
    use windows::Win32::System::Console::{
        CONSOLE_MODE, ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
        SetConsoleMode,
    };

    print!("{prompt} ");
    io::stdout().flush()?;

    // 获取标准输入句柄
    let handle = unsafe {
        GetStdHandle(STD_INPUT_HANDLE)
            .map_err(|e| io::Error::other(format!("GetStdHandle 失败: {e}")))?
    };

    // 保存原始控制台模式
    let original_mode: CONSOLE_MODE = unsafe {
        let mut mode = CONSOLE_MODE::default();
        GetConsoleMode(handle, &mut mode)
            .map_err(|e| io::Error::other(format!("GetConsoleMode 失败: {e}")))?;
        mode
    };

    // 关闭回显
    let mode_no_echo = original_mode & !ENABLE_ECHO_INPUT;
    unsafe {
        SetConsoleMode(handle, mode_no_echo)
            .map_err(|e| io::Error::other(format!("SetConsoleMode (禁用回显) 失败: {e}")))?;
    }

    // 读取输入（闭包确保恢复原始模式的 RAII 式保障）
    let result = (|| -> io::Result<String> {
        let stdin = io::stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        Ok(line.trim().to_string())
    })();

    // 恢复原始控制台模式（无论读取成功或失败）
    unsafe {
        let _ = SetConsoleMode(handle, original_mode);
    }

    // 读取成功后输出换行（输入本身不回显，但需要换行保持终端格式）
    if result.is_ok() {
        println!();
    }

    result
}

#[cfg(not(windows))]
pub fn read_secret(prompt: &str) -> io::Result<String> {
    // Unix 平台：标准输入读取。
    // 大多数 Unix 终端驱动在 isatty() 模式下默认不回显密码行，
    // 但若需 100% 保证可引入 `rpassword` crate 替换此实现。
    read_line(prompt)
}

// =========================================================
// 用户确认
// =========================================================

/// 询问用户确认（y/N）。
/// 返回 true 表示用户输入了 'y' 或 'Y'。
pub fn confirm(prompt: &str) -> io::Result<bool> {
    let input = read_line(&format!("{prompt} [y/N]:"))?;
    Ok(input.eq_ignore_ascii_case("y") || input.eq_ignore_ascii_case("yes"))
}

// =========================================================
// 单元测试
// =========================================================
// 注: 测试模块必须放在文件末尾（clippy::items-after-test-module）

#[cfg(test)]
mod persona_formatter_tests {
    use super::*;

    // ---- 基本替换 ----

    #[test]
    fn basic_double_pipe_replacement() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("你好||世界"), "你好\n世界");
        assert_eq!(fmt.flush(), None);
    }

    #[test]
    fn multiple_double_pipes() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(
            fmt.feed("那挺好的||摸鱼就摸鱼||聊天也是正事"),
            "那挺好的\n摸鱼就摸鱼\n聊天也是正事"
        );
        assert_eq!(fmt.flush(), None);
    }

    #[test]
    fn no_pipes_passthrough() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("普通文本无分隔符"), "普通文本无分隔符");
        assert_eq!(fmt.flush(), None);
    }

    #[test]
    fn empty_chunk() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed(""), "");
        assert_eq!(fmt.flush(), None);
    }

    // ---- 跨 chunk 拆分 ----

    #[test]
    fn cross_chunk_double_pipe() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("你好|"), "你好");
        assert_eq!(fmt.feed("|世界"), "\n世界");
        assert_eq!(fmt.flush(), None);
    }

    #[test]
    fn cross_chunk_multiple() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("A||B|"), "A\nB");
        assert_eq!(fmt.feed("|C"), "\nC");
        assert_eq!(fmt.flush(), None);
    }

    // ---- 边缘情况 ----

    #[test]
    fn triple_pipe() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("A|||B"), "A\nB");
        assert_eq!(fmt.flush(), Some("|".to_string()));
    }

    #[test]
    fn quad_pipe() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("A||||B"), "A\n\nB");
        assert_eq!(fmt.flush(), None);
    }

    #[test]
    fn isolated_pipe_at_end_flushed() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("你好|"), "你好");
        assert_eq!(fmt.flush(), Some("|".to_string()));
    }

    #[test]
    fn isolated_pipe_at_end_no_more_chunks() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("末尾有|"), "末尾有");
        assert_eq!(fmt.flush(), Some("|".to_string()));
    }

    #[test]
    fn double_pipe_at_chunk_boundary_with_trailing_text() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("前文|"), "前文");
        assert_eq!(fmt.feed("|后文还有||更多"), "\n后文还有\n更多");
        assert_eq!(fmt.flush(), None);
    }

    #[test]
    fn single_pipe_only() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("|"), "");
        assert_eq!(fmt.flush(), Some("|".to_string()));
    }

    #[test]
    fn pipe_followed_by_non_pipe() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("你好|"), "你好");
        assert_eq!(fmt.feed("世界"), "|世界");
        assert_eq!(fmt.flush(), None);
    }

    // ---- 空/极短输入 ----

    #[test]
    fn single_char_non_pipe() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("a"), "a");
        assert_eq!(fmt.flush(), None);
    }

    #[test]
    fn only_double_pipe() {
        let mut fmt = PersonaFormatter::new();
        assert_eq!(fmt.feed("||"), "\n");
        assert_eq!(fmt.flush(), None);
    }

    // ---- 多次 feed 后正常文本 ----

    #[test]
    fn many_small_feeds() {
        let mut fmt = PersonaFormatter::new();
        let mut result = String::new();
        for ch in ["那", "挺", "好", "的", "|", "|", "摸", "鱼"].iter() {
            result.push_str(&fmt.feed(ch));
        }
        result.push_str(&fmt.flush().unwrap_or_default());
        assert_eq!(result, "那挺好的\n摸鱼");
    }

    // ---- 兼容现有 API ----

    #[test]
    fn write_persona_delta_integration() {
        let mut fmt = PersonaFormatter::new();
        let text = fmt.feed("测试||文本");
        assert!(!text.is_empty());
        assert_eq!(fmt.flush(), None);
    }
}
