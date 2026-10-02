//! crates/ramaria-llm/src/transport/mod.rs - Ramaria OpenAI-compatible HTTP 传输模块
//!
//! 设计特点:
//! - 真正的 SSE 流式处理：使用 `reqwest::Response::bytes_stream` + `futures::channel::mpsc`
//!   逐块读取、逐行解析，不一次性读取响应体
//! - SSE 解析器支持缓冲区拼接跨 chunk 的不完整行
//! - 错误分类：HTTP 4xx → Validation/Llm 错误，5xx → Llm 错误，网络错误 → Llm 错误
//! - 非流式请求（`stream: false`）直接解析完整 JSON 响应
//! - SSE 单行 > 10KB 截断并 warn；流式整体 600s 超时保护（首事件 60s 快速失败）

mod client;
mod error;
mod sse;

#[cfg(test)]
mod tests;

pub use client::OpenAiTransport;

#[cfg(test)]
pub(crate) use error::http_error;

#[cfg(test)]
pub(crate) use sse::{parse_sse_line, sse_read_loop_inner, summarize_stream_error};
