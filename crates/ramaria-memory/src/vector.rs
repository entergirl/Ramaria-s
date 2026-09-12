//! crates/ramaria-memory/src/vector.rs — 向量检索引擎封装
//!
//! 设计特点:
//! - 定义 `VectorIndex` trait：统一的向量存储与检索接口
//! - 提供 `BruteForceIndex`：暴力余弦相似度检索（无外部依赖）
//! - 预留时间衰减加权位（`VectorEntry::retention`），时间衰减由 `decay` 模块负责
//! - 索引实现经 `VectorIndex` trait 解耦，预留替换位；当前仅内置 `BruteForceIndex`（零新增依赖）
//! - 纯内存实现，不依赖数据库或异步运行时
//!
//! 设计决策:
//! - 不使用外部 crates（hnsw/annoy 等），保持零新增依赖
//! - BruteForce 在 L1+L2 <= 10000 文档规模下延迟可控（< 10ms）
//! - 预留 `VectorIndexError` 错误枚举供扩展

use std::collections::HashMap;
use std::sync::Mutex;

use ramaria_core::lock::lock_recover;

// =========================================================
// 核心类型
// =========================================================

/// 向量索引中的条目。
#[derive(Debug, Clone)]
pub struct VectorEntry {
    /// 向量数据
    pub vector: Vec<f32>,
    /// 关联的文档标识（供 retriever 层映射回 MemoryL1/MemoryEvent）
    pub doc_label: String,
    /// 时间衰减因子 R（0.0..1.0），用于调整检索相似度：
    /// adjusted_similarity = similarity * retention。
    ///
    /// 当前仅由 `add` 固定写为 1.0（不随时间衰减）；检索结果层的时间
    /// 衰减由 `decay` 模块统一计算，本字段仅预留加权位。
    pub retention: f64,
    /// 创建/更新时间（Unix 毫秒），用于时间衰减计算
    pub created_at: i64,
}

/// 单条向量检索结果。
#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    /// 关联的文档标识
    pub doc_label: String,
    /// 余弦相似度（未调整）0.0..1.0
    pub similarity: f64,
    /// 时间衰减调整后的相似度
    /// adjusted_similarity = similarity * retention
    pub adjusted_similarity: f64,
}

/// 向量索引特质的错误类型。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VectorIndexError {
    /// 向量维度不匹配
    DimensionMismatch { expected: usize, got: usize },
    /// 索引中无数据
    Empty,
    /// 文档不存在
    NotFound,
}

impl std::fmt::Display for VectorIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VectorIndexError::DimensionMismatch { expected, got } => {
                write!(f, "向量维度不匹配: 期望 {} 维, 收到 {} 维", expected, got)
            }
            VectorIndexError::Empty => write!(f, "向量索引为空"),
            VectorIndexError::NotFound => write!(f, "文档不存在"),
        }
    }
}

/// 向量索引配置。
#[derive(Debug, Clone)]
pub struct VectorIndexConfig {
    /// 检索返回的最大结果数
    pub top_k: usize,
    /// 最小相似度阈值（低于此值的结果被过滤）
    pub min_similarity: f64,
}

impl Default for VectorIndexConfig {
    fn default() -> Self {
        Self {
            top_k: 20,
            min_similarity: 0.0,
        }
    }
}

// =========================================================
// VectorIndex trait
// =========================================================

/// 向量索引抽象 trait。
///
/// 职责:
/// - 提供统一的向量存储、检索、移除接口
/// - 允许在 BruteForce / HNSW / Annoy 等实现间切换
///
/// 实现要求:
/// - `add` 幂等：同一 label 重复添加应覆盖旧向量
/// - `search` 按 adjusted_similarity 降序排列返回
/// - 所有方法不 panic，错误通过 Result 传播
pub trait VectorIndex: Send + Sync {
    /// 添加/更新一条向量。
    ///
    /// 若 label 已存在，覆盖旧记录。
    fn add(&mut self, label: &str, vector: Vec<f32>, created_at: i64);

    /// 批量添加向量。
    fn add_batch(&mut self, entries: Vec<(String, Vec<f32>, i64)>) {
        for (label, vec, ts) in entries {
            self.add(&label, vec, ts);
        }
    }

    /// 检索与 query 最相似的 top_k 个向量。
    ///
    /// 使用 decay.rs 中的 `adjust_distance` 逻辑：
    /// - 计算余弦相似度
    /// - 乘以时间保留率得到 adjusted_similarity
    /// - 按 adjusted_similarity 降序排列
    fn search(
        &self,
        query: &[f32],
        config: &VectorIndexConfig,
    ) -> Result<Vec<VectorHit>, VectorIndexError>;

    /// 移除指定 label 的向量。
    fn remove(&mut self, label: &str);

    /// 清空索引。
    fn clear(&mut self);

    /// 索引中的条目数。
    fn len(&self) -> usize;

    /// 索引是否为空。
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 获取当前所有条目的 label 列表。
    fn labels(&self) -> Vec<String>;
}

// =========================================================
// BruteForceIndex — 暴力余弦相似度检索
// =========================================================

/// 暴力余弦相似度向量索引。
///
/// 适用场景:
/// - L1 + L2 文档总量 < 10000
/// - 嵌入维度 <= 1024
/// - 不需要近似最近邻
///
/// 时间复杂度: O(N·D)，N=文档数，D=维度
///
/// 替换方案:
/// - 文档数 > 10000 → 替换为 HNSW (hnsw_rs crate)
/// - 内存敏感 → 替换为 Annoy (annoy-rs crate)
#[derive(Debug, Clone)]
pub struct BruteForceIndex {
    entries: HashMap<String, VectorEntry>,
    dimension: Option<usize>,
}

impl BruteForceIndex {
    /// 创建空的暴力检索索引。
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            dimension: None,
        }
    }

    /// 获取当前索引的向量维度。
    pub fn dimension(&self) -> Option<usize> {
        self.dimension
    }

    /// 计算两个向量之间的余弦相似度。
    ///
    /// 公式: cos(a,b) = (a·b) / (||a||·||b||)
    ///
    /// 说明（v1.5 收敛）:
    /// - 实现统一收敛到 `crate::similarity::cosine_similarity`，本函数为薄包装。
    /// - 统一实现返回 [-1.0, 1.0]；本模块在 `search()` 调用处以 `.max(0.0)`
    ///   保持原 [0.0, 1.0] 语义（负相关视为 0，不惩罚）。
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
        crate::similarity::cosine_similarity(a, b)
    }
}

impl Default for BruteForceIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorIndex for BruteForceIndex {
    fn add(&mut self, label: &str, vector: Vec<f32>, created_at: i64) {
        let dim = vector.len();

        // 0 维向量（空 embedding）不得被记为首个期望维度：一旦记录，
        // 后续真实维度的向量会被全部拒绝，向量通道整体静默失效（只剩 BM25/关键词）。
        if dim == 0 {
            tracing::warn!(label = %label, "拒绝写入 0 维向量（空 embedding），该条不入向量索引");
            return;
        }

        if let Some(expected) = self.dimension {
            if expected != dim {
                // trait 定义为无返回值（为了接口简洁），无法返回 Result。
                // 维度不匹配是严重配置错误，必须通过日志告警便于排查。
                tracing::warn!(
                    label = %label,
                    expected_dim = expected,
                    got_dim = dim,
                    "向量维度不匹配，跳过此条（可能导致检索结果不完整）"
                );
                return;
            }
        } else {
            self.dimension = Some(dim);
        }

        self.entries.insert(
            label.to_string(),
            VectorEntry {
                vector,
                doc_label: label.to_string(),
                retention: 1.0, // 初始保留率 = 1.0，后续通过 decay 计算
                created_at,
            },
        );
    }

    fn search(
        &self,
        query: &[f32],
        config: &VectorIndexConfig,
    ) -> Result<Vec<VectorHit>, VectorIndexError> {
        if self.entries.is_empty() {
            return Err(VectorIndexError::Empty);
        }

        if let Some(expected) = self.dimension
            && query.len() != expected
        {
            return Err(VectorIndexError::DimensionMismatch {
                expected,
                got: query.len(),
            });
        }

        let mut hits: Vec<VectorHit> = self
            .entries
            .values()
            .map(|entry| {
                // 统一实现返回 [-1,1]，此处保持原 [0,1] 语义（负相关视为 0，不惩罚）
                let similarity = Self::cosine_similarity(query, &entry.vector).max(0.0);
                let adjusted = similarity * entry.retention;
                VectorHit {
                    doc_label: entry.doc_label.clone(),
                    similarity,
                    adjusted_similarity: adjusted,
                }
            })
            .filter(|h| h.adjusted_similarity >= config.min_similarity)
            .collect();

        // 按调整后的相似度降序排列
        hits.sort_by(|a, b| {
            b.adjusted_similarity
                .partial_cmp(&a.adjusted_similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if hits.len() > config.top_k {
            hits.truncate(config.top_k);
        }

        Ok(hits)
    }

    fn remove(&mut self, label: &str) {
        self.entries.remove(label);
        if self.entries.is_empty() {
            self.dimension = None;
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.dimension = None;
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn labels(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }
}

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
    cache: Mutex<CacheEntries>,
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
}

// =========================================================
// 辅助函数
// =========================================================

/// 为向量索引构建 label 字符串。
///
/// 格式: "L1:{uuid}" 或 "L2:{id}"
pub fn make_vector_label(layer: &str, id: &str) -> String {
    format!("{}:{}", layer.to_uppercase(), id)
}

/// 从向量 label 解析层级和 ID。
pub fn parse_vector_label(label: &str) -> Option<(&str, &str)> {
    let (layer, id) = label.split_once(':')?;
    Some((layer, id))
}

/// 生成用于测试的随机向量。
#[cfg(test)]
pub fn random_vector(dim: usize) -> Vec<f32> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos() as u64;

    let mut state = seed;
    let mut vec = Vec::with_capacity(dim);
    for _ in 0..dim {
        // 简单的线性同余生成器
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let val = ((state >> 32) as f32) / (u32::MAX as f32);
        vec.push(val);
    }
    vec
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- BruteForceIndex ----

    #[test]
    fn index_add_and_search() {
        let mut idx = BruteForceIndex::new();
        let v1 = vec![1.0, 0.0, 0.0];
        let v2 = vec![0.0, 1.0, 0.0];
        let v3 = vec![0.0, 0.0, 1.0];

        idx.add("doc1", v1.clone(), 1000);
        idx.add("doc2", v2.clone(), 1000);
        idx.add("doc3", v3.clone(), 1000);

        assert_eq!(idx.len(), 3);
        assert_eq!(idx.dimension(), Some(3));

        // 查询 [1.0, 0.0, 0.0]，doc1 应排第一
        let query = vec![1.0, 0.0, 0.0];
        let config = VectorIndexConfig::default();
        let hits = idx.search(&query, &config).unwrap();

        assert!(!hits.is_empty());
        assert_eq!(hits[0].doc_label, "doc1");
        assert!((hits[0].similarity - 1.0).abs() < 0.01);
    }

    #[test]
    fn index_search_empty_returns_error() {
        let idx = BruteForceIndex::new();
        let config = VectorIndexConfig::default();
        let result = idx.search(&[1.0, 0.0], &config);
        assert_eq!(result, Err(VectorIndexError::Empty));
    }

    #[test]
    fn index_dimension_mismatch() {
        let mut idx = BruteForceIndex::new();
        idx.add("doc1", vec![1.0, 0.0, 0.0], 1000);

        let config = VectorIndexConfig::default();
        let result = idx.search(&[1.0, 0.0], &config);
        assert!(matches!(
            result,
            Err(VectorIndexError::DimensionMismatch { .. })
        ));
    }

    /// 0 维向量（空 embedding）不得被记为首个期望维度：
    /// 一旦记录，后续真实维度会被全部拒绝，向量通道整体静默失效（只剩 BM25/关键词）。
    #[test]
    fn add_rejects_zero_dim_and_keeps_channel_usable() {
        let mut idx = BruteForceIndex::new();

        idx.add("empty", Vec::new(), 1000);
        assert_eq!(idx.len(), 0, "0 维向量不入索引");
        assert_eq!(idx.dimension(), None, "0 维不得被记为首个期望维度");

        idx.add("doc1", vec![1.0, 0.0, 0.0], 1000);
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.dimension(), Some(3));

        let config = VectorIndexConfig::default();
        let hits = idx.search(&[1.0, 0.0, 0.0], &config).unwrap();
        assert_eq!(hits[0].doc_label, "doc1", "真实维度写入后向量通道仍可用");
    }

    #[test]
    fn index_remove_and_clear() {
        let mut idx = BruteForceIndex::new();
        idx.add("doc1", vec![1.0, 0.0], 1000);
        idx.add("doc2", vec![0.0, 1.0], 1000);
        assert_eq!(idx.len(), 2);

        idx.remove("doc1");
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.dimension(), Some(2));

        idx.clear();
        assert_eq!(idx.len(), 0);
        assert_eq!(idx.dimension(), None);
    }

    #[test]
    fn index_add_overwrite() {
        let mut idx = BruteForceIndex::new();
        idx.add("doc1", vec![1.0, 0.0], 1000);
        idx.add("doc1", vec![0.0, 1.0], 2000);

        let config = VectorIndexConfig::default();
        let hits = idx.search(&[0.0, 1.0], &config).unwrap();
        assert_eq!(hits[0].doc_label, "doc1");
        assert!((hits[0].similarity - 1.0).abs() < 0.01);
    }

    #[test]
    fn index_top_k_truncation() {
        let mut idx = BruteForceIndex::new();
        for i in 0..10 {
            let mut v = vec![0.0_f32; 10];
            v[i] = 1.0;
            idx.add(&format!("doc{}", i), v, 1000);
        }

        let config = VectorIndexConfig {
            top_k: 3,
            ..Default::default()
        };
        let mut query = vec![0.0_f32; 10];
        query[0] = 1.0;

        let hits = idx.search(&query, &config).unwrap();
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn index_min_similarity_filter() {
        let mut idx = BruteForceIndex::new();
        idx.add("a", vec![1.0, 0.0], 1000);
        idx.add("b", vec![0.0, 1.0], 1000);

        let config = VectorIndexConfig {
            min_similarity: 0.9,
            ..Default::default()
        };

        // 查询与 "a" 非常相似
        let hits = idx.search(&[0.99, 0.14], &config).unwrap();
        assert!(hits.iter().any(|h| h.doc_label == "a"));
        // "b" 相似度低，应被过滤
        assert!(hits.iter().all(|h| h.doc_label == "a"));
    }

    // ---- label utilities ----

    /// make_vector_label / parse_vector_label 往返与非法输入验证。
    #[test]
    fn vector_label_cases() {
        let label = make_vector_label("l1", "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(label, "L1:550e8400-e29b-41d4-a716-446655440000");
        let parsed = parse_vector_label(&label).unwrap();
        assert_eq!(parsed.0, "L1");
        assert_eq!(parsed.1, "550e8400-e29b-41d4-a716-446655440000");
        // 非法格式 → None
        assert!(parse_vector_label("invalid").is_none());
    }

    // ---- VectorIndex trait object ----

    #[test]
    fn vector_index_trait_object() {
        fn _accept(v: &dyn VectorIndex) {
            let _ = v.len();
        }

        let idx = BruteForceIndex::new();
        _accept(&idx);
    }

    // ---- CachedVectorIndex（LRU 容量策略）----

    /// 缓存容量满时淘汰"最久未使用"（LRU），而非最早插入（FIFO）。
    ///
    /// 场景: max_entries=2，依次查询 A/B/A/C：
    /// - A、B 入缓存 [A, B]；再查 A → LRU 提升 [B, A]；
    /// - 查 C（未命中）→ 容量满 → 淘汰队头 B（最久未使用）→ 缓存 [A, C]；
    /// - 若为 FIFO 则淘汰 A，B 仍在——本断言锁定 LRU 语义。
    #[test]
    fn cached_index_evicts_least_recently_used() {
        let mut inner = BruteForceIndex::new();
        inner.add("a", vec![1.0, 0.0, 0.0], 1);
        inner.add("b", vec![0.0, 1.0, 0.0], 2);
        inner.add("c", vec![0.0, 0.0, 1.0], 3);
        let cfg = VectorCacheConfig {
            max_entries: 2,
            enabled: true,
        };
        let idx = CachedVectorIndex::new(inner, Some(cfg));
        let conf = VectorIndexConfig::default();

        let qa = [1.0, 0.0, 0.0];
        let qb = [0.0, 1.0, 0.0];
        let qc = [0.0, 0.0, 1.0];

        idx.search(&qa, &conf).unwrap(); // 缓存 [A]
        idx.search(&qb, &conf).unwrap(); // 缓存 [A, B]
        assert_eq!(idx.cache_len(), 2);
        idx.search(&qa, &conf).unwrap(); // 命中 A → LRU 提升 [B, A]
        idx.search(&qc, &conf).unwrap(); // C 未命中 → 驱逐队头 B → [A, C]

        assert_eq!(idx.cache_len(), 2);
        let cache = idx.cache.lock().unwrap();
        let labels: Vec<&str> = cache
            .iter()
            .map(|(_, _, _, hits)| hits[0].doc_label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec!["a", "c"],
            "应淘汰最久未使用的 B（LRU 而非 FIFO）"
        );
    }

    /// 缓存命中提升：重复查询同一向量走缓存，不重复全量扫描（cache_len 不增长）。
    #[test]
    fn cached_index_hit_does_not_grow_cache() {
        let mut inner = BruteForceIndex::new();
        inner.add("a", vec![1.0, 0.0], 1);
        let cfg = VectorCacheConfig {
            max_entries: 8,
            enabled: true,
        };
        let idx = CachedVectorIndex::new(inner, Some(cfg));
        let conf = VectorIndexConfig::default();

        let q = [1.0, 0.0];
        for _ in 0..5 {
            let hits = idx.search(&q, &conf).unwrap();
            assert_eq!(hits[0].doc_label, "a");
        }
        assert_eq!(idx.cache_len(), 1, "同查询命中缓存，不新增条目");
    }

    /// 负分量不得被折叠为同一缓存 key：
    /// 量化若把负分量截断为 0，仅负分量不同的查询会误命中同一缓存并返回错误结果。
    #[test]
    fn cached_index_negative_components_do_not_collide() {
        let mut inner = BruteForceIndex::new();
        inner.add("a", vec![1.0, -0.9, 0.2], 1);
        inner.add("b", vec![1.0, -0.1, 0.2], 2);
        let cfg = VectorCacheConfig {
            max_entries: 8,
            enabled: true,
        };
        let idx = CachedVectorIndex::new(inner, Some(cfg));
        let conf = VectorIndexConfig::default();

        // q1 与 a 同向，q2 与 b 同向；两者仅负分量不同
        let q1 = [1.0, -0.9, 0.2];
        let hits1 = idx.search(&q1, &conf).unwrap();
        assert_eq!(hits1[0].doc_label, "a");

        let q2 = [1.0, -0.1, 0.2];
        let hits2 = idx.search(&q2, &conf).unwrap();
        assert_eq!(
            hits2[0].doc_label, "b",
            "负分量不同不得命中同一缓存 key（否则返回上一次的错误结果）"
        );
    }

    /// `max_entries=0` 不得触发空表 `remove(0)` 越界 panic：
    /// 容量下限保护为 1，退化为"仅保留最近一条"，两次不同查询结果各自正确。
    #[test]
    fn cached_index_zero_capacity_does_not_panic() {
        let mut inner = BruteForceIndex::new();
        inner.add("a", vec![1.0, 0.0, 0.0], 1);
        inner.add("b", vec![0.0, 1.0, 0.0], 2);
        let cfg = VectorCacheConfig {
            max_entries: 0,
            enabled: true,
        };
        let idx = CachedVectorIndex::new(inner, Some(cfg));
        let conf = VectorIndexConfig::default();

        let hits_a = idx.search(&[1.0, 0.0, 0.0], &conf).unwrap();
        assert_eq!(hits_a[0].doc_label, "a");

        let hits_b = idx.search(&[0.0, 1.0, 0.0], &conf).unwrap();
        assert_eq!(hits_b[0].doc_label, "b");

        assert_eq!(idx.cache_len(), 1, "容量 0 退化为仅保留最近一条");
    }
}
