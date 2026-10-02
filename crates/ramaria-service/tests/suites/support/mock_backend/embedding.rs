//! crates/ramaria-service/tests/suites/support/mock_backend/embedding.rs - Mock EmbeddingProvider
//!
//! 设计特点:
//! - 固定 128 维零向量输出，单条与批量口径一致，供检索链路确定性断言
//! - 不上真实模型、不触碰文件系统，`download_model` 为空操作
//! - `is_available` 恒为 true、`download_progress` 恒为 1.0，避免降级分支干扰用例
//! - 提供 `Default`，可直接用于 `Arc<dyn EmbeddingProvider>` 装配

use async_trait::async_trait;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingModelInfo, EmbeddingProvider};

// =========================================================
// MockEmbedding — 占位 Embedding Provider
// =========================================================

/// Mock Embedding Provider（不上真实模型）。
#[allow(dead_code)]
pub struct MockEmbedding {
    model_info: EmbeddingModelInfo,
}

impl Default for MockEmbedding {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
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
