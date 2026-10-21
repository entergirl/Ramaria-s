//! crates/ramaria-storage/src/repo/memory_vectors.rs - 记忆向量存取模块
//!
//! 设计特点:
//! - 管理 memory_vectors 表：L1 摘要 / L2 事件 / utt 话语块的向量持久化
//! - 主键 (doc_type, doc_id)：单行单模型，换模型覆盖写
//! - 向量以 f32 小端 BLOB 存储；解码失败逐行告警并跳过，不整体失败
//! - 提供按文档 / 类型 / 模型删除与孤儿清理，避免多态 doc_id 产生悬空行

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{MemoryVector, decode_vector, encode_vector};
use sqlx::SqlitePool;

#[derive(sqlx::FromRow)]
struct VectorRow {
    doc_type: String,
    doc_id: String,
    model: String,
    dim: i64,
    vector: Vec<u8>,
    updated_at: i64,
}

impl VectorRow {
    fn into_vector(self) -> RamariaResult<MemoryVector> {
        let vector = decode_vector(&self.vector)?;
        Ok(MemoryVector {
            doc_type: self.doc_type,
            doc_id: self.doc_id,
            model: self.model,
            dim: self.dim.max(0) as u32,
            vector,
            updated_at: self.updated_at,
        })
    }
}

const VECTOR_COLUMNS: &str = "doc_type, doc_id, model, dim, vector, updated_at";

// =========================================================
// 写入
// =========================================================

/// 写入（覆盖）一条记忆向量。
///
/// 说明:
/// - 同 (doc_type, doc_id) 已存在时覆盖模型、维度、向量与更新时间。
pub async fn upsert(pool: &SqlitePool, vector: &MemoryVector) -> RamariaResult<()> {
    let bytes = encode_vector(&vector.vector);
    sqlx::query(
        "INSERT INTO memory_vectors (doc_type, doc_id, model, dim, vector, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(doc_type, doc_id) DO UPDATE SET
             model = excluded.model,
             dim = excluded.dim,
             vector = excluded.vector,
             updated_at = excluded.updated_at",
    )
    .bind(&vector.doc_type)
    .bind(&vector.doc_id)
    .bind(&vector.model)
    .bind(vector.dim as i64)
    .bind(bytes)
    .bind(vector.updated_at)
    .execute(pool)
    .await
    .storage_err("写入记忆向量失败")?;
    Ok(())
}

/// 批量写入（覆盖）记忆向量（单事务多值 upsert）。
///
/// 参数:
/// - `pool`: SQLite 连接池。
/// - `vectors`: 待写入向量；空切片直接返回，不开启事务。
///
/// 说明:
/// - 按 120 行 / 批切分（每行 6 个绑定参数，低于 SQLite 默认 999 上限）；
/// - 全部行在同一事务内提交：任一行失败整体回滚，不留下半批数据。
pub async fn upsert_many(pool: &SqlitePool, vectors: &[MemoryVector]) -> RamariaResult<()> {
    const ROWS_PER_STATEMENT: usize = 120;
    if vectors.is_empty() {
        return Ok(());
    }

    let mut tx = pool
        .begin()
        .await
        .storage_err("开启记忆向量批量写入事务失败")?;
    for chunk in vectors.chunks(ROWS_PER_STATEMENT) {
        let placeholders = std::iter::repeat_n("(?, ?, ?, ?, ?, ?)", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO memory_vectors (doc_type, doc_id, model, dim, vector, updated_at)
             VALUES {placeholders}
             ON CONFLICT(doc_type, doc_id) DO UPDATE SET
                 model = excluded.model,
                 dim = excluded.dim,
                 vector = excluded.vector,
                 updated_at = excluded.updated_at"
        );
        let mut query = sqlx::query(&sql);
        for vector in chunk {
            query = query
                .bind(&vector.doc_type)
                .bind(&vector.doc_id)
                .bind(&vector.model)
                .bind(vector.dim as i64)
                .bind(encode_vector(&vector.vector))
                .bind(vector.updated_at);
        }
        query
            .execute(&mut *tx)
            .await
            .storage_err("批量写入记忆向量失败")?;
    }
    tx.commit().await.storage_err("提交记忆向量批量写入失败")?;
    Ok(())
}

// =========================================================
// 读取
// =========================================================

/// 按模型指纹加载全部记忆向量。
///
/// 返回:
/// - 指纹匹配的行；单行解码失败时记 warn 并跳过该行。
pub async fn load_by_model(pool: &SqlitePool, model: &str) -> RamariaResult<Vec<MemoryVector>> {
    let rows = sqlx::query_as::<_, VectorRow>(&format!(
        "SELECT {VECTOR_COLUMNS} FROM memory_vectors WHERE model = ?"
    ))
    .bind(model)
    .fetch_all(pool)
    .await
    .storage_err("加载记忆向量失败")?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let doc_type = row.doc_type.clone();
        let doc_id = row.doc_id.clone();
        match row.into_vector() {
            Ok(v) => out.push(v),
            Err(e) => {
                tracing::warn!(
                    doc_type = %doc_type,
                    doc_id = %doc_id,
                    error = %e,
                    "记忆向量解码失败，已跳过该行"
                );
            }
        }
    }
    Ok(out)
}

/// 统计记忆向量总行数。
pub async fn count(pool: &SqlitePool) -> RamariaResult<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM memory_vectors")
        .fetch_one(pool)
        .await
        .storage_err("统计记忆向量失败")
}

// =========================================================
// 删除
// =========================================================

/// 删除单条记忆向量（按文档类型与文档标识）。
pub async fn delete_by_doc(pool: &SqlitePool, doc_type: &str, doc_id: &str) -> RamariaResult<()> {
    sqlx::query("DELETE FROM memory_vectors WHERE doc_type = ? AND doc_id = ?")
        .bind(doc_type)
        .bind(doc_id)
        .execute(pool)
        .await
        .storage_err("删除记忆向量失败")?;
    Ok(())
}

/// 删除某类文档的全部记忆向量。
pub async fn delete_by_type(pool: &SqlitePool, doc_type: &str) -> RamariaResult<()> {
    sqlx::query("DELETE FROM memory_vectors WHERE doc_type = ?")
        .bind(doc_type)
        .execute(pool)
        .await
        .storage_err("按类型删除记忆向量失败")?;
    Ok(())
}

/// 删除指定模型指纹的全部记忆向量。
pub async fn delete_by_model(pool: &SqlitePool, model: &str) -> RamariaResult<()> {
    sqlx::query("DELETE FROM memory_vectors WHERE model = ?")
        .bind(model)
        .execute(pool)
        .await
        .storage_err("按模型删除记忆向量失败")?;
    Ok(())
}

/// 清理孤儿向量：向量行对应的业务行已不存在。
///
/// 说明:
/// - 按 doc_type 分别比对业务主表（l1 → memory_l1.id，l2 → memory_events.id，
///   utt → utt_blocks.id）；doc_id 以 CAST 比较，非数字标识不会误删。
/// - 未知 doc_type 不做处理（保留待排查）。
///
/// 返回:
/// - 删除的行数。
pub async fn prune_orphans(pool: &SqlitePool) -> RamariaResult<u64> {
    let mut deleted = 0u64;
    let targets = [
        (
            "l1",
            "DELETE FROM memory_vectors WHERE doc_type = 'l1' AND doc_id NOT IN (SELECT id FROM memory_l1)",
        ),
        (
            "l2",
            "DELETE FROM memory_vectors WHERE doc_type = 'l2' AND CAST(doc_id AS INTEGER) NOT IN (SELECT id FROM memory_events)",
        ),
        (
            "utt",
            "DELETE FROM memory_vectors WHERE doc_type = 'utt' AND CAST(doc_id AS INTEGER) NOT IN (SELECT id FROM utt_blocks)",
        ),
    ];
    for (doc_type, sql) in targets {
        let result = sqlx::query(sql)
            .execute(pool)
            .await
            .storage_err(format!("清理 {doc_type} 孤儿记忆向量失败"))?;
        deleted += result.rows_affected();
    }
    if deleted > 0 {
        tracing::info!(deleted, "已清理孤儿记忆向量");
    }
    Ok(deleted)
}

// =========================================================
// 单元测试（内存库）
// =========================================================

#[cfg(test)]
mod tests;
