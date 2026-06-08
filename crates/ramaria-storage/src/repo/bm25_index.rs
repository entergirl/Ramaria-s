//! rust/crates/ramaria-storage/src/repo/bm25_index.rs - BM25 索引 CRUD

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
