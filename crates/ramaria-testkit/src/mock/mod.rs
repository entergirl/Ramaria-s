//! crates/ramaria-testkit/src/mock/mod.rs - Ramaria 测试 mock 模块
//!
//! 设计特点:
//! - 单份实现：存储、LLM、嵌入三类 mock 全仓共用，调用方只改 import
//! - LLM 覆盖固定回复 / 恒失败 / 流式脚本 / 流中错误 / 健康探测 / vision
//! - 嵌入覆盖零向量占位、确定性向量与调用计数三种口径
//! - 所有 mock 均为 Send + Sync，可直接放入 Arc<dyn Trait>

pub mod embedding;
pub mod llm;
pub mod storage;

pub use embedding::{CountingEmbedding, DeterministicEmbedding, MockEmbedding};
pub use llm::{MockFailingLlm, MockLlm, MockLlmProvider, ScriptedLlm};
pub use storage::MockStorage;
