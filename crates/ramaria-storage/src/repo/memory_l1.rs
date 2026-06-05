//! rust/crates/ramaria-storage/src/repo/memory_l1.rs - L1 单次会话摘要 CRUD
//!
//! 设计特点:
//! - 按 session_id 查询 L1 列表
//! - 支持 absorbed 标记批量更新
//! - 支持未吸收 L1 查询（供 L2 merger 使用）
//! - 索引一致性字段（indexed_at / index_version）可读写

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::MemoryL1;
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

/// 保存 L1 记忆。
pub async fn save_memory_l1(pool: &SqlitePool, memory: &MemoryL1) -> RamariaResult<()> {
    sqlx::query(
        "INSERT INTO memory_l1 (id, session_id, summary, keywords, time_period, atmosphere, \
         valence, salience, absorbed, created_at, last_accessed_at, indexed_at, index_version) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(memory.id.to_string())
    .bind(memory.session_id.to_string())
    .bind(&memory.summary)
    .bind(&memory.keywords)
    .bind(&memory.time_period)
    .bind(&memory.atmosphere)
    .bind(memory.valence)
    .bind(memory.salience)
    .bind(memory.absorbed as i32)
    .bind(memory.created_at)
    .bind(memory.last_accessed_at)
    .bind(None::<i64>) // indexed_at: 初始未索引
    .bind(None::<i32>) // index_version: 初始未索引
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("保存 L1 记忆失败", e))?;
    Ok(())
}

/// 获取指定 session 的 L1 列表。
pub async fn list_memory_l1(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
    let rows = sqlx::query(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, \
         valence, salience, absorbed, created_at, last_accessed_at, indexed_at, index_version \
         FROM memory_l1 WHERE session_id = ? ORDER BY created_at ASC",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("列出 L1 记忆失败", e))?;

    rows.iter().map(row_to_memory_l1).collect()
}

/// 获取单个 L1 记忆。
pub async fn get_memory_l1(pool: &SqlitePool, id: Uuid) -> RamariaResult<Option<MemoryL1>> {
    let row = sqlx::query(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, \
         valence, salience, absorbed, created_at, last_accessed_at, indexed_at, index_version \
         FROM memory_l1 WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询 L1 记忆失败", e))?;

    match row {
        Some(r) => Ok(Some(row_to_memory_l1(&r)?)),
        None => Ok(None),
    }
}

/// 批量标记 L1 已被 L2 吸收。
pub async fn mark_l1_absorbed(pool: &SqlitePool, l1_ids: &[Uuid]) -> RamariaResult<()> {
    if l1_ids.is_empty() {
        return Ok(());
    }

    // SQLite 不支持数组参数，使用参数化 IN 子句
    let placeholders: Vec<String> = (0..l1_ids.len()).map(|i| format!("?{}", i + 1)).collect();
    let sql = format!(
        "UPDATE memory_l1 SET absorbed = 1 WHERE id IN ({})",
        placeholders.join(", ")
    );

    let mut query = sqlx::query(&sql);
    for id in l1_ids {
        query = query.bind(id.to_string());
    }

    query
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("批量标记 L1 吸收失败", e))?;
    Ok(())
}

/// 查询所有未吸收的 L1 记忆。
pub async fn list_unabsorbed_l1(pool: &SqlitePool) -> RamariaResult<Vec<MemoryL1>> {
    let rows = sqlx::query(
        "SELECT id, session_id, summary, keywords, time_period, atmosphere, \
         valence, salience, absorbed, created_at, last_accessed_at, indexed_at, index_version \
         FROM memory_l1 WHERE absorbed = 0 ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("列出未吸收 L1 失败", e))?;

    rows.iter().map(row_to_memory_l1).collect()
}

// =========================================================
// 行映射
// =========================================================

fn row_to_memory_l1(row: &sqlx::sqlite::SqliteRow) -> RamariaResult<MemoryL1> {
    let id_str: String = row.get("id");
    let sid_str: String = row.get("session_id");

    let id = Uuid::parse_str(&id_str)
        .map_err(|e| RamariaError::storage_with_source("L1 ID 格式非法", e))?;
    let session_id = Uuid::parse_str(&sid_str)
        .map_err(|e| RamariaError::storage_with_source("L1 session_id 格式非法", e))?;
    let absorbed_int: i32 = row.get("absorbed");

    Ok(MemoryL1 {
        id,
        session_id,
        summary: row.get("summary"),
        keywords: row.get("keywords"),
        time_period: row.get("time_period"),
        atmosphere: row.get("atmosphere"),
        valence: row.get("valence"),
        salience: row.get("salience"),
        absorbed: absorbed_int != 0,
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
    use crate::repo::sessions;

    #[tokio::test]
    async fn save_and_get_l1() {
        let pool = test_pool().await.unwrap();
        let session = ramaria_core::types::Session::new();
        sessions::create_session(&pool, &session).await.unwrap();

        let l1 = MemoryL1::new(session.id, "这是一段测试摘要".into(), Some("上午".into()));
        save_memory_l1(&pool, &l1).await.unwrap();

        let fetched = get_memory_l1(&pool, l1.id).await.unwrap();
        assert!(fetched.is_some());
        let m = fetched.unwrap();
        assert_eq!(m.summary, "这是一段测试摘要");
        assert_eq!(m.time_period.as_deref(), Some("上午"));
        assert!(!m.absorbed);
    }

    #[tokio::test]
    async fn mark_absorbed_updates_batch() {
        let pool = test_pool().await.unwrap();
        let session = ramaria_core::types::Session::new();
        sessions::create_session(&pool, &session).await.unwrap();

        let l1a = MemoryL1::new(session.id, "摘要 A".into(), None);
        let l1b = MemoryL1::new(session.id, "摘要 B".into(), None);
        let l1c = MemoryL1::new(session.id, "摘要 C".into(), None);

        save_memory_l1(&pool, &l1a).await.unwrap();
        save_memory_l1(&pool, &l1b).await.unwrap();
        save_memory_l1(&pool, &l1c).await.unwrap();

        // 标记前两个
        mark_l1_absorbed(&pool, &[l1a.id, l1b.id]).await.unwrap();

        // l1a, l1b 应已吸收
        assert!(
            get_memory_l1(&pool, l1a.id)
                .await
                .unwrap()
                .unwrap()
                .absorbed
        );
        assert!(
            get_memory_l1(&pool, l1b.id)
                .await
                .unwrap()
                .unwrap()
                .absorbed
        );

        // l1c 应未吸收
        assert!(
            !get_memory_l1(&pool, l1c.id)
                .await
                .unwrap()
                .unwrap()
                .absorbed
        );
    }

    #[tokio::test]
    async fn list_unabsorbed_returns_correct() {
        let pool = test_pool().await.unwrap();
        let session = ramaria_core::types::Session::new();
        sessions::create_session(&pool, &session).await.unwrap();

        let l1a = MemoryL1::new(session.id, "未吸收".into(), None);
        let l1b = MemoryL1::new(session.id, "也未吸收".into(), None);

        save_memory_l1(&pool, &l1a).await.unwrap();
        save_memory_l1(&pool, &l1b).await.unwrap();

        assert_eq!(list_unabsorbed_l1(&pool).await.unwrap().len(), 2);

        mark_l1_absorbed(&pool, &[l1a.id]).await.unwrap();
        assert_eq!(list_unabsorbed_l1(&pool).await.unwrap().len(), 1);
    }
}
