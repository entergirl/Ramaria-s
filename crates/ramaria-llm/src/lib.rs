//! rust/crates/ramaria-llm/src/lib.rs - Ramaria LLM Provider 层入口
//!
//! 设计特点:
//! - 实现 `ramaria_core::traits::LlmProvider` trait，支持 LM Studio / DeepSeek / OpenAI
//! - 真正的 SSE 流式传输（不一次性读取响应体），通过 futures channel + tokio spawn 实现
//! - 统一重试策略：网络错误和 5xx 重试（指数退避），鉴权错误不重试
//! - API key 通过 OS keychain 读取，不进入日志或配置文件
//! - 共享 transport 层抽象，避免三个 provider 重复实现 HTTP/SSE 逻辑
//! - 通过 feature `embedding-native` 支持原生 safetensors 嵌入模型（candle 推理引擎）
//! - 支持 BERT 架构（bge-small-zh-v1.5）和 LLaMA/Qwen3 架构（Qwen3-Embedding-0.6B）
//! - 保留旧 feature `embedding-onnx` 向后兼容（Phase 2 后移除）

pub mod keychain;
pub mod provider;
pub mod transport;

// Provider 实现
pub mod deepseek;
pub mod lm_studio;
pub mod openai;

// Embedding Provider 实现（可选 feature）
pub mod embedding;

// 重新导出常用类型
pub use provider::ProviderBase;
pub use transport::OpenAiTransport;
