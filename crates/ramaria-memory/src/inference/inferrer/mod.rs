//! crates/ramaria-memory/src/inference/inferrer/mod.rs - 三步 LLM 结构化性格推断
//!
//! 设计特点:
//! - Step 1: 逐分类个性模式提取 — 统计指标 + 态度聚类结果 → 分类级性格信号
//! - Step 2: 跨分类一致性比较 — 识别底色/主色调/点缀候选
//! - Step 3: 合成三层结构化性格画像 → PersonalityTrait 记录
//! - 输出后处理: 语义匹配/新增/废弃/差量更新（简化版：按 trait_label 去重）
//! - Mock 推断: 基于 StatsSummary 生成确定性人格标签，支持无 LLM 测试
//! - 本模块对外仅 re-export 各子模块公开项，公共 API 与原单文件模块一致

mod mock;
mod postprocess;
mod prompt;
mod types;

#[cfg(test)]
mod tests;

pub use mock::mock_infer;
pub use postprocess::{compute_trait_diff, post_process_inference};
pub use prompt::{build_step1_prompt, build_step2_prompt, build_step3_prompt, format_motive_stats};
pub use types::{
    CategorySignal, ConsistencyAnalysis, DiffAction, InferenceResult, InferredTrait,
    InferrerConfig, PostProcessResult, TraitDiff,
};
