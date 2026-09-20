//! crates/ramaria-storage/src/repo/schema_meta.rs - Schema 与索引版本管理模块
//!
//! 设计特点:
//! - 通过 key-value 表管理 schema_version 和 index_version
//! - schema_version 由 migration 写入；get_schema_version 供调用方显式校验（当前无启动期自动拦截）
//! - index_version 由应用层管理，索引重建后递增
//! - 版本值统一解析为 i32，非法值时返回 Storage 错误而非静默回退

use crate::repo::StorageResultExt;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::IndexCorpusStamp;
use sqlx::SqlitePool;

pub async fn get_schema_version(pool: &SqlitePool) -> RamariaResult<i32> {
    let version_str: String =
        sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = 'schema_version'")
            .fetch_optional(pool)
            .await
            .storage_err("查询 schema 版本失败")?
            .unwrap_or_else(|| "1".to_string());
    version_str
        .parse()
        .map_err(|_| RamariaError::storage("schema_version 值非法"))
}

pub async fn get_index_version(pool: &SqlitePool) -> RamariaResult<i32> {
    let version_str: String =
        sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = 'index_version'")
            .fetch_optional(pool)
            .await
            .storage_err("查询索引版本失败")?
            .unwrap_or_else(|| "1".to_string());
    version_str
        .parse()
        .map_err(|_| RamariaError::storage("index_version 值非法"))
}

pub async fn set_index_version(pool: &SqlitePool, version: i32) -> RamariaResult<()> {
    sqlx::query("INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('index_version', ?)")
        .bind(version.to_string())
        .execute(pool)
        .await
        .storage_err("更新索引版本失败")?;
    Ok(())
}

/// 读取记忆语料统计戳（跨进程索引刷新检测）。
///
/// 说明:
/// - 单条 SQL 的标量子查询聚合：返回参与内存检索索引的四类语料
///   （L1 摘要 / L2 事件 / utt 块 / 人格）的条数与最新写入时间；
/// - 只返回计数与时间戳，不含任何内容字段（隐私红线）；
/// - 常数级开销，供长驻进程在每次召回前调用。
pub async fn index_corpus_stamp(pool: &SqlitePool) -> RamariaResult<IndexCorpusStamp> {
    let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64)>(
        "SELECT
            (SELECT COUNT(*) FROM memory_l1),
            (SELECT COALESCE(MAX(created_at), 0) FROM memory_l1),
            (SELECT COUNT(*) FROM memory_events),
            (SELECT COALESCE(MAX(created_at), 0) FROM memory_events),
            (SELECT COUNT(*) FROM utt_blocks),
            (SELECT COUNT(*) FROM personas)",
    )
    .fetch_one(pool)
    .await
    .storage_err("查询索引语料统计失败")?;

    Ok(IndexCorpusStamp {
        l1_count: row.0,
        l1_max_created_at: row.1,
        event_count: row.2,
        event_max_created_at: row.3,
        utt_count: row.4,
        persona_count: row.5,
    })
}
