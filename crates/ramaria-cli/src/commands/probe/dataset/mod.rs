//! crates/ramaria-cli/src/commands/probe/dataset/mod.rs - 探针测试集构建模块入口
//!
//! 设计特点:
//! - `probe build` 数据集构建：来源优先级为数据源文件 > 数据库 > 内置夹具兜底（静默降级）
//! - tone / emotion / fact 三维候选收集与确定性抽样（seed 固定可复跑），不足用夹具补齐
//! - tone / emotion 题项携带 question 前紧邻上文（容量上限 `CONTEXT_TURNS`），保留社交语境
//! - 内置夹具仅含问题与参考文本，不含对话原文；确定性 RNG / 时间戳复用根模块
//! - 子模块按域拆分，对外名称在此逐项 re-export（`probe::dataset::X` 路径不变）

mod build;
mod fixture;

pub use build::{
    build_dataset, build_dataset_with_ablation, build_from_file, default_variants, run_build,
    select_target_persona,
};
pub use fixture::{
    build_from_fixture, fixture_emotion_pairs, fixture_fact_events, fixture_tone_pairs,
    has_emotion_cue, has_negative_cue, has_positive_cue, sample_with_fallback,
};

#[cfg(test)]
pub(crate) use fixture::tone_pairs_from_messages;
