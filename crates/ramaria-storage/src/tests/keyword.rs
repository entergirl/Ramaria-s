//! crates/ramaria-storage/src/tests/keyword.rs - 关键词池存储测试
//!
//! 设计特点:
//! - 覆盖关键词 upsert 与非法文本显式拒绝
//! - 覆盖规范词 / 已确认词表读取口径（pending 别名排除）
//! - 覆盖待确认别名登记、确认与驳回链路
//! - 覆盖全量词条行视图（rowid / 别名状态 / 规范词指向）

use super::*;

#[tokio::test]
async fn keyword_upsert() {
    let storage = setup().await;
    storage.upsert_keyword("工作").await.unwrap();
    storage.upsert_keyword("工作").await.unwrap();
    let keywords = storage.list_keywords().await.unwrap();
    assert!(keywords.contains(&"工作".to_string()));
}

/// 规范词读取：仅返回 canonical（canonical_id IS NULL），排除 pending 别名。
#[tokio::test]
async fn list_canonical_keywords_excludes_aliases() {
    let storage = setup().await;

    // 规范词（canonical 形态）
    let canonical = KeywordToken::new("工作压力").unwrap();
    repo::keyword::upsert_with_alias(&storage.pool, &canonical, 0, "canonical")
        .await
        .unwrap();
    // 纯 upsert（alias_status NULL）形态的规范词
    storage.upsert_keyword("爬山").await.unwrap();
    // pending 别名（指向 工作压力）——不应出现在规范词列表
    let canonical_id: i64 = sqlx::query_scalar("SELECT rowid FROM keyword_pool WHERE keyword = ?")
        .bind("工作压力")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
    let alias = KeywordToken::new("职场焦虑").unwrap();
    repo::keyword::upsert_with_alias(&storage.pool, &alias, canonical_id, "pending")
        .await
        .unwrap();

    let canonicals = storage.list_canonical_keywords().await.unwrap();
    assert!(canonicals.contains(&"工作压力".to_string()));
    assert!(canonicals.contains(&"爬山".to_string()));
    assert!(
        !canonicals.contains(&"职场焦虑".to_string()),
        "pending 别名不应出现在规范词（词典）列表"
    );
}

/// 已确认词表读取：canonical + 已确认 alias 入表，pending 排除（与 memory 侧同口径）。
#[tokio::test]
async fn list_established_keywords_includes_alias_excludes_pending() {
    let storage = setup().await;

    // canonical 形态（alias_status = 'canonical'）
    repo::keyword::upsert_with_alias(
        &storage.pool,
        &KeywordToken::new("工作压力").unwrap(),
        0,
        "canonical",
    )
    .await
    .unwrap();
    // 纯 upsert 形态（alias_status NULL）的规范词
    storage.upsert_keyword("爬山").await.unwrap();
    let canonical_id: i64 = sqlx::query_scalar("SELECT rowid FROM keyword_pool WHERE keyword = ?")
        .bind("工作压力")
        .fetch_one(&storage.pool)
        .await
        .unwrap();

    // 已确认 alias（指向 工作压力）
    repo::keyword::upsert_with_alias(
        &storage.pool,
        &KeywordToken::new("职业倦怠").unwrap(),
        canonical_id,
        "alias",
    )
    .await
    .unwrap();
    // pending（指向 工作压力）——不应出现在已确认词表
    repo::keyword::upsert_with_alias(
        &storage.pool,
        &KeywordToken::new("职场焦虑").unwrap(),
        canonical_id,
        "pending",
    )
    .await
    .unwrap();

    let established = storage.list_established_keywords().await.unwrap();
    assert!(established.contains(&"工作压力".to_string()));
    assert!(established.contains(&"爬山".to_string()));
    assert!(established.contains(&"职业倦怠".to_string()));
    assert!(
        !established.contains(&"职场焦虑".to_string()),
        "pending 别名不应出现在已确认词表"
    );
}

/// 全量词条行读取（StoreCrud trait 方法 → repo::list_pool_rows 接线）：
/// 返回行带 rowid / 别名状态 / 规范词指向。
#[tokio::test]
async fn list_keyword_pool_entries_via_trait() {
    let storage = setup().await;
    storage.upsert_keyword("工作压力").await.unwrap();
    let canonical_id: i64 = sqlx::query_scalar("SELECT rowid FROM keyword_pool WHERE keyword = ?")
        .bind("工作压力")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
    repo::keyword::upsert_with_alias(
        &storage.pool,
        &KeywordToken::new("职场焦虑").unwrap(),
        canonical_id,
        "pending",
    )
    .await
    .unwrap();

    let rows = storage.list_keyword_pool_entries().await.unwrap();
    assert_eq!(rows.len(), 2, "规范词 + pending 别名共 2 条");
    let canonical = rows.iter().find(|r| r.keyword == "工作压力").unwrap();
    assert_eq!(canonical.rowid, canonical_id);
    let pending = rows.iter().find(|r| r.keyword == "职场焦虑").unwrap();
    assert_eq!(pending.alias_status.as_deref(), Some("pending"));
    assert_eq!(pending.canonical_id, Some(canonical_id));
}

/// 待确认别名链路：列表 → 确认（pending 迁移为 alias）→ 驳回；未命中返回 false。
#[tokio::test]
async fn keyword_alias_pending_flow_via_trait() {
    let storage = setup().await;
    storage.upsert_keyword("工作压力").await.unwrap();
    let canonical_id: i64 = sqlx::query_scalar("SELECT rowid FROM keyword_pool WHERE keyword = ?")
        .bind("工作压力")
        .fetch_one(&storage.pool)
        .await
        .unwrap();

    // 两条 pending：一条用于确认、一条用于驳回
    for alias in ["职场焦虑", "职业倦怠"] {
        repo::keyword::upsert_with_alias(
            &storage.pool,
            &KeywordToken::new(alias).unwrap(),
            canonical_id,
            "pending",
        )
        .await
        .unwrap();
    }

    let pending = storage.list_pending_aliases().await.unwrap();
    assert_eq!(pending.len(), 2);
    let anxious = pending
        .iter()
        .find(|p| p.alias_keyword == "职场焦虑")
        .expect("职场焦虑应为待确认别名");
    assert_eq!(anxious.canonical_id, canonical_id);
    assert_eq!(anxious.canonical_keyword, "工作压力");
    assert!(anxious.created_at > 0);

    // 确认：pending → alias；重复确认未命中返回 false
    assert!(
        storage
            .confirm_keyword_alias(anxious.alias_id)
            .await
            .unwrap()
    );
    assert!(
        !storage
            .confirm_keyword_alias(anxious.alias_id)
            .await
            .unwrap()
    );

    // 驳回：另一条 pending → 独立规范词；重复驳回未命中返回 false
    let burnout = pending
        .iter()
        .find(|p| p.alias_keyword == "职业倦怠")
        .expect("职业倦怠应为待确认别名");
    assert!(
        storage
            .reject_keyword_alias(burnout.alias_id)
            .await
            .unwrap()
    );
    assert!(
        !storage
            .reject_keyword_alias(burnout.alias_id)
            .await
            .unwrap()
    );

    // 全链路收尾：pending 清空；不存在的 rowid 同样未命中
    assert!(storage.list_pending_aliases().await.unwrap().is_empty());
    assert!(!storage.confirm_keyword_alias(999_999).await.unwrap());
    assert!(!storage.reject_keyword_alias(999_999).await.unwrap());
}

/// 待确认别名登记（StoreCrud trait 方法 → repo::upsert_pending 接线）：
/// 首次插入 true、重复 false 且不改状态，列表指针正确；非法文本显式拒绝。
#[tokio::test]
async fn upsert_pending_alias_via_trait() {
    let storage = setup().await;
    storage.upsert_keyword("工作压力").await.unwrap();
    let canonical_id: i64 = sqlx::query_scalar("SELECT rowid FROM keyword_pool WHERE keyword = ?")
        .bind("工作压力")
        .fetch_one(&storage.pool)
        .await
        .unwrap();

    assert!(
        storage
            .upsert_pending_alias("职场压力", canonical_id, 4)
            .await
            .unwrap(),
        "首次登记应插入"
    );
    assert!(
        !storage
            .upsert_pending_alias("职场压力", canonical_id, 9)
            .await
            .unwrap(),
        "重复登记应未命中"
    );

    let pending = storage.list_pending_aliases().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].alias_keyword, "职场压力");
    assert_eq!(pending[0].canonical_id, canonical_id);
    assert_eq!(pending[0].canonical_keyword, "工作压力");

    // 非法文本显式拒绝（不静默丢词）
    let err = storage
        .upsert_pending_alias("   ", canonical_id, 1)
        .await
        .expect_err("空文本应拒绝");
    assert!(matches!(err, RamariaError::Validation { .. }));
}

/// 非法关键词必须显式报错（不再 warn 后 Ok，避免静默丢词）。
#[tokio::test]
async fn upsert_keyword_rejects_invalid_token() {
    let storage = setup().await;
    let err = storage.upsert_keyword("").await.expect_err("空串应拒绝");
    assert!(matches!(err, RamariaError::Validation { .. }));
    // 合法词条仍可正常写入
    storage.upsert_keyword("工作").await.unwrap();
}
