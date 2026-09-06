//! crates/ramaria-memory/src/inference/orchestrator/mod.rs - Phase B/C 编排层
//!
//! 设计特点:
//! - Phase B: 三步 prompt 构建 → LLM 调用 → JSON 解析 → post_process → 写入 DB（phase_b.rs）。
//! - Phase C: 加载已有 traits/evidence → confidence_update → drift_detection → 持久化（phase_c.rs）。
//! - 分层先验收缩集成（shrink.rs）、语义标签持久化与跨版本簇匹配（semantic.rs）。
//! - 依赖注入: 通过 LlmProvider + StorageBackend trait 解耦具体实现。
//! - 本模块对外仅 re-export 各子模块公开项，公共 API 与原单文件模块一致。

mod phase_b;
mod phase_c;
mod semantic;
mod shrink;
mod types;

#[cfg(test)]
mod tests;

pub use phase_b::run_phase_b_inference;
pub use phase_c::run_phase_c_update;
pub use semantic::{
    generate_semantic_labels_for_clusters, persist_cluster_snapshots_with_semantic_labels,
    query_cross_version_matches,
};
pub use shrink::{apply_layered_shrinkage, build_layer_hints_from_traits};
pub use types::{PhaseBResult, PhaseBSource, PhaseCResult};
