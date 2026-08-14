//! rust/crates/ramaria-storage/src/repo/l2_fingerprint.rs - L2 聚类去重指纹存取模块
//!
//! 设计特点:
//! - 管理 `l2_cluster_fingerprints` 表（v1.5 新增），记录"已聚类且无产出"的 L1 集合指纹
//! - 指纹为 SHA-256 hex，按 persona_uid 严格隔离
//! - 主键 (persona_uid, fingerprint)，同一集合只记录一次
//! - 供 `ramaria-memory` 事件提取器在聚类前查重、无产出后登记

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use sqlx::SqlitePool;

/// 判断指定 persona 是否已记录过该 L1 集合指纹。
///
/// 返回:
/// - `Ok(true)`: 已记录，调用方应跳过本次聚类（同集合不重复聚类）。
/// - `Ok(false)`: 未记录，正常聚类。
pub async fn exists(
    pool: &SqlitePool,
    persona_uid: &str,
    fingerprint: &str,
) -> RamariaResult<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM l2_cluster_fingerprints \
         WHERE persona_uid = ? AND fingerprint = ?",
    )
    .bind(persona_uid)
    .bind(fingerprint)
    .fetch_one(pool)
    .await
    .storage_err("查询 L2 聚类指纹失败")?;
    Ok(n > 0)
}

/// 记录一次"已聚类且无产出"的 L1 集合指纹。
///
/// 说明:
/// - 同 (persona_uid, fingerprint) 重复记录时忽略（幂等）。
pub async fn insert(
    pool: &SqlitePool,
    persona_uid: &str,
    fingerprint: &str,
    now_ms: i64,
) -> RamariaResult<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO l2_cluster_fingerprints (persona_uid, fingerprint, created_at) \
         VALUES (?, ?, ?)",
    )
    .bind(persona_uid)
    .bind(fingerprint)
    .bind(now_ms)
    .execute(pool)
    .await
    .storage_err("记录 L2 聚类指纹失败")
    .map(|_| ())
}

/// 查询指定 persona 已记录的全部指纹。
///
/// 用途:
/// - 诊断与审计（`ramaria diagnostics` 类命令可复用）。
pub async fn list_by_persona(pool: &SqlitePool, persona_uid: &str) -> RamariaResult<Vec<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT fingerprint FROM l2_cluster_fingerprints \
         WHERE persona_uid = ? ORDER BY created_at DESC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询 L2 聚类指纹列表失败")?;
    Ok(rows)
}

// =========================================================
// 单元测试（内存库）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::init_test_pool;

    #[tokio::test]
    async fn insert_then_exists_returns_true() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        insert(&pool, "p1", "fp-a", 1_000).await.expect("记录成功");
        assert!(exists(&pool, "p1", "fp-a").await.expect("查询成功"));
    }

    #[tokio::test]
    async fn exists_miss_returns_false() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        assert!(!exists(&pool, "p1", "fp-a").await.expect("查询成功"));
    }

    #[tokio::test]
    async fn fingerprint_is_isolated_by_persona() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        insert(&pool, "p1", "fp-a", 1_000).await.expect("记录成功");
        assert!(exists(&pool, "p1", "fp-a").await.expect("查询成功"));
        assert!(
            !exists(&pool, "p2", "fp-a")
                .await
                .expect("跨 persona 不应命中")
        );
    }

    #[tokio::test]
    async fn duplicate_insert_is_idempotent() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        insert(&pool, "p1", "fp-a", 1_000)
            .await
            .expect("首次记录成功");
        insert(&pool, "p1", "fp-a", 2_000)
            .await
            .expect("重复记录应幂等");
        let list = list_by_persona(&pool, "p1").await.expect("列表查询成功");
        assert_eq!(list.len(), 1, "同指纹只记录一次");
    }

    #[tokio::test]
    async fn list_by_persona_orders_by_created_desc() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        insert(&pool, "p1", "fp-old", 1_000)
            .await
            .expect("记录成功");
        insert(&pool, "p1", "fp-new", 2_000)
            .await
            .expect("记录成功");
        let list = list_by_persona(&pool, "p1").await.expect("列表查询成功");
        assert_eq!(list, vec!["fp-new".to_string(), "fp-old".to_string()]);
    }
}
