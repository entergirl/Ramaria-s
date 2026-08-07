//! rust/crates/ramaria-app/src/stages/mod.rs - Stage 模块入口
//!
//! 设计特点:
//! - 管理全部 10 个 Pipeline Stage 的模块声明与 re-export
//! - 每个 Stage 独立文件，职责单一，可独立单元测试
//! - Stage 通过 PipelineStage trait 统一接口，由 SendMessagePipeline 编排器按序执行

pub mod build_prompt;
pub mod build_request;
pub mod call_llm;
pub mod check_privacy;
pub mod check_state;
pub mod load_history;
pub mod persist_message;
pub mod resolve_session;
pub mod retrieve_memory;
pub mod token_budget;

#[cfg(test)]
#[cfg(test)]
pub(crate) mod test_utils;

// re-export 全部 Stage 供外部使用
pub use build_prompt::StageBuildPrompt;
pub use build_request::StageBuildRequest;
pub use call_llm::StageCallLlm;
pub use check_privacy::StageCheckPrivacy;
pub use check_state::StageCheckState;
pub use load_history::StageLoadHistory;
pub use persist_message::StagePersistMessage;
pub use resolve_session::StageResolveSession;
pub use retrieve_memory::StageRetrieveMemory;
pub use token_budget::StageTokenBudget;
