//! crates/ramaria-memory/src/inference/orchestrator/phase_b/mod.rs - Phase B 三步 LLM 推断编排
//!
//! 设计特点:
//! - run: 加载旧 traits → 注入因果链/动机文本 → 三步推断 → 后处理 diff → 持久化。
//! - three_step: Step1 分类信号 / Step2 一致性 / Step3 合成结构化 traits。
//! - parse: JSON 三步递进解析（直接解析 → 剥离 think 标签 → 正则提取），失败结构化报错。
//! - convert: LLM confidence 优先，缺失时按统计指标动态计算。
//! - 降级由主编排决定：LLM 任一步骤失败回退 mock_infer（基于统计规则的推断）。
//! - 本模块对外逐项 re-export 子模块项，公共 API 路径与原单文件模块一致。

mod convert;
mod parse;
mod run;
mod three_step;

pub use run::run_phase_b_inference;

// 纯函数解析与转换在 crate 内仅由测试经 `phase_b::X` 调用；生产路径直接从子模块引用，
// 故此处 re-export 仅随测试构建存在，避免生产构建报未使用导入。
#[cfg(test)]
pub(super) use convert::convert_to_personality_traits;
#[cfg(test)]
pub(super) use parse::{
    parse_category_signals, parse_consistency_analysis, parse_inferred_traits,
    parse_json_with_degrade,
};
