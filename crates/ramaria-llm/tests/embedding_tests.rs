//! crates/ramaria-llm/tests/embedding_tests.rs — 嵌入模型集成测试
//!
//! 设计特点:
//! - 使用 `NoopEmbeddingProvider` 进行无需真实模型的单元测试
//! - 测试 EmbeddingProvider trait 的完整接口契约
//! - 覆盖：可用性检查、验证、空输入、批量操作、模型信息一致性
//!
//! 说明:
//! - ONNX 后端已停用（无 crate 启用 `embedding-onnx` feature），不存在对应的 ONNX
//!   集成测试；本文件仅覆盖 `NoopEmbeddingProvider` 实现与 `EmbeddingProvider` trait 契约。

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

// （原 noop_empty_text_batch 断言 `is_err() || unwrap().is_empty()` 恒真，
//  embed_batch 无条件返回 Err，与 noop_embed_batch_returns_unsupported 同路径，已删除）

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

// （原 degraded_mode_detection 用局部变量重算常量、断言恒真，未触达被测代码，已删除）
