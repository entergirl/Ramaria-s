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

/// 旧库升级回填：迁移前已存在的会话经新增列迁移后 `last_read_at` 取最大消息时间
/// （既有会话一律视为已读，零虚假未读）。
///
/// 说明:
/// - 模拟口径：在已全量迁移的测试库上删除本版新增列还原"旧库"状态，
///   写入旧式数据后执行本版 migration 文件（与 `sqlx::migrate!` 应用的是同一份 SQL），
///   验证 SQLite 对既有行的回填与列约束。
/// - 新增列不在任何索引 / 外键 / 约束中，`DROP COLUMN` 仅用于本测试的模拟。
#[tokio::test]
async fn last_read_at_backfills_existing_sessions_on_upgrade() {
    let pool = database::init_test_pool()
        .await
        .expect("测试数据库初始化失败");

    // 空库全量迁移后即含新列
    let has_column: Option<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('sessions') WHERE name = 'last_read_at'",
    )
    .fetch_optional(&pool)
    .await
    .expect("查询列信息应成功");
    assert_eq!(
        has_column.as_deref(),
        Some("last_read_at"),
        "空库初始化后应含 last_read_at 列"
    );

    // 还原旧库：移除本版新增列
    sqlx::query("ALTER TABLE sessions DROP COLUMN last_read_at")
        .execute(&pool)
        .await
        .expect("删除新增列以模拟旧库应成功");

    // 旧式数据：一个带消息的会话（user 100 / assistant 200）与一个空会话
    let with_messages = Uuid::new_v4();
    let empty_session = Uuid::new_v4();
    for session_id in [with_messages, empty_session] {
        sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
            .bind(session_id.to_string())
            .execute(&pool)
            .await
            .expect("插入 session fixture 应成功");
    }
    for (role, ts) in [("user", 100_i64), ("assistant", 200)] {
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, content, created_at, source) \
             VALUES (?, ?, ?, '旧消息', ?, 'local')",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(with_messages.to_string())
        .bind(role)
        .bind(ts)
        .execute(&pool)
        .await
        .expect("写入旧式消息应成功");
    }

    // 执行本版 migration（迁移文件内容即被 sqlx::migrate! 应用的 SQL）
    sqlx::raw_sql(include_str!("../../migrations/20261005_v2.5_unread.sql"))
        .execute(&pool)
        .await
        .expect("执行新增 migration 应成功");

    // 既有会话回填为最大消息时间（已读）；空会话回填 0
    let last_read: i64 = sqlx::query_scalar("SELECT last_read_at FROM sessions WHERE id = ?")
        .bind(with_messages.to_string())
        .fetch_one(&pool)
        .await
        .expect("读取回填值应成功");
    assert_eq!(last_read, 200, "旧会话应回填为最大消息时间（视为已读）");
    let empty_read: i64 = sqlx::query_scalar("SELECT last_read_at FROM sessions WHERE id = ?")
        .bind(empty_session.to_string())
        .fetch_one(&pool)
        .await
        .expect("读取回填值应成功");
    assert_eq!(empty_read, 0, "无消息旧会话应回填 0");

    // 旧库升级后零虚假未读
    let unread = crate::repo::messages::list_unread_counts(&pool)
        .await
        .expect("查询未读应成功");
    assert!(unread.is_empty(), "旧库升级后既有会话应零未读");

    // 列约束：NOT NULL DEFAULT 0（省略该列的写入同样取 0）
    let (notnull, default_value): (i64, Option<String>) = sqlx::query_as(
        "SELECT \"notnull\", dflt_value FROM pragma_table_info('sessions') WHERE name = 'last_read_at'",
    )
    .fetch_one(&pool)
    .await
    .expect("查询列属性应成功");
    assert_eq!(notnull, 1, "新增列应为 NOT NULL");
    assert_eq!(default_value.as_deref(), Some("0"), "新增列默认值应为 0");

    // 省略列写入取默认 0：新会话 + 晚于的助手消息 → 产生未读
    let new_session = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
        .bind(new_session.to_string())
        .execute(&pool)
        .await
        .expect("省略新列写入应成功");
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source) \
         VALUES (?, ?, 'assistant', '新回复', 300, 'local')",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(new_session.to_string())
    .execute(&pool)
    .await
    .expect("写入新式消息应成功");
    let unread = crate::repo::messages::list_unread_counts(&pool)
        .await
        .expect("查询未读应成功");
    assert_eq!(
        unread.get(&new_session).copied(),
        Some(1),
        "新会话晚于默认基线的助手消息应计未读"
    );
}
