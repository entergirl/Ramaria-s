//! crates/ramaria-storage/src/tests/schema.rs - schema 与迁移完整性测试
//!
//! 设计特点:
//! - 覆盖 schema 版本读取与基线终值（表 / 列 / 显式索引 / 外键计数）
//! - 覆盖关键表的列形态与主键（向量表、utt 块、后台任务、L2 去重指纹）
//! - 覆盖迁移记录数与迁移目录文件数一致
//! - 覆盖列表查询所需复合索引的齐备性

use super::*;

#[tokio::test]
async fn schema_version() {
    let storage = setup().await;
    let v = storage.get_schema_version().await.unwrap();
    assert_eq!(v, 3, "schema 版本应为 3");
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
    assert_eq!(columns, 259, "列数应为 259");
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
