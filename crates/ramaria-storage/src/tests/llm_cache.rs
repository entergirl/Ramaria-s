//! crates/ramaria-storage/src/tests/llm_cache.rs - LLM 响应缓存存储测试
//!
//! 设计特点:
//! - 覆盖 LRU 与 FIFO 两种容量淘汰策略
//! - 覆盖不限容量（max_entries=0）时全部保留
//! - 覆盖 LRU 命中刷新访问顺序
//! - 用例间写入间隔以毫秒保证时间戳可区分

use super::*;

// =========================================================
// SqliteLlmCache 容量自淘汰（v1.5 C）
// =========================================================

/// 写入三条记录（间隔 2ms 保证时间戳可区分顺序），
/// 返回各 key 供断言。
async fn fill_cache(cache: &SqliteLlmCache) {
    for (key, resp) in [("k1", "r1"), ("k2", "r2"), ("k3", "r3")] {
        cache
            .put(key, resp, "test-model", "test-version")
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn llm_cache_evicts_lru_beyond_capacity() {
    let pool = database::init_test_pool().await.expect("测试库初始化失败");
    let cache = SqliteLlmCache::new(pool, 2, CacheEviction::Lru);
    fill_cache(&cache).await;

    // 容量 2，写入 3 条 → 自动淘汰最旧 1 条
    assert_eq!(cache.count().await.unwrap(), 2);
    assert!(
        cache.get("k1").await.unwrap().is_none(),
        "LRU 应淘汰最早写入的 k1"
    );
    assert_eq!(cache.get("k3").await.unwrap().as_deref(), Some("r3"));
}

#[tokio::test]
async fn llm_cache_evicts_fifo_beyond_capacity() {
    let pool = database::init_test_pool().await.expect("测试库初始化失败");
    let cache = SqliteLlmCache::new(pool, 2, CacheEviction::Fifo);
    fill_cache(&cache).await;

    // FIFO 按写入顺序淘汰：即便 k3 先被访问，淘汰的仍是 early 写入的 k1
    assert_eq!(cache.count().await.unwrap(), 2);
    assert!(
        cache.get("k1").await.unwrap().is_none(),
        "FIFO 应按写入时间淘汰最早的 k1"
    );
    assert_eq!(cache.get("k3").await.unwrap().as_deref(), Some("r3"));
}

#[tokio::test]
async fn llm_cache_unlimited_capacity_keeps_all() {
    let pool = database::init_test_pool().await.expect("测试库初始化失败");
    // max_entries=0 表示不限制容量
    let cache = SqliteLlmCache::new(pool, 0, CacheEviction::Lru);
    fill_cache(&cache).await;
    assert_eq!(cache.count().await.unwrap(), 3, "不限制容量时不应淘汰");
}

#[tokio::test]
async fn llm_cache_hit_refreshes_lru_order() {
    let pool = database::init_test_pool().await.expect("测试库初始化失败");
    let cache = SqliteLlmCache::new(pool, 2, CacheEviction::Lru);
    for (key, resp) in [("k1", "r1"), ("k2", "r2")] {
        cache.put(key, resp, "m", "v").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    // 命中 k1 刷新其访问时间 → 之后写入 k3 时应淘汰 k2（而非 k1）
    assert_eq!(cache.get("k1").await.unwrap().as_deref(), Some("r1"));
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    cache.put("k3", "r3", "m", "v").await.unwrap();
    assert_eq!(cache.count().await.unwrap(), 2);
    assert!(
        cache.get("k1").await.unwrap().is_some(),
        "被命中的 k1 应保留"
    );
    assert!(
        cache.get("k2").await.unwrap().is_none(),
        "LRU 应淘汰未命中的 k2"
    );
}
