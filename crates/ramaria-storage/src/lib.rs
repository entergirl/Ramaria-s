//! 存储层：SQLite 数据库、索引存取、检索索引。
//! 不写业务聚合逻辑。

/// Phase 0 POC：验证 sqlx migrate + CRUD 可用
#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    /// 在内存 SQLite 中跑 migration，验证流程通顺
    #[tokio::test]
    async fn poc_migration_and_crud() {
        // 1. 连接内存数据库
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("连接 SQLite 失败");

        // 2. 执行 migration
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migration 执行失败");

        // 3. 插入数据（UUID v4 + Unix 毫秒时间戳）
        let id = uuid::Uuid::new_v4().to_string();
        let now_ms = chrono::Utc::now().timestamp_millis();

        sqlx::query("INSERT INTO poc_test (id, name, created_at) VALUES (?, ?, ?)")
            .bind(&id)
            .bind("POC 验证记录")
            .bind(now_ms)
            .execute(&pool)
            .await
            .expect("插入失败");

        // 4. 查询回来
        let row: (String, String, i64) =
            sqlx::query_as("SELECT id, name, created_at FROM poc_test WHERE id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .expect("查询失败");

        // 5. 断言数据一致
        assert_eq!(row.0, id, "ID 不一致");
        assert_eq!(row.1, "POC 验证记录");
        assert_eq!(row.2, now_ms);

        // 6. 删除数据
        let deleted = sqlx::query("DELETE FROM poc_test WHERE id = ?")
            .bind(&id)
            .execute(&pool)
            .await
            .expect("删除失败");
        assert_eq!(deleted.rows_affected(), 1);

        // 7. 确认已删除
        let count: (i32,) = sqlx::query_as("SELECT COUNT(*) as cnt FROM poc_test")
            .fetch_one(&pool)
            .await
            .expect("查询 count 失败");
        assert_eq!(count.0, 0, "删除后应无记录");

        println!("✅ SQLite POC 通过：migration + INSERT + SELECT + DELETE 全部正常");
    }
}
