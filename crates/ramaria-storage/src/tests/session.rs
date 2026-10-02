//! crates/ramaria-storage/src/tests/session.rs - 会话存储测试
//!
//! 设计特点:
//! - 覆盖会话 CRUD、persona_uid 绑定与活跃 / 全量列表读取
//! - 覆盖 sessions 表 channel / external_ref 列与联合索引的 schema 形态
//! - 明确 channel 默认值保证存量行升级后取 local

use super::*;

#[tokio::test]
async fn session_crud() {
    let storage = setup().await;
    let session = storage.create_session(None).await.unwrap();
    assert!(session.ended_at.is_none());

    let got = storage.get_session(session.id).await.unwrap().unwrap();
    assert_eq!(got.id, session.id);

    storage.close_session(session.id).await.unwrap();
    let closed = storage.get_session(session.id).await.unwrap().unwrap();
    assert!(closed.ended_at.is_some());
}

// =========================================================
// Session-Persona 绑定测试
// =========================================================

/// 创建 session 时可传入 persona_uid，get 时正确返回。
#[tokio::test]
async fn session_with_persona_uid() {
    let storage = setup().await;
    let session = storage.create_session(Some("user-0001")).await.unwrap();

    assert_eq!(session.persona_uid.as_deref(), Some("user-0001"));
    assert!(session.ended_at.is_none());

    // get 应返回相同 persona_uid
    let got = storage.get_session(session.id).await.unwrap().unwrap();
    assert_eq!(got.persona_uid.as_deref(), Some("user-0001"));
}

/// 存量兼容：不传 persona_uid 时，session.persona_uid 为 None。
#[tokio::test]
async fn session_without_persona_uid_compatible() {
    let storage = setup().await;
    let session = storage.create_session(None).await.unwrap();

    assert!(session.persona_uid.is_none());
    assert!(session.ended_at.is_none());

    // get 应返回 None
    let got = storage.get_session(session.id).await.unwrap().unwrap();
    assert!(got.persona_uid.is_none());
}

/// 活跃 session 列表正确返回 persona_uid。
#[tokio::test]
async fn active_sessions_preserve_persona_uid() {
    let storage = setup().await;

    let s1 = storage.create_session(Some("char-0001")).await.unwrap();
    let s2 = storage.create_session(Some("char-0002")).await.unwrap();
    let _s3 = storage.create_session(None).await.unwrap();

    let active = storage.list_active_sessions().await.unwrap();
    // 所有 session 都是活跃的
    assert!(active.len() >= 3);

    let got1 = active.iter().find(|s| s.id == s1.id).unwrap();
    assert_eq!(got1.persona_uid.as_deref(), Some("char-0001"));

    let got2 = active.iter().find(|s| s.id == s2.id).unwrap();
    assert_eq!(got2.persona_uid.as_deref(), Some("char-0002"));
}

/// 全部 session 列表正确返回 persona_uid。
#[tokio::test]
async fn all_sessions_preserve_persona_uid() {
    let storage = setup().await;

    let s = storage.create_session(Some("rama-0001")).await.unwrap();
    storage.close_session(s.id).await.unwrap();

    let all = storage.list_sessions().await.unwrap();
    let got = all.iter().find(|x| x.id == s.id).unwrap();
    assert_eq!(got.persona_uid.as_deref(), Some("rama-0001"));
}

/// 会话通道列：空库初始化后 `sessions` 表含 channel / external_ref 与联合索引，
/// 且 `channel` 默认值保证存量行升级后取 `local`。
#[tokio::test]
async fn sessions_channel_columns_present() {
    let pool = database::init_test_pool()
        .await
        .expect("测试数据库初始化失败");

    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('sessions')")
        .fetch_all(&pool)
        .await
        .expect("查询 sessions 表结构失败");
    assert!(columns.contains(&"channel".to_string()), "缺少 channel 列");
    assert!(
        columns.contains(&"external_ref".to_string()),
        "缺少 external_ref 列"
    );

    // channel 默认值：存量行（不显式写 channel）升级后取 'local'
    let default_value: Option<String> = sqlx::query_scalar(
        "SELECT dflt_value FROM pragma_table_info('sessions') WHERE name = 'channel'",
    )
    .fetch_one(&pool)
    .await
    .expect("查询 channel 默认值失败");
    assert_eq!(
        default_value.as_deref(),
        Some("'local'"),
        "channel 默认值应为 'local'（存量行取默认值）"
    );

    let indexes: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_index_list('sessions')")
        .fetch_all(&pool)
        .await
        .expect("查询 sessions 索引失败");
    assert!(
        indexes.contains(&"idx_sessions_channel_external_ref".to_string()),
        "缺少 (channel, external_ref) 联合索引"
    );
}
