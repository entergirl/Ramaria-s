//! rust/crates/ramaria-storage/src/repo/cluster.rs - ClusterSnapshot CRUD
//!
//! 设计特点:
//! - 管理态度聚类快照，支撑跨版本簇匹配（语义标签→embedding 相似度）
//! - get_current 按 (persona_uid, category) 查询最新版本快照
//! - v1.3 新增 semantic_label / semantic_label_embedding 读写
//! - 使用 sqlx::query_as 自动映射 ClusterRow → ClusterSnapshot

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::ClusterSnapshot;
use sqlx::SqlitePool;

/// 数据库行映射结构。
///
/// semantic_label_embedding 从 BLOB 列读取为 `Vec<u8>`，
/// 通过 `into_snapshot()` 转换为 `ClusterSnapshot`。
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
    /// v1.3: 语义标签文本（可为 NULL，兼容旧记录）
    semantic_label: Option<String>,
    /// v1.3: 语义标签 embedding BLOB（可为 NULL）
    semantic_label_embedding: Option<Vec<u8>>,
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
            semantic_label: self.semantic_label,
            semantic_label_embedding: self.semantic_label_embedding,
        }
    }
}

/// 保存聚类快照（含 v1.3 语义标签和 embedding）。
///
/// 参数:
/// - `s`: 快照数据。`semantic_label` 和 `semantic_label_embedding` 为 `None` 时写入 NULL。
///
/// 返回:
/// - 新插入行的自增 id。
pub async fn save(pool: &SqlitePool, s: &ClusterSnapshot) -> RamariaResult<i64> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO persona_cluster_snapshots (
            persona_uid, category, cluster_label, samples, count, is_current, created_at,
            semantic_label, semantic_label_embedding
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&s.persona_uid)
    .bind(&s.category)
    .bind(&s.cluster_label)
    .bind(&s.samples)
    .bind(s.count)
    .bind(s.is_current as i64)
    .bind(s.created_at)
    .bind(&s.semantic_label)
    .bind(&s.semantic_label_embedding)
    .fetch_one(pool)
    .await
    .storage_err("保存聚类快照失败")
}

/// 查询指定 persona 和 category 的最新版本快照。
///
/// 返回:
/// - 按 `count DESC` 排序的快照列表（包含 semantic_label 和 embedding）。
pub async fn get_current(
    pool: &SqlitePool,
    persona_uid: &str,
    category: &str,
) -> RamariaResult<Vec<ClusterSnapshot>> {
    let rows = sqlx::query_as::<_, ClusterRow>(
        "SELECT id, persona_uid, category, cluster_label, samples,
                count, is_current, created_at, semantic_label, semantic_label_embedding
         FROM persona_cluster_snapshots
         WHERE persona_uid = ? AND category = ? AND is_current = 1
         ORDER BY count DESC",
    )
    .bind(persona_uid)
    .bind(category)
    .fetch_all(pool)
    .await
    .storage_err("查询聚类快照失败")?;
    Ok(rows.into_iter().map(|r| r.into_snapshot()).collect())
}

/// 查询该 persona 的所有历史快照（含非 current），用于跨版本匹配。
///
/// 返回:
/// - 所有快照按 `created_at DESC` 排序。
/// - 仅返回 `semantic_label_embedding` 不为 NULL 的条目。
pub async fn get_all_with_embeddings(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<ClusterSnapshot>> {
    let rows = sqlx::query_as::<_, ClusterRow>(
        "SELECT id, persona_uid, category, cluster_label, samples,
                count, is_current, created_at, semantic_label, semantic_label_embedding
         FROM persona_cluster_snapshots
         WHERE persona_uid = ? AND semantic_label_embedding IS NOT NULL
         ORDER BY created_at DESC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询历史聚类快照失败")?;
    Ok(rows.into_iter().map(|r| r.into_snapshot()).collect())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use ramaria_core::types::ClusterSnapshot;

    #[test]
    fn snapshot_serialize_deserialize_roundtrip() {
        let embedding = vec![0.1_f32, -0.5, 0.75, 0.0];
        let blob = ClusterSnapshot::serialize_embedding(&embedding);
        assert_eq!(blob.len(), 16); // 4 × 4 bytes

        let recovered = ClusterSnapshot::deserialize_embedding(&blob);
        assert!(recovered.is_some());
        let recovered = recovered.unwrap();
        assert_eq!(recovered.len(), 4);
        for (i, (&a, &b)) in embedding.iter().zip(recovered.iter()).enumerate() {
            assert!((a - b).abs() < 1e-6, "索引 {} 处不匹配: {} vs {}", i, a, b);
        }
    }

    #[test]
    fn snapshot_deserialize_empty_blob() {
        assert!(ClusterSnapshot::deserialize_embedding(&[]).is_none());
    }

    #[test]
    fn snapshot_deserialize_malformed_blob() {
        // 3 字节不能被 4 整除
        assert!(ClusterSnapshot::deserialize_embedding(&[0, 1, 2]).is_none());
    }

    #[test]
    fn snapshot_new_has_v13_fields_none() {
        let snap = ClusterSnapshot::new("uid".into(), "工作".into(), "簇A".into());
        assert_eq!(snap.persona_uid, "uid");
        assert!(snap.semantic_label.is_none());
        assert!(snap.semantic_label_embedding.is_none());
        assert!(snap.is_current);
    }
}
