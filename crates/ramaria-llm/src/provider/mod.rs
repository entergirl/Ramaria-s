//! crates/ramaria-llm/src/provider/mod.rs - Ramaria Provider 共享基础设施模块
//!
//! 设计特点:
//! - `ProviderBase`: 封装 HTTP 传输、消息组装、重试/超时策略
//! - `RetryConfig`: 指数退避重试配置（网络错误 + 5xx 重试，鉴权错误不重试）
//! - `build_messages`: 将 `ChatRequest` 组装为 OpenAI 兼容消息数组，含 Prompt Injection 防护
//! - 三个 provider 通过组合 `ProviderBase` + keychain 实现 `LlmProvider` trait
//! - 在线 provider 的构造器与 trait 实现由 `impl_online_provider*!` 宏生成，消除重复

mod base;
mod macros;
mod request;
mod retry;

#[cfg(test)]
mod tests;

pub use base::resolve_constructor_key;
pub use retry::RetryConfig;

pub(crate) use base::ProviderBase;

#[cfg(test)]
pub(crate) use request::{build_messages, cache_key, sanitize_user_message};
