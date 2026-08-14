//! crates/ramaria-memory/src/l1/mod.rs - L0→L1 摘要管线模块
//!
//! 设计特点:
//! - 负责 session 结束后的 L1 摘要生成
//! - 依赖 LLM trait 和 StorageBackend trait
//! - summarizer.rs: 编排 L0→L1 摘要生成流程（获取消息→格式化→调LLM→解析→校验→存储）
//! - prompt.rs: LLM Prompt 模板管理（双版本：基础版/关键词注入版）
//! - mock.rs: 测试用 mock LlmProvider + StorageBackend（仅 #[cfg(test)]）

pub mod prompt;
pub mod summarizer;

// 测试 mock
#[cfg(test)]
mod mock;

pub use summarizer::{L1Summarizer, L1SummarizerConfig};
