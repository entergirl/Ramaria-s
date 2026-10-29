//! crates/ramaria-storage/src/tests/schema.rs - schema 与迁移完整性测试
//!
//! 设计特点:
//! - 覆盖 schema 版本读取与基线终值（表 / 列 / 显式索引 / 外键计数）
//! - 覆盖关键表的列形态与主键（向量表、utt 块、后台任务、L2 去重指纹）
//! - 覆盖迁移记录数与迁移目录文件数一致
//! - 覆盖列表查询所需复合索引的齐备性
//! - 覆盖启动期 schema 版本门禁（旧库拒绝、原库不被改写、新库可重复启动）

use super::*;

#[tokio::test]
async fn schema_version() {
    let storage = setup().await;
    let v = storage.get_schema_version().await.unwrap();
    assert_eq!(
        v,
        crate::repo::schema_meta::CURRENT_SCHEMA_VERSION,
        "schema 版本应与当前程序常量一致"
    );
}

/// 迁移完整性：空库初始化后 `_sqlx_migrations` 记录数与迁移目录文件数一致。
///
/// 说明:
/// - 基线为单个 migration 文件，空库初始化即得完整 schema；
/// - 记录数与文件数不等说明初始化未完整执行，或迁移目录混入了多余文件。
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
    assert_eq!(count, files, "迁移记录数应与迁移目录中的 SQL 文件数一致");
    assert_eq!(files, 1, "基线应为单一迁移文件");
}

/// 基线终值：表 / 列 / 显式索引 / 外键计数与 schema 审计一致。
#[tokio::test]
async fn baseline_schema_totals() {
    let storage = setup().await;
    let pool = &storage.pool;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .expect("查询表清单应成功");
    assert_eq!(tables.len(), 29, "表数应为 29");

    let mut columns = 0usize;
    let mut foreign_keys = 0usize;
    for table in &tables {
        let cols: Vec<String> =
            sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .fetch_all(pool)
                .await
                .expect("查询列信息应成功");
        columns += cols.len();

        let fks: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT \"table\" FROM pragma_foreign_key_list('{table}')"
        ))
        .fetch_all(pool)
        .await
        .expect("查询外键应成功");
        foreign_keys += fks.len();
    }
    assert_eq!(columns, 261, "列数应为 261");
    assert_eq!(foreign_keys, 28, "外键数应为 28");

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(pool)
    .await
    .expect("查询索引清单应成功");
    assert_eq!(indexes.len(), 45, "显式索引数应为 45");
}

/// 已删除对象不得出现在基线中。
#[tokio::test]
async fn removed_tables_and_columns_absent() {
    let storage = setup().await;
    let pool = &storage.pool;

    for table in ["pending_push", "bm25_index", "keyword_refs"] {
        let found: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(table)
                .fetch_optional(pool)
                .await
                .expect("查询表应成功");
        assert!(found.is_none(), "表 {table} 不应存在");
    }

    let utt_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('utt_blocks')")
            .fetch_all(pool)
            .await
            .expect("查询 utt_blocks 列应成功");
    assert!(
        !utt_columns.iter().any(|c| c == "embedding"),
        "utt_blocks 不应含 embedding 列"
    );

    let event_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('memory_events')")
            .fetch_all(pool)
            .await
            .expect("查询 memory_events 列应成功");
    for gone in ["indexed_at", "index_version"] {
        assert!(
            !event_columns.iter().any(|c| c == gone),
            "memory_events 不应含 {gone} 列"
        );
    }

    let key: Option<String> =
        sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = 'index_version'")
            .fetch_optional(pool)
            .await
            .expect("查询 schema_meta 应成功");
    assert!(key.is_none(), "schema_meta 不应再写入索引版本键");
}

/// 关键表列形态与主键：向量表、后台任务、L2 去重指纹。
#[tokio::test]
async fn key_table_shapes() {
    let storage = setup().await;
    let pool = &storage.pool;

    let vector_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('memory_vectors')")
            .fetch_all(pool)
            .await
            .expect("查询 memory_vectors 列应成功");
    assert_eq!(
        vector_columns,
        vec!["doc_type", "doc_id", "model", "dim", "vector", "updated_at"]
    );
    let pk: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('memory_vectors') WHERE pk > 0 ORDER BY pk",
    )
    .fetch_all(pool)
    .await
    .expect("查询主键应成功");
    assert_eq!(
        pk,
        vec!["doc_type", "doc_id"],
        "主键应为 (doc_type, doc_id)"
    );

    let job_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('background_jobs')")
            .fetch_all(pool)
            .await
            .expect("查询 background_jobs 列应成功");
    assert_eq!(job_columns.len(), 7, "background_jobs 应为 7 列");
    for gone in ["started_at", "retry_count", "max_retries"] {
        assert!(
            !job_columns.iter().any(|c| c == gone),
            "background_jobs 不应含 {gone} 列"
        );
    }

    let fingerprint_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('l2_cluster_fingerprints')")
            .fetch_all(pool)
            .await
            .expect("查询 l2_cluster_fingerprints 列应成功");
    assert_eq!(
        fingerprint_columns,
        vec!["persona_uid", "fingerprint"],
        "去重指纹表只保留主键两列"
    );
}

/// 复合索引齐备（过滤列 + 排序列成对，列表查询不再建临时 B-tree）。
#[tokio::test]
async fn schema_pair_indexes_present() {
    let storage = setup().await;
    let expected = [
        "idx_messages_persona_created",
        "idx_messages_session_created",
        "idx_memory_l1_persona_created",
        "idx_memory_l1_persona_occurred",
        "idx_memory_events_persona_created",
        "idx_utt_blocks_session_created",
        "idx_memory_vectors_model",
        "idx_messages_platform_message_id",
        "idx_personas_kind_source_ref",
    ];
    for name in expected {
        let found: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?")
                .bind(name)
                .fetch_optional(&storage.pool)
                .await
                .unwrap();
        assert_eq!(found.as_deref(), Some(name), "缺少索引 {name}");
    }
}

// =========================================================
// 启动期 schema 版本门禁
// =========================================================

/// 临时目录路径（用例结束清理）。
fn gate_temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ramaria-schema-gate-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    dir
}

/// 用最小 DDL 造一个旧版本库（模拟历史产物，不复制完整基线）。
async fn write_legacy_db(db_path: &std::path::Path, version: &str) {
    use sqlx::Connection;
    use sqlx::sqlite::SqliteConnectOptions;

    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true);
    let mut conn = sqlx::SqliteConnection::connect_with(&options)
        .await
        .expect("创建旧库失败");
    sqlx::query("CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&mut conn)
        .await
        .expect("建 schema_meta 失败");
    sqlx::query("INSERT INTO schema_meta (key, value) VALUES ('schema_version', ?)")
        .bind(version)
        .execute(&mut conn)
        .await
        .expect("写版本键失败");
    conn.close().await.expect("关闭旧库失败");
}

/// 旧版本库（v3）被拒绝：错误可读，原文件逐字节不变，不留下附属文件。
#[tokio::test]
async fn startup_gate_rejects_legacy_version_without_touching_file() {
    let dir = gate_temp_dir();
    let db_path = dir.join("ramaria.db");
    write_legacy_db(&db_path, "3").await;

    let before = std::fs::read(&db_path).expect("读取旧库失败");
    let err = database::init_pool(Some(db_path.clone()))
        .await
        .expect_err("旧版本库应被拒绝");
    let msg = err.to_string();
    assert!(msg.contains("实际版本 3"), "错误应含实际版本: {msg}");
    assert!(msg.contains("当前版本 4"), "错误应含当前版本: {msg}");
    assert!(msg.contains("备份后删除或改名"), "错误应含处置提示: {msg}");

    let after = std::fs::read(&db_path).expect("复读旧库失败");
    assert_eq!(before, after, "拒绝路径不得改写原库");

    let mut entries: Vec<String> = std::fs::read_dir(&dir)
        .expect("读取临时目录失败")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec!["ramaria.db".to_string()],
        "不得留下 -wal/-shm 附属文件"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 非空但缺少 schema_meta 的库被拒绝：按“无法识别”报错，原文件不变。
#[tokio::test]
async fn startup_gate_rejects_unrecognized_database() {
    let dir = gate_temp_dir();
    let db_path = dir.join("ramaria.db");
    write_legacy_db(&db_path, "4").await;
    // 抹掉版本表，模拟来源不明的 SQLite 文件
    {
        use sqlx::Connection;
        use sqlx::sqlite::SqliteConnectOptions;

        let options = SqliteConnectOptions::new().filename(&db_path);
        let mut conn = sqlx::SqliteConnection::connect_with(&options)
            .await
            .expect("打开库失败");
        sqlx::query("DROP TABLE schema_meta")
            .execute(&mut conn)
            .await
            .expect("删表失败");
        sqlx::query("CREATE TABLE messages (id TEXT PRIMARY KEY)")
            .execute(&mut conn)
            .await
            .expect("建用户表失败");
        conn.close().await.expect("关闭库失败");
    }

    let before = std::fs::read(&db_path).expect("读取失败");
    let err = database::init_pool(Some(db_path.clone()))
        .await
        .expect_err("无 schema_meta 的库应被拒绝");
    let msg = err.to_string();
    assert!(msg.contains("无法识别"), "错误应含无法识别: {msg}");
    assert!(msg.contains("备份后删除或改名"), "错误应含处置提示: {msg}");

    let after = std::fs::read(&db_path).expect("复读失败");
    assert_eq!(before, after, "拒绝路径不得改写原库");

    std::fs::remove_dir_all(&dir).ok();
}

/// 已建好的当前版本库可重复启动：门禁放行，版本键仍为当前版本。
#[tokio::test]
async fn startup_gate_allows_current_version_reopen() {
    let dir = gate_temp_dir();
    let db_path = dir.join("ramaria.db");

    let pool = database::init_pool(Some(db_path.clone()))
        .await
        .expect("首次建库失败");
    let first = crate::repo::schema_meta::get_schema_version(&pool)
        .await
        .expect("读取版本失败");
    assert_eq!(first, crate::repo::schema_meta::CURRENT_SCHEMA_VERSION);
    pool.close().await;

    let reopened = database::init_pool(Some(db_path.clone()))
        .await
        .expect("当前版本库应可再次启动");
    let second = crate::repo::schema_meta::get_schema_version(&reopened)
        .await
        .expect("读取版本失败");
    assert_eq!(second, crate::repo::schema_meta::CURRENT_SCHEMA_VERSION);
    reopened.close().await;

    std::fs::remove_dir_all(&dir).ok();
}

/// 零字节文件按新库处理：migration 建表并写入当前版本。
#[tokio::test]
async fn startup_gate_treats_empty_file_as_new_database() {
    let dir = gate_temp_dir();
    let db_path = dir.join("ramaria.db");
    std::fs::File::create(&db_path).expect("创建空文件失败");

    let pool = database::init_pool(Some(db_path.clone()))
        .await
        .expect("零字节文件应按新库建表");
    let version = crate::repo::schema_meta::get_schema_version(&pool)
        .await
        .expect("读取版本失败");
    assert_eq!(version, crate::repo::schema_meta::CURRENT_SCHEMA_VERSION);
    pool.close().await;

    std::fs::remove_dir_all(&dir).ok();
}
