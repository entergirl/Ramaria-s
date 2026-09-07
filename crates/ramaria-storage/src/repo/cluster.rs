//! crates/ramaria-storage/src/repo/cluster.rs - ClusterSnapshot CRUD
//!
//! 设计特点:
//! - 管理态度聚类快照，支撑跨版本簇匹配（语义标签→embedding 相似度）
//! - save 语义 = “归档 + 写入”：先把该 (persona_uid, category) 的既有 current 快照
//!   置为非 current，再插入新快照 is_current=1（单期语义，事务包裹）
//! - get_current 返回该 (persona_uid, category) 的最新一期快照（至多 1 条）
//! - 新增 semantic_label / semantic_label_embedding 读写
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
    /// 语义标签文本（可为 NULL，兼容旧记录）
    semantic_label: Option<String>,
    /// 语义标签 embedding BLOB（可为 NULL）
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

/// 保存聚类快照（含语义标签和 embedding），采用“归档 + 写入”单期语义。
///
/// 流程（写入 `is_current=true` 快照时）:
/// 1. 先把该 (persona_uid, category) 的既有 current 快照置为非 current（归档）；
/// 2. 再插入新快照 is_current=1。
///
/// 两步在同一事务内完成；任一步失败整体回滚并返回 Storage 错误，不留下半写入状态。
///
/// 参数:
/// - `s`: 快照数据。`semantic_label` 和 `semantic_label_embedding` 为 `None` 时写入 NULL。
///   当 `s.is_current=false` 时不触发归档，仅插入非 current 行。
///
/// 返回:
/// - 新插入行的自增 id。
pub async fn save(pool: &SqlitePool, s: &ClusterSnapshot) -> RamariaResult<i64> {
    let mut tx = pool.begin().await.storage_err("开启聚类快照归档事务失败")?;

    // 归档既有 current 快照：同 persona + category 只保留最新一期为 current。
    if s.is_current {
        sqlx::query(
            "UPDATE persona_cluster_snapshots SET is_current = 0
             WHERE persona_uid = ? AND category = ? AND is_current = 1",
        )
        .bind(&s.persona_uid)
        .bind(&s.category)
        .execute(&mut *tx)
        .await
        .storage_err("归档聚类快照失败")?;
    }

    let id = sqlx::query_scalar::<_, i64>(
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
    .fetch_one(&mut *tx)
    .await
    .storage_err("保存聚类快照失败")?;

    tx.commit().await.storage_err("提交聚类快照事务失败")?;
    Ok(id)
}

/// 查询指定 persona 和 category 的最新一期（current）快照。
///
/// 单期语义: save 每次写入会归档旧 current 快照，因此同一 (persona_uid, category)
/// 至多一条 current 快照。返回按 `created_at DESC, id DESC` 取最新一条，
/// 对历史遗留的多 current 脏数据同样收敛到最新一期，避免均值重叠。
///
/// 返回:
/// - 含该快照的 Vec（长度 0 或 1，包含 semantic_label 和 embedding）。
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
         ORDER BY created_at DESC, id DESC
         LIMIT 1",
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
    use super::*;
    use crate::database;
    use ramaria_core::types::ClusterSnapshot;
    use sqlx::SqlitePool;

    /// 插入 persona 行，满足 persona_cluster_snapshots 的外键约束。
    async fn insert_persona(pool: &SqlitePool, uid: &str) {
        sqlx::query(
            "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
             VALUES (?, '测试', 'char', 1, 'local', 0, 0)",
        )
        .bind(uid)
        .execute(pool)
        .await
        .expect("插入 persona fixture 应成功");
    }

    /// 构造一期刊聚合快照，samples 携带 valence_mean 便于断言哪一期被取回。
    fn make_snapshot(
        uid: &str,
        category: &str,
        created_at: i64,
        count: i32,
        valence: f64,
    ) -> ClusterSnapshot {
        let samples = serde_json::json!({
            "category": category,
            "n_effective": count,
            "valence_mean": valence,
        })
        .to_string();
        ClusterSnapshot {
            id: 0,
            persona_uid: uid.to_string(),
            category: category.to_string(),
            cluster_label: format!("cluster_{category}"),
            samples: Some(samples),
            count,
            is_current: true,
            created_at,
            semantic_label: None,
            semantic_label_embedding: None,
        }
    }

    /// save 归档语义：同 (persona, category) 二期先后写入后，旧期 is_current 置 0，
    /// get_current 只返回最新一期（单条）。
    #[tokio::test]
    async fn save_archives_previous_current_snapshot() {
        let pool = database::init_test_pool().await.expect("测试库初始化失败");
        insert_persona(&pool, "char-snap").await;

        let first = make_snapshot("char-snap", "工作", 1_000, 50, 0.8);
        let first_id = save(&pool, &first).await.expect("第一期写入应成功");
        let second = make_snapshot("char-snap", "工作", 2_000, 10, 0.1);
        save(&pool, &second).await.expect("第二期写入应成功");

        let current = get_current(&pool, "char-snap", "工作")
            .await
            .expect("查询应成功");
        assert_eq!(current.len(), 1, "get_current 应只返回最新一期");
        assert_eq!(current[0].count, 10, "应取回第二期（created_at 更新）");
        let samples = current[0]
            .samples
            .as_deref()
            .expect("第二期 samples 应存在");
        assert!(samples.contains("\"valence_mean\":0.1"));

        // 第一期虽保留为历史行，但 is_current 已被置 0。
        let old_flag: i64 =
            sqlx::query_scalar("SELECT is_current FROM persona_cluster_snapshots WHERE id = ?")
                .bind(first_id)
                .fetch_one(&pool)
                .await
                .expect("查询旧快照标记应成功");
        assert_eq!(old_flag, 0, "旧一期快照应被归档为非 current");
    }

    /// 归档仅作用于同 (persona, category)：其他 persona 或同 persona 其他分类不受影响。
    #[tokio::test]
    async fn save_archive_is_scoped_by_persona_and_category() {
        let pool = database::init_test_pool().await.expect("测试库初始化失败");
        insert_persona(&pool, "char-a").await;
        insert_persona(&pool, "char-b").await;

        save(&pool, &make_snapshot("char-a", "工作", 1_000, 10, 0.8))
            .await
            .unwrap();
        save(&pool, &make_snapshot("char-a", "社交", 1_000, 10, 0.5))
            .await
            .unwrap();
        save(&pool, &make_snapshot("char-b", "工作", 1_000, 10, 0.5))
            .await
            .unwrap();
        // 归档 char-a/工作 的旧期
        save(&pool, &make_snapshot("char-a", "工作", 2_000, 20, 0.2))
            .await
            .unwrap();

        let a_work = get_current(&pool, "char-a", "工作").await.unwrap();
        assert_eq!(a_work.len(), 1);
        assert_eq!(a_work[0].count, 20, "char-a/工作 应更新为第二期");
        let a_social = get_current(&pool, "char-a", "社交").await.unwrap();
        assert_eq!(a_social.len(), 1, "char-a/社交 不应被归档");
        let b_work = get_current(&pool, "char-b", "工作").await.unwrap();
        assert_eq!(b_work.len(), 1, "char-b/工作 不应被归档");
        assert_eq!(b_work[0].count, 10);
    }

    /// 非 current 快照写入不触发归档，也不作为 get_current 结果返回。
    #[tokio::test]
    async fn save_non_current_does_not_archive() {
        let pool = database::init_test_pool().await.expect("测试库初始化失败");
        insert_persona(&pool, "char-snap").await;

        save(&pool, &make_snapshot("char-snap", "家庭", 1_000, 10, 0.5))
            .await
            .unwrap();
        let mut archival = make_snapshot("char-snap", "家庭", 2_000, 99, 0.9);
        archival.is_current = false;
        save(&pool, &archival).await.expect("非 current 写入应成功");

        let current = get_current(&pool, "char-snap", "家庭").await.unwrap();
        assert_eq!(current.len(), 1, "非 current 行不应进入 current 结果");
        assert_eq!(current[0].count, 10, "原 current 快照应保持不变");
    }

    /// 历史遗留多 current 脏数据：get_current 收敛到最新一期（created_at DESC）。
    #[tokio::test]
    async fn get_current_converges_legacy_multiple_current_rows() {
        let pool = database::init_test_pool().await.expect("测试库初始化失败");
        insert_persona(&pool, "char-snap").await;

        // 直接写两行 is_current=1（模拟旧版 save 未归档的累积脏数据）
        for (created_at, count) in [(1_000i64, 50i32), (2_000i64, 5i32)] {
            let snap = make_snapshot("char-snap", "健康", created_at, count, 0.3);
            sqlx::query(
                "INSERT INTO persona_cluster_snapshots (
                    persona_uid, category, cluster_label, samples, count, is_current, created_at
                 ) VALUES (?, ?, ?, ?, ?, 1, ?)",
            )
            .bind(&snap.persona_uid)
            .bind(&snap.category)
            .bind(&snap.cluster_label)
            .bind(&snap.samples)
            .bind(snap.count)
            .bind(snap.created_at)
            .execute(&pool)
            .await
            .expect("直接插入脏数据应成功");
        }

        let current = get_current(&pool, "char-snap", "健康").await.unwrap();
        assert_eq!(current.len(), 1, "脏数据场景也应收敛到单条");
        assert_eq!(
            current[0].count, 5,
            "应按 created_at DESC 取最新一期而非 count 最大"
        );
    }

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

    /// deserialize_embedding 无效 blob 参数化验证。
    #[test]
    fn snapshot_deserialize_invalid_blobs() {
        // 空 blob → None
        assert!(ClusterSnapshot::deserialize_embedding(&[]).is_none());
        // 3 字节不能被 4 整除 → None
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
