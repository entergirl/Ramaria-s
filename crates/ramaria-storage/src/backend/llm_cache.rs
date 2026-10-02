//! crates/ramaria-storage/src/backend/llm_cache.rs - Ramaria LLM 响应缓存模块
//!
//! 设计特点:
//! - `SqliteLlmCache` 实现 `LlmResponseCache`，供 `ramaria-llm` 的 ProviderBase 注入使用（llm_response_cache 表）
//! - 写入后按 `[cache].max_entries` 容量上限自动淘汰（LRU/FIFO），防止表无限增长
//! - 淘汰失败仅记 warn（不阻塞主流程，缓存淘汰是优化而非正确性约束）
//! - 只存响应，不存原文输入（key 为哈希）

use ramaria_core::config::CacheEviction;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::now_ms;
use sqlx::SqlitePool;

use crate::repo;

// =========================================================
// SqliteLlmCache —— LlmResponseCache trait 实现
// =========================================================

/// SQLite 实现的 LLM 响应精确缓存。
///
/// 职责:
/// - 实现 `ramaria_core::traits::LlmResponseCache`，供 `ramaria-llm` 的
///   `ProviderBase` 注入使用（`llm_response_cache` 表）。
/// - 写入后按 `[cache].max_entries` 容量上限自动淘汰（LRU/FIFO），
///   防止表无限增长；淘汰失败仅记 warn（不阻塞主流程）。
///
/// 安全约束:
/// - 只存响应，不存原文输入（key 为哈希，见 migration 注释）。
pub struct SqliteLlmCache {
    pool: SqlitePool,
    /// 容量上限（条目数）；0 表示不限制（仅测试/特殊场景）。
    max_entries: u64,
    /// 淘汰策略：true = FIFO（按 created_at），false = LRU（按 last_accessed_at）。
    fifo: bool,
}

impl SqliteLlmCache {
    /// 创建缓存实例。
    ///
    /// 参数:
    /// - `pool`: 数据库连接池（与主存储共用，保证同库事务一致）。
    /// - `max_entries`: 容量上限（`[cache].max_entries`）。
    /// - `eviction`: 淘汰策略（`[cache].eviction`，lru | fifo）。
    pub fn new(pool: SqlitePool, max_entries: u64, eviction: CacheEviction) -> Self {
        Self {
            pool,
            max_entries,
            fifo: eviction == CacheEviction::Fifo,
        }
    }
}

#[async_trait::async_trait]
impl ramaria_core::traits::LlmResponseCache for SqliteLlmCache {
    async fn get(&self, key: &str) -> RamariaResult<Option<String>> {
        let now = now_ms();
        match repo::llm_response_cache::get(&self.pool, key, now).await? {
            Some(entry) => Ok(Some(entry.response)),
            None => Ok(None),
        }
    }

    async fn put(
        &self,
        key: &str,
        response: &str,
        model_id: &str,
        template_version: &str,
    ) -> RamariaResult<()> {
        let entry = repo::llm_response_cache::LlmCacheEntry {
            key: key.to_string(),
            response: response.to_string(),
            model_id: model_id.to_string(),
            template_version: template_version.to_string(),
            created_at: 0,
            last_accessed_at: 0,
            hit_count: 0,
        };
        repo::llm_response_cache::put(&self.pool, &entry, now_ms()).await?;

        // 容量自淘汰（v1.5）：写入后若超出上限，按配置策略淘汰最旧条目。
        // 淘汰失败仅记 warn——缓存淘汰是优化而非正确性约束，不阻塞响应返回。
        if self.max_entries > 0
            && let Err(e) =
                repo::llm_response_cache::evict_oldest(&self.pool, self.max_entries, self.fifo)
                    .await
        {
            tracing::warn!(error = %e, max_entries = self.max_entries, "LLM 响应缓存容量淘汰失败（非致命）");
        }
        Ok(())
    }

    async fn count(&self) -> RamariaResult<u64> {
        repo::llm_response_cache::count(&self.pool).await
    }

    async fn evict_oldest(&self, keep: u64) -> RamariaResult<u64> {
        // 使用实例配置的淘汰策略（来自 [cache].eviction）。
        repo::llm_response_cache::evict_oldest(&self.pool, keep, self.fifo).await
    }
}
