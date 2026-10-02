//! crates/ramaria-memory/src/inference/causal/mod.rs - A8 因果链特征提取
//!
//! 设计特点:
//! - 从 event_relations 表（CausedBy 关系）构建有向图，DFS 计算最长因果路径
//! - 循环模式探测：识别重复出现的因果链序列，指向稳定的行为脚本
//! - 时延分布与情绪沿链走势为扩展特征（独立开关，无数据时为空缺省形态）
//! - 纯函数设计：不依赖 DB 或 LLM，输入 MemoryEvent + EventRelation 即可运算
//! - 本模块对外仅 re-export 各子模块公开项，公共 API 与原单文件模块一致

mod extract;
mod format;
mod graph;
mod types;

#[cfg(test)]
mod tests;

pub use extract::{extract_causal_features, extract_causal_features_extended};
pub use format::format_causal_features_text;
pub use types::{CausalChainFeatures, CausalEmotionTrend, CausalLatencyStats, CyclePattern};
