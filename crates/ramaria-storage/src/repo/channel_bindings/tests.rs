//! crates/ramaria-storage/src/repo/channel_bindings/tests.rs - 通道绑定行存取单元测试
//!
//! 设计特点:
//! - 覆盖首建与同键 upsert 更新的时间语义（created_at 保持、updated_at 刷新）
//! - 覆盖查询命中与未命中、列表排序与删除幂等
//! - 覆盖 persona_uid 的 NULL 与有值往返

use super::*;
use crate::database::init_test_pool;

/// 首建：返回完整行，时间戳为正，人格未覆盖。
#[tokio::test]
async fn upsert_inserts_new_binding() {
    let pool = init_test_pool().await.expect("测试库初始化失败");

    let binding = upsert(&pool, "qq", "c2c:10001", None, true, false)
        .await
        .expect("首建 upsert 应成功");
    assert!(binding.id > 0, "应分配自增 id");
    assert_eq!(binding.channel, "qq");
    assert_eq!(binding.external_ref, "c2c:10001");
    assert_eq!(binding.persona_uid, None, "未指定人格应为 None");
    assert!(binding.enabled, "enabled 应为 true");
    assert!(!binding.allow_proactive, "allow_proactive 应为 false");
    assert!(binding.created_at > 0, "首建 created_at 应为当前时间");
    assert!(binding.updated_at >= binding.created_at);
}

/// 同键 upsert 更新：created_at 保持、updated_at 刷新、字段覆盖。
#[tokio::test]
async fn upsert_updates_existing_binding() {
    let pool = init_test_pool().await.expect("测试库初始化失败");

    let first = upsert(&pool, "qq", "group:20002", None, true, false)
        .await
        .expect("首建 upsert 应成功");
    let second = upsert(&pool, "qq", "group:20002", Some("char-0001"), false, true)
        .await
        .expect("更新 upsert 应成功");

    assert_eq!(second.id, first.id, "同键不应新增行");
    assert_eq!(second.created_at, first.created_at, "更新应保持 created_at");
    assert!(
        second.updated_at >= first.updated_at,
        "更新应刷新 updated_at"
    );
    assert_eq!(second.persona_uid.as_deref(), Some("char-0001"));
    assert!(!second.enabled, "开关应被覆盖");
    assert!(second.allow_proactive, "开关应被覆盖");

    let all = list_all(&pool).await.expect("列出应成功");
    assert_eq!(all.len(), 1, "同键重复写入不应新增行");
}

/// find 命中 / 未命中（未命中为 Ok(None)，非错误）。
#[tokio::test]
async fn find_hits_and_misses() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    upsert(&pool, "telegram", "c2c:42", Some("char-0009"), true, false)
        .await
        .expect("写入应成功");

    let hit = find(&pool, "telegram", "c2c:42")
        .await
        .expect("查询应成功")
        .expect("应命中绑定");
    assert_eq!(hit.persona_uid.as_deref(), Some("char-0009"));

    let miss = find(&pool, "telegram", "c2c:43").await.expect("查询应成功");
    assert_eq!(miss, None, "未命中应为 Ok(None)");
    let cross = find(&pool, "qq", "c2c:42").await.expect("查询应成功");
    assert_eq!(cross, None, "不同通道同外部标识不应命中");
}

/// 列表按 (channel, external_ref) 升序返回。
#[tokio::test]
async fn list_all_orders_by_channel_and_ref() {
    let pool = init_test_pool().await.expect("测试库初始化失败");

    // 乱序插入
    upsert(&pool, "telegram", "c2c:42", None, true, false)
        .await
        .expect("写入应成功");
    upsert(&pool, "qq", "group:20002", None, true, false)
        .await
        .expect("写入应成功");
    upsert(&pool, "qq", "c2c:10001", None, true, false)
        .await
        .expect("写入应成功");

    let all = list_all(&pool).await.expect("列出应成功");
    let keys: Vec<(&str, &str)> = all
        .iter()
        .map(|b| (b.channel.as_str(), b.external_ref.as_str()))
        .collect();
    assert_eq!(
        keys,
        vec![
            ("qq", "c2c:10001"),
            ("qq", "group:20002"),
            ("telegram", "c2c:42"),
        ],
        "应按 channel / external_ref 升序"
    );
}

/// 删除幂等：删后 find 无；重复删除仍成功。
#[tokio::test]
async fn delete_is_idempotent() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    upsert(&pool, "qq", "c2c:10001", None, true, false)
        .await
        .expect("写入应成功");

    delete(&pool, "qq", "c2c:10001").await.expect("删除应成功");
    assert_eq!(
        find(&pool, "qq", "c2c:10001").await.expect("查询应成功"),
        None,
        "删后不应命中"
    );
    delete(&pool, "qq", "c2c:10001")
        .await
        .expect("重复删除应成功（幂等）");
    assert!(
        list_all(&pool).await.expect("列出应成功").is_empty(),
        "删除后列表应为空"
    );
}

/// persona_uid 往返：None 保持 NULL、有值保持原值。
#[tokio::test]
async fn persona_uid_roundtrips() {
    let pool = init_test_pool().await.expect("测试库初始化失败");

    upsert(&pool, "qq", "c2c:10001", None, true, false)
        .await
        .expect("写入应成功");
    upsert(&pool, "qq", "group:20002", Some("char-0001"), true, false)
        .await
        .expect("写入应成功");

    let none_row = find(&pool, "qq", "c2c:10001")
        .await
        .expect("查询应成功")
        .expect("应命中");
    assert_eq!(none_row.persona_uid, None, "NULL 应往返为 None");

    let some_row = find(&pool, "qq", "group:20002")
        .await
        .expect("查询应成功")
        .expect("应命中");
    assert_eq!(some_row.persona_uid.as_deref(), Some("char-0001"));

    // 覆盖回 None：更新后应回到未覆盖态
    upsert(&pool, "qq", "group:20002", None, true, false)
        .await
        .expect("覆盖更新应成功");
    let cleared = find(&pool, "qq", "group:20002")
        .await
        .expect("查询应成功")
        .expect("应命中");
    assert_eq!(cleared.persona_uid, None, "覆盖为 None 应回未覆盖态");
}
