//! crates/ramaria-memory/src/vector/cache.rs - 向量检索查询缓存装饰器
//!
//! 设计特点:
//! - 查询向量 L2 归一化 + 保号量化后哈希，同向量必命中、方向差异不误命中
//! - LRU：命中提升到队尾，容量满淘汰队头；索引变更清空全部缓存
//! - Mutex 内部可变，保持 search(&self) 接口不变

use super::*;

// =========================================================
// CachedVectorIndex — 查询结果缓存装饰器
// =========================================================

/// 向量索引查询缓存配置。
///
/// 对 BruteForceIndex 添加 LRU 查询结果缓存。
/// 量化 key 相同的查询向量可直接返回缓存结果，避免 O(N·D) 全量扫描。
///
/// 缓存策略:
/// - 对查询向量做 L2 归一化 + 保号量化（i8）后哈希：同一查询向量必命中，
///   方向差异明显的查询不会误命中
/// - 缓存未命中 → 执行底层检索后存入缓存（使用 Mutex 实现 search(&self) 内部可变）
/// - 命中 → LRU 提升（条目移到末尾，最近使用优先保留）；容量满时淘汰队头最久未使用
/// - 索引变更（add/remove/clear）时清空全部缓存
#[derive(Debug, Clone)]
pub struct VectorCacheConfig {
    /// 最大缓存条目数（默认 128）
    pub max_entries: usize,
    /// 是否启用缓存
    pub enabled: bool,
}

impl Default for VectorCacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 128,
            enabled: true,
        }
    }
}

/// 缓存条目类型：(查询哈希, top_k, min_similarity量化, 结果列表)
type CacheEntries = Vec<(u64, usize, u64, Vec<VectorHit>)>;

/// 带查询缓存的向量索引装饰器。
///
/// 职责:
/// - 包装任意 `VectorIndex` 实现，透明添加查询缓存
/// - 对查询向量做 L2 归一化 + 保号量化（i8）后哈希：同一查询向量必命中，
///   方向差异明显的查询不会误命中
/// - 索引变更时自动清空缓存
/// - 使用 `Mutex` 实现搜索时的缓存读写（search 为 &self，缓存为内部可变）
///
/// 适用场景:
/// - 同一 session 内多次相似查询（例如流式对话中的反复 RAG 检索）
/// - 前端轮询记忆卡片时的重复查询
///
/// 注意事项:
/// - 缓存基于查询向量 + config 的复合 key，config 变更视为不同查询
/// - `top_k` 和 `min_similarity` 是 key 的一部分——不同参数不会误命中
/// - 使用 `std::sync::Mutex` 替代 RefCell 以满足 `VectorIndex: Send + Sync` 约束
/// - MutexGuard 仅在同线程内短期持有，不跨 .await，不会死锁
///
/// 容量策略（LRU）:
/// - 查询命中时条目移到队尾（最近使用），容量满时淘汰队头（最久未使用）。
/// - 容量上限按 `max_entries.max(1)` 生效：配置为 0 时退化为"仅保留最近一条"。
/// - 已接线进 `Retriever`（`vector_index: CachedVectorIndex<BruteForceIndex>`）。
#[derive(Debug)]
pub struct CachedVectorIndex<I: VectorIndex> {
    /// 底层索引实现
    inner: I,
    /// 缓存配置
    cache_config: VectorCacheConfig,
    /// 缓存条目: (量化哈希, top_k, min_similarity) → Vec<VectorHit>
    /// Mutex 实现内部可变性，满足 VectorIndex: Send + Sync 约束
    #[allow(clippy::type_complexity)]
    pub(super) cache: Mutex<CacheEntries>,
}

impl<I: VectorIndex> CachedVectorIndex<I> {
    /// 创建带缓存的向量索引包装。
    ///
    /// 参数:
    /// - `inner`: 底层索引实现（如 `BruteForceIndex`）。
    /// - `cache_config`: 缓存配置（None 使用默认值）。
    pub fn new(inner: I, cache_config: Option<VectorCacheConfig>) -> Self {
        Self {
            inner,
            cache_config: cache_config.unwrap_or_default(),
            cache: Mutex::new(Vec::new()),
        }
    }

    /// 手动清空查询缓存。
    pub fn invalidate_cache(&self) {
        lock_recover(&self.cache, "CachedVectorIndex::invalidate_cache").clear();
    }

    /// 返回当前缓存条目数。
    pub fn cache_len(&self) -> usize {
        lock_recover(&self.cache, "CachedVectorIndex::cache_len").len()
    }

    /// 对查询向量做 L2 归一化 + 保号量化（i8）后折叠为 u64 哈希。
    ///
    /// 先按 L2 范数归一化（零向量按全零处理，避免除零），再将每个分量
    /// 保号量化到 [-127, 127] 并依次并入哈希。同一查询向量必命中同一缓存；
    /// 方向差异明显的查询不会误命中。
    ///
    /// 参数:
    /// - `query`: 查询向量。
    ///
    /// 返回:
    /// - 量化后的 64 位哈希值。
    fn quantize_query(query: &[f32]) -> u64 {
        // L2 归一化：只比较查询方向，整体缩放不影响缓存 key
        let norm = query
            .iter()
            .map(|v| (*v as f64) * (*v as f64))
            .sum::<f64>()
            .sqrt();
        // 零向量（含 NaN 分量）按 0 处理，避免除零；此时所有分量量化为 0
        let inv_norm = if norm > 0.0 { 1.0 / norm } else { 0.0 };

        let mut hash: u64 = 0;
        for (i, &val) in query.iter().enumerate() {
            // 保号量化：归一化后映射到 [-127, 127]，负分量不再被折叠为 0
            let quantized = ((val as f64) * inv_norm * 127.0)
                .round()
                .clamp(-127.0, 127.0) as i8;
            // 旋转哈希，每个量化值影响不同位；以 i8 的字节位模式并入
            hash = hash.wrapping_mul(31).wrapping_add((quantized as u8) as u64);
            // 混合位置信息
            hash ^= (i as u64).wrapping_mul(0x9E3779B97F4A7C15);
        }
        hash
    }

    /// 将 `min_similarity` 量化为 u64。
    ///
    /// 将 f64 相似度阈值转换为可哈希的整数表示。
    fn quantize_min_sim(min_sim: f64) -> u64 {
        // 保留 4 位小数精度：0.1234 → 1234
        (min_sim * 10000.0_f64).clamp(0.0, 10000.0) as u64
    }
}

impl<I: VectorIndex + std::fmt::Display> std::fmt::Display for CachedVectorIndex<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cache = lock_recover(&self.cache, "CachedVectorIndex::fmt");
        write!(
            f,
            "CachedVectorIndex(cache={}/{}, inner={})",
            cache.len(),
            self.cache_config.max_entries,
            self.inner
        )
    }
}

impl<I: VectorIndex> VectorIndex for CachedVectorIndex<I> {
    fn add(&mut self, label: &str, vector: Vec<f32>, created_at: i64) {
        self.invalidate_cache();
        self.inner.add(label, vector, created_at);
    }

    fn add_batch(&mut self, entries: Vec<(String, Vec<f32>, i64)>) {
        self.invalidate_cache();
        for (label, vec, ts) in entries {
            self.inner.add(&label, vec, ts);
        }
    }

    fn search(
        &self,
        query: &[f32],
        config: &VectorIndexConfig,
    ) -> Result<Vec<VectorHit>, VectorIndexError> {
        if !self.cache_config.enabled {
            return self.inner.search(query, config);
        }

        // 构建缓存 key
        let qhash = Self::quantize_query(query);
        let sim_key = Self::quantize_min_sim(config.min_similarity);

        // 查找缓存（LRU：命中条目移到末尾，最近使用优先保留）
        {
            let mut cache = lock_recover(&self.cache, "CachedVectorIndex::search.lookup");
            let hit_index = cache.iter().position(|(c_qhash, c_topk, c_sim, _)| {
                *c_qhash == qhash && *c_topk == config.top_k && *c_sim == sim_key
            });
            if let Some(idx) = hit_index {
                tracing::trace!(
                    cache_hit = true,
                    qhash,
                    top_k = config.top_k,
                    "向量查询缓存命中（LRU 提升）"
                );
                let entry = cache.remove(idx);
                let hits = entry.3.clone();
                cache.push(entry);
                return Ok(hits);
            }
        }

        // 缓存未命中：执行实际检索
        tracing::trace!(
            cache_hit = false,
            qhash,
            top_k = config.top_k,
            "向量查询缓存未命中，执行实际检索并回填缓存"
        );
        let results = self.inner.search(query, config)?;

        // 回填缓存（LRU 驱逐：容量满时淘汰队头最久未使用）
        // 容量下限保护为 1：`max_entries=0` 时退化为"仅保留最近一条"，
        // 不再出现空表 `remove(0)` 越界 panic。
        let mut cache = lock_recover(&self.cache, "CachedVectorIndex::search.fill");
        let limit = self.cache_config.max_entries.max(1);
        if cache.len() >= limit {
            cache.remove(0);
        }
        cache.push((qhash, config.top_k, sim_key, results.clone()));

        Ok(results)
    }

    fn remove(&mut self, label: &str) {
        self.invalidate_cache();
        self.inner.remove(label);
    }

    fn clear(&mut self) {
        self.invalidate_cache();
        self.inner.clear();
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn labels(&self) -> Vec<String> {
        self.inner.labels()
    }

    fn dimension_mismatch_count(&self) -> u64 {
        self.inner.dimension_mismatch_count()
    }
}
