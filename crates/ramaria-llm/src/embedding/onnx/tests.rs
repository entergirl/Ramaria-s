//! crates/ramaria-llm/src/embedding/onnx/tests.rs - ONNX 嵌入 Provider 单元测试
//!
//! 设计特点:
//! - 覆盖 provider 构造（不加载模型）/ 空输入 / 无模型目录的错误路径
//! - 不触发 ONNX Runtime 初始化（构造路径仅探测文件与 config.json）

use super::*;
use ramaria_core::traits::EmbeddingProvider;

/// 测试 provider 构造（不加载模型）
#[test]
fn provider_creation_without_model() {
    let provider = OnnxEmbeddingProvider::new("/nonexistent/path");
    assert!(!provider.is_available());
    assert_eq!(provider.download_progress(), 0.0);
    assert_eq!(provider.model_info().dimension, 384);
}

/// 测试空文本 embed 应报错
#[tokio::test]
async fn embed_empty_text_returns_error() {
    let provider = OnnxEmbeddingProvider::new("/nonexistent/path");
    let result = provider.embed("").await;
    assert!(result.is_err());
}

/// 测试批量空列表
#[tokio::test]
async fn embed_batch_empty_list_returns_empty() {
    let provider = OnnxEmbeddingProvider::new("/nonexistent/path");
    let result = provider.embed_batch(&[]).await.unwrap();
    assert!(result.is_empty());
}

/// 测试在无模型目录时 validate 报错
#[tokio::test]
async fn validate_without_model_fails() {
    let provider = OnnxEmbeddingProvider::new("/nonexistent/path");
    let result = provider.validate().await;
    assert!(result.is_err());
}

/// 测试 download_model 在无模型时报错
#[tokio::test]
async fn download_without_model_errors() {
    let provider = OnnxEmbeddingProvider::new("/nonexistent/path");
    let result = provider.download_model().await;
    assert!(result.is_err());
}
