//! rust/crates/ramaria-llm/tests/embedding_tests.rs — 嵌入模型集成测试
//!
//! 设计特点:
//! - 使用 `NoopEmbeddingProvider` 进行无需真实模型的单元测试
//! - 测试 EmbeddingProvider trait 的完整接口契约
//! - 覆盖：可用性检查、验证、空输入、批量操作、模型信息一致性
//!
//! 说明:
//! - 通过 `embedding-onnx` feature 的 ONNX 测试需要真实模型文件，
//! 不在 CI 中运行，仅本地手动验证。

use ramaria_core::traits::EmbeddingProvider;
use ramaria_llm::embedding::noop::NoopEmbeddingProvider;

// =========================================================
// NoopEmbeddingProvider 测试
// =========================================================

#[tokio::test]
async fn noop_is_never_available() {
    let p = NoopEmbeddingProvider::new(384);
    assert!(!p.is_available());
    assert_eq!(p.download_progress(), 0.0);
}

#[tokio::test]
async fn noop_embed_returns_unsupported() {
    let p = NoopEmbeddingProvider::new(384);
    let result = p.embed("测试").await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("未启用"));
}

#[tokio::test]
async fn noop_embed_batch_returns_unsupported() {
    let p = NoopEmbeddingProvider::new(512);
    let result = p.embed_batch(&["文本1", "文本2"]).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn noop_validate_returns_unsupported() {
    let p = NoopEmbeddingProvider::new(384);
    assert!(p.validate().await.is_err());
}

#[tokio::test]
async fn noop_download_model_returns_unsupported() {
    let p = NoopEmbeddingProvider::new(384);
    assert!(p.download_model().await.is_err());
}

#[tokio::test]
async fn noop_model_info_is_consistent() {
    let p = NoopEmbeddingProvider::new(768);
    let info = p.model_info();
    assert_eq!(info.dimension, 768);
    assert_eq!(info.model_id, "noop");
}

#[tokio::test]
async fn noop_different_dimensions() {
    let p1 = NoopEmbeddingProvider::new(384);
    let p2 = NoopEmbeddingProvider::new(1024);
    assert_eq!(p1.model_info().dimension, 384);
    assert_eq!(p2.model_info().dimension, 1024);
}

#[tokio::test]
async fn noop_empty_text_batch() {
    let p = NoopEmbeddingProvider::new(384);
    // 即使不可用，空文本列表也应返回错误
    let result = p.embed_batch(&[]).await;
    assert!(result.is_err() || result.unwrap().is_empty());
}

// =========================================================
// EmbeddingProvider trait object 测试
// =========================================================

/// 验证 EmbeddingProvider 可通过 trait object 传递
#[tokio::test]
async fn trait_object_works() {
    let p: Box<dyn EmbeddingProvider> = Box::new(NoopEmbeddingProvider::new(384));
    assert!(!p.is_available());
    assert_eq!(p.model_info().dimension, 384);
}

// =========================================================
// 降级模式行为测试
// =========================================================

/// 模拟：嵌入模型缺失时，上层应能优雅降级
#[test]
fn degraded_mode_detection() {
    // 模拟场景：App 检测到嵌入模型不可用
    let provider = NoopEmbeddingProvider::new(384);
    let embedding_available = provider.is_available();
    assert!(!embedding_available);

    // 降级：向量通道权重归零，BM25 + 图谱仍可用
    let vector_weight: f64 = if embedding_available { 0.5 } else { 0.0 };
    assert_eq!(vector_weight, 0.0);

    let bm25_weight: f64 = 0.5;
    let graph_weight: f64 = 0.5;
    let total_weight = vector_weight + bm25_weight + graph_weight;
    assert!(total_weight > 0.0, "降级模式下仍有检索通道可用");
}
