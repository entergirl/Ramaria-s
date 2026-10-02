//! crates/ramaria-memory/src/event/batcher/mod.rs - TopicBatcher 主题批量构建器
//!
//! 设计特点:
//! - 从未吸收 L1 摘要构建关键词 Jaccard 图，通过连通分量实现语义聚类
//! - L1 embedding 语义增强: 与关键词 Jaccard 按 α 权重融合（默认 α=0.5）
//! - embedding 不可用时自动退化为纯关键词图（α=1.0），保证离线可用性
//! - `L1Item` 为 `MemoryL1` 的聚类专用精简视图，通过 `From` trait 转换
//! - `TopicBatcherConfig` 集中管理所有聚类参数，支持 Builder 模式
//! - 纯计算模块，不依赖 LLM 或数据库，仅依赖 ramaria-core 的 KeywordToken
//! - 类型/编排/相似度按职责拆入子模块（item/cluster/config/topic/similarity），
//!   本文件仅保留声明与逐项 re-export，公共 API 路径与原单文件模块一致

pub mod buffer;
pub mod graph;

mod cluster;
mod config;
mod item;
mod similarity;
mod topic;

#[cfg(test)]
mod tests;

pub use cluster::TopicCluster;
pub use config::TopicBatcherConfig;
pub use item::L1Item;
pub use similarity::{
    absorb_orphans, compute_semantic_score, cosine_similarity, jaccard_similarity,
};
pub use topic::TopicBatcher;
