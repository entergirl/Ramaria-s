//! crates/ramaria-testkit/tests/testkit.rs - Ramaria 测试基础设施自测
//!
//! 覆盖:
//! - 临时库由 migration 驱动：表数、schema 版本
//! - 两个临时库互相隔离
//! - MockStorage 会话 / 消息 / L1 往返
//! - 失败注入生效与清除
//! - 夹具确定性

use ramaria_core::traits::StoreCrud;
use ramaria_testkit::fixtures;
use ramaria_testkit::mock::MockStorage;
use sqlx::Row;

#[tokio::test]
async fn db_applies_full_migration() {
    let db = ramaria_testkit::db().await;
    let pool = db.pool();

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .expect("统计表数失败");
    assert_eq!(tables, 29, "迁移后应有 29 张业务表");

    let row = sqlx::query("SELECT value FROM schema_meta WHERE key = 'schema_version'")
        .fetch_one(pool)
        .await
        .expect("读取 schema_version 失败");
    let version: String = row.get("value");
    assert_eq!(version, "3");

    db.close().await;
}

#[tokio::test]
async fn two_dbs_are_isolated() {
    let first = ramaria_testkit::db().await;
    let second = ramaria_testkit::db().await;

    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, ?)")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(1000_i64)
        .execute(first.pool())
        .await
        .expect("写入第一个库失败");

    let first_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(first.pool())
        .await
        .expect("统计第一个库失败");
    let second_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(second.pool())
        .await
        .expect("统计第二个库失败");
    assert_eq!(first_count, 1);
    assert_eq!(second_count, 0);

    first.close().await;
    second.close().await;
}

#[tokio::test]
async fn mock_storage_roundtrip() {
    let storage = MockStorage::new();
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话失败");
    let message = fixtures::user_message(session.id, "你好");
    storage.save_message(&message).await.expect("写入消息失败");

    let l1 = fixtures::l1(session.id, Some("char-0001"), "问候摘要");
    storage.save_memory_l1(&l1).await.expect("写入 L1 失败");

    let messages = storage
        .list_messages(session.id)
        .await
        .expect("读取消息失败");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "你好");

    let l1s = storage
        .list_memory_l1(session.id)
        .await
        .expect("读取 L1 失败");
    assert_eq!(l1s.len(), 1);
    assert_eq!(l1s[0].summary, "问候摘要");
}

#[tokio::test]
async fn mock_storage_failure_injection() {
    let storage = MockStorage::new();
    storage.fail_on("create_session");
    let error = storage
        .create_session(None)
        .await
        .expect_err("注入失败后应返回错误");
    assert_eq!(error.category(), "storage");

    storage.clear_failures();
    storage
        .create_session(None)
        .await
        .expect("清除注入后应成功");

    storage.fail_next("create_session");
    storage
        .create_session(None)
        .await
        .expect_err("一次性注入应失败");
    storage
        .create_session(None)
        .await
        .expect("一次性注入用尽后应成功");
}

#[test]
fn fixtures_are_deterministic() {
    let first = fixtures::session(Some("char-0001"));
    let second = fixtures::session(Some("char-0001"));
    assert_eq!(first.started_at, second.started_at);
    assert_eq!(first.ended_at, second.ended_at);
    assert_eq!(first.channel, second.channel);
    assert_eq!(first.persona_uid, second.persona_uid);

    let first_l1 = fixtures::l1(first.id, Some("char-0001"), "摘要");
    let second_l1 = fixtures::l1(second.id, Some("char-0001"), "摘要");
    assert_eq!(first_l1.created_at, second_l1.created_at);
    assert_eq!(first_l1.occurred_at, second_l1.occurred_at);
    assert_eq!(first_l1.valence, second_l1.valence);
    assert_eq!(first_l1.salience, second_l1.salience);
}
