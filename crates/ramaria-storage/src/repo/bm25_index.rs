//! rust/crates/ramaria-storage/src/repo/bm25_index.rs - BM25 全文索引存取模块
//!
//! 设计特点:
//! - 以 (doc_id, layer) 为复合主键，存储 tokens 的 JSON 序列化结果
//! - save 使用 INSERT OR REPLACE 实现增量更新幂等
//! - 不在此层执行分词或检索——仅负责原始 token 数据的持久化
//! - 上层 ramaria-memory 负责 jieba-rs 分词、BM25 评分和 RRF 融合

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

pub async fn save(
    pool: &SqlitePool,
    doc_id: i64,
    layer: &str,
    tokens_json: &str,
) -> RamariaResult<()> {
    sqlx::query("INSERT OR REPLACE INTO bm25_index (doc_id, layer, tokens_json) VALUES (?, ?, ?)")
        .bind(doc_id)
        .bind(layer)
        .bind(tokens_json)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("保存 BM25 索引失败", e))?;
    Ok(())
}

pub async fn list_by_doc(pool: &SqlitePool, doc_id: i64) -> RamariaResult<Vec<(String, String)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        layer: String,
        tokens_json: String,
    }
    let rows =
        sqlx::query_as::<_, Row>("SELECT layer, tokens_json FROM bm25_index WHERE doc_id = ?")
            .bind(doc_id)
            .fetch_all(pool)
            .await
            .map_err(|e| RamariaError::storage_with_source("查询 BM25 索引失败", e))?;
    Ok(rows.into_iter().map(|r| (r.layer, r.tokens_json)).collect())
}

pub async fn delete_by_doc(pool: &SqlitePool, doc_id: i64) -> RamariaResult<()> {
    sqlx::query("DELETE FROM bm25_index WHERE doc_id = ?")
        .bind(doc_id)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("删除 BM25 索引失败", e))?;
    Ok(())
}
