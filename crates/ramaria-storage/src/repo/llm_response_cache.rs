//! crates/ramaria-storage/src/repo/llm_response_cache.rs - LLM 响应精确缓存存取模块
//!
//! 设计特点:
//! - 管理 `llm_response_cache` 表（v1.5 新增），LLM 响应精确缓存的持久化读写
//! - key 为 sha256 哈希（只存响应不存原文输入，隐私红线）
//! - get 命中时原子更新 last_accessed_at / hit_count（LRU 淘汰依据）
//! - evict_oldest 按访问时间（LRU）或写入时间（FIFO）淘汰最旧条目
//! - put 使用 INSERT OR REPLACE，同 key 覆盖（key 即业务语义主键）

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use sqlx::SqlitePool;

// =========================================================
// 缓存条目
// =========================================================

/// `llm_response_cache` 表的一行。
///
/// 字段约定:
/// - `key`: sha256(model_id + template_version + prompt) hex，主键。
/// - `response`: LLM 响应文本（唯一存储的业务内容）。
/// - `model_id` / `template_version`: 审计用途（key 已包含二者语义）。
/// - `created_at` / `last_accessed_at`: epoch ms。
/// - `hit_count`: 累计命中次数。
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct LlmCacheEntry {
    pub key: String,
    pub response: String,
    pub model_id: String,
    pub template_version: String,
    pub created_at: i64,
    pub last_accessed_at: i64,
    pub hit_count: i64,
}

// =========================================================
// 查询 / 写入
// =========================================================

/// 按 key 查询缓存条目。
///
/// 说明:
/// - 命中时在同一事务内更新 `last_accessed_at` 与 `hit_count`，
///   供 LRU 淘汰与审计统计使用。
/// - 未命中返回 `Ok(None)`，不视为错误。
pub async fn get(
    pool: &SqlitePool,
    key: &str,
    now_ms: i64,
) -> RamariaResult<Option<LlmCacheEntry>> {
    let row: Option<LlmCacheEntry> = sqlx::query_as::<_, LlmCacheEntry>(
        "SELECT key, response, model_id, template_version, created_at, \
                last_accessed_at, hit_count \
         FROM llm_response_cache WHERE key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .storage_err("查询 LLM 响应缓存失败")?;

    if let Some(entry) = &row {
        // 命中：更新访问时间与命中计数（失败不阻断返回，仅记 warn）
        if let Err(e) = sqlx::query(
            "UPDATE llm_response_cache SET last_accessed_at = ?, hit_count = hit_count + 1 \
             WHERE key = ?",
        )
        .bind(now_ms)
        .bind(key)
        .execute(pool)
        .await
        .storage_err("更新 LLM 响应缓存命中计数失败")
        {
            tracing::warn!(cache_key = %key, error = %e, "更新缓存命中计数失败（非致命）");
        }
        return Ok(Some(entry.clone()));
    }

    Ok(None)
}

/// 写入一条缓存条目（同 key 覆盖）。
///
/// 参数:
/// - `now_ms`: 当前 epoch ms（created_at 与 last_accessed_at 共用）。
pub async fn put(pool: &SqlitePool, entry: &LlmCacheEntry, now_ms: i64) -> RamariaResult<()> {
    sqlx::query(
        "INSERT INTO llm_response_cache \
            (key, response, model_id, template_version, created_at, last_accessed_at, hit_count) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET \
            response = excluded.response, \
            model_id = excluded.model_id, \
            template_version = excluded.template_version, \
            created_at = excluded.created_at, \
            last_accessed_at = excluded.last_accessed_at, \
            hit_count = excluded.hit_count",
    )
    .bind(&entry.key)
    .bind(&entry.response)
    .bind(&entry.model_id)
    .bind(&entry.template_version)
    .bind(now_ms)
    .bind(now_ms)
    .bind(0i64)
    .execute(pool)
    .await
    .storage_err("写入 LLM 响应缓存失败")
    .map(|_| ())
}

// =========================================================
// 统计与淘汰
// =========================================================

/// 返回缓存条目总数。
pub async fn count(pool: &SqlitePool) -> RamariaResult<u64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM llm_response_cache")
        .fetch_one(pool)
        .await
        .storage_err("统计 LLM 响应缓存条目数失败")?;
    Ok(n.max(0) as u64)
}

/// 按淘汰策略删除最旧条目，使剩余条目数不超过 `keep`。
///
/// 参数:
/// - `keep`: 保留上限（`[cache].max_entries`）。
/// - `fifo`: true 按写入时间（created_at）淘汰；false 按访问时间（LRU）淘汰。
///
/// 返回:
/// - 实际删除的条目数。
pub async fn evict_oldest(pool: &SqlitePool, keep: u64, fifo: bool) -> RamariaResult<u64> {
    let total = count(pool).await?;
    if total <= keep {
        return Ok(0);
    }
    let excess = total - keep;
    let order_col = if fifo {
        "created_at"
    } else {
        "last_accessed_at"
    };
    let sql = format!(
        "DELETE FROM llm_response_cache WHERE key IN ( \
            SELECT key FROM llm_response_cache \
            ORDER BY {order_col} ASC \
            LIMIT ? \
        )"
    );
    let result = sqlx::query(&sql)
        .bind(excess as i64)
        .execute(pool)
        .await
        .storage_err("按容量淘汰 LLM 响应缓存失败")?;
    Ok(result.rows_affected())
}

// =========================================================
// 单元测试（内存库）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::init_test_pool;

    fn entry(key: &str, model: &str, version: &str) -> LlmCacheEntry {
        LlmCacheEntry {
            key: key.to_string(),
            response: format!("response-{key}"),
            model_id: model.to_string(),
            template_version: version.to_string(),
            created_at: 1_000,
            last_accessed_at: 1_000,
            hit_count: 0,
        }
    }

    #[tokio::test]
    async fn put_then_get_hit_returns_response() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        put(&pool, &entry("k1", "m1", "v1"), 1_000)
            .await
            .expect("写入成功");
        let got = get(&pool, "k1", 2_000).await.expect("查询成功");
        assert_eq!(
            got.as_ref().map(|e| e.response.as_str()),
            Some("response-k1")
        );
        // 首次 get 返回 SELECT 时刻快照（hit_count=0），随后命中计数已递增
        assert_eq!(got.unwrap().hit_count, 0, "首次查询返回快照 hit_count=0");
        // 第二次 get 应看到上次命中后的计数 1
        let again = get(&pool, "k1", 3_000)
            .await
            .expect("查询成功")
            .expect("应命中");
        assert_eq!(again.hit_count, 1, "二次查询应看到已递增的命中计数");
    }

    #[tokio::test]
    async fn get_miss_returns_none() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let got = get(&pool, "missing", 1_000).await.expect("查询成功");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn put_same_key_overwrites() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        put(&pool, &entry("k1", "m1", "v1"), 1_000)
            .await
            .expect("首次写入成功");
        let mut updated = entry("k1", "m2", "v2");
        updated.response = "new-response".to_string();
        put(&pool, &updated, 2_000).await.expect("覆盖写入成功");
        let got = get(&pool, "k1", 3_000)
            .await
            .expect("查询成功")
            .expect("应命中");
        assert_eq!(got.response, "new-response");
        assert_eq!(got.model_id, "m2");
    }

    #[tokio::test]
    async fn count_reflects_entries() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        assert_eq!(count(&pool).await.expect("计数成功"), 0);
        put(&pool, &entry("k1", "m1", "v1"), 1_000)
            .await
            .expect("写入成功");
        put(&pool, &entry("k2", "m1", "v1"), 1_000)
            .await
            .expect("写入成功");
        assert_eq!(count(&pool).await.expect("计数成功"), 2);
    }

    #[tokio::test]
    async fn evict_oldest_lru_removes_least_recently_accessed() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        put(&pool, &entry("old", "m1", "v1"), 1_000)
            .await
            .expect("写入成功");
        put(&pool, &entry("new", "m1", "v1"), 2_000)
            .await
            .expect("写入成功");
        // 访问 "new"，使其 last_accessed_at 更新到 3_000，LRU 应淘汰 "old"
        get(&pool, "new", 3_000).await.expect("查询成功");
        let removed = evict_oldest(&pool, 1, false).await.expect("淘汰成功");
        assert_eq!(removed, 1);
        assert!(get(&pool, "old", 4_000).await.expect("查询成功").is_none());
        assert!(get(&pool, "new", 4_000).await.expect("查询成功").is_some());
    }

    #[tokio::test]
    async fn evict_oldest_fifo_removes_earliest_created() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        put(&pool, &entry("early", "m1", "v1"), 1_000)
            .await
            .expect("写入成功");
        put(&pool, &entry("late", "m1", "v1"), 2_000)
            .await
            .expect("写入成功");
        // FIFO 按 created_at：即使 "late" 先被访问，淘汰的仍是 "early"
        get(&pool, "late", 9_000).await.expect("查询成功");
        let removed = evict_oldest(&pool, 1, true).await.expect("淘汰成功");
        assert_eq!(removed, 1);
        assert!(
            get(&pool, "early", 9_000)
                .await
                .expect("查询成功")
                .is_none()
        );
        assert!(get(&pool, "late", 9_000).await.expect("查询成功").is_some());
    }

    #[tokio::test]
    async fn evict_oldest_within_capacity_is_noop() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        put(&pool, &entry("k1", "m1", "v1"), 1_000)
            .await
            .expect("写入成功");
        put(&pool, &entry("k2", "m1", "v1"), 1_000)
            .await
            .expect("写入成功");
        let removed = evict_oldest(&pool, 10, false).await.expect("淘汰成功");
        assert_eq!(removed, 0);
        assert_eq!(count(&pool).await.expect("计数成功"), 2);
    }
}
