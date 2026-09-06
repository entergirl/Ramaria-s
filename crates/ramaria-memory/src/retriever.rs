//! crates/ramaria-memory/src/retriever.rs — 三通道组合检索编排器
//!
//! 设计特点:
//! - 编排 BM25 + 向量 + 图谱三个独立通道，结果通过 RRF 融合
//! - 支持按 persona_uid 过滤（各通道独立过滤）
//! - 纯编排逻辑，各通道的索引维护由各自模块负责
//! - 不直接访问数据库——通过注入的回调函数获取文档
//! - 根文件仅保留类型再导出与结构/构造逻辑，主体逻辑拆分至 index/search/utt/helpers 子模块
//!
//! 检索流程:
//! 1. BM25 通道: 对 L1/L2 文本字段执行 BM25 评分
//! 2. 向量通道: 使用 EmbeddingProvider 生成 query 向量，执行余弦检索
//! 3. 图谱通道: 从查询提取实体，执行 1-hop 图谱遍历
//! 4. RRF 融合: 三个通道结果通过 rrf_fuse 合并排序
//! 5. 解析 doc_id → 加载实际文档 → 返回检索结果

use crate::bm25::Bm25Index;
use crate::graph_retriever::GraphRetriever;
use crate::vector::{BruteForceIndex, CachedVectorIndex};

// 以下类型仅供 `tests.rs` 经 `use super::*` 引用，故仅测试编译时引入。
#[cfg(test)]
use crate::bm25::DocId;
#[cfg(test)]
use crate::decay::DecayConfig;
#[cfg(test)]
use ramaria_core::types::MemoryL1;

mod helpers;
mod index;
mod search;
mod types;
mod utt;

pub use types::{
    L1DocView, L2DocView, RetrieverConfig, SearchRequest, SearchResult, UttDocView, UttHit,
};

#[cfg(test)]
mod tests;

// =========================================================
// 三通道检索器
// =========================================================

/// 三通道组合检索器。
///
/// 职责:
/// - 持有各通道的索引（BM25 / 向量 / 图谱）
/// - 提供统一的 `search` 入口
/// - 管理索引的构建和增量更新
///
/// 内存假设:
/// - L1/L2 文档视图完整存储在内存 HashMap 中，用于 BM25 搜索结果的文档解析。
/// - 在 10k 文档规模下，内存占用约 5-15 MB，
///   完全在 200MB 空闲目标范围内。
/// - 超过 LRU 容量上限时，按文档创建时间淘汰最旧条目，同时清理对应的 BM25 索引。
///
/// LRU 驱逐策略:
/// - 当文档数超过 `lru_max_entries`（默认 50_000）时触发驱逐。
/// - 驱逐从 BM25 索引和内存 HashMap 中同步移除最早创建的文档。
/// - 驱逐阈值远高于典型场景（10k），在不影响常规使用的前提下防止内存无限增长。
#[derive(Debug)]
pub struct Retriever {
    /// 检索配置
    config: RetrieverConfig,
    /// BM25 索引
    bm25_index: Bm25Index,
    /// 向量索引（带 LRU 查询缓存，v1.6 接线：L1/L2 文档向量与 utt 块向量共存）
    vector_index: CachedVectorIndex<BruteForceIndex>,
    /// 图谱检索器
    graph_retriever: GraphRetriever,
    /// LRU 容量上限：超过此值时驱逐最旧文档（0 表示不限制）
    lru_max_entries: usize,
    /// doc_id → L1 视图（BM25 结果解析用）
    l1_docs: std::collections::HashMap<uuid::Uuid, L1DocView>,
    /// doc_id → L2 视图（BM25 结果解析用）
    l2_docs: std::collections::HashMap<i64, L2DocView>,
    /// utt 块 ID → 块视图（原文通道；不参与 LRU 驱逐，块数量级远小于 L1/L2）
    utt_docs: std::collections::HashMap<i64, UttDocView>,
}

/// 默认 LRU 容量上限：50,000 条文档。
///
/// 此值远超 / 典型场景（~10k 文档），仅在长期大量导入场景下才会触发驱逐，
/// 确保常规使用不受影响。
pub const DEFAULT_LRU_MAX_ENTRIES: usize = 50_000;

impl Retriever {
    /// 使用默认配置创建检索器。
    pub fn new() -> Self {
        Self {
            config: RetrieverConfig::default(),
            bm25_index: Bm25Index::new(),
            vector_index: CachedVectorIndex::new(BruteForceIndex::new(), None),
            graph_retriever: GraphRetriever::new(),
            lru_max_entries: DEFAULT_LRU_MAX_ENTRIES,
            l1_docs: std::collections::HashMap::new(),
            l2_docs: std::collections::HashMap::new(),
            utt_docs: std::collections::HashMap::new(),
        }
    }

    /// 使用自定义配置创建检索器。
    pub fn with_config(config: RetrieverConfig) -> Self {
        Self {
            config,
            bm25_index: Bm25Index::new(),
            vector_index: CachedVectorIndex::new(BruteForceIndex::new(), None),
            graph_retriever: GraphRetriever::new(),
            lru_max_entries: DEFAULT_LRU_MAX_ENTRIES,
            l1_docs: std::collections::HashMap::new(),
            l2_docs: std::collections::HashMap::new(),
            utt_docs: std::collections::HashMap::new(),
        }
    }

    /// 设置 LRU 容量上限（0 表示不限制）。
    ///
    /// 当 L1+L2 总文档数超过此值时，按 `created_at` 驱逐最早创建的文档。
    /// 默认值: [`DEFAULT_LRU_MAX_ENTRIES`]（50,000）。
    pub fn set_lru_max_entries(&mut self, max_entries: usize) {
        self.lru_max_entries = max_entries;
    }

    /// 获取当前 LRU 容量上限。
    pub fn lru_max_entries(&self) -> usize {
        self.lru_max_entries
    }

    /// 获取内部 BM25 索引的可变引用。
    pub fn bm25_mut(&mut self) -> &mut Bm25Index {
        &mut self.bm25_index
    }

    /// 获取内部向量索引的可变引用。
    pub fn vector_mut(&mut self) -> &mut CachedVectorIndex<BruteForceIndex> {
        &mut self.vector_index
    }

    /// 获取内部图谱检索器的可变引用。
    pub fn graph_mut(&mut self) -> &mut GraphRetriever {
        &mut self.graph_retriever
    }

    /// 获取检索配置的引用。
    pub fn config(&self) -> &RetrieverConfig {
        &self.config
    }

    /// 获取检索配置的可变引用。
    pub fn config_mut(&mut self) -> &mut RetrieverConfig {
        &mut self.config
    }

    /// 设置是否启用 BM25 通道，返回修改前的值。
    ///
    /// 供重建流程在 `RebuildConfig.rebuild_bm25=false` 时临时禁用 BM25 索引
    /// 构建（仅加载文档映射），重建结束后恢复原值。
    pub fn set_bm25_enabled(&mut self, enabled: bool) -> bool {
        let prev = self.config.enable_bm25;
        self.config.enable_bm25 = enabled;
        prev
    }
}

impl Default for Retriever {
    fn default() -> Self {
        Self::new()
    }
}
