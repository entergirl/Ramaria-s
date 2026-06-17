//! rust/crates/ramaria-app/src/stages/mod.rs - Stage 模块入口
//!
//! 设计特点:
//! - 管理全部 10 个 Pipeline Stage 的模块声明与 re-export
//! - 每个 Stage 独立文件，职责单一，可独立单元测试
//! - Stage 通过 PipelineStage trait 统一接口，由 SendMessagePipeline 编排器按序执行

pub mod check_privacy;
pub mod check_state;
pub mod load_history;
pub mod resolve_session;
pub mod retrieve_memory;

#[cfg(test)]
mod test_utils;

// re-export 全部 Stage 供外部使用
pub use check_privacy::StageCheckPrivacy;
pub use check_state::StageCheckState;
pub use load_history::StageLoadHistory;
pub use resolve_session::StageResolveSession;
pub use retrieve_memory::StageRetrieveMemory;
