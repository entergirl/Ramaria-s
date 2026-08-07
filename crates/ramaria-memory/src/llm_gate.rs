//! rust/crates/ramaria-memory/src/llm_gate.rs - LLM 批量请求节流（L1/L2 共用）
//!
//! 背景:
//! - 导入/空闲批量封存/事件提取会**连续**发起多个 LLM 请求：
//!   - L1: 导入 N 个 session 生成摘要（N × 2 次调用）、空闲批量封存；
//!   - L2: 事件提取逐簇调用。
//! - 短时间密集调用易触发远程 API 速率限制（典型表现：HTTP 200 + 空内容，
//!   或 HTTP 429），导致摘要/事件提取失败并进入重试。
//! - L2 事件提取此前已有"簇间延迟"（`[thresholds].cluster_delay_ms`，默认
//!   800ms），本模块将其抽象为公共函数，供 L1 批量摘要与 L2 事件提取共用，
//!   消除 L1 路径无节流的空白。
//!
//! 设计:
//! - 纯 async 函数，零状态：每次 LLM 调用后调用一次即可保证请求间隔。
//! - `delay_ms <= 0` 时立即返回（测试/本地场景/配置关闭）。
//! - 日志只记延迟与上下文，不涉及任何对话内容（隐私红线）。
//!
//! 配置来源:
//! - 统一使用 `[thresholds].cluster_delay_ms`（语义扩展为"批量 LLM 请求间
//!   最小间隔"，配置名保持兼容，不新增配置键）。

// =========================================================
// 公共 API
// =========================================================

/// LLM 批量请求间最小间隔节流。
///
/// 用法: 在批量循环中，**每次** LLM 调用完成后调用一次，保证相邻两次
/// 请求的间隔 ≥ `delay_ms`，避免触发远程 API 速率限制。
///
/// 参数:
/// - `delay_ms`: 请求间最小间隔（毫秒）。`<= 0` 时跳过（不等待）。
/// - `context`: 日志上下文（如 `"L1 导入批量摘要"` / `"L2 簇间"`），
///   仅用于区分调用来源，不记录任何请求内容。
pub async fn inter_llm_delay(delay_ms: u64, context: &str) {
    if delay_ms > 0 {
        tracing::debug!(
            context,
            delay_ms,
            "LLM 请求间等待 {}ms（{}）",
            delay_ms,
            context
        );
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// delay_ms=0 → 不等待，立即返回（配置关闭/测试场景）。
    #[tokio::test]
    async fn zero_delay_skips_sleep() {
        let start = tokio::time::Instant::now();
        inter_llm_delay(0, "测试").await;
        assert!(
            start.elapsed() < Duration::from_millis(5),
            "delay=0 不应等待，实际 elapsed={:?}",
            start.elapsed()
        );
    }

    /// delay_ms>0 → 至少等待指定时长。
    #[tokio::test]
    async fn positive_delay_waits_at_least_requested() {
        let start = tokio::time::Instant::now();
        inter_llm_delay(30, "测试").await;
        assert!(
            start.elapsed() >= Duration::from_millis(30),
            "应至少等待 30ms，实际 elapsed={:?}",
            start.elapsed()
        );
    }

    /// 连续调用时两次延迟累计（批量循环语义）。
    #[tokio::test]
    async fn repeated_calls_accumulate_gap() {
        let start = tokio::time::Instant::now();
        inter_llm_delay(15, "测试").await;
        inter_llm_delay(15, "测试").await;
        assert!(
            start.elapsed() >= Duration::from_millis(30),
            "两次 15ms 延迟应累计 ≥30ms，实际 elapsed={:?}",
            start.elapsed()
        );
    }
}
