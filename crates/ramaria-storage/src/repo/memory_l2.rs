//! rust/crates/ramaria-storage/src/repo/memory_l2.rs - L2 时间段聚合摘要及溯源 CRUD
//!
//! 设计特点:
//! - L2 记忆与 L2→L1 溯源关系分表存储
//! - save_l2_sources 批量插入溯源关系
//! - get_l2_sources 返回来源 L1 ID 列表

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::MemoryL2;
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

/// 保存 L2 记忆。
pub async fn save_memory_l2(pool: &SqlitePool, memory: &MemoryL2) -> RamariaResult<()> {
    sqlx::query(
        "INSERT INTO memory_l2 (id, summary, keywords, period_start, period_end, \
         created_at, last_accessed_at, indexed_at, index_version) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(memory.id.to_string())
    .bind(&memory.summary)
    .bind(&memory.keywords)
    .bind(memory.period_start)
    .bind(memory.period_end)
    .bind(memory.created_at)
    .bind(memory.last_accessed_at)
    .bind(None::<i64>)
    .bind(None::<i32>)
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("保存 L2 记忆失败", e))?;
    Ok(())
}

/// 保存 L2 → L1 溯源关系（批量）。
pub async fn save_l2_sources(pool: &SqlitePool, l2_id: Uuid, l1_ids: &[Uuid]) -> RamariaResult<()> {
    if l1_ids.is_empty() {
        return Ok(());
    }

    for l1_id in l1_ids {
        sqlx::query("INSERT OR IGNORE INTO l2_sources (l2_id, l1_id) VALUES (?, ?)")
            .bind(l2_id.to_string())
            .bind(l1_id.to_string())
            .execute(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("保存 L2 溯源失败", e))?;
    }
    Ok(())
}

/// 列出所有 L2 记忆。
pub async fn list_memory_l2(pool: &SqlitePool) -> RamariaResult<Vec<MemoryL2>> {
    let rows = sqlx::query(
        "SELECT id, summary, keywords, period_start, period_end, \
         created_at, last_accessed_at, indexed_at, index_version \
         FROM memory_l2 ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("列出 L2 记忆失败", e))?;

    rows.iter().map(row_to_memory_l2).collect()
}

/// 获取 L2 的来源 L1 ID 列表。
pub async fn get_l2_sources(pool: &SqlitePool, l2_id: Uuid) -> RamariaResult<Vec<Uuid>> {
    let rows = sqlx::query("SELECT l1_id FROM l2_sources WHERE l2_id = ?")
        .bind(l2_id.to_string())
        .fetch_all(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("查询 L2 溯源失败", e))?;

    rows.iter()
        .map(|r| {
            let s: String = r.get("l1_id");
            Uuid::parse_str(&s)
                .map_err(|e| RamariaError::storage_with_source("L2 溯源 L1 ID 格式非法", e))
        })
        .collect()
}

// =========================================================
// 行映射
// =========================================================

fn row_to_memory_l2(row: &sqlx::sqlite::SqliteRow) -> RamariaResult<MemoryL2> {
    let id_str: String = row.get("id");
    let id = Uuid::parse_str(&id_str)
        .map_err(|e| RamariaError::storage_with_source("L2 ID 格式非法", e))?;

    Ok(MemoryL2 {
        id,
        summary: row.get("summary"),
        keywords: row.get("keywords"),
        period_start: row.get("period_start"),
        period_end: row.get("period_end"),
        created_at: row.get("created_at"),
        last_accessed_at: row.get("last_accessed_at"),
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::test_pool;
    use crate::repo::{memory_l1, sessions};

    #[tokio::test]
    async fn save_and_list_l2() {
        let pool = test_pool().await.unwrap();

        let now = ramaria_core::types::now_ms();
        let l2 = MemoryL2::new("L2 聚合摘要".into(), now - 86_400_000, now);
        save_memory_l2(&pool, &l2).await.unwrap();

        let list = list_memory_l2(&pool).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].summary, "L2 聚合摘要");
    }

    #[tokio::test]
    async fn l2_sources_traceability() {
        let pool = test_pool().await.unwrap();
        let session = ramaria_core::types::Session::new();
        sessions::create_session(&pool, &session).await.unwrap();

        // 创建 L1 记忆
        let l1a = ramaria_core::types::MemoryL1::new(session.id, "L1-A".into(), None);
        let l1b = ramaria_core::types::MemoryL1::new(session.id, "L1-B".into(), None);
        memory_l1::save_memory_l1(&pool, &l1a).await.unwrap();
        memory_l1::save_memory_l1(&pool, &l1b).await.unwrap();

        // 创建 L2 + 溯源
        let now = ramaria_core::types::now_ms();
        let l2 = MemoryL2::new("聚合 A+B".into(), now - 1000, now);
        save_memory_l2(&pool, &l2).await.unwrap();
        save_l2_sources(&pool, l2.id, &[l1a.id, l1b.id])
            .await
            .unwrap();

        // 验证溯源
        let sources = get_l2_sources(&pool, l2.id).await.unwrap();
        assert_eq!(sources.len(), 2);
        assert!(sources.contains(&l1a.id));
        assert!(sources.contains(&l1b.id));
    }

    #[tokio::test]
    async fn save_l2_sources_empty_is_noop() {
        let pool = test_pool().await.unwrap();
        let l2_id = Uuid::new_v4();
        save_l2_sources(&pool, l2_id, &[]).await.unwrap();
        // 不报错即可
    }
}
