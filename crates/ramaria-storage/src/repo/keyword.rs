//! rust/crates/ramaria-storage/src/repo/keyword.rs - KeywordPool CRUD
//!
//! 设计特点:
//! - 管理关键词词典，防止摘要关键词随时间发散为大量同义词
//! - upsert 在冲突时递增 use_count，实现复用计数
//! - list_all 按使用频率降序排列，供 L1 摘要生成时作为候选列表

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use sqlx::SqlitePool;

pub async fn upsert(pool: &SqlitePool, keyword: &str) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query(
        "INSERT INTO keyword_pool (keyword, use_count, last_used_at, created_at) VALUES (?, 1, ?, ?)
         ON CONFLICT(keyword) DO UPDATE SET use_count = use_count + 1, last_used_at = ?"
    ).bind(keyword).bind(now).bind(now).bind(now)
        .execute(pool).await
        .storage_err("upsert 关键词失败")?;
    Ok(())
}

pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<String>> {
    let rows =
        sqlx::query_scalar::<_, String>("SELECT keyword FROM keyword_pool ORDER BY use_count DESC")
            .fetch_all(pool)
            .await
            .storage_err("查询关键词列表失败")?;
    Ok(rows)
}
