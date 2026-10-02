//! crates/ramaria-memory/src/behavior/clustering/mod.rs - 行为样本聚类（D2）
//!
//! 设计特点:
//! - 双通道向量化：反应通道 r = embedding(paraphrase⊕attitude)、情境通道 s = embedding(关键词拼接)
//! - 三路融合相似度：sim = β1·cos(r_i,r_j) + β2·cos(s_i,s_j) + (1−β1−β2)·Jaccard(K_i,K_j)
//!   —— 通道缺向量时对应权重归零并归一化（embedding 不可用 → 纯关键词 Jaccard，β=0 降级）
//! - 密度聚类：邻域 sim ≥ θ_nb、核心样本邻居数 ≥ min_cluster_size、密度可达连接、
//!   边界软分配、孤立点不入簇；孤立点比例 > 60% 触发失败模式检查（下调 θ_nb 重试）
//! - 簇提炼：关键词并集（频次 Top-N）/ 簇中心向量 / valence 加权均值与标准差 /
//!   presentation 分布 / situation_strength 均值 / 时间跨度 / 簇质量（内聚度 × 一致性）
//! - 纯计算函数零 I/O；向量化通过 `EmbeddingProvider` trait 注入，便于 mock 确定性测试
//! - 本模块对外逐项 re-export 子模块公开项，公共 API 路径与原单文件模块一致

mod cluster;
mod pipeline;
mod refine;
mod sample;
mod similarity;
mod vectorize;

pub use cluster::{ClusterAssignment, DensityClusterResult, RawCluster, density_cluster};
pub use pipeline::BehaviorClusterer;
pub use refine::{ClusterMember, KEYWORD_TOP_N, RefinedCluster, refine_cluster};
pub use sample::{BehaviorSample, dedup_keywords, sample_from_event};
pub use similarity::{cosine_clipped, fused_similarity, jaccard};
pub use vectorize::vectorize;

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
