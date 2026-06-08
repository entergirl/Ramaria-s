//! rust/crates/ramaria-storage/src/repo/cluster.rs - ClusterSnapshot CRUD

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::ClusterSnapshot;
use sqlx::SqlitePool;

#[derive(sqlx::FromRow)]
struct ClusterRow {
    id: i64,
    persona_uid: String,
    category: String,
    cluster_label: String,
    samples: Option<String>,
    count: i64,
    is_current: i64,
    created_at: i64,
}

impl ClusterRow {
    fn into_snapshot(self) -> ClusterSnapshot {
        ClusterSnapshot {
            id: self.id,
            persona_uid: self.persona_uid,
            category: self.category,
            cluster_label: self.cluster_label,
            samples: self.samples,
            count: self.count as i32,
            is_current: self.is_current != 0,
            created_at: self.created_at,
        }
    }
}

pub async fn save(pool: &SqlitePool, s: &ClusterSnapshot) -> RamariaResult<i64> {
    sqlx::query(
        "INSERT INTO persona_cluster_snapshots (persona_uid, category, cluster_label, samples, count, is_current, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&s.persona_uid).bind(&s.category).bind(&s.cluster_label)
    .bind(&s.samples).bind(s.count).bind(s.is_current as i64).bind(s.created_at)
    .execute(pool).await
    .map_err(|e| RamariaError::storage_with_source("保存聚类快照失败", e))?;

    let id = sqlx::query_scalar::<_, i64>("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("获取快照 id 失败", e))?;
    Ok(id)
}

pub async fn get_current(
    pool: &SqlitePool,
    persona_uid: &str,
    category: &str,
) -> RamariaResult<Vec<ClusterSnapshot>> {
    let rows = sqlx::query_as::<_, ClusterRow>(
        "SELECT id, persona_uid, category, cluster_label, samples, count, is_current, created_at
         FROM persona_cluster_snapshots WHERE persona_uid = ? AND category = ? AND is_current = 1
         ORDER BY count DESC",
    )
    .bind(persona_uid)
    .bind(category)
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询聚类快照失败", e))?;
    Ok(rows.into_iter().map(|r| r.into_snapshot()).collect())
}
