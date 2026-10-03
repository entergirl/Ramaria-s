//! crates/ramaria-storage/src/tests/schema.rs - schema 与迁移完整性测试
//!
//! 设计特点:
//! - 覆盖 schema 版本读取
//! - 覆盖迁移记录数与迁移目录文件数一致
//! - 覆盖列表查询所需复合索引的齐备性

use super::*;

#[tokio::test]
async fn schema_version() {
    let storage = setup().await;
    let v = storage.get_schema_version().await.unwrap();
    assert!(v >= 1);
}

/// 迁移完整性：空库初始化后 `_sqlx_migrations` 记录数与迁移目录文件数一致。
///
/// 说明:
/// - 历史增量迁移已合并为单个基线 SQL，空库初始化即得基线 schema；
///   其后新增能力以独立增量文件追加（只增不删，不修改既有迁移）。
/// - 若记录数少于文件数，说明初始化未完整执行；多于文件数说明迁移目录混入了
///   已移除的文件（违反增量纪律）。
#[tokio::test]
async fn migration_records_match_directory() {
    let pool = database::init_test_pool()
        .await
        .expect("测试数据库初始化失败");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("查询 migration 记录失败");
    let files = std::fs::read_dir("./migrations")
        .expect("读取迁移目录失败")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sql"))
        .count() as i64;
    assert_eq!(
        count, files,
        "迁移记录数应与迁移目录中的 SQL 文件数一致（基线 + 增量）"
    );
    assert!(files >= 1, "至少应有一个基线迁移文件");
}

/// 旧库升级回填：迁移前已存在的行经新增列迁移后取默认 0。
///
/// 说明:
/// - 模拟口径：在已全量迁移的测试库上删除本版新增列还原"旧库"状态，
///   写入一行旧式数据后执行本版 migration 文件（与 `sqlx::migrate!` 应用的
///   是同一份 SQL），验证 SQLite 对既有行的默认值回填与列约束。
/// - 新增列不在任何索引 / 外键 / 约束中，`DROP COLUMN` 仅用于本测试的模拟。
#[tokio::test]
async fn is_proactive_backfills_existing_rows_on_upgrade() {
    let pool = database::init_test_pool()
        .await
        .expect("测试数据库初始化失败");

    // 还原旧库：移除本版新增列
    sqlx::query("ALTER TABLE messages DROP COLUMN is_proactive")
        .execute(&pool)
        .await
        .expect("删除新增列以模拟旧库应成功");

    // 旧式写入：消息必须归属存在的会话（外键约束）
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
        .bind(session_id.to_string())
        .execute(&pool)
        .await
        .expect("插入 session fixture 应成功");
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source) \
         VALUES (?, ?, 'user', '旧消息', 1, 'local')",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(session_id.to_string())
    .execute(&pool)
    .await
    .expect("写入旧式消息应成功");

    // 执行本版 migration（迁移文件内容即被 sqlx::migrate! 应用的 SQL）
    sqlx::raw_sql(include_str!("../../migrations/20261002_v2.4_proactive.sql"))
        .execute(&pool)
        .await
        .expect("执行新增 migration 应成功");

    // 旧行回填 0
    let backfilled: i64 = sqlx::query_scalar("SELECT is_proactive FROM messages LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("读取回填值应成功");
    assert_eq!(backfilled, 0, "迁移前旧行应回填默认值 0");

    // 列约束：NOT NULL DEFAULT 0（省略该列的写入同样取 0）
    let (notnull, default_value): (i64, Option<String>) = sqlx::query_as(
        "SELECT \"notnull\", dflt_value FROM pragma_table_info('messages') WHERE name = 'is_proactive'",
    )
    .fetch_one(&pool)
    .await
    .expect("查询列属性应成功");
    assert_eq!(notnull, 1, "新增列应为 NOT NULL");
    assert_eq!(default_value.as_deref(), Some("0"), "新增列默认值应为 0");

    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source) \
         VALUES (?, ?, 'assistant', '新消息', 2, 'local')",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(session_id.to_string())
    .execute(&pool)
    .await
    .expect("省略新列写入应成功");
    let defaulted: i64 =
        sqlx::query_scalar("SELECT is_proactive FROM messages WHERE content = '新消息'")
            .fetch_one(&pool)
            .await
            .expect("读取默认值应成功");
    assert_eq!(defaulted, 0, "省略列写入应取默认 0");
}

/// 复合索引齐备（过滤列 + 排序列成对，列表查询不再建临时 B-tree）。
#[tokio::test]
async fn schema_pair_indexes_present() {
    let storage = setup().await;
    let expected = [
        "idx_messages_persona_created",
        "idx_messages_session_created",
        "idx_memory_l1_persona_created",
        "idx_memory_events_persona_created",
        "idx_utt_blocks_session_created",
    ];
    for name in expected {
        let found: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?")
                .bind(name)
                .fetch_optional(&storage.pool)
                .await
                .unwrap();
        assert_eq!(found.as_deref(), Some(name), "缺少复合索引 {name}");
    }
}
