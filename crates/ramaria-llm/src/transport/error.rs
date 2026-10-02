//! crates/ramaria-llm/src/transport/error.rs - HTTP 错误分类
//!
//! 设计特点:
//! - 将 HTTP 错误状态码映射为 `RamariaError::Llm`
//! - 401/403 鉴权、429 限流、4xx 请求错误、5xx 服务端错误分别给出可读文案
//! - 响应体摘要截断到 500 字符以内，保留 status code 便于诊断
//! - 不包含 API key 与完整请求内容

use ramaria_core::error::RamariaError;

// =========================================================
// HTTP 错误分类
// =========================================================

/// 将 HTTP 错误状态码映射为 `RamariaError::Llm`。
///
/// 分类:
/// - 401 / 403: 鉴权错误（API key 无效或过期）
/// - 429: 速率限制
/// - 4xx: 请求错误（模型名、参数等）
/// - 5xx: 服务端错误
pub(crate) fn http_error(status: u16, body: &str) -> RamariaError {
    let summary: String = ramaria_core::text::truncate_chars_bare(body, 500);
    let context = match status {
        401 => "LLM 鉴权失败 (HTTP 401): API key 无效或过期。请检查 keychain 中的密钥是否正确"
            .to_string(),
        403 => "LLM 访问被拒绝 (HTTP 403): 请检查 API key 权限或账户状态".to_string(),
        429 => "LLM 请求频率超限 (HTTP 429): 请稍后重试".to_string(),
        400..=499 => format!("LLM 请求错误 (HTTP {status}): {summary}"),
        500..=599 => format!("LLM 服务端错误 (HTTP {status}): {summary}"),
        _ => format!("LLM 未知 HTTP 错误 ({status}): {summary}"),
    };
    RamariaError::llm(context)
}
