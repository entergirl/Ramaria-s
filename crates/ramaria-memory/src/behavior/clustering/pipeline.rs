//! crates/ramaria-memory/src/behavior/clustering/pipeline.rs - 聚类学习管线编排
//!
//! 设计特点:
//! - 事件 → 样本 → 向量化 → 密度聚类 → 簇提炼的完整编排入口。
//! - 孤立点比例超限时按失败模式检查下调 θ_nb（每次 −0.1，最多 2 次）重试。
//! - 支持调用方预构造样本（Manual 强锚点注入），锚点样本保留预填向量。
//! - 输出簇按簇 id 升序，保证顺序稳定。

use ramaria_core::config::BehaviorConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::MemoryEvent;

use super::cluster::density_cluster;
use super::refine::{RefinedCluster, refine_cluster};
use super::sample::{BehaviorSample, sample_from_event};
use super::vectorize::vectorize;

// =========================================================
// 学习管线编排（D2 入口）
// =========================================================

/// 行为聚类编排器。
///
/// 职责:
/// - 事件 → 样本 → 向量化 → 密度聚类（含失败模式检查重试）→ 簇提炼。
pub struct BehaviorClusterer<'a> {
    config: &'a BehaviorConfig,
    embedder: Option<&'a dyn EmbeddingProvider>,
}

impl<'a> BehaviorClusterer<'a> {
    /// 创建聚类编排器。
    ///
    /// 参数:
    /// - `config`: 行为层配置（θ_nb/min_cluster_size/β 权重/孤立点比例上限）。
    /// - `embedder`: 嵌入模型 provider；`None` 表示 embedding 不可用（纯关键词降级）。
    pub fn new(config: &'a BehaviorConfig, embedder: Option<&'a dyn EmbeddingProvider>) -> Self {
        Self { config, embedder }
    }

    /// 执行完整聚类管线。
    ///
    /// 流程:
    /// 1. `sample_from_event` 构造样本。
    /// 2. `vectorize` 双通道向量化（embedding 不可用 → 纯关键词）。
    /// 3. `density_cluster` 密度聚类；孤立点比例 > `max_outlier_ratio` 时
    ///    按失败模式检查下调 θ_nb（每次 −0.1，最多 2 次）重试。
    /// 4. `refine_cluster` 逐簇提炼。
    ///
    /// 返回:
    /// - 提炼后的簇列表（仅含有效簇，孤立点不产生簇）。
    /// - 输入为空 → 空列表。
    pub async fn cluster_events(
        &self,
        events: &[MemoryEvent],
    ) -> RamariaResult<Vec<RefinedCluster>> {
        let mut samples: Vec<BehaviorSample> = events.iter().map(sample_from_event).collect();
        self.cluster_samples(events, &mut samples).await
    }

    /// 对"已构造样本"执行聚类管线（支持 Manual 强锚点注入，v3.1 §9.3）。
    ///
    /// 与 `cluster_events` 的区别:
    /// - `samples` 由调用方构造，可混入非事件样本（如 Manual 规则锚点，
    ///   event_id 用负值标记——聚类与簇提炼照常参与，锚点可偏移簇中心）。
    /// - `vectorize` 只填充 `events` 中真实事件对应的样本（锚点样本保留
    ///   调用方预填的向量）。
    /// - 返回的簇中 `member_event_ids` 可能含负 id（锚点）；调用方在生成
    ///   证据链时应过滤（锚点不是真实事件，不写入规则 evidence）。
    ///
    /// 参数:
    /// - `events`: 真实事件（供向量化文本来源）。
    /// - `samples`: 聚类输入样本（长度 ≥ events.len()，前段为事件样本）。
    pub async fn cluster_samples(
        &self,
        events: &[MemoryEvent],
        samples: &mut [BehaviorSample],
    ) -> RamariaResult<Vec<RefinedCluster>> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        vectorize(samples, events, self.embedder).await?;

        // 失败模式检查：孤立点比例超限 → 下调 θ_nb 重试（至多 2 次）
        let mut theta_nb = self.config.theta_nb;
        let mut result = density_cluster(
            samples,
            theta_nb,
            self.config.min_cluster_size,
            self.config.beta1,
            self.config.beta2,
        );
        let mut retries = 0;
        while result.outlier_ratio > self.config.max_outlier_ratio && retries < 2 {
            theta_nb = (theta_nb - 0.1).max(0.05);
            tracing::warn!(
                outlier_ratio = %format!("{:.2}", result.outlier_ratio),
                theta_nb,
                "行为聚类孤立点比例超限，下调 θ_nb 重试"
            );
            result = density_cluster(
                samples,
                theta_nb,
                self.config.min_cluster_size,
                self.config.beta1,
                self.config.beta2,
            );
            retries += 1;
        }
        if result.outlier_ratio > self.config.max_outlier_ratio {
            tracing::warn!(
                outlier_ratio = %format!("{:.2}", result.outlier_ratio),
                "行为聚类孤立点比例仍超限，接受当前结果（孤立点不产生规则）"
            );
        }

        // 簇提炼（按簇 id 升序，保证输出顺序稳定）
        let mut refined = Vec::with_capacity(result.cluster_count);
        for cid in 0..result.cluster_count {
            let members = &result.clusters[cid].member_indices;
            let rc = refine_cluster(samples, members, self.config.beta1, self.config.beta2);
            refined.push(rc);
        }
        Ok(refined)
    }
}
