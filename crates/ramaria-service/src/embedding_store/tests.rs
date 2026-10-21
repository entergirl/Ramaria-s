//! crates/ramaria-service/src/embedding_store/tests.rs - 记忆向量门面测试
//!
//! 设计特点:
//! - 覆盖指纹格式与零维度空串两条口径

use super::*;

#[test]
fn fingerprint_contains_model_dim_and_size() {
    let info = EmbeddingModelInfo {
        model_id: "bge-small".to_string(),
        dimension: 512,
    };
    assert_eq!(model_fingerprint(&info), "bge-small@512#2048");
}

#[test]
fn fingerprint_is_empty_for_zero_dimension() {
    let info = EmbeddingModelInfo {
        model_id: "unknown".to_string(),
        dimension: 0,
    };
    assert!(model_fingerprint(&info).is_empty());
}

/// 向量通道关闭：写入直接返回 false，不落向量行。
#[tokio::test]
async fn store_returns_false_when_vector_channel_disabled() {
    use crate::test_support::{CountingEmbedding, MockLlm, engine_with_llm_config_and_embedding};
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::EmbeddingProvider;
    use std::sync::Arc;

    let mut config = RamariaConfig::default();
    config.retrieval.enable_vector = false;
    let embedding = Arc::new(CountingEmbedding::new());
    let provider: Arc<dyn EmbeddingProvider> = embedding.clone();
    let (engine, _storage, dir) = engine_with_llm_config_and_embedding(
        "embedding-store-disabled",
        MockLlm::local(),
        config,
        Some(provider),
    )
    .await;

    let vector = CountingEmbedding::vector("关闭向量通道");
    assert!(
        !store(&engine, "l1", "doc-1", &vector).await,
        "关闭向量通道时 store 应返回 false"
    );
    let rows = engine
        .storage_ref()
        .load_memory_vectors(&CountingEmbedding::fingerprint())
        .await
        .expect("加载向量应成功");
    assert!(rows.is_empty(), "关闭向量通道不应落向量行");

    let _ = std::fs::remove_dir_all(&dir);
}
