//! crates/ramaria-service/src/index/tests/keyword_fuzzy.rs - 关键词镜像语义层开关用例
//!
//! 设计特点:
//! - 覆盖关键词语义层开关与文档向量通道开关的相互独立性
//! - 复用父测试模块的引擎装配与计数嵌入夹具（经 `use super::*` 取用）
//! - 断言以嵌入调用次数与镜像挂载状态为准，不依赖真实模型

use super::*;

/// 关闭关键词语义层开关 → 不挂载 Fuzzy 且零嵌入调用
/// （文档向量通道同时关闭，排除缺失补算带来的嵌入调用）。
#[tokio::test]
async fn keyword_fuzzy_disabled_skips_embeddings() {
    let mut config = RamariaConfig::default();
    config.retrieval.enable_keyword_fuzzy = false;
    config.retrieval.enable_vector = false;
    let (engine, storage, embedding, dir) =
        engine_with_counting_embedding("index-fuzzy-off", config).await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(
        &storage,
        "char-0001",
        "用户最近工作压力很大，常常加班到深夜",
    )
    .await;
    storage
        .upsert_keyword("工作压力")
        .await
        .expect("写入规范词应成功");

    engine.rebuild_index().await.expect("重建应成功");

    assert_eq!(embedding.embed_calls(), 0, "语义层关闭时不得单条嵌入");
    assert_eq!(embedding.batch_calls(), 0, "语义层关闭时不得批量嵌入");
    let mirror = engine.keyword_mirror();
    let guard = read_recover(&*mirror, "index.keyword_mirror");
    assert!(guard.pool_len() >= 1, "词典池仍应装载");
    assert!(
        guard.composite().fuzzy().is_none(),
        "语义层关闭时不应挂载 Fuzzy"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 仅关闭文档向量通道时，关键词语义层按自身开关（默认开启）仍构建：
/// 两个开关互不隐含。
#[tokio::test]
async fn keyword_fuzzy_builds_when_vector_channel_disabled() {
    let mut config = RamariaConfig::default();
    config.retrieval.enable_vector = false;
    let (engine, storage, embedding, dir) =
        engine_with_counting_embedding("index-fuzzy-vector-off", config).await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(
        &storage,
        "char-0001",
        "用户最近工作压力很大，常常加班到深夜",
    )
    .await;
    storage
        .upsert_keyword("工作压力")
        .await
        .expect("写入规范词应成功");

    engine.rebuild_index().await.expect("重建应成功");

    assert!(
        embedding.batch_calls() >= 1,
        "语义层仍应批量嵌入词条（文档向量关闭不影响语义层）"
    );
    assert_eq!(embedding.embed_calls(), 0, "文档向量通道关闭时不得逐条嵌入");
    let mirror = engine.keyword_mirror();
    let guard = read_recover(&*mirror, "index.keyword_mirror");
    let fuzzy = guard
        .composite()
        .fuzzy()
        .expect("文档向量关闭不影响语义层挂载");
    assert!(fuzzy.is_ready());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 仅关闭关键词语义层时，文档向量通道照常：已落库向量直接装载、
/// 不触发任何嵌入，语义层不挂载（与"仅关向量"方向互补）。
#[tokio::test]
async fn keyword_fuzzy_disabled_keeps_document_vectors() {
    let mut config = RamariaConfig::default();
    config.retrieval.enable_keyword_fuzzy = false;
    let (engine, storage, embedding, dir) =
        engine_with_counting_embedding("index-fuzzy-off-vector-on", config).await;
    seed_persona(&storage, "char-0001").await;
    let l1_id = seed_l1(
        &storage,
        "char-0001",
        "用户最近工作压力很大，常常加班到深夜",
    )
    .await;
    storage
        .upsert_keyword("工作压力")
        .await
        .expect("写入规范词应成功");
    seed_vector(
        &storage,
        ramaria_core::types::VECTOR_DOC_L1,
        &l1_id.to_string(),
        &CountingEmbedding::fingerprint(),
        "用户最近工作压力很大，常常加班到深夜",
    )
    .await;

    engine.rebuild_index().await.expect("重建应成功");

    assert_eq!(
        embedding.batch_calls(),
        0,
        "向量命中且语义层关闭时不得批量嵌入"
    );
    assert_eq!(embedding.embed_calls(), 0, "不得单条嵌入");
    assert_eq!(engine.index_status().loaded, 1, "文档向量应照常装载");
    let mirror = engine.keyword_mirror();
    let guard = read_recover(&*mirror, "index.keyword_mirror");
    assert!(
        guard.composite().fuzzy().is_none(),
        "语义层关闭时不应挂载 Fuzzy"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
