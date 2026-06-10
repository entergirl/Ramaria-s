//! rust/crates/ramaria-memory/src/vector.rs — 向量检索引擎封装
//!
//! 设计特点:
//! - 定义 `VectorIndex` trait：统一的向量存储与检索接口
//! - 提供 `BruteForceIndex`：暴力余弦相似度检索（无外部依赖）
//! - 支持时间衰减加权（对接 `decay.rs` 的调整距离公式）
//! - Phase 3 接入真实 EmbeddingProvider 后可替换为 HNSW/Annoy 等高维索引
//! - 纯内存实现，不依赖数据库或异步运行时
//!
//! 设计决策:
//! - 不使用外部 crates（hnsw/annoy 等），保持零新增依赖
//! - BruteForce 在 L1+L2 <= 10000 文档规模下延迟可控（< 10ms）
//! - 预留 `VectorIndexError` 错误枚举供扩展

use std::collections::HashMap;

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
    /// 时间衰减因子 R（0.0..1.0），用于调整检索距离
    /// distance_adjusted = cosine_distance / max(R, 0.1)
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
/// Phase 3 替换方案:
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
    /// 返回 0.0..1.0 之间的值。
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
        debug_assert_eq!(a.len(), b.len(), "向量维度必须一致");

        let mut dot = 0.0_f64;
        let mut norm_a = 0.0_f64;
        let mut norm_b = 0.0_f64;

        for i in 0..a.len() {
            let ai = a[i] as f64;
            let bi = b[i] as f64;
            dot += ai * bi;
            norm_a += ai * ai;
            norm_b += bi * bi;
        }

        let denom = (norm_a * norm_b).sqrt();
        if denom < 1e-12 {
            0.0 // 零向量
        } else {
            (dot / denom).clamp(0.0, 1.0)
        }
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
                let similarity = Self::cosine_similarity(query, &entry.vector);
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

        let mut config = VectorIndexConfig::default();
        config.top_k = 3;
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

        let mut config = VectorIndexConfig::default();
        config.min_similarity = 0.9;

        // 查询与 "a" 非常相似
        let hits = idx.search(&[0.99, 0.14], &config).unwrap();
        assert!(hits.iter().any(|h| h.doc_label == "a"));
        // "b" 相似度低，应被过滤
        assert!(hits.iter().all(|h| h.doc_label == "a"));
    }

    #[test]
    fn cosine_similarity_identical() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = BruteForceIndex::cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 0.0001);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = BruteForceIndex::cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 0.0001);
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 0.0];
        let sim = BruteForceIndex::cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 0.0001);
    }

    // ---- label utilities ----

    #[test]
    fn vector_label_roundtrip() {
        let label = make_vector_label("l1", "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(label, "L1:550e8400-e29b-41d4-a716-446655440000");

        let parsed = parse_vector_label(&label).unwrap();
        assert_eq!(parsed.0, "L1");
        assert_eq!(parsed.1, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn vector_label_parse_invalid() {
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
}
