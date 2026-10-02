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
