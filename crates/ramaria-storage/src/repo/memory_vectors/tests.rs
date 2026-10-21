//! crates/ramaria-storage/src/repo/memory_vectors/tests.rs - 记忆向量存取测试
//!
//! 设计特点:
//! - 覆盖往返、覆盖写、按模型过滤、按文档删除与孤儿清理
//! - 覆盖非法 BLOB 逐行跳过（不整体失败）
//! - 直接以 SQL 插入业务 fixture，向量经 repo 接口落库

use super::*;
use crate::database::init_test_pool;

fn vector(doc_type: &str, doc_id: &str, model: &str, values: Vec<f32>) -> MemoryVector {
    let dim = values.len() as u32;
    MemoryVector {
        doc_type: doc_type.to_string(),
        doc_id: doc_id.to_string(),
        model: model.to_string(),
        dim,
        vector: values,
        updated_at: 1_700_000_000_000,
    }
}

/// 插入 session fixture（memory_l1 / utt_blocks 的外键要求）。
async fn insert_session(pool: &SqlitePool, id: &str) {
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
        .bind(id)
        .execute(pool)
        .await
        .expect("插入 session fixture 应成功");
}

#[tokio::test]
async fn upsert_then_load_roundtrip() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let v = vector("l1", "doc-1", "m@2#8", vec![0.5, -1.0]);
    upsert(&pool, &v).await.expect("写入应成功");

    let loaded = load_by_model(&pool, "m@2#8").await.expect("加载应成功");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0], v);
    assert_eq!(count(&pool).await.expect("计数应成功"), 1);
}

#[tokio::test]
async fn upsert_overwrites_same_doc() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    upsert(&pool, &vector("l1", "doc-1", "m@2#8", vec![1.0, 2.0]))
        .await
        .expect("首次写入应成功");
    let updated = MemoryVector {
        updated_at: 1_800_000_000_000,
        ..vector("l1", "doc-1", "m2@2#8", vec![3.0, 4.0])
    };
    upsert(&pool, &updated).await.expect("覆盖写入应成功");

    let loaded = load_by_model(&pool, "m2@2#8").await.expect("加载应成功");
    assert_eq!(loaded.len(), 1, "同 (doc_type, doc_id) 应只保留一行");
    assert_eq!(loaded[0].vector, vec![3.0, 4.0]);
    assert_eq!(loaded[0].updated_at, 1_800_000_000_000);
    assert!(
        load_by_model(&pool, "m@2#8")
            .await
            .expect("加载应成功")
            .is_empty(),
        "旧模型指纹不应命中已覆盖行"
    );
}

#[tokio::test]
async fn load_by_model_filters_rows() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    upsert(&pool, &vector("l1", "a", "m1@2#8", vec![1.0, 1.0]))
        .await
        .expect("写入应成功");
    upsert(&pool, &vector("l2", "b", "m2@2#8", vec![2.0, 2.0]))
        .await
        .expect("写入应成功");

    let first = load_by_model(&pool, "m1@2#8").await.expect("加载应成功");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].doc_id, "a");
}

#[tokio::test]
async fn delete_by_doc_and_by_type() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    upsert(&pool, &vector("l1", "a", "m@2#8", vec![1.0, 1.0]))
        .await
        .expect("写入应成功");
    upsert(&pool, &vector("l1", "b", "m@2#8", vec![2.0, 2.0]))
        .await
        .expect("写入应成功");
    upsert(&pool, &vector("l2", "c", "m@2#8", vec![3.0, 3.0]))
        .await
        .expect("写入应成功");

    delete_by_doc(&pool, "l1", "a").await.expect("删除应成功");
    assert_eq!(count(&pool).await.expect("计数应成功"), 2);

    delete_by_type(&pool, "l1").await.expect("删除应成功");
    let left = load_by_model(&pool, "m@2#8").await.expect("加载应成功");
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].doc_type, "l2");
}

#[tokio::test]
async fn prune_orphans_removes_missing_business_rows() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    insert_session(&pool, "11111111-1111-4111-8111-111111111111").await;
    sqlx::query(
        "INSERT INTO memory_l1 (id, session_id, summary, created_at)
         VALUES ('l1-existing', '11111111-1111-4111-8111-111111111111', '摘要', 0)",
    )
    .execute(&pool)
    .await
    .expect("插入 L1 fixture 应成功");

    upsert(&pool, &vector("l1", "l1-existing", "m@2#8", vec![1.0]))
        .await
        .expect("写入应成功");
    upsert(&pool, &vector("l1", "l1-missing", "m@2#8", vec![1.0]))
        .await
        .expect("写入应成功");
    upsert(&pool, &vector("l2", "999", "m@2#8", vec![1.0]))
        .await
        .expect("写入应成功");

    let deleted = prune_orphans(&pool).await.expect("清理应成功");
    assert_eq!(deleted, 2);
    let left = load_by_model(&pool, "m@2#8").await.expect("加载应成功");
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].doc_id, "l1-existing");
}

#[tokio::test]
async fn load_skips_undecodable_blob() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    sqlx::query(
        "INSERT INTO memory_vectors (doc_type, doc_id, model, dim, vector, updated_at)
         VALUES ('l1', 'bad', 'm@2#8', 2, X'0102', 0)",
    )
    .execute(&pool)
    .await
    .expect("插入非法向量应成功");

    let loaded = load_by_model(&pool, "m@2#8").await.expect("加载应成功");
    assert!(loaded.is_empty(), "非法长度 BLOB 应被跳过而非整体失败");
}
