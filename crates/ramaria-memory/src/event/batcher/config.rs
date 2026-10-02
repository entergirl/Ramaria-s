//! crates/ramaria-memory/src/event/batcher/config.rs - TopicBatcher 聚类参数配置
//!
//! 设计特点:
//! - 集中管理所有聚类参数，避免散布的魔法值
//! - 提供 `default()` 与 builder 模式，边界输入自动钳制
//! - 纯配置结构，不依赖 LLM 或数据库

// =========================================================
// TopicBatcherConfig — 聚类参数配置
// =========================================================

/// TopicBatcher 配置。
///
/// 职责:
/// - 集中管理所有聚类参数，避免散布的魔法值。
/// - 提供 `default()` 和 builder 模式方便构造。
///
/// 字段约定:
/// - `min_cluster_size`: 簇的最小 L1 条目数。不足此数的簇进入 Pending Buffer。默认 3。
/// - `max_cluster_size`: 簇的最大 L1 条目数。超此数触发模块度 Q 二分拆分。默认 25。
/// - `similarity_threshold` (θ_sim): Jaccard 边的相似度阈值。仅保留 sim ≥ θ_sim 的边。默认 0.2。
/// - `alpha`: 关键词-语义融合权重。α=1.0 为纯关键词图，α=0.0 为纯语义图。默认 0.5。
/// - `modularity_min` (Q_min): 模块度拆分停止阈值。Q < Q_min 时不再继续拆分。默认 0.3。
#[derive(Debug, Clone)]
pub struct TopicBatcherConfig {
    pub min_cluster_size: usize,
    pub max_cluster_size: usize,
    pub similarity_threshold: f64,
    pub alpha: f64,
    pub modularity_min: f64,
}

impl Default for TopicBatcherConfig {
    fn default() -> Self {
        Self {
            min_cluster_size: 3,
            max_cluster_size: 25,
            similarity_threshold: 0.2,
            alpha: 0.5,
            modularity_min: 0.3,
        }
    }
}

impl TopicBatcherConfig {
    /// 创建使用默认值的配置。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置最小簇大小。
    pub fn with_min_cluster_size(mut self, size: usize) -> Self {
        self.min_cluster_size = size;
        self
    }

    /// 设置最大簇大小。
    pub fn with_max_cluster_size(mut self, size: usize) -> Self {
        self.max_cluster_size = size;
        self
    }

    /// 设置 Jaccard 相似度阈值。
    pub fn with_similarity_threshold(mut self, threshold: f64) -> Self {
        self.similarity_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// 设置关键词-语义融合权重 α。
    ///
    /// 参数:
    /// - `alpha`: 0.0..1.0，1.0=纯关键词图，0.0=纯语义图。
    pub fn with_alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }

    /// 设置模块度最小值 Q_min。
    pub fn with_modularity_min(mut self, q_min: f64) -> Self {
        self.modularity_min = q_min.clamp(0.0, 1.0);
        self
    }
}
