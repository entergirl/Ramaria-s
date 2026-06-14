//! rust/crates/ramaria-llm/src/embedding/noop.rs - 空占位 Embedding Provider
//!
//! 设计特点:
//! - 当 `embedding-onnx` feature 未启用时编译，避免上层条件编译散落
//! - `is_available()` 始终返回 false，`embed()` 返回 Unsupported 错误
//! - 用于测试和降级场景

use async_trait::async_trait;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingModelInfo, EmbeddingProvider};

// =========================================================
// NoopEmbeddingProvider
// =========================================================

/// 空占位 embedding provider。
///
/// 职责:
/// - 提供编译期占位，使上层代码无需条件编译。
/// - 所有嵌入操作返回 `Unsupported` 错误。
///
/// 用法:
/// ```ignore
/// let provider = NoopEmbeddingProvider::new(384);
/// assert!(!provider.is_available());
/// ```
pub struct NoopEmbeddingProvider {
    /// 模型信息（dimension 可配置）
    info: EmbeddingModelInfo,
}

impl NoopEmbeddingProvider {
    /// 创建新的空占位 provider。
    ///
    /// 参数:
    /// - `dimension`: 宣称的向量维度（实际不工作）。
    pub fn new(dimension: usize) -> Self {
        Self {
            info: EmbeddingModelInfo {
                model_id: "noop".to_string(),
                dimension,
            },
        }
    }
}

#[async_trait]
impl EmbeddingProvider for NoopEmbeddingProvider {
    async fn embed(&self, _text: &str) -> RamariaResult<Vec<f32>> {
        Err(ramaria_core::error::RamariaError::unsupported(
            "嵌入模型未启用（编译时未启用 embedding-native 或 embedding-onnx feature）",
        ))
    }

    async fn embed_batch(&self, _texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        Err(ramaria_core::error::RamariaError::unsupported(
            "嵌入模型未启用（编译时未启用 embedding-native 或 embedding-onnx feature）",
        ))
    }

    fn model_info(&self) -> &EmbeddingModelInfo {
        &self.info
    }

    async fn validate(&self) -> RamariaResult<()> {
        Err(ramaria_core::error::RamariaError::unsupported(
            "嵌入模型未启用",
        ))
    }

    async fn download_model(&self) -> RamariaResult<()> {
        Err(ramaria_core::error::RamariaError::unsupported(
            "嵌入模型下载不可用（编译时未启用嵌入支持 feature）",
        ))
    }

    fn download_progress(&self) -> f64 {
        0.0
    }

    fn is_available(&self) -> bool {
        false
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_is_never_available() {
        let p = NoopEmbeddingProvider::new(384);
        assert!(!p.is_available());
        assert_eq!(p.download_progress(), 0.0);
    }

    #[tokio::test]
    async fn noop_embed_returns_unsupported() {
        let p = NoopEmbeddingProvider::new(384);
        let result = p.embed("测试文本").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn noop_validate_returns_unsupported() {
        let p = NoopEmbeddingProvider::new(384);
        assert!(p.validate().await.is_err());
    }

    #[tokio::test]
    async fn noop_model_info_is_consistent() {
        let p = NoopEmbeddingProvider::new(768);
        assert_eq!(p.model_info().dimension, 768);
        assert_eq!(p.model_info().model_id, "noop");
    }
}
