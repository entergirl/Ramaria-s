//! crates/ramaria-service/src/test_support/embedding.rs - Ramaria 服务层测试用嵌入 mock 模块
//!
//! 设计特点:
//! - 确定性向量：无模型文件、无网络、无随机性；
//! - 让"嵌入模型已加载"的路径在无模型环境下可测（热更新 / 读取 / 可用性判定）；
//! - 固定维度，供维度断言复用；
//! - 单条与批量嵌入共用同一确定性算法，结果可复现。

use ramaria_core::error::RamariaResult;

// =========================================================
// 最小嵌入 mock
// =========================================================

/// 最小嵌入 mock：确定性向量（无模型文件、无网络、无随机性）。
///
/// 职责:
/// - 让"嵌入模型已加载"的路径在无模型环境下可测（热更新 / 读取 / 可用性判定）；
/// - 提供固定维度，供维度断言复用。
pub(crate) struct DeterministicEmbedding {
    info: ramaria_core::traits::EmbeddingModelInfo,
}

impl DeterministicEmbedding {
    /// 向量维度（足够区分测试文本即可）。
    pub(crate) const DIMENSION: usize = 16;

    /// 构造可用的嵌入 provider。
    pub(crate) fn new() -> Self {
        Self {
            info: ramaria_core::traits::EmbeddingModelInfo {
                model_id: "mock-deterministic-embedding".to_string(),
                dimension: Self::DIMENSION,
            },
        }
    }

    /// 计算确定性向量（字符哈希分桶后 L2 归一化）。
    fn vector(text: &str) -> Vec<f32> {
        use std::hash::{Hash, Hasher};

        let mut vector = vec![0.0_f32; Self::DIMENSION];
        for ch in text.chars().filter(|ch| !ch.is_whitespace()) {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            ch.hash(&mut hasher);
            let index = (hasher.finish() as usize) % Self::DIMENSION;
            vector[index] += 1.0;
        }
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut vector {
                *value /= norm;
            }
        }
        vector
    }
}

#[async_trait::async_trait]
impl ramaria_core::traits::EmbeddingProvider for DeterministicEmbedding {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        Ok(Self::vector(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|text| Self::vector(text)).collect())
    }

    fn model_info(&self) -> ramaria_core::traits::EmbeddingModelInfo {
        self.info.clone()
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    async fn download_model(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn download_progress(&self) -> f64 {
        1.0
    }

    fn is_available(&self) -> bool {
        true
    }
}
