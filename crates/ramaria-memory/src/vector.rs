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

    /// 维度不符被拒绝写入的累计条数（诊断用；实现不支持时返回 0）。
    ///
    /// 用途:
    /// - 维度不一致的向量会被静默跳过（trait 的写入接口无返回值），
    ///   调用方可据此判断向量通道是否因配置错误而缺条。
    fn dimension_mismatch_count(&self) -> u64 {
        0
    }
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
    /// 维度不符被拒绝写入的累计条数
    dimension_mismatches: u64,
    /// 维度不符是否已告警（只告警首次，后续静默累计）
    dimension_mismatch_warned: bool,
}

impl BruteForceIndex {
    /// 创建空的暴力检索索引。
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            dimension: None,
            dimension_mismatches: 0,
            dimension_mismatch_warned: false,
        }
    }

    /// 获取当前索引的向量维度。
    pub fn dimension(&self) -> Option<usize> {
        self.dimension
    }

    /// 维度不符被拒绝写入的累计条数。
    ///
    /// 返回:
    /// - 自上次 `clear` 以来，因维度与索引首条不一致而被丢弃的向量条数。
    pub fn dimension_mismatch_count(&self) -> u64 {
        self.dimension_mismatches
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
                // 维度不匹配是严重配置错误：首次告警，后续只累计计数（批量导入时
                // 逐条告警会刷屏），计数经 dimension_mismatch_count 供诊断读取。
                self.dimension_mismatches += 1;
                if !self.dimension_mismatch_warned {
                    self.dimension_mismatch_warned = true;
                    tracing::warn!(
                        label = %label,
                        expected_dim = expected,
                        got_dim = dim,
                        "向量维度不匹配，跳过此条（可能导致检索结果不完整；后续同类条目不重复告警）"
                    );
                }
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
        self.dimension_mismatches = 0;
        self.dimension_mismatch_warned = false;
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn labels(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    fn dimension_mismatch_count(&self) -> u64 {
        self.dimension_mismatches
    }
}

mod cache;
pub use cache::{CachedVectorIndex, VectorCacheConfig};

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
mod tests;
