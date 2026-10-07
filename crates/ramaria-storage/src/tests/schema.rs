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

/// 旧库升级：messages 新增发送者身份两列（可空无默认）与 session_members 成员表；
/// 旧行两列为 NULL、省略新列写入成功、成员表唯一约束生效。
///
/// 说明:
/// - 模拟口径：在已全量迁移的测试库上删除新增表 / 索引 / 列还原"旧库"状态，
///   写入旧式数据后执行本版 migration 文件（与 `sqlx::migrate!` 应用的是同一份 SQL）。
/// - 带索引的列必须先从属于它的索引删起，否则 `DROP COLUMN` 失败。
#[tokio::test]
async fn sender_identity_migration_on_upgrade() {
    let pool = database::init_test_pool()
        .await
        .expect("测试数据库初始化失败");

    // 空库全量迁移后即含新列与新表
    for column in ["sender_ref", "sender_name"] {
        let (notnull, default_value): (i64, Option<String>) = sqlx::query_as(
            "SELECT \"notnull\", dflt_value FROM pragma_table_info('messages') WHERE name = ?",
        )
        .bind(column)
        .fetch_one(&pool)
        .await
        .expect("查询列属性应成功");
        assert_eq!(notnull, 0, "sender 身份列应可空");
        assert_eq!(default_value, None, "sender 身份列应无默认值");
    }
    let table: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'session_members'",
    )
    .fetch_optional(&pool)
    .await
    .expect("查询表应成功");
    assert_eq!(
        table.as_deref(),
        Some("session_members"),
        "空库初始化后应含 session_members 表"
    );

    // 还原旧库：移除新增表 / 索引 / 列（列上索引必须先删，否则 DROP COLUMN 失败）
    sqlx::query("DROP TABLE session_members")
        .execute(&pool)
        .await
        .expect("删除新增表以模拟旧库应成功");
    sqlx::query("DROP INDEX idx_messages_sender_ref")
        .execute(&pool)
        .await
        .expect("删除新增索引以模拟旧库应成功");
    sqlx::query("ALTER TABLE messages DROP COLUMN sender_ref")
        .execute(&pool)
        .await
        .expect("删除新增列以模拟旧库应成功");
    sqlx::query("ALTER TABLE messages DROP COLUMN sender_name")
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
    sqlx::raw_sql(include_str!("../../migrations/20261007_v2.6_identity.sql"))
        .execute(&pool)
        .await
        .expect("执行新增 migration 应成功");

    // 旧行两列为 NULL
    let (sender_ref, sender_name): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT sender_ref, sender_name FROM messages WHERE content = '旧消息'")
            .fetch_one(&pool)
            .await
            .expect("读取旧行应成功");
    assert_eq!(sender_ref, None, "旧行 sender_ref 应为 NULL");
    assert_eq!(sender_name, None, "旧行 sender_name 应为 NULL");

    // 省略新列的写入成功且取 NULL
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source) \
         VALUES (?, ?, 'assistant', '新消息', 2, 'local')",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(session_id.to_string())
    .execute(&pool)
    .await
    .expect("省略新列写入应成功");
    let (sender_ref, sender_name): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT sender_ref, sender_name FROM messages WHERE content = '新消息'")
            .fetch_one(&pool)
            .await
            .expect("读取新行应成功");
    assert_eq!(sender_ref, None, "省略列写入应取 NULL");
    assert_eq!(sender_name, None, "省略列写入应取 NULL");

    // UNIQUE(session_id, platform_ref) 生效：同键二次插入被拒绝，异键正常
    sqlx::query(
        "INSERT INTO session_members (session_id, platform_ref, name, first_seen_at, last_seen_at) \
         VALUES (?, 'u_a', '成员', 10, 20)",
    )
    .bind(session_id.to_string())
    .execute(&pool)
    .await
    .expect("插入成员行应成功");
    let dup = sqlx::query(
        "INSERT INTO session_members (session_id, platform_ref, name, first_seen_at, last_seen_at) \
         VALUES (?, 'u_a', '成员', 10, 20)",
    )
    .bind(session_id.to_string())
    .execute(&pool)
    .await
    .expect_err("同 (session_id, platform_ref) 应被 UNIQUE 拒绝");
    let unique_in_chain = std::iter::successors(std::error::Error::source(&dup), |e| e.source())
        .map(|e| e.to_string())
        .chain([dup.to_string()])
        .any(|msg| msg.contains("UNIQUE"));
    assert!(
        unique_in_chain,
        "底层错误链应含 UNIQUE 约束冲突，实际: {dup}"
    );
    sqlx::query(
        "INSERT INTO session_members (session_id, platform_ref, name, first_seen_at, last_seen_at) \
         VALUES (?, 'u_b', '另一成员', 30, 40)",
    )
    .bind(session_id.to_string())
    .execute(&pool)
    .await
    .expect("不同 platform_ref 写入应成功");
}

/// 旧库升级：新增 message_attachments 附件表与三个索引；
/// 列缺省值、外键级联在升级后的旧库上生效。
///
/// 说明:
/// - 模拟口径：在已全量迁移的测试库上删除新增表还原“旧库”状态，
///   写入旧式数据后执行本版 migration 文件（与 `sqlx::migrate!` 应用的是同一份 SQL）。
#[tokio::test]
async fn attachments_migration_on_upgrade() {
    let pool = database::init_test_pool()
        .await
        .expect("测试数据库初始化失败");

    // 空库全量迁移后即含附件表与三个索引
    let table: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'message_attachments'",
    )
    .fetch_optional(&pool)
    .await
    .expect("查询表应成功");
    assert_eq!(
        table.as_deref(),
        Some("message_attachments"),
        "空库初始化后应含 message_attachments 表"
    );
    for index in [
        "idx_attachments_message_id",
        "idx_attachments_status",
        "idx_attachments_md5",
    ] {
        let found: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?")
                .bind(index)
                .fetch_optional(&pool)
                .await
                .expect("查询索引应成功");
        assert_eq!(
            found.as_deref(),
            Some(index),
            "空库初始化后应含索引 {index}"
        );
    }

    // 还原旧库：删除新增表（表上索引随表一并删除）
    sqlx::query("DROP TABLE message_attachments")
        .execute(&pool)
        .await
        .expect("删除新增表以模拟旧库应成功");

    // 旧式数据：消息必须归属存在的会话（外键约束）
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
        .bind(session_id.to_string())
        .execute(&pool)
        .await
        .expect("插入 session fixture 应成功");
    let message_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source) \
         VALUES (?, ?, 'user', '旧消息', 1, 'local')",
    )
    .bind(message_id.to_string())
    .bind(session_id.to_string())
    .execute(&pool)
    .await
    .expect("写入旧式消息应成功");

    // 执行本版 migration（迁移文件内容即被 sqlx::migrate! 应用的 SQL）
    sqlx::raw_sql(include_str!(
        "../../migrations/20261008_v2.6_attachments.sql"
    ))
    .execute(&pool)
    .await
    .expect("执行新增 migration 应成功");

    // 缺省值：省略可选列写入取默认（source_ref '' / status pending / 时间 0）
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO message_attachments (message_id, kind) VALUES (?, 'image') RETURNING id",
    )
    .bind(message_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("省略可选列写入应成功");
    let (source_ref, status, created_at, updated_at): (String, String, i64, i64) = sqlx::query_as(
        "SELECT source_ref, status, created_at, updated_at FROM message_attachments WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("读取默认值应成功");
    assert_eq!(source_ref, "", "source_ref 缺省应为空串");
    assert_eq!(status, "pending", "status 缺省应为 pending");
    assert_eq!(created_at, 0, "created_at 缺省应为 0");
    assert_eq!(updated_at, 0, "updated_at 缺省应为 0");

    // 完整字段写入回读
    sqlx::query(
        "INSERT INTO message_attachments \
             (message_id, kind, source_ref, md5, size, width, height, sub_type, status, \
              description, description_model, created_at, updated_at) \
         VALUES (?, 'image', 'images/a.png', 'aabbccddeeff00112233445566778899', 1024, 640, 480, \
                 'photo', 'done', '一只橘猫', 'vision-mock', 10, 20)",
    )
    .bind(message_id.to_string())
    .execute(&pool)
    .await
    .expect("完整字段写入应成功");
    let (md5, description): (String, String) = sqlx::query_as(
        "SELECT md5, description FROM message_attachments WHERE message_id = ? AND status = 'done'",
    )
    .bind(message_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("读取完整字段应成功");
    assert_eq!(md5, "aabbccddeeff00112233445566778899");
    assert_eq!(description, "一只橘猫");

    // 外键级联：删除消息后附件行随之消失
    sqlx::query("DELETE FROM messages WHERE id = ?")
        .bind(message_id.to_string())
        .execute(&pool)
        .await
        .expect("删除消息应成功");
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM message_attachments WHERE message_id = ?")
            .bind(message_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("计数应成功");
    assert_eq!(count, 0, "删除消息后附件应级联删除");
}
