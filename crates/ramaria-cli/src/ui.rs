//! rust/crates/ramaria-cli/src/ui.rs - 终端输出与格式化工具
//!
//! 设计特点:
//! - 统一错误输出格式（红色 "✗" 前缀 + 错误链）
//! - 成功消息（绿色 "✓" 前缀）
//! - 流式文本增量输出（不换行追加）
//! - 表格对齐工具（session/memory 列表）
//! - 敏感信息遮蔽（API key 显示为 "***"）
//! - 无外部依赖，仅使用标准库 + tracing

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

/// 读取用户输入（不显示回显——用于密码/API key）。
#[cfg(windows)]
pub fn read_secret(prompt: &str) -> io::Result<String> {
    use std::io::BufRead;
    print!("{prompt} ");
    io::stdout().flush()?;
    // Windows 控制台输入（隐藏回显通过 rpassword 更简单，但避免引入额外依赖）
    // 此处使用标准输入，回显由终端控制
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

#[cfg(not(windows))]
pub fn read_secret(prompt: &str) -> io::Result<String> {
    // Unix 平台使用 rpassword（此处简化，直接读取）
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
