//! crates/ramaria-cli/tests/common/embedding.rs - MockEmbedding（EmbeddingProvider 实现）
//!
//! 设计特点:
//! - 确定性伪向量 EmbeddingProvider，供检索相关集成测试使用
//! - 经 `common::MockEmbedding` 对外复用（mod.rs re-export）
//! - 仅服务 CLI 集成测试，不加载真实模型

use async_trait::async_trait;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingModelInfo, EmbeddingProvider};

pub struct MockEmbedding {
    model_info: EmbeddingModelInfo,
}

impl MockEmbedding {
    pub fn new() -> Self {
        Self {
            model_info: EmbeddingModelInfo {
                model_id: "mock-embedding".into(),
                dimension: 128,
            },
        }
    }
}

#[async_trait]
impl EmbeddingProvider for MockEmbedding {
    async fn embed(&self, _text: &str) -> RamariaResult<Vec<f32>> {
        Ok(vec![0.0; 128])
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0; 128]).collect())
    }

    fn model_info(&self) -> EmbeddingModelInfo {
        self.model_info.clone()
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
