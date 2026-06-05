//! rust/crates/ramaria-storage/src/repo/bm25_index.rs - BM25 索引持久化
//!
//! 设计特点:
//! - 按 (doc_id, layer) 复合主键存储分词结果
//! - tokens_json 以 JSON 数组存储
//! - 支持按 layer 查询

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::Row;
use sqlx::SqlitePool;

/// BM25 索引条目。
#[derive(Debug, Clone)]
pub struct Bm25Entry {
    pub doc_id: String,
    pub layer: String,
    pub tokens_json: String,
}

/// 保存 BM25 索引条目。
pub async fn save_bm25_entry(pool: &SqlitePool, entry: &Bm25Entry) -> RamariaResult<()> {
    sqlx::query("INSERT OR REPLACE INTO bm25_index (doc_id, layer, tokens_json) VALUES (?, ?, ?)")
        .bind(&entry.doc_id)
        .bind(&entry.layer)
        .bind(&entry.tokens_json)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("保存 BM25 条目失败", e))?;
    Ok(())
}

/// 按 layer 查询所有 BM25 条目。
pub async fn list_bm25_entries(pool: &SqlitePool, layer: &str) -> RamariaResult<Vec<Bm25Entry>> {
    let rows = sqlx::query("SELECT doc_id, layer, tokens_json FROM bm25_index WHERE layer = ?")
        .bind(layer)
        .fetch_all(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("列出 BM25 条目失败", e))?;

    let entries = rows
        .iter()
        .map(|r| Bm25Entry {
            doc_id: r.get("doc_id"),
            layer: r.get("layer"),
            tokens_json: r.get("tokens_json"),
        })
        .collect();

    Ok(entries)
}

/// 删除指定 doc 的 BM25 条目。
pub async fn delete_bm25_entry(pool: &SqlitePool, doc_id: &str, layer: &str) -> RamariaResult<()> {
    sqlx::query("DELETE FROM bm25_index WHERE doc_id = ? AND layer = ?")
        .bind(doc_id)
        .bind(layer)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("删除 BM25 条目失败", e))?;
    Ok(())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::test_pool;

    #[tokio::test]
    async fn save_and_list_bm25() {
        let pool = test_pool().await.unwrap();

        let entry = Bm25Entry {
            doc_id: "doc-1".into(),
            layer: "l1".into(),
            tokens_json: r#"["今天","天气","真好"]"#.into(),
        };
        save_bm25_entry(&pool, &entry).await.unwrap();

        let entries = list_bm25_entries(&pool, "l1").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].doc_id, "doc-1");
        assert!(entries[0].tokens_json.contains("今天"));
    }

    #[tokio::test]
    async fn replace_existing_bm25_entry() {
        let pool = test_pool().await.unwrap();

        let e1 = Bm25Entry {
            doc_id: "doc-2".into(),
            layer: "l1".into(),
            tokens_json: r#"["旧","分词"]"#.into(),
        };
        save_bm25_entry(&pool, &e1).await.unwrap();

        let e2 = Bm25Entry {
            doc_id: "doc-2".into(),
            layer: "l1".into(),
            tokens_json: r#"["新","分词"]"#.into(),
        };
        save_bm25_entry(&pool, &e2).await.unwrap();

        let entries = list_bm25_entries(&pool, "l1").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].tokens_json.contains("新"));
    }

    #[tokio::test]
    async fn delete_bm25_entry_works() {
        let pool = test_pool().await.unwrap();

        let entry = Bm25Entry {
            doc_id: "doc-del".into(),
            layer: "l2".into(),
            tokens_json: r#"["测试"]"#.into(),
        };
        save_bm25_entry(&pool, &entry).await.unwrap();

        super::delete_bm25_entry(&pool, "doc-del", "l2")
            .await
            .unwrap();

        let entries = list_bm25_entries(&pool, "l2").await.unwrap();
        assert!(entries.is_empty());
    }
}
