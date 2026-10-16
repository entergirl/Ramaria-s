//! crates/ramaria-storage/src/repo/sticker_assets/tests.rs - 表情资产行存取单元测试
//!
//! 设计特点:
//! - 覆盖插入 / 查询 / 列表排序 / 部分字段更新 / 删除的往返
//! - 覆盖更新 None 保持现值与未命中返回 false
//! - 覆盖中文标签经 JSON 文本落库后的读回

use super::*;
use crate::database::init_test_pool;

/// 构造表情资产行（时间固定，便于断言 updated_at 刷新）。
fn asset(file_name: &str, tags: &[&str]) -> StickerAsset {
    StickerAsset {
        id: 0,
        file_name: file_name.to_string(),
        file_path: file_name.to_string(),
        md5: Some("aabbccddeeff00112233445566778899".to_string()),
        size: Some(2048),
        description: None,
        tags: tags.iter().map(|s| s.to_string()).collect(),
        created_at: 100,
        updated_at: 100,
    }
}

/// 插入后按 id 读回全字段（含中文标签与 md5 / size）。
#[tokio::test]
async fn insert_and_get_roundtrip() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let id = insert(&pool, &asset("cat.png", &["猫猫", "表情"]))
        .await
        .expect("插入应成功");
    assert!(id > 0, "自增 id 应为正");

    let got = get(&pool, id).await.unwrap().expect("应命中");
    assert_eq!(got.file_name, "cat.png");
    assert_eq!(got.file_path, "cat.png");
    assert_eq!(got.md5.as_deref(), Some("aabbccddeeff00112233445566778899"));
    assert_eq!(got.size, Some(2048));
    assert_eq!(got.description, None);
    assert_eq!(got.tags, vec!["猫猫".to_string(), "表情".to_string()]);
    assert_eq!(got.created_at, 100);
    assert_eq!(got.updated_at, 100);
    assert!(
        get(&pool, id + 999).await.unwrap().is_none(),
        "未命中 id 应为 None"
    );
}

/// 列表按 id 升序返回全部资产。
#[tokio::test]
async fn list_all_orders_by_id() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let first = insert(&pool, &asset("a.png", &[]))
        .await
        .expect("插入应成功");
    let second = insert(&pool, &asset("b.png", &["风景"]))
        .await
        .expect("插入应成功");

    let all = list_all(&pool).await.expect("列表应成功");
    let ids: Vec<i64> = all.iter().map(|item| item.id).collect();
    assert_eq!(ids, vec![first, second], "应按 id 升序返回");
    assert_eq!(all[1].tags, vec!["风景".to_string()]);
}

/// 部分字段更新：None 保持现值、Some 覆盖，且刷新 updated_at；未命中返回 false。
#[tokio::test]
async fn update_overrides_and_keeps() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let id = insert(&pool, &asset("c.png", &["旧标签"]))
        .await
        .expect("插入应成功");

    // 仅覆盖描述：标签保持原值
    let updated = update(&pool, id, Some("一只橘猫"), None)
        .await
        .expect("更新应成功");
    assert!(updated, "命中应返回 true");
    let got = get(&pool, id).await.unwrap().expect("应命中");
    assert_eq!(got.description.as_deref(), Some("一只橘猫"));
    assert_eq!(got.tags, vec!["旧标签".to_string()], "None 应保持标签现值");
    assert!(got.updated_at > got.created_at, "更新应刷新 updated_at");

    // 仅覆盖标签：描述保持前值
    let new_tags = vec!["猫".to_string(), "橘".to_string()];
    let updated = update(&pool, id, None, Some(&new_tags))
        .await
        .expect("更新应成功");
    assert!(updated, "命中应返回 true");
    let got = get(&pool, id).await.unwrap().expect("应命中");
    assert_eq!(
        got.description.as_deref(),
        Some("一只橘猫"),
        "None 应保持描述现值"
    );
    assert_eq!(got.tags, new_tags);

    assert!(
        !update(&pool, id + 999, Some("x"), None).await.unwrap(),
        "未命中 id 应返回 false"
    );
}

/// 删除命中返回 true 并移除行；未命中返回 false。
#[tokio::test]
async fn delete_hits_and_misses() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let id = insert(&pool, &asset("d.png", &[]))
        .await
        .expect("插入应成功");

    assert!(delete(&pool, id).await.unwrap(), "命中删除应返回 true");
    assert!(get(&pool, id).await.unwrap().is_none(), "删除后不应命中");
    assert!(!delete(&pool, id).await.unwrap(), "重复删除应返回 false");
}
