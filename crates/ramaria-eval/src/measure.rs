//! crates/ramaria-eval/src/measure.rs - 评测测量原语
//!
//! 设计特点:
//! - token 估算复用产品口径（经服务层转出），不复制实现、不直连 memory
//! - 注入规模按条目逐条累计，不先拼接文本，保持与产品逐条注入一致
//! - 耗时取单调时钟毫秒数并饱和到 u64，不回绕
//! - RSS 取进程工作集（含共享页），只适合同机趋势对照；读取失败返回 None

use std::time::Instant;

#[cfg(test)]
mod tests;

/// 估算文本的 token 数（产品口径）。
pub fn estimate_tokens(text: &str) -> usize {
    ramaria_service::estimate_tokens(text)
}

/// 一次记忆注入的规模。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InjectionMeasure {
    /// 注入条目数
    pub items: usize,
    /// 各条字符数之和（按 Unicode 字符计）
    pub chars: usize,
    /// 各条 token 估算之和
    pub tokens: usize,
}

/// 统计一组注入条目的规模。
///
/// 说明: token 按条目逐条估算后求和，不先拼接再估算——
/// 拼接会改变字符类型分布与条目边界，与产品逐条注入时的预算口径不一致。
pub fn measure_injection<S: AsRef<str>>(items: &[S]) -> InjectionMeasure {
    let mut measure = InjectionMeasure {
        items: items.len(),
        chars: 0,
        tokens: 0,
    };
    for item in items {
        let text = item.as_ref();
        measure.chars += text.chars().count();
        measure.tokens += estimate_tokens(text);
    }
    measure
}

/// 从计时起点到当前的毫秒数。
pub fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// 当前进程的常驻工作集字节数（Windows）。
///
/// 说明:
/// - 取 `WorkingSetSize`，含共享页，只作同机趋势对照，不作跨机绝对值比较；
/// - 读取失败返回 None，不中断评测。
#[cfg(windows)]
pub fn current_rss_bytes() -> Option<u64> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // safety: 伪句柄指向当前进程，计数器结构由本地栈持有且长度字段与类型一致；
    // 调用失败时不读取可能未被写入的字段。
    let result = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    result.ok().map(|()| counters.WorkingSetSize as u64)
}

/// 当前进程的常驻工作集字节数（非 Windows 平台无实现）。
#[cfg(not(windows))]
pub fn current_rss_bytes() -> Option<u64> {
    None
}
