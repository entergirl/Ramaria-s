//! crates/ramaria-storage/src/repo/keyword/tests.rs - 关键词池存储单元测试
//!
//! 设计特点:
//! - 覆盖 keyword_pool CRUD 与使用计数
//! - 覆盖别名归一化 CRUD 与已确认 / 待确认词表口径
//! - 覆盖 CLI 行视图 / rowid / 幂等种子
//! - 覆盖 pending 幂等登记与行内状态保持

use super::*;
use crate::database;
use ramaria_core::keyword::KeywordToken;

/// 创建测试数据库连接池（内存 SQLite，自动运行 migration）
async fn setup() -> SqlitePool {
    database::init_test_pool()
        .await
        .expect("创建测试数据库失败")
}

// ── keyword_pool CRUD ──

#[tokio::test]
async fn test_upsert_and_list() {
    let pool = setup().await;

    let kw = KeywordToken::new("工作压力").unwrap();
    upsert(&pool, &kw).await.unwrap();
    upsert(&pool, &kw).await.unwrap(); // 重复 upsert 递增 use_count

    let list = list_all(&pool).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].as_str(), "工作压力");
}

#[tokio::test]
async fn test_upsert_with_alias() {
    let pool = setup().await;

    let canonical = KeywordToken::new("工作压力").unwrap();
    let alias = KeywordToken::new("职场焦虑").unwrap();

    // 注册规范词
    upsert_with_alias(&pool, &canonical, 0, "canonical")
        .await
        .unwrap();
    // 注册别名（假设规范词 ID 为 1——自增主键从 1 开始）
    upsert_with_alias(&pool, &alias, 1, "alias").await.unwrap();

    // 查询别名列表
    let aliases = list_aliases(&pool, 1).await.unwrap();
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0], "职场焦虑");
}

#[tokio::test]
async fn test_find_canonical_id() {
    let pool = setup().await;

    let kw = KeywordToken::new("压力").unwrap();
    upsert_with_alias(&pool, &kw, 0, "canonical").await.unwrap();

    let cid = find_canonical_id(&pool, &kw).await.unwrap();
    // canonical_id=0 被转为 None 写入 DB，读回应为 None
    assert!(cid.is_none());
}

#[tokio::test]
async fn test_update_alias_status() {
    let pool = setup().await;

    let kw = KeywordToken::new("压力").unwrap();
    upsert(&pool, &kw).await.unwrap();

    // 更新为别名状态
    update_alias_status(&pool, &kw, 1, "alias").await.unwrap();

    let cid = find_canonical_id(&pool, &kw).await.unwrap();
    assert_eq!(cid, Some(1));
}

#[tokio::test]
async fn test_list_all_with_counts() {
    let pool = setup().await;

    upsert(&pool, &KeywordToken::new("A").unwrap())
        .await
        .unwrap();
    upsert(&pool, &KeywordToken::new("A").unwrap())
        .await
        .unwrap();
    upsert(&pool, &KeywordToken::new("B").unwrap())
        .await
        .unwrap();

    let counts = list_all_with_counts(&pool).await.unwrap();
    // A 使用 2 次，B 使用 1 次，按 use_count DESC 排列
    assert_eq!(counts.len(), 2);
    assert_eq!(counts[0], ("a".to_string(), 2));
    assert_eq!(counts[1], ("b".to_string(), 1));
}

/// 取词条 rowid（供别名流测试；避免依赖自增起始值假设）。
async fn rowid_of(pool: &SqlitePool, keyword: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT rowid FROM keyword_pool WHERE keyword = ?")
        .bind(keyword)
        .fetch_one(pool)
        .await
        .unwrap()
}

// ── 别名归一化 CRUD（M3 T-V20-3-005）──

/// 造一组数据：规范词 工作压力 + pending 别名 职场焦虑。
async fn seed_pending(pool: &SqlitePool) -> (i64, i64) {
    upsert_with_alias(
        pool,
        &KeywordToken::new("工作压力").unwrap(),
        0,
        "canonical",
    )
    .await
    .unwrap();
    let canonical_id = rowid_of(pool, "工作压力").await;
    upsert_with_alias(
        pool,
        &KeywordToken::new("职场焦虑").unwrap(),
        canonical_id,
        "pending",
    )
    .await
    .unwrap();
    let alias_id = rowid_of(pool, "职场焦虑").await;
    (canonical_id, alias_id)
}

#[tokio::test]
async fn test_list_canonicals_excludes_aliases() {
    let pool = setup().await;
    let (_, _) = seed_pending(&pool).await;

    // 追加一个纯 upsert（alias_status NULL）的规范词
    upsert(&pool, &KeywordToken::new("爬山").unwrap())
        .await
        .unwrap();

    let canonicals = list_canonicals(&pool).await.unwrap();
    // pending 别名不入规范词列表；upsert 形态（alias_status NULL）与 'canonical' 形态均计入
    let texts: Vec<&str> = canonicals.iter().map(|r| r.keyword.as_str()).collect();
    assert!(texts.contains(&"工作压力"));
    assert!(texts.contains(&"爬山"));
    assert!(
        !texts.contains(&"职场焦虑"),
        "pending 别名不应出现在规范词列表"
    );
    // rowid 与 use_count 带回
    assert!(canonicals.iter().all(|r| r.rowid > 0));
}

#[tokio::test]
async fn test_list_established_includes_alias_excludes_pending() {
    let pool = setup().await;
    let (canonical_id, _) = seed_pending(&pool).await;

    // 已确认 alias（指向 工作压力）
    upsert_with_alias(
        &pool,
        &KeywordToken::new("职业倦怠").unwrap(),
        canonical_id,
        "alias",
    )
    .await
    .unwrap();
    // 孤儿 alias（canonical_id 缺失）——数据异常，按未确认排除
    upsert_with_alias(&pool, &KeywordToken::new("孤儿别名").unwrap(), 0, "alias")
        .await
        .unwrap();

    let established = list_established(&pool).await.unwrap();
    let texts: Vec<&str> = established.iter().map(|r| r.keyword.as_str()).collect();
    assert!(texts.contains(&"工作压力"), "canonical 保留");
    assert!(texts.contains(&"职业倦怠"), "已确认 alias 入表");
    assert!(!texts.contains(&"职场焦虑"), "pending 排除");
    assert!(
        !texts.contains(&"孤儿别名"),
        "缺 canonical_id 的 alias 排除"
    );
    assert!(established.iter().all(|r| r.rowid > 0));
    // 排序稳定：use_count 降序
    let counts: Vec<i64> = established.iter().map(|r| r.use_count).collect();
    assert!(counts.windows(2).all(|w| w[0] >= w[1]), "use_count 降序");
}

#[tokio::test]
async fn test_pending_list_and_confirm_flow() {
    let pool = setup().await;
    let (canonical_id, alias_id) = seed_pending(&pool).await;

    // 1. pending 冲突列表正确（含规范词文本）
    let pending = list_pending_aliases(&pool).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].alias_id, alias_id);
    assert_eq!(pending[0].alias_keyword, "职场焦虑");
    assert_eq!(pending[0].canonical_id, canonical_id);
    assert_eq!(pending[0].canonical_keyword, "工作压力");

    // 2. confirm → pending 清空、状态转 alias
    assert!(confirm_alias(&pool, alias_id).await.unwrap());
    assert!(list_pending_aliases(&pool).await.unwrap().is_empty());
    assert_eq!(
        get_canonical_name(&pool, alias_id)
            .await
            .unwrap()
            .as_deref(),
        Some("工作压力")
    );

    // 3. 幂等：再次 confirm 返回 false
    assert!(!confirm_alias(&pool, alias_id).await.unwrap());
}

#[tokio::test]
async fn test_pending_reject_promotes_to_canonical() {
    let pool = setup().await;
    let (canonical_id, alias_id) = seed_pending(&pool).await;

    assert!(reject_alias(&pool, alias_id).await.unwrap());
    // 晋升后：pending 清空、alias 变独立规范词、canonical 指向解除
    assert!(list_pending_aliases(&pool).await.unwrap().is_empty());
    assert_eq!(
        get_canonical_name(&pool, alias_id)
            .await
            .unwrap()
            .as_deref(),
        Some("职场焦虑")
    );
    let canonicals = list_canonicals(&pool).await.unwrap();
    assert!(canonicals.iter().any(|r| r.keyword == "职场焦虑"));

    // canonical_id 仍指向旧规范词？reject 已清除 → find_canonical_id 应 None
    let cid = find_canonical_id(&pool, &KeywordToken::new("职场焦虑").unwrap())
        .await
        .unwrap();
    assert!(cid.is_none());
    let _ = canonical_id;

    // 幂等：再次 reject 返回 false
    assert!(!reject_alias(&pool, alias_id).await.unwrap());
}

#[tokio::test]
async fn test_get_canonical_name_self_and_missing() {
    let pool = setup().await;
    let (canonical_id, _) = seed_pending(&pool).await;

    // 规范词自身 → 返回自身文本
    assert_eq!(
        get_canonical_name(&pool, canonical_id)
            .await
            .unwrap()
            .as_deref(),
        Some("工作压力")
    );
    // 不存在的 rowid → None
    assert!(get_canonical_name(&pool, 999_999).await.unwrap().is_none());
}

// ── M3 CLI 行视图 / rowid / 幂等种子 ──

#[tokio::test]
async fn test_list_entries_order_and_status_fields() {
    let pool = setup().await;
    // 工作压力 use_count 2、爬山 use_count 1（纯 upsert 形态规范词）
    upsert(&pool, &KeywordToken::new("工作压力").unwrap())
        .await
        .unwrap();
    upsert(&pool, &KeywordToken::new("工作压力").unwrap())
        .await
        .unwrap();
    upsert(&pool, &KeywordToken::new("爬山").unwrap())
        .await
        .unwrap();
    // 职场焦虑 → pending 别名（指向 工作压力）
    let canonical_id = rowid_of(&pool, "工作压力").await;
    upsert_with_alias(
        &pool,
        &KeywordToken::new("职场焦虑").unwrap(),
        canonical_id,
        "pending",
    )
    .await
    .unwrap();
    // 职业倦怠 → 已确认 alias（指向 工作压力）
    upsert_with_alias(
        &pool,
        &KeywordToken::new("职业倦怠").unwrap(),
        canonical_id,
        "alias",
    )
    .await
    .unwrap();

    let entries = list_entries(&pool).await.unwrap();
    // 排序：use_count DESC（工作压力 2 排第一），其余 use_count=1 按 keyword ASC
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].keyword, "工作压力");
    assert_eq!(entries[0].use_count, 2);
    // 同 use_count=1 的三条按 keyword 升序（UTF-8 字节序）：
    // 爬山(U+722C) < 职业倦怠(U+804C 4E1A) < 职场焦虑(U+804C 573A)
    let tail: Vec<&str> = entries[1..].iter().map(|r| r.keyword.as_str()).collect();
    assert_eq!(tail, vec!["爬山", "职业倦怠", "职场焦虑"]);

    // 状态字段：pending 别名携带 canonical_id 与 LEFT JOIN 出的规范词文本
    let pending = entries
        .iter()
        .find(|r| r.keyword == "职场焦虑")
        .expect("pending 别名应出现");
    assert_eq!(pending.alias_status.as_deref(), Some("pending"));
    assert_eq!(pending.canonical_id, Some(canonical_id));
    assert_eq!(pending.canonical_keyword.as_deref(), Some("工作压力"));

    // alias 状态携带规范词文本
    let alias = entries
        .iter()
        .find(|r| r.keyword == "职业倦怠")
        .expect("alias 应出现");
    assert_eq!(alias.alias_status.as_deref(), Some("alias"));
    assert_eq!(alias.canonical_keyword.as_deref(), Some("工作压力"));

    // 规范词自身：canonical_id/canonical_keyword 均 None，alias_status NULL
    let canonical = entries
        .iter()
        .find(|r| r.keyword == "爬山")
        .expect("规范词应出现");
    assert_eq!(canonical.canonical_id, None);
    assert_eq!(canonical.canonical_keyword, None);
    assert_eq!(canonical.alias_status, None);
    assert!(canonical.created_at > 0);
}

#[tokio::test]
async fn test_find_rowid_existing_and_missing() {
    let pool = setup().await;
    upsert(&pool, &KeywordToken::new("工作压力").unwrap())
        .await
        .unwrap();

    let rid = find_rowid(&pool, "工作压力").await.unwrap();
    assert_eq!(rid, Some(rowid_of(&pool, "工作压力").await));

    // 未标准化/不存在均返回 None
    assert!(find_rowid(&pool, "Work Stress").await.unwrap().is_none());
    assert!(find_rowid(&pool, "不存在的词").await.unwrap().is_none());
}

#[tokio::test]
async fn test_seed_canonical_new_inserts_zero_count() {
    let pool = setup().await;
    let kw = KeywordToken::new("手工词").unwrap();

    assert!(seed_canonical(&pool, &kw).await.unwrap(), "新词应插入");

    let entries = list_entries(&pool).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].keyword, "手工词");
    assert_eq!(entries[0].use_count, 0, "种子词 use_count 从 0 起");
    assert_eq!(entries[0].alias_status, None, "种子词为规范词形态");
    assert_eq!(entries[0].canonical_id, None);
    assert_eq!(entries[0].canonical_keyword, None);
}

#[tokio::test]
async fn test_seed_canonical_existing_idempotent_keeps_count() {
    let pool = setup().await;
    let kw = KeywordToken::new("工作压力").unwrap();
    // 先自然累积 2 次（use_count=2）
    upsert(&pool, &kw).await.unwrap();
    upsert(&pool, &kw).await.unwrap();
    let before = rowid_of(&pool, "工作压力").await;

    // 再次种子 → false，不递增 use_count、不改状态
    assert!(
        !seed_canonical(&pool, &kw).await.unwrap(),
        "已存在不应重复插入"
    );
    let entries = list_entries(&pool).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].use_count, 2, "幂等种子不得递增 use_count");
    assert_eq!(rowid_of(&pool, "工作压力").await, before, "rowid 不变");
}

#[tokio::test]
async fn test_seed_canonical_existing_alias_untouched() {
    let pool = setup().await;
    // 工作压力（canonical）+ 职场焦虑（pending 指向它）
    let (canonical_id, alias_id) = seed_pending(&pool).await;
    let alias_kw = KeywordToken::new("职场焦虑").unwrap();
    let canonical_kw = KeywordToken::new("工作压力").unwrap();

    // 对已存在的 pending 别名词条执行种子：不得触碰状态 / canonical_id / use_count
    assert!(!seed_canonical(&pool, &alias_kw).await.unwrap());
    // 对已存在的规范词执行种子：同样保持不动
    assert!(!seed_canonical(&pool, &canonical_kw).await.unwrap());

    let entries = list_entries(&pool).await.unwrap();
    assert_eq!(entries.len(), 2);
    let pending = entries
        .iter()
        .find(|r| r.keyword == "职场焦虑")
        .expect("pending 应保留");
    assert_eq!(pending.alias_status.as_deref(), Some("pending"));
    assert_eq!(pending.canonical_id, Some(canonical_id));
    assert_eq!(pending.canonical_keyword.as_deref(), Some("工作压力"));
    assert_eq!(pending.use_count, 1, "种子不得递增 use_count");
    // 规范词未被改名/改状态
    assert!(
        list_canonicals(&pool)
            .await
            .unwrap()
            .iter()
            .any(|r| r.keyword == "工作压力")
    );
    let _ = alias_id;
}

// ── list_pool_rows（服务镜像装载行）──

/// 装载行携带 rowid + 三态状态字段；canonical_keyword 由 LEFT JOIN 解析。
#[tokio::test]
async fn test_list_pool_rows_carries_rowid_and_status() {
    let pool = setup().await;
    let (canonical_id, _) = seed_pending(&pool).await;
    // 追加一个已确认 alias（指向 工作压力）
    upsert_with_alias(
        &pool,
        &KeywordToken::new("职业倦怠").unwrap(),
        canonical_id,
        "alias",
    )
    .await
    .unwrap();

    let rows = list_pool_rows(&pool).await.unwrap();
    // 3 条：canonical 工作压力 + pending 职场焦虑 + alias 职业倦怠
    assert_eq!(rows.len(), 3);

    let canonical = rows.iter().find(|r| r.keyword == "工作压力").unwrap();
    assert_eq!(canonical.rowid, canonical_id);
    // 规范词形态：alias_status 为 NULL 或 "canonical"（本例 seed 写入为 "canonical"），
    // canonical_id / canonical_keyword 恒为 None
    assert!(matches!(
        canonical.alias_status.as_deref(),
        None | Some("canonical")
    ));
    assert_eq!(canonical.canonical_id, None);
    assert_eq!(canonical.canonical_keyword, None);
    assert!(canonical.created_at > 0);

    let pending = rows.iter().find(|r| r.keyword == "职场焦虑").unwrap();
    assert_eq!(pending.alias_status.as_deref(), Some("pending"));
    assert_eq!(pending.canonical_id, Some(canonical_id));
    assert_eq!(pending.canonical_keyword.as_deref(), Some("工作压力"));

    let alias = rows.iter().find(|r| r.keyword == "职业倦怠").unwrap();
    assert_eq!(alias.alias_status.as_deref(), Some("alias"));
    assert_eq!(alias.canonical_id, Some(canonical_id));
    assert_eq!(alias.canonical_keyword.as_deref(), Some("工作压力"));
}

#[tokio::test]
async fn test_seed_canonical_concurrent_is_idempotent() {
    let pool = setup().await;
    let kw = KeywordToken::new("并发词").unwrap();
    let (a, b) = tokio::join!(seed_canonical(&pool, &kw), seed_canonical(&pool, &kw));
    let inserted = [a.unwrap(), b.unwrap()];
    assert_eq!(
        inserted.iter().filter(|x| **x).count(),
        1,
        "并发 seed 同一词条恰好插入一次"
    );
    let entries = list_entries(&pool).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].use_count, 0);
}

/// canonical_id 契约：别名行的 canonical_id 恒等于规范词行的隐式 rowid
/// （keyword_pool 无显式 INTEGER 主键，故禁止对该表执行 VACUUM）。
#[tokio::test]
async fn test_alias_points_to_canonical_rowid() {
    let pool = setup().await;
    let (canonical_id, alias_id) = seed_pending(&pool).await;
    let stored: Option<i64> =
        sqlx::query_scalar("SELECT canonical_id FROM keyword_pool WHERE rowid = ?")
            .bind(alias_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        stored,
        Some(canonical_id),
        "别名 canonical_id 必须指向规范词 rowid"
    );
    assert_eq!(
        get_canonical_name(&pool, alias_id)
            .await
            .unwrap()
            .as_deref(),
        Some("工作压力")
    );
}

// ── pending 幂等登记（建议落库）──

/// 首次登记 true 且携带观测计数；重复登记 false 且不改动已有行（状态 / 指针 / 计数）。
#[tokio::test]
async fn test_upsert_pending_inserts_then_is_idempotent() {
    let pool = setup().await;
    let (canonical_id, _) = seed_pending(&pool).await;
    let alias = KeywordToken::new("压力").unwrap();

    assert!(
        upsert_pending(&pool, &alias, canonical_id, 5)
            .await
            .unwrap(),
        "新别名首次登记应插入"
    );

    let entries = list_entries(&pool).await.unwrap();
    let pending = entries
        .iter()
        .find(|e| e.keyword == "压力")
        .expect("pending 别名应存在");
    assert_eq!(pending.alias_status.as_deref(), Some("pending"));
    assert_eq!(pending.canonical_id, Some(canonical_id));
    assert_eq!(pending.canonical_keyword.as_deref(), Some("工作压力"));
    assert_eq!(pending.use_count, 5, "use_count 取观测计数");

    // 重复登记（换观测计数）→ false 且整行保持不动
    assert!(
        !upsert_pending(&pool, &alias, canonical_id, 9)
            .await
            .unwrap(),
        "已存在词条重复登记应未命中"
    );
    let entries = list_entries(&pool).await.unwrap();
    let pending = entries
        .iter()
        .find(|e| e.keyword == "压力")
        .expect("pending 别名应保留");
    assert_eq!(pending.use_count, 5, "重复登记不得改写 use_count");
    assert_eq!(pending.alias_status.as_deref(), Some("pending"));

    // 待确认列表可列出且指针正确（另含 seed_pending 预置的「职场焦虑」）
    let listed = list_pending_aliases(&pool).await.unwrap();
    assert_eq!(listed.len(), 2);
    let mine = listed
        .iter()
        .find(|row| row.alias_keyword == "压力")
        .expect("新登记别名应出现在待确认列表");
    assert_eq!(mine.canonical_id, canonical_id);
    assert_eq!(mine.canonical_keyword, "工作压力");
}

// ── list_keyword_statuses（按文本批量状态查询）──

/// 混合三态返回：canonical / alias / pending 各一；不存在的文本不返回；空输入空结果。
#[tokio::test]
async fn test_list_keyword_statuses_mixed_and_missing() {
    let pool = setup().await;
    // 工作压力（canonical）+ 职场焦虑（pending 指向它）
    let (canonical_id, _) = seed_pending(&pool).await;
    // 职业倦怠（已确认 alias，指向 工作压力）
    upsert_with_alias(
        &pool,
        &KeywordToken::new("职业倦怠").unwrap(),
        canonical_id,
        "alias",
    )
    .await
    .unwrap();

    let queried = vec![
        "工作压力".to_string(),
        "职场焦虑".to_string(),
        "职业倦怠".to_string(),
        "不存在的词".to_string(),
    ];
    let result = list_keyword_statuses(&pool, &queried).await.unwrap();
    assert_eq!(result.len(), 3, "不存在的文本不应出现在结果中");

    let status = |text: &str| {
        result
            .iter()
            .find(|(keyword, _)| keyword == text)
            .map(|(_, alias_status)| alias_status.clone())
            .expect("查询文本应出现在结果中")
    };
    assert_eq!(status("工作压力"), Some("canonical".to_string()));
    assert_eq!(status("职场焦虑"), Some("pending".to_string()));
    assert_eq!(status("职业倦怠"), Some("alias".to_string()));
    assert!(!result.iter().any(|(keyword, _)| keyword == "不存在的词"));

    // 空输入 → 空结果
    assert!(list_keyword_statuses(&pool, &[]).await.unwrap().is_empty());
}

/// 分片路径：超过单片上限（500）的输入按片查询，不存在的词条全部不返回。
#[tokio::test]
async fn test_list_keyword_statuses_chunks_large_input() {
    let pool = setup().await;
    upsert(&pool, &KeywordToken::new("工作压力").unwrap())
        .await
        .unwrap();

    // 501 个文本触发两片查询（首片含命中词，其余全部缺席）
    let mut queried = vec!["工作压力".to_string()];
    for i in 0..500 {
        queried.push(format!("缺席词{i}"));
    }
    let result = list_keyword_statuses(&pool, &queried).await.unwrap();
    assert_eq!(result.len(), 1, "仅有命中词条返回");
    assert_eq!(result[0].0, "工作压力");
    assert_eq!(
        result[0].1, None,
        "纯 upsert 形态规范词 alias_status 为 NULL"
    );
}

/// 已存在规范词不被登记改写：对 canonical 行登记 pending → false 且状态不变。
#[tokio::test]
async fn test_upsert_pending_keeps_existing_canonical() {
    let pool = setup().await;
    upsert_with_alias(
        &pool,
        &KeywordToken::new("工作压力").unwrap(),
        0,
        "canonical",
    )
    .await
    .unwrap();
    upsert_with_alias(
        &pool,
        &KeywordToken::new("职场压力").unwrap(),
        0,
        "canonical",
    )
    .await
    .unwrap();
    let canonical_id = rowid_of(&pool, "工作压力").await;

    assert!(
        !upsert_pending(
            &pool,
            &KeywordToken::new("职场压力").unwrap(),
            canonical_id,
            4
        )
        .await
        .unwrap(),
        "已存在词条不应被改写为 pending"
    );

    let entries = list_entries(&pool).await.unwrap();
    let existing = entries
        .iter()
        .find(|e| e.keyword == "职场压力")
        .expect("规范词应保留");
    assert_eq!(existing.alias_status.as_deref(), Some("canonical"));
    assert_eq!(existing.canonical_id, None);
    assert_eq!(existing.use_count, 1);
    assert!(list_pending_aliases(&pool).await.unwrap().is_empty());
}
