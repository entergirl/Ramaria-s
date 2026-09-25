//! crates/ramaria-memory/src/l1/mod.rs - L0→L1 摘要管线模块
//!
//! 设计特点:
//! - 负责 session 结束后的 L1 摘要生成
//! - 依赖 LLM trait 和 StorageBackend trait
//! - summarizer.rs: 编排 L0→L1 摘要生成流程（获取消息→格式化→调LLM→解析→校验→存储）
//! - orchestrate.rs: 生成编排（backend 预算传播 + JobManager 包裹 + 读回）与失败任务补扫
//!   （桌面与服务层共用，避免两条消费路径漂移）
//! - prompt.rs: LLM Prompt 模板管理（双版本：基础版/关键词注入版）
//! - mock.rs: 测试用 mock LlmProvider + StorageBackend（仅 #[cfg(test)]）

pub mod orchestrate;
pub mod prompt;
pub mod summarizer;

// 测试 mock（crate 内其他测试模块复用）
#[cfg(test)]
pub(crate) mod mock;

pub use orchestrate::{
    L1GenerateRequest, L1RetryObserver, L1RetryStats, MAX_L1_RETRY_JOBS_PER_RUN,
    generate_l1_summaries, retry_pending_l1_jobs,
};
pub use summarizer::{L1Summarizer, L1SummarizerConfig};
