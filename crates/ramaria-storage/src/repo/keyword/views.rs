//! crates/ramaria-storage/src/repo/keyword/views.rs - 词条行视图与幂等种子
//!
//! 设计特点:
//! - 行视图覆盖规范词 / 已确认别名 / 待确认别名三种形态
//! - 别名行经 LEFT JOIN 带出规范词文本，避免二次查库
//! - 批量状态查询与 rowid 定位服务别名确认 / 驳回
//! - 手工种子由主键冲突 DO NOTHING 保证并发幂等

use ramaria_core::error::RamariaResult;
use ramaria_core::keyword::{KeywordPoolRow, KeywordToken};
use sqlx::SqlitePool;

use crate::repo::StorageResultExt;

use super::KeywordEntryRow;

// =========================================================
// M3 CLI 行视图 / rowid / 幂等种子（ramaria keyword）
// =========================================================

/// 列出 keyword_pool 全部词条（全字段行视图），按 use_count 降序、keyword 升序稳定排序。
///
/// 返回:
/// - `Vec<KeywordEntryRow>`: 词条全字段 + LEFT JOIN 解析出的指向规范词文本。
///
/// 说明:
/// - 输出覆盖规范词 / 已确认别名 / 待确认别名三种形态，供
///   `ramaria keyword list` 展示与词典装载共用。
/// - 排序稳定（use_count DESC、keyword ASC），重复调用输出一致。
pub async fn list_entries(pool: &SqlitePool) -> RamariaResult<Vec<KeywordEntryRow>> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            i64,
            Option<String>,
            Option<i64>,
            i64,
            Option<String>,
        ),
    >(
        "SELECT e.keyword, e.use_count, e.alias_status, e.canonical_id, e.created_at, c.keyword
         FROM keyword_pool e
         LEFT JOIN keyword_pool c ON e.canonical_id = c.rowid
         ORDER BY e.use_count DESC, e.keyword ASC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询关键词行视图失败")?;

    Ok(rows
        .into_iter()
        .map(
            |(keyword, use_count, alias_status, canonical_id, created_at, canonical_keyword)| {
                KeywordEntryRow {
                    keyword,
                    use_count,
                    alias_status,
                    canonical_id,
                    created_at,
                    canonical_keyword,
                }
            },
        )
        .collect())
}

/// 列出 keyword_pool 全部词条装载行（含 rowid / 别名状态 / 规范词指向），供服务镜像装载。
///
/// 说明:
/// - 与 `list_entries` 覆盖同一批词条，但额外带出 `rowid`——`KeywordStatus::Alias/Pending`
///   的 `canonical_id` 指向 keyword_pool.rowid，KeywordPool 三态状态机装载必须保留该字段。
/// - 输出覆盖规范词 / 已确认别名 / 待确认别名三种形态。
/// - 排序稳定（use_count DESC、keyword ASC），重复调用输出一致。
pub async fn list_pool_rows(pool: &SqlitePool) -> RamariaResult<Vec<KeywordPoolRow>> {
    let rows = sqlx::query_as::<
        _,
        (i64, String, i64, i64, Option<String>, Option<i64>, Option<String>),
    >(
        "SELECT e.rowid, e.keyword, e.use_count, e.created_at, e.alias_status, e.canonical_id, c.keyword
         FROM keyword_pool e
         LEFT JOIN keyword_pool c ON e.canonical_id = c.rowid
         ORDER BY e.use_count DESC, e.keyword ASC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询关键词装载行失败")?;

    Ok(rows
        .into_iter()
        .map(
            |(
                rowid,
                keyword,
                use_count,
                created_at,
                alias_status,
                canonical_id,
                canonical_keyword,
            )| {
                KeywordPoolRow {
                    rowid,
                    keyword,
                    use_count,
                    created_at,
                    alias_status,
                    canonical_id,
                    canonical_keyword,
                }
            },
        )
        .collect())
}

/// 按文本批量查询词条别名状态（供增量镜像与持久化状态保持一致）。
///
/// 参数:
/// - `keywords`: 待查询的标准化词条文本（不存在的文本不出现在结果中）。
///
/// 返回:
/// - `(keyword, alias_status)` 列表：`None` / `Some("canonical")` 为规范词，
///   `Some("alias")` 已确认别名，`Some("pending")` 待确认别名；
///   结果顺序不保证（调用方按文本查找消费）。
///
/// 说明:
/// - 空输入直接返回空（不访问数据库）；
/// - 按 500 分片查询（SQLite 变量上限保护），动态占位符只拼接 `?`，值全部绑定。
pub async fn list_keyword_statuses(
    pool: &SqlitePool,
    keywords: &[String],
) -> RamariaResult<Vec<(String, Option<String>)>> {
    if keywords.is_empty() {
        return Ok(Vec::new());
    }

    const CHUNK_SIZE: usize = 500;

    let mut result: Vec<(String, Option<String>)> = Vec::with_capacity(keywords.len());
    for chunk in keywords.chunks(CHUNK_SIZE) {
        let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT keyword, alias_status FROM keyword_pool WHERE keyword IN ({})",
            placeholders.join(", ")
        );
        let mut query = sqlx::query_as::<_, (String, Option<String>)>(&sql);
        for keyword in chunk {
            query = query.bind(keyword);
        }
        let rows = query
            .fetch_all(pool)
            .await
            .storage_err("查询词条状态失败")?;
        result.extend(rows);
    }
    Ok(result)
}

/// 按词条文本取其 rowid（供 alias confirm/reject 以文本定位行）。
///
/// 参数:
/// - `keyword`: 标准化后的关键词文本（通常来自 `KeywordToken::as_str()`）。
///
/// 返回:
/// - `Some(rowid)`: 词条存在。
/// - `None`: 词条不存在。
pub async fn find_rowid(pool: &SqlitePool, keyword: &str) -> RamariaResult<Option<i64>> {
    let rowid: Option<i64> = sqlx::query_scalar("SELECT rowid FROM keyword_pool WHERE keyword = ?")
        .bind(keyword)
        .fetch_optional(pool)
        .await
        .storage_err("查询词条 rowid 失败")?;
    Ok(rowid)
}

/// 幂等手工种子：向 keyword_pool 注入一个规范词（use_count=0、canonical_id NULL、alias_status NULL）。
///
/// 参数:
/// - `keyword`: 标准化后的规范词（KeywordToken Newtype）。
///
/// 返回:
/// - `true`: 本次新插入（use_count 从 0 起，表示未经自然出现累积）。
/// - `false`: 词条已存在，**保持已有行完全不动**（use_count/别名状态均不修改）。
///
/// 说明:
/// - 由主键冲突直接 DO NOTHING 保证并发幂等：并发调用同一词条恰好插入一次，
///   不存在"先查后插"两方都判定不存在、第二次插入报错的竞态窗口。
/// - 已存在词条即使为 pending/alias 也不触碰、不提示——由调用方依 `list_entries`
///   行视图判断现状（如"已是别名/待确认，可用 alias confirm/reject 处理"）。
/// - 手工种子供词典增强分词（keyword_pool 规范词 → BigramWithDictionaryNormalizer
///   词典）消费，幂等性保证重复执行不递增 use_count、不改别名状态。
pub async fn seed_canonical(pool: &SqlitePool, keyword: &KeywordToken) -> RamariaResult<bool> {
    let now = ramaria_core::types::now_ms();
    // 并发幂等：由主键冲突直接 DO NOTHING，不依赖"先查后插"
    // （后者在并发下存在两方都判定不存在、第二次插入报错的窗口）。
    let result = sqlx::query(
        "INSERT INTO keyword_pool (keyword, use_count, last_used_at, created_at, canonical_id, alias_status)
         VALUES (?, 0, NULL, ?, NULL, NULL)
         ON CONFLICT(keyword) DO NOTHING",
    )
    .bind(keyword.as_str())
    .bind(now)
    .execute(pool)
    .await
    .storage_err("种子注入关键词失败")?;
    Ok(result.rows_affected() > 0)
}
