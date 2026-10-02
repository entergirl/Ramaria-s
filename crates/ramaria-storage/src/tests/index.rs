//! crates/ramaria-storage/src/tests/index.rs - 索引版本与语料戳存储测试
//!
//! 设计特点:
//! - 覆盖 BM25 分词版本辅助（缺省 / 往返 / 非法值回退）
//! - 覆盖索引语料戳随写入变化的统计口径
//! - 覆盖索引版本缺键按未构建返回与读写往返
//! - 覆盖索引版本修正迁移仅作用于空库

use super::*;

/// BM25 分词版本辅助：缺失默认旧版 1、读写往返、settings 键真实落库。
#[tokio::test]
async fn bm25_index_version_helper() {
    let storage = setup().await;

    // 键缺失 → 默认旧版本 1（None/缺失视为 1）
    assert_eq!(storage.get_bm25_index_version().await.unwrap(), 1);
    assert_eq!(
        storage.get_bm25_index_version().await.unwrap(),
        BM25_INDEX_VERSION_LEGACY
    );

    // 写 2 → 读 2 往返，且底层 settings 表键真实写入
    storage
        .set_bm25_index_version(BM25_INDEX_VERSION_CURRENT)
        .await
        .unwrap();
    assert_eq!(
        storage.get_bm25_index_version().await.unwrap(),
        BM25_INDEX_VERSION_CURRENT
    );
    assert_eq!(
        storage
            .get_setting(SETTING_BM25_INDEX_VERSION)
            .await
            .unwrap()
            .as_deref(),
        Some("2")
    );

    // 非法值回退旧版（防御）
    storage
        .set_setting(SETTING_BM25_INDEX_VERSION, "not-a-number")
        .await
        .unwrap();
    assert_eq!(
        storage.get_bm25_index_version().await.unwrap(),
        BM25_INDEX_VERSION_LEGACY
    );
}

/// 索引语料戳：空库为零值，写入 L1 后条数与时间戳同步变化。
#[tokio::test]
async fn index_corpus_stamp_tracks_writes() {
    let storage = setup().await;

    // 空库：全部为 0（默认值语义，无 NULL 泄漏）
    let empty = storage
        .index_corpus_stamp()
        .await
        .unwrap()
        .expect("SQLite 后端应提供语料统计");
    assert_eq!(empty, IndexCorpusStamp::default());

    // 写入 L1 后：条数增加、最新写入时间非 0（跨进程刷新检测的触发源）
    let session = storage.create_session(None).await.unwrap();
    let l1 = MemoryL1::new(session.id, "用户提到最近在准备考试".to_string(), None);
    storage.save_memory_l1(&l1).await.unwrap();

    let stamp = storage.index_corpus_stamp().await.unwrap().unwrap();
    assert_eq!(stamp.l1_count, 1);
    assert!(stamp.l1_max_created_at > 0, "有写入时最新时间戳应非 0");
    assert_ne!(stamp, empty, "语料变化必须体现在统计戳上");
    // 其余语料未写入：保持 0
    assert_eq!(stamp.event_count, 0);
    assert_eq!(stamp.utt_count, 0);
    assert_eq!(stamp.persona_count, 0);
}

/// 索引版本：缺键按未构建（0）返回；显式写 0 / 1 读写往返一致。
#[tokio::test]
async fn index_version_defaults_to_unbuilt_and_roundtrips() {
    let storage = setup().await;

    // 删除 migration 预置值 → 缺键按未构建口径返回 0
    sqlx::query("DELETE FROM schema_meta WHERE key = 'index_version'")
        .execute(&storage.pool)
        .await
        .expect("删除索引版本键应成功");
    assert_eq!(
        storage.get_index_version().await.unwrap(),
        0,
        "缺键应按未构建（0）返回"
    );

    // 显式写 1 → 读回 1
    storage.set_index_version(1).await.unwrap();
    assert_eq!(storage.get_index_version().await.unwrap(), 1);

    // 显式写 0 → 读回 0（尚未构建）
    storage.set_index_version(0).await.unwrap();
    assert_eq!(storage.get_index_version().await.unwrap(), 0);
}

/// 新库（空库）初始化后索引版本为未构建（0）：空库首启应走"构建 → 就绪"真实链路，
/// 而不是按预置的"已构建"口径跳过构建。
#[tokio::test]
async fn fresh_db_starts_with_unbuilt_index_version() {
    let storage = setup().await;
    assert_eq!(
        storage.get_index_version().await.unwrap(),
        0,
        "空库初始化后索引版本应为 0（尚未构建）"
    );
}

/// 索引版本修正迁移语义：仅空库（四张业务表均无行）把"已构建"值修正为 0；
/// 任一业务表有行时保持原值不动（既有库零改动）。
#[tokio::test]
async fn index_version_migration_only_touches_empty_db() {
    let pool = database::init_test_pool()
        .await
        .expect("测试数据库初始化失败");
    let storage = SqliteStorage::new(pool.clone());
    let migration_sql = include_str!("../../migrations/20261001_v2.3_index_version.sql");

    // 迁移文本覆盖四张业务表的空库判定（防漏检：任一表有行都必须阻止修正）
    for table in ["sessions", "messages", "memory_l1", "memory_events"] {
        assert!(
            migration_sql.contains(table),
            "迁移应包含 {table} 表的空库判定"
        );
    }

    // ---- 空库分支：预置值 1 → 迁移执行后修正为 0 ----
    storage.set_index_version(1).await.unwrap();
    sqlx::raw_sql(migration_sql)
        .execute(&pool)
        .await
        .expect("空库执行迁移 SQL 应成功");
    assert_eq!(
        storage.get_index_version().await.unwrap(),
        0,
        "空库应被修正为未构建（0）"
    );

    // ---- 有业务数据分支：四表任一有行 → 值保持 1 不变（模拟既有库升级启动） ----
    // 表间存在外键引用（messages / memory_l1 → sessions，memory_events → personas），
    // 本用例只验证"表内是否有行"的判定，关闭外键检查以逐表独立造数。
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&pool)
        .await
        .expect("关闭外键检查应成功");
    let now = now_ms();
    let cases: [(&str, String); 4] = [
        (
            "sessions",
            format!("INSERT INTO sessions (id, started_at) VALUES ('seed-session', {now})"),
        ),
        (
            "messages",
            format!(
                "INSERT INTO messages (id, session_id, role, content, created_at, source) \
                 VALUES ('seed-message', 'seed-session', 'user', 'x', {now}, 'local')"
            ),
        ),
        (
            "memory_l1",
            format!(
                "INSERT INTO memory_l1 (id, session_id, summary, created_at) \
                 VALUES ('seed-l1', 'seed-session', 'x', {now})"
            ),
        ),
        (
            "memory_events",
            format!(
                "INSERT INTO memory_events (id, persona_uid, title, summary, start, \"end\", created_at) \
                 VALUES (1, 'seed-persona', 'x', 'x', {now}, {now}, {now})"
            ),
        ),
    ];
    for (table, insert_sql) in cases {
        for clear in ["messages", "memory_l1", "memory_events", "sessions"] {
            sqlx::query(&format!("DELETE FROM {clear}"))
                .execute(&pool)
                .await
                .expect("清空业务表应成功");
        }
        storage.set_index_version(1).await.unwrap();
        sqlx::query(&insert_sql)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("向 {table} 写入种子行应成功: {e}"));

        sqlx::raw_sql(migration_sql)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("{table} 有数据时执行迁移 SQL 应成功: {e}"));
        assert_eq!(
            storage.get_index_version().await.unwrap(),
            1,
            "{table} 有业务数据时索引版本不得被修正"
        );
    }
}
