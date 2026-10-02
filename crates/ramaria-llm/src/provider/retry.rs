//! crates/ramaria-llm/src/provider/retry.rs - 指数退避重试配置
//!
//! 设计特点:
//! - `RetryConfig`: 最大重试次数 / 初始退避 / 退避上限 / 退避倍数
//! - 可重试判定：HTTP 5xx 与 429 重试；其余 4xx（含 401/403）与配置类错误不重试
//! - 从错误上下文提取 "HTTP <状态码>" 文本，按状态码分级判定可重试性
//! - 退避公式: min(initial * multiplier^n, max_backoff_ms)

use ramaria_core::error::RamariaError;

// =========================================================
// 重试配置
// =========================================================

/// 指数退避重试配置。
///
/// 字段约定:
/// - `max_retries`: 最大重试次数（不含首次尝试）。默认 3。
/// - `initial_backoff_ms`: 首次重试等待毫秒数。默认 500。
/// - `max_backoff_ms`: 最大等待毫秒数上限。默认 10000。
/// - `backoff_multiplier`: 退避倍数。默认 2.0。
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// 最大重试次数
    pub max_retries: u32,
    /// 首次退避等待（毫秒）
    pub initial_backoff_ms: u64,
    /// 最大退避等待（毫秒）
    pub max_backoff_ms: u64,
    /// 退避倍数
    pub backoff_multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff_ms: 500,
            max_backoff_ms: 10_000,
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryConfig {
    /// 判断 HTTP 状态码是否应重试。
    ///
    /// 可重试: 5xx（服务端临时故障）、429（速率限制）
    /// 不重试: 4xx（除 429，含 401/403 鉴权错误）
    pub fn should_retry_http(status: u16) -> bool {
        status >= 500 || status == 429
    }

    /// 判断 `RamariaError` 是否应重试。
    ///
    /// 可重试: Llm 错误中的网络/服务端/限流问题（5xx、429、连接超时等）
    /// 不重试: Config / Validation / Privacy 错误 + Llm 中的客户端错误（4xx，除 429）
    ///
    /// 客户端错误（400/401/403/404 等）通过 context 文本中的 "HTTP <状态码>" 识别——
    /// 请求无效或鉴权失败时重试无意义且浪费配额；429（速率限制）与 5xx 可重试。
    pub fn should_retry_error(&self, err: &RamariaError) -> bool {
        let _ = self; // 保持方法签名一致性，供 RetryConfig 实例调用
        match err {
            RamariaError::Llm { context, .. } => {
                // 能提取到 HTTP 状态码时按状态码判定（4xx 除 429 不重试）
                if let Some(status) = extract_http_status(context) {
                    return Self::should_retry_http(status);
                }
                // 无状态码的网络/服务端错误（连接失败、超时等）视为可重试
                true
            }
            // 非 Llm 错误（Config / Validation / Privacy 等）一律不重试
            _ => false,
        }
    }

    /// 计算第 n 次重试的退避时长。
    ///
    /// 公式: min(initial * multiplier^n, max_backoff_ms)
    pub(super) fn backoff_ms(&self, attempt: u32) -> u64 {
        let ms =
            (self.initial_backoff_ms as f64 * self.backoff_multiplier.powi(attempt as i32)) as u64;
        ms.min(self.max_backoff_ms)
    }
}

/// 从错误上下文中提取 "HTTP <状态码>" 形式的 HTTP 状态码。
///
/// 说明:
/// - 传输层错误文案统一为 `...(HTTP {status}): ...` 格式（见 transport.rs）。
/// - 提取失败（无状态码的网络错误等）返回 None，由调用方按可重试处理。
fn extract_http_status(context: &str) -> Option<u16> {
    context
        .split("HTTP ")
        .nth(1)?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse::<u16>()
        .ok()
}
