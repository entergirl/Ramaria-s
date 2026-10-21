//! crates/ramaria-testkit/src/lib.rs - Ramaria 测试基础设施模块
//!
//! 设计特点:
//! - 只服务测试：不依赖 service / memory / llm / importer，避免测试地基反向耦合业务层
//! - 迁移驱动的临时库：测试一律使用真实 schema，不写 DDL
//! - 单份内存存储 mock：全部存储 trait 方法在一处实现，失败注入统一收口
//! - 单份 LLM / 嵌入 mock：流式、vision、健康探测语义与各调用方保持一致
//! - 确定性领域夹具：时间基准固定，便于跨用例对照

pub mod db;
pub mod fixtures;
pub mod mock;

pub use db::{TempDb, db, memory_db};
pub use mock::embedding::{CountingEmbedding, DeterministicEmbedding};
pub use mock::storage::MockStorage;
pub use mock::{MockEmbedding, MockFailingLlm, MockLlm, MockLlmProvider, ScriptedLlm};
