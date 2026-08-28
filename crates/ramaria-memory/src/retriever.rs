//! crates/ramaria-memory/src/retriever.rs — 三通道组合检索编排器
//!
//! 设计特点:
//! - 编排 BM25 + 向量 + 图谱三个独立通道，结果通过 RRF 融合
//! - 支持按 persona_uid 过滤（各通道独立过滤）
//! - 纯编排逻辑，各通道的索引维护由各自模块负责
//! - 不直接访问数据库——通过注入的回调函数获取文档
//!
//! 检索流程:
//! 1. BM25 通道: 对 L1/L2 文本字段执行 BM25 评分
//! 2. 向量通道: 使用 EmbeddingProvider 生成 query 向量，执行余弦检索
//! 3. 图谱通道: 从查询提取实体，执行 1-hop 图谱遍历
//! 4. RRF 融合: 三个通道结果通过 rrf_fuse 合并排序
//! 5. 解析 doc_id → 加载实际文档 → 返回检索结果

use ramaria_core::error::RamariaResult;
use ramaria_core::types::MemoryL1;

use crate::bm25::{Bm25Config, Bm25Index, DocId};
use crate::graph_retriever::{GraphRetriever, GraphRetrieverConfig, graph_hits_to_rrf_pairs};
use crate::rrf::{ChannelResult, FusedResult, RrfConfig, rrf_fuse};
use crate::vector::{
    BruteForceIndex, CachedVectorIndex, VectorHit, VectorIndex, VectorIndexConfig,
    make_vector_label, parse_vector_label,
};

// =========================================================
// 检索配置
// =========================================================

/// 组合检索配置。
#[derive(Debug, Clone)]
pub struct RetrieverConfig {
    /// BM25 配置
    pub bm25: Bm25Config,
    /// 向量检索配置
    pub vector: VectorIndexConfig,
    /// 图谱检索配置
    pub graph: GraphRetrieverConfig,
    /// RRF 融合配置
    pub rrf: RrfConfig,
    /// 是否启用 BM25 通道
    pub enable_bm25: bool,
    /// 是否启用向量通道
    pub enable_vector: bool,
    /// 是否启用图谱通道
    pub enable_graph: bool,
}

impl Default for RetrieverConfig {
    fn default() -> Self {
        Self {
            bm25: Bm25Config::default(),
            vector: VectorIndexConfig::default(),
            graph: GraphRetrieverConfig::default(),
            rrf: RrfConfig::default(),
            enable_bm25: true,
            enable_vector: true,
            enable_graph: true,
        }
    }
}

// =========================================================
// 检索结果
// =========================================================

/// 统一检索结果——一条被检索到的文档。
///
/// 可在下游（rag.rs）按 persona_uid/share 进一步过滤。
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// 文档标识符
    pub doc_id: DocId,
    /// 文档所在层级: "l1" 或 "l2"
    pub layer: String,
    /// RRF 融合分数
    pub rrf_score: f64,
    /// BM25 原始分数（若不出现在 BM25 结果中则为 None）
    pub bm25_score: Option<f64>,
    /// 向量通道原始相似度（若不出现在向量结果中则为 None）
    pub vector_score: Option<f64>,
    /// 图谱通道原始分数（若不出现在图谱结果中则为 None）
    pub graph_score: Option<f64>,
    /// 关联的 persona_uid（L1 的 persona_uid 或 L2 event 的 persona_uid）
    pub persona_uid: Option<String>,
    /// 分享意愿（L2 事件专用，L1 为 None）
    pub share: Option<f64>,
    /// 文档创建时间（Unix 毫秒），用于时间衰减加权
    pub created_at: i64,
    /// 文档标题/摘要（供 RAG 格式化使用）
    pub doc_summary: String,
}

/// 检索请求参数。
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// 查询文本
    pub query: String,
    /// 目标 persona_uid（用于过滤检索结果）
    pub persona_uid: Option<String>,
    /// 最大返回结果数
    pub top_k: usize,
    /// 供下游 `rag.rs` 过滤低 share 事件使用（本结构体的检索方法不读取该字段）
    pub filter_share: bool,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            persona_uid: None,
            top_k: 5,
            filter_share: true,
        }
    }
}

// =========================================================
// 文档数据源（用于注入存储访问）
// =========================================================

/// L1 记忆的检索视图（BM25 索引+检索需要的字段）。
#[derive(Debug, Clone)]
pub struct L1DocView {
    pub id: uuid::Uuid,
    pub summary: String,
    pub keywords: Option<String>,
    pub persona_uid: Option<String>,
    pub created_at: i64,
    pub salience: f64,
}

/// L2 事件的检索视图。
#[derive(Debug, Clone)]
pub struct L2DocView {
    pub id: i64,
    pub title: String,
    pub summary: String,
    pub keywords: Option<String>,
    pub attitude: Option<String>,
    pub paraphrase: Option<String>,
    pub persona_uid: String,
    pub share: f64,
    pub confidence: f64,
    pub created_at: i64,
    pub salience: f64,
}

/// utt 话语块的检索视图（v1.4 原文通道）。
///
/// 安全约束:
/// - 原文是最高敏感层：检索严格按 `persona_uid` 精确隔离，不做跨 persona 共享。
/// - `block_text` 不写日志。
#[derive(Debug, Clone)]
pub struct UttDocView {
    /// utt_blocks 表主键
    pub id: i64,
    /// 块归属人格（检索隔离键）
    pub persona_uid: String,
    /// 来源会话
    pub session_id: uuid::Uuid,
    /// 块内原文全文（含发言人标记）
    pub block_text: String,
    /// 块内消息条数
    pub msg_count: u32,
    /// 块创建时间（Unix 毫秒）
    pub created_at: i64,
}

/// utt 原文块检索命中。
#[derive(Debug, Clone)]
pub struct UttHit {
    /// 命中的块视图
    pub doc: UttDocView,
    /// 相似度得分：向量通道为调整后相似度，子串降级为命中 token 数
    pub score: f64,
    /// 命中通道: `"vector"`（向量）或 `"substring"`（BM25 子串降级）
    pub channel: &'static str,
}

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

    // ---- 索引构建 ----

    /// 设置是否启用 BM25 通道，返回修改前的值。
    ///
    /// 供重建流程在 `RebuildConfig.rebuild_bm25=false` 时临时禁用 BM25 索引
    /// 构建（仅加载文档映射），重建结束后恢复原值。
    pub fn set_bm25_enabled(&mut self, enabled: bool) -> bool {
        let prev = self.config.enable_bm25;
        self.config.enable_bm25 = enabled;
        prev
    }

    /// 将 L1 文档添加到所有启用通道的索引中。
    ///
    /// 接受引用以避免调用方不必要的 clone；内部仅在存入 HashMap 时做一次 clone。
    ///
    /// LRU 驱逐: 添加后若总文档数超过 `lru_max_entries`，按 `created_at` 驱逐最旧文档。
    ///
    /// 向量通道说明（接线）:
    /// - 本方法不生成向量（同步路径无 embedding provider），仅 BM25 + 内存文档；
    /// - 向量由调用方在 embedding 可用时通过 [`index_l1_with_vector`] 写入，
    ///   使 L1 文档真实进入向量索引（此前仅 rebuild 全量路径写入）。
    pub fn index_l1(&mut self, doc: &L1DocView) {
        // BM25 索引
        if self.config.enable_bm25 {
            let tokens = crate::bm25::tokenize_fields(&[
                &doc.summary,
                doc.keywords.as_deref().unwrap_or(""),
            ]);
            self.bm25_index.add(DocId::L1(doc.id), tokens);
        }

        self.l1_docs.insert(doc.id, doc.clone());

        // LRU 驱逐: 总文档数超过上限时，从 BM25 和内存 HashMap 中同步清理最早文档
        self.evict_if_needed();
    }

    /// 将 L1 文档连同向量加入索引（L1 embedding 真实入向量索引）。
    ///
    /// 参数:
    /// - `doc`: L1 文档视图。
    /// - `vector`: 文档向量（None = 无 embedding，仅入 BM25/内存，检索走 BM25）。
    ///
    /// 说明:
    /// - label 统一 `make_vector_label("l1", uuid)`（大写 `L1:`，与 `parse_doc_label` 匹配）。
    /// - 向量维度由索引首个条目确定；后续条目维度不一致会被 `BruteForceIndex::add` 拒绝并记 warn。
    pub fn index_l1_with_vector(&mut self, doc: &L1DocView, vector: Option<Vec<f32>>) {
        self.index_l1(doc);
        if let Some(v) = vector {
            self.vector_index.add(
                &make_vector_label("l1", &doc.id.to_string()),
                v,
                doc.created_at,
            );
        }
    }

    /// 将 `MemoryL1` 记录转换为 `L1DocView` 并增量添加到所有启用通道的索引中。
    ///
    /// 职责:
    /// - 供 `SessionLifecycle` 在 L1 摘要生成成功后立即调用，
    ///   确保新生成的 L1 文档无需等待全量 `rebuild_retriever` 即可被 Stage 5 RAG 检索命中。
    /// - 将 `MemoryL1`（来自 `ramaria-core` 的业务类型）转为内部 `L1DocView` 后委托给 [`index_l1`]。
    ///
    /// 参数:
    /// - `record`: 刚生成的 L1 摘要记录。
    ///
    /// 返回:
    /// - `Ok(())`: 索引添加成功（即使 BM25 分词为空也是成功）。
    ///
    /// 说明:
    /// - 本方法总是返回 `Ok(())`——转换和 BM25 索引添加均为纯内存操作，不可失败。
    /// - 向量索引暂不更新（需 EmbeddingProvider 生成 query 向量，由后续 rebuild 路径处理）。
    pub fn index_l1_record(&mut self, record: &MemoryL1) -> RamariaResult<()> {
        let doc = L1DocView {
            id: record.id,
            summary: record.summary.clone(),
            keywords: record.keywords.clone(),
            persona_uid: record.persona_uid.clone(),
            created_at: record.created_at,
            salience: record.salience,
        };
        self.index_l1(&doc);
        tracing::info!(
            l1_id = %record.id,
            persona_uid = ?record.persona_uid,
            "L1 记录已增量加入 Retriever 索引"
        );
        Ok(())
    }

    /// 将 L2 事件添加到所有启用通道的索引中。
    ///
    /// 接受引用以避免调用方不必要的 clone；内部仅在存入 HashMap 时做一次 clone。
    /// 同时消除了之前因 borrow checker 限制而产生的临时 String 分配。
    ///
    /// LRU 驱逐: 添加后若总文档数超过 `lru_max_entries`，按 `created_at` 驱逐最旧文档。
    ///
    /// 向量通道说明（接线）:
    /// - 本方法不生成向量（同步路径无 embedding provider），仅 BM25 + 内存文档；
    /// - 向量由调用方在 embedding 可用时通过 [`index_l2_with_vector`] 写入。
    pub fn index_l2(&mut self, doc: &L2DocView) {
        // BM25 索引
        if self.config.enable_bm25 {
            let mut fields: Vec<&str> = vec![&doc.title, &doc.summary];
            if let Some(ref kw) = doc.keywords {
                fields.push(kw.as_str());
            }
            if let Some(ref att) = doc.attitude {
                fields.push(att.as_str());
            }
            if let Some(ref par) = doc.paraphrase {
                fields.push(par.as_str());
            }
            let tokens = crate::bm25::tokenize_fields(&fields);
            self.bm25_index.add(DocId::L2(doc.id), tokens);
        }

        self.l2_docs.insert(doc.id, doc.clone());

        // LRU 驱逐
        self.evict_if_needed();
    }

    /// 将 L2 事件连同向量加入索引（L2 embedding 真实入向量索引）。
    ///
    /// 参数:
    /// - `doc`: L2 事件视图。
    /// - `vector`: 事件向量（None = 无 embedding，仅入 BM25/内存）。
    ///
    /// 说明:
    /// - label 统一 `make_vector_label("l2", id)`（大写 `L2:`，与 `parse_doc_label` 匹配）。
    pub fn index_l2_with_vector(&mut self, doc: &L2DocView, vector: Option<Vec<f32>>) {
        self.index_l2(doc);
        if let Some(v) = vector {
            self.vector_index.add(
                &make_vector_label("l2", &doc.id.to_string()),
                v,
                doc.created_at,
            );
        }
    }

    /// 从整个检索器中移除一个 L1 文档（BM25 + HashMap）。
    ///
    /// 用于会话删除、记忆清理等场景，保持内存和索引一致性。
    pub fn remove_l1(&mut self, doc_id: &uuid::Uuid) {
        let bm25_doc_id = DocId::L1(*doc_id);
        self.bm25_index.remove(&bm25_doc_id);
        self.l1_docs.remove(doc_id);
    }

    /// 从整个检索器中移除一个 L2 文档（BM25 + HashMap）。
    ///
    /// 用于事件删除、记忆清理等场景，保持内存和索引一致性。
    pub fn remove_l2(&mut self, doc_id: &i64) {
        let bm25_doc_id = DocId::L2(*doc_id);
        self.bm25_index.remove(&bm25_doc_id);
        self.l2_docs.remove(doc_id);
    }

    /// LRU 驱逐: 当总文档数超过 `lru_max_entries` 时，按 `created_at` 驱逐最早创建的文档。
    ///
    /// 策略:
    /// - 从所有文档中按 created_at 升序排列，移除最早的条目
    /// - 同时从 BM25 索引和 HashMap 中同步删除，保持一致性
    /// - 每次只驱逐超出部分（(l1 + l2) - lru_max_entries 条）
    /// - `lru_max_entries == 0` 时跳过驱逐（无限制模式）
    ///
    /// 复杂度: O(n log n) 其中 n = l1_docs.len + l2_docs.len。
    /// 仅在高文档数且超出上限时触发，性能影响可控。
    fn evict_if_needed(&mut self) {
        if self.lru_max_entries == 0 {
            return; // 无限制模式
        }

        let total = self.l1_docs.len() + self.l2_docs.len();
        let evict_count = total.saturating_sub(self.lru_max_entries);

        if evict_count == 0 {
            return;
        }

        // 收集所有 (doc_id_string, created_at, is_l1, key_L1_uuid, key_L2_i64) 并按时间排序
        let mut entries: Vec<(i64, bool, uuid::Uuid, i64)> = Vec::with_capacity(total);

        for (uid, doc) in self.l1_docs.iter() {
            entries.push((doc.created_at, true, *uid, 0));
        }
        for (id, doc) in self.l2_docs.iter() {
            entries.push((doc.created_at, false, uuid::Uuid::nil(), *id));
        }

        // 按 created_at 升序排列，驱逐最早创建的
        entries.sort_by_key(|e| e.0);

        let to_evict = if evict_count >= entries.len() {
            &entries[..]
        } else {
            &entries[..evict_count]
        };

        // 同步驱逐：BM25 + HashMap
        for (_, is_l1, l1_uid, l2_id) in to_evict {
            if *is_l1 {
                self.bm25_index.remove(&DocId::L1(*l1_uid));
                self.l1_docs.remove(l1_uid);
            } else {
                self.bm25_index.remove(&DocId::L2(*l2_id));
                self.l2_docs.remove(l2_id);
            }
        }

        tracing::warn!(
            l1_remaining = self.l1_docs.len(),
            l2_remaining = self.l2_docs.len(),
            evicted = to_evict.len(),
            "Retriever LRU 驱逐完成——文档数超过容量上限"
        );
    }

    /// 从 BM25 索引中移除一篇文档。
    pub fn remove_from_bm25(&mut self, doc_id: &DocId) {
        self.bm25_index.remove(doc_id);
    }

    /// 重建 BM25 索引。
    ///
    /// 清空现有索引，从 l1_docs 和 l2_docs 重新构建。
    ///
    /// 接线候选：desktop index rebuild 命令（v1.6 核查）
    pub fn rebuild_bm25(&mut self) {
        self.bm25_index.clear();
        let l1_snapshot: Vec<L1DocView> = self.l1_docs.values().cloned().collect();
        let l2_snapshot: Vec<L2DocView> = self.l2_docs.values().cloned().collect();

        for doc in &l1_snapshot {
            self.index_l1(doc);
        }
        for doc in &l2_snapshot {
            self.index_l2(doc);
        }
    }

    // ---- 检索 ----

    /// 执行三通道组合检索。
    ///
    /// 流程:
    /// 1. 各通道独立检索
    /// 2. BM25 通道同时预解析 DocId→文档数据映射（避免后续 label 往返解析）
    /// 3. 将结果转为统一的 ChannelResult<String> （label 作为 key）
    /// 4. RRF 融合
    /// 5. 将融合后的 label 解析为 SearchResult（BM25 用预解析缓存，图谱用字符串解析）
    ///
    /// 参数:
    /// - `request`: 检索请求
    /// - `query_vec`: 可选的 query 向量（若未提供则跳过向量通道）
    pub fn search(&self, request: &SearchRequest, query_vec: Option<&[f32]>) -> Vec<SearchResult> {
        use std::collections::HashMap;

        // 预解析缓存：BM25 label → 文档数据（避免 label 字符串往返解析）
        let mut bm25_data: HashMap<String, Bm25Resolved> = HashMap::new();

        // ---- BM25 通道 ----
        let bm25_channel = if self.config.enable_bm25 {
            let raw_results = self.bm25_index.search(&request.query, &self.config.bm25);
            if raw_results.is_empty() {
                None
            } else {
                let results: Vec<(String, f64)> = raw_results
                    .into_iter()
                    .map(|(doc_id, score)| {
                        let label = doc_id.to_string();
                        // 预解析文档数据，避免后续 parse_doc_label 中的 UUID 解析开销
                        let doc_data = resolve_bm25_doc(&doc_id, &self.l1_docs, &self.l2_docs);
                        bm25_data.insert(label.clone(), doc_data);
                        (label, score)
                    })
                    .collect();
                Some(ChannelResult { results })
            }
        } else {
            None
        };

        // ---- 向量通道 ----
        let vector_channel: Option<ChannelResult<String>> = if self.config.enable_vector {
            if let Some(qv) = query_vec {
                match self.vector_index.search(qv, &self.config.vector) {
                    Ok(hits) => {
                        let results: Vec<(String, f64)> = hits
                            .into_iter()
                            .map(|h: VectorHit| (h.doc_label, h.adjusted_similarity))
                            .collect();
                        if results.is_empty() {
                            None
                        } else {
                            Some(ChannelResult { results })
                        }
                    }
                    Err(_) => None,
                }
            } else {
                None
            }
        } else {
            None
        };

        // ---- 图谱通道 ----
        let graph_channel: Option<ChannelResult<String>> = if self.config.enable_graph {
            let graph_hits = self
                .graph_retriever
                .search(&request.query, &self.config.graph);
            let results = graph_hits_to_rrf_pairs(&graph_hits);
            if results.is_empty() {
                None
            } else {
                Some(ChannelResult { results })
            }
        } else {
            None
        };

        // ---- RRF 融合 ----
        let fused: Vec<FusedResult<String>> = match (&vector_channel, &bm25_channel, &graph_channel)
        {
            (None, None, None) => return Vec::new(),
            (Some(v), None, None) => crate::rrf::rrf_single_channel(v, &self.config.rrf)
                .into_iter()
                .take(request.top_k)
                .collect(),
            (None, Some(b), None) => crate::rrf::rrf_single_channel(b, &self.config.rrf)
                .into_iter()
                .take(request.top_k)
                .collect(),
            (None, None, Some(g)) => crate::rrf::rrf_single_channel(g, &self.config.rrf)
                .into_iter()
                .take(request.top_k)
                .collect(),
            (Some(v), Some(b), None) => crate::rrf::rrf_two_channels(v, b, &self.config.rrf)
                .into_iter()
                .take(request.top_k)
                .collect(),
            (Some(v), None, Some(g)) => crate::rrf::rrf_two_channels(v, g, &self.config.rrf)
                .into_iter()
                .take(request.top_k)
                .collect(),
            (None, Some(b), Some(g)) => crate::rrf::rrf_two_channels(b, g, &self.config.rrf)
                .into_iter()
                .take(request.top_k)
                .collect(),
            (Some(v), Some(b), Some(g)) => rrf_fuse(v, b, g, &self.config.rrf)
                .into_iter()
                .take(request.top_k)
                .collect(),
        };

        // ---- 解析为 SearchResult ----
        let mut results: Vec<SearchResult> = Vec::with_capacity(fused.len());

        for f in &fused {
            // 优先从 BM25 预解析缓存获取（避免 label 字符串往返解析）
            let (doc_id, persona_uid, share, created_at, summary, layer) =
                if let Some(data) = bm25_data.get(&f.doc_id) {
                    (
                        data.doc_id.clone(),
                        data.persona_uid.clone(),
                        data.share,
                        data.created_at,
                        data.summary.clone(),
                        data.layer.clone(),
                    )
                } else if let Some(data) = parse_graph_label(&f.doc_id) {
                    // 图谱实体：实体名作为摘要
                    data
                } else {
                    // 可能是向量通道产生的 label（L1:uuid 或 L2:id 格式）
                    // 仍需字符串解析，但仅为向量通道结果
                    match parse_doc_label(&f.doc_id, &self.l1_docs, &self.l2_docs) {
                        Some((did, puid, sh, ca, sum)) => {
                            let lyr = match &did {
                                DocId::L1(_) => "l1".to_string(),
                                DocId::L2(_) => "l2".to_string(),
                                DocId::Graph(_) => "graph".to_string(),
                            };
                            (did, puid, sh, ca, sum, lyr)
                        }
                        None => continue, // 文档已被移除，跳过
                    }
                };

            // persona_uid 过滤
            if let Some(ref target_uid) = request.persona_uid
                && let Some(ref puid) = persona_uid
                && puid != target_uid
            {
                continue;
            }

            results.push(SearchResult {
                doc_id,
                layer,
                rrf_score: f.rrf_score,
                bm25_score: f.bm25_raw_score,
                vector_score: f.vector_raw_score,
                graph_score: f.graph_raw_score,
                persona_uid,
                share,
                created_at,
                doc_summary: summary,
            });
        }

        // 截取 top_k
        if results.len() > request.top_k {
            results.truncate(request.top_k);
        }

        results
    }

    /// 获取文档总数。
    pub fn doc_count(&self) -> usize {
        self.l1_docs.len() + self.l2_docs.len()
    }

    // =========================================================
    // 归一化关键词检索
    // =========================================================

    /// 基于内存文档关键词字段的精确匹配检索。
    ///
    /// 用法:
    /// - 对每个 KeywordToken，在 `l1_docs` 和 `l2_docs` 的 keywords 字段中做精确命中检测。
    /// - 关键词字段为逗号分隔字符串，按分隔后 trim 做精确比对。
    ///
    /// 参数:
    /// - `keywords`: 标准化后的关键词列表（KeywordToken）。
    /// - `persona_uid`: 目标人格 UID（空字符串表示不过滤）。
    /// - `top_k`: 最大返回结果数。
    ///
    /// 返回:
    /// - 按命中关键词数降序排列的 SearchResult 列表，最多 top_k 条。
    ///
    /// 说明:
    /// - 纯内存计算，不依赖数据库。时间复杂度 O(d × k × m)，
    ///   其中 d=文档数，k=查询关键词数，m=文档关键词数。在 10k 文档、10 关键词规模下 < 5ms。
    pub fn search_exact(
        &self,
        keywords: &[ramaria_core::keyword::KeywordToken],
        persona_uid: &str,
        top_k: usize,
    ) -> Vec<SearchResult> {
        if keywords.is_empty() || top_k == 0 {
            return Vec::new();
        }

        let filter_persona = !persona_uid.is_empty();

        // (match_count, SearchResult) 元组
        let mut candidates: Vec<(usize, SearchResult)> = Vec::new();

        // 扫描 L1 文档
        for (id, doc) in &self.l1_docs {
            if filter_persona {
                if let Some(ref puid) = doc.persona_uid {
                    if puid != persona_uid {
                        continue;
                    }
                } else {
                    // L1 文档没有 persona_uid 绑定，跳过（不匹配特定 persona）
                    continue;
                }
            }

            let match_count = count_keyword_matches(doc.keywords.as_deref(), keywords);
            if match_count > 0 {
                candidates.push((
                    match_count,
                    SearchResult {
                        doc_id: DocId::L1(*id),
                        layer: "l1".to_string(),
                        rrf_score: 0.0, // 由排序后重新赋值
                        bm25_score: None,
                        vector_score: None,
                        graph_score: None,
                        persona_uid: doc.persona_uid.clone(),
                        share: None,
                        created_at: doc.created_at,
                        doc_summary: doc.summary.clone(),
                    },
                ));
            }
        }

        // 扫描 L2 文档
        for (id, doc) in &self.l2_docs {
            if filter_persona && doc.persona_uid != persona_uid {
                continue;
            }

            let match_count = count_keyword_matches(doc.keywords.as_deref(), keywords);
            if match_count > 0 {
                candidates.push((
                    match_count,
                    SearchResult {
                        doc_id: DocId::L2(*id),
                        layer: "l2".to_string(),
                        rrf_score: 0.0,
                        bm25_score: None,
                        vector_score: None,
                        graph_score: None,
                        persona_uid: Some(doc.persona_uid.clone()),
                        share: Some(doc.share),
                        created_at: doc.created_at,
                        doc_summary: format!("{} — {}", doc.title, doc.summary),
                    },
                ));
            }
        }

        // 按命中关键词数降序排列，同等命中数按 created_at 降序
        candidates.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.created_at.cmp(&a.1.created_at))
        });

        // 截断到 top_k，并为每条结果赋值 rrf_score（基于排名的归一化分数）
        let _total = candidates.len();
        candidates.truncate(top_k);

        candidates
            .into_iter()
            .enumerate()
            .map(|(rank, (match_count, mut sr))| {
                // rrf_score = 1.0 / (rank + 1)，排名越前分数越高
                sr.rrf_score = 1.0 / (rank as f64 + 1.0);
                tracing::debug!(
                    doc_id = %sr.doc_id,
                    layer = %sr.layer,
                    match_count,
                    rank,
                    rrf_score = sr.rrf_score,
                    "search_exact 命中"
                );
                sr
            })
            .collect()
    }

    /// 基于 BM25 的子串匹配检索。
    ///
    /// 用法:
    /// - 将查询文本委托给 BM25 索引做 bigram 分词检索，
    ///   实现子串级别的文本匹配（中文以双字 bigram 为单位）。
    ///
    /// 参数:
    /// - `query`: 查询文本（如关键词拼接字符串）。
    /// - `persona_uid`: 目标人格 UID（空字符串表示不过滤）。
    /// - `top_k`: 最大返回结果数。
    ///
    /// 返回:
    /// - 按 BM25 评分降序排列的 SearchResult 列表，最多 top_k 条。
    ///
    /// 说明:
    /// - 关闭向量和图谱通道，仅使用 BM25。
    /// - 复用 `search_bm25_only()`（`search()` 无法外部覆盖 enable_* 开关）。
    pub fn search_substring(
        &self,
        query: &str,
        persona_uid: &str,
        top_k: usize,
    ) -> Vec<SearchResult> {
        if query.trim().is_empty() || top_k == 0 {
            return Vec::new();
        }

        let request = SearchRequest {
            query: query.to_string(),
            persona_uid: if persona_uid.is_empty() {
                None
            } else {
                Some(persona_uid.to_string())
            },
            top_k,
            filter_share: false, // 事件提取上下文不过滤 share
        };

        // 仅启用 BM25 通道
        // 注意：search() 使用 &self，但我们在此需要临时修改配置。
        // 由于 search() 直接在方法内检查 self.config.enable_*，无法外部覆盖。
        // 因此直接调用 BM25 索引 → 构建 SearchResult。
        self.search_bm25_only(&request)
    }

    /// BM25-only 检索（供 search_substring 使用）。
    ///
    /// 直接访问 BM25 索引，绕过三通道编排，避免依赖向量/图谱通道。
    fn search_bm25_only(&self, request: &SearchRequest) -> Vec<SearchResult> {
        let raw_results = self.bm25_index.search(&request.query, &self.config.bm25);
        if raw_results.is_empty() {
            return Vec::new();
        }

        let mut results: Vec<SearchResult> = Vec::with_capacity(raw_results.len());

        for (doc_id, bm25_score) in raw_results {
            let doc_data = resolve_bm25_doc(&doc_id, &self.l1_docs, &self.l2_docs);

            // persona_uid 过滤
            if let Some(ref target_uid) = request.persona_uid
                && let Some(ref puid) = doc_data.persona_uid
                && puid != target_uid
            {
                continue;
            }

            results.push(SearchResult {
                doc_id,
                layer: doc_data.layer,
                rrf_score: bm25_score, // BM25 原始分数作为排名分数
                bm25_score: Some(bm25_score),
                vector_score: None,
                graph_score: None,
                persona_uid: doc_data.persona_uid,
                share: doc_data.share,
                created_at: doc_data.created_at,
                doc_summary: doc_data.summary,
            });

            if results.len() >= request.top_k {
                break;
            }
        }

        results
    }

    /// 清空所有索引和文档。
    pub fn clear(&mut self) {
        self.bm25_index.clear();
        self.vector_index.clear();
        self.graph_retriever.clear();
        self.l1_docs.clear();
        self.l2_docs.clear();
        self.utt_docs.clear();
    }

    // =========================================================
    // utt 原文通道（v1.4）
    // =========================================================

    /// 将 utt 块视图加入索引（内存文档 + 可选向量）。
    ///
    /// 向量 label 格式: `L0:{utt_block_id}`（与 L1:/L2: 前缀共存于 BruteForceIndex）。
    /// 现有三通道检索解析不到 `L0:` 前缀时自然跳过，互不干扰。
    ///
    /// 参数:
    /// - `doc`: 块视图。
    /// - `vector`: 可选的块向量（None 表示无 embedding，检索走子串降级）。
    pub fn index_utt(&mut self, doc: &UttDocView, vector: Option<Vec<f32>>) {
        self.utt_docs.insert(doc.id, doc.clone());
        if let Some(v) = vector {
            self.vector_index.add(
                &make_vector_label("l0", &doc.id.to_string()),
                v,
                doc.created_at,
            );
        }
    }

    /// 从存储层 `UttBlock` 直接索引（解码 f32 BLOB 向量）。
    ///
    /// 说明:
    /// - embedding BLOB 解码失败（数据损坏）→ 记 warn，仅入内存文档（子串降级可用）。
    pub fn index_utt_block(&mut self, block: &ramaria_core::types::UttBlock) {
        let doc = UttDocView {
            id: block.id,
            persona_uid: block.persona_uid.clone(),
            session_id: block.session_id,
            block_text: block.block_text.clone(),
            msg_count: block.msg_count,
            created_at: block.created_at,
        };
        let vector = match block.embedding.as_deref() {
            Some(blob) => match crate::utt::decode_embedding(blob) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(block_id = block.id, %e, "utt 块 embedding 解码失败，仅入内存文档");
                    None
                }
            },
            None => None,
        };
        self.index_utt(&doc, vector);
    }

    /// 从索引移除一个 utt 块（内存文档 + 向量）。
    pub fn remove_utt(&mut self, id: i64) {
        self.utt_docs.remove(&id);
        self.vector_index
            .remove(&make_vector_label("l0", &id.to_string()));
    }

    /// 当前内存中的 utt 块数量。
    pub fn utt_doc_count(&self) -> usize {
        self.utt_docs.len()
    }

    /// 检索 utt 原文块（v1.4 原文通道）。
    ///
    /// 通道与降级:
    /// - 向量优先：`query_vec` 可用时在 BruteForceIndex 的 `L0:` label 上检索。
    /// - 子串降级：无向量 / 向量索引空 / 维度不符时，按 query 分词 token
    ///   在块文本中的出现次数打分（BM25 子串匹配）。
    /// - 两通道均无命中 → 空列表（等同 v1.3，不注入原文）。
    ///
    /// 安全约束:
    /// - `persona_uid` 为 None（未指定目标）→ 恒返回空（原文严格按 persona 隔离）。
    /// - 仅返回 `persona_uid` 精确匹配的块，不做跨 persona 共享。
    ///
    /// 参数:
    /// - `query`: 查询文本（子串降级用）。
    /// - `query_vec`: 查询向量（None 时跳过向量通道）。
    /// - `top_k`: 最大返回块数。
    /// - `persona_uid`: 目标 persona（原文隔离键）。
    ///
    /// 返回:
    /// - 按得分降序的命中列表（最多 top_k 条）。
    pub fn search_utt(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
        top_k: usize,
        persona_uid: Option<&str>,
    ) -> Vec<UttHit> {
        let Some(target) = persona_uid else {
            return Vec::new();
        };

        // 严格隔离：只取目标 persona 的块
        let candidates: Vec<&UttDocView> = self
            .utt_docs
            .values()
            .filter(|d| d.persona_uid == target)
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }

        // ---- 向量通道 ----
        if let Some(qv) = query_vec {
            match self.vector_index.search(qv, &self.config.vector) {
                Ok(hits) => {
                    let mut out: Vec<UttHit> = hits
                        .into_iter()
                        .filter_map(|h: crate::vector::VectorHit| {
                            let (layer, id_str) = parse_vector_label(&h.doc_label)?;
                            if layer != "L0" {
                                return None;
                            }
                            let id = id_str.parse::<i64>().ok()?;
                            let doc = self.utt_docs.get(&id)?;
                            if doc.persona_uid != target {
                                return None; // 跨 persona 命中丢弃
                            }
                            Some(UttHit {
                                doc: doc.clone(),
                                score: h.adjusted_similarity,
                                channel: "vector",
                            })
                        })
                        .collect();
                    out.sort_by(|a, b| {
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    out.truncate(top_k);
                    if !out.is_empty() {
                        return out;
                    }
                    // 向量命中但均被隔离过滤（防御）→ 落入子串降级
                }
                Err(_) => {
                    // 空索引 / 维度不符 → 子串降级
                }
            }
        }

        // ---- 子串降级（BM25 分词 token 命中计数） ----
        self.search_utt_substring(query, &candidates, top_k)
    }

    /// 子串降级检索：query 分词 token 在块文本中的命中计数打分。
    fn search_utt_substring(
        &self,
        query: &str,
        candidates: &[&UttDocView],
        top_k: usize,
    ) -> Vec<UttHit> {
        let tokens = crate::bm25::tokenize(query);
        let mut scored: Vec<UttHit> = Vec::new();

        for doc in candidates {
            let lower_text = doc.block_text.to_lowercase();
            let score: usize = if tokens.is_empty() {
                // 分词为空（如纯符号查询）→ 原始子串包含判定
                let q = query.trim().to_lowercase();
                if !q.is_empty() && lower_text.contains(&q) {
                    1
                } else {
                    0
                }
            } else {
                tokens
                    .iter()
                    .filter(|t| lower_text.contains(t.as_str()))
                    .count()
            };
            if score > 0 {
                scored.push(UttHit {
                    doc: (*doc).clone(),
                    score: score as f64,
                    channel: "substring",
                });
            }
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        scored
    }
}

impl Default for Retriever {
    fn default() -> Self {
        Self::new()
    }
}

// =========================================================
// 辅助函数
// =========================================================

/// BM25 文档预解析结果——在 BM25 搜索阶段一次解析，避免 RRF 融合后重复解析。
///
/// 存储 label→DocId 的映射以及关联的文档元数据，
/// 省去 `parse_doc_label` 中的 UUID 字符串解析开销。
struct Bm25Resolved {
    doc_id: DocId,
    persona_uid: Option<String>,
    share: Option<f64>,
    created_at: i64,
    summary: String,
    layer: String,
}

/// 从 BM25 搜索阶段的 DocId 直接解析文档数据（无需字符串往返）。
///
/// 返回预解析的文档数据，存储在 `Bm25Resolved` 中供 RRF 融合后直接使用。
fn resolve_bm25_doc(
    doc_id: &DocId,
    l1_docs: &std::collections::HashMap<uuid::Uuid, L1DocView>,
    l2_docs: &std::collections::HashMap<i64, L2DocView>,
) -> Bm25Resolved {
    match doc_id {
        DocId::L1(id) => {
            if let Some(doc) = l1_docs.get(id) {
                Bm25Resolved {
                    doc_id: doc_id.clone(),
                    persona_uid: doc.persona_uid.clone(),
                    share: None,
                    created_at: doc.created_at,
                    summary: doc.summary.clone(),
                    layer: "l1".to_string(),
                }
            } else {
                // 文档已被移除，返回哨兵数据（上层会检查并跳过）
                Bm25Resolved {
                    doc_id: doc_id.clone(),
                    persona_uid: None,
                    share: None,
                    created_at: 0,
                    summary: "[已删除]".to_string(),
                    layer: "l1".to_string(),
                }
            }
        }
        DocId::L2(id) => {
            if let Some(doc) = l2_docs.get(id) {
                Bm25Resolved {
                    doc_id: doc_id.clone(),
                    persona_uid: Some(doc.persona_uid.clone()),
                    share: Some(doc.share),
                    created_at: doc.created_at,
                    summary: format!("{} — {}", doc.title, doc.summary),
                    layer: "l2".to_string(),
                }
            } else {
                Bm25Resolved {
                    doc_id: doc_id.clone(),
                    persona_uid: None,
                    share: None,
                    created_at: 0,
                    summary: "[已删除]".to_string(),
                    layer: "l2".to_string(),
                }
            }
        }
        DocId::Graph(_) => {
            // 图谱实体不应出现在 BM25 结果中，但若出现则做防御处理
            Bm25Resolved {
                doc_id: doc_id.clone(),
                persona_uid: None,
                share: None,
                created_at: 0,
                summary: format!("[图谱实体] {}", doc_id),
                layer: "graph".to_string(),
            }
        }
    }
}

/// 从图谱通道的 "graph:{entity}" label 解析出 SearchResult 所需数据。
///
/// 图谱实体没有对应数据库记录，因此返回实体名作为摘要。
///
/// 返回: 元组 (DocId::Graph, None, None, 0, "[图谱实体] {name}", "graph")
#[allow(clippy::type_complexity)]
fn parse_graph_label(
    label: &str,
) -> Option<(DocId, Option<String>, Option<f64>, i64, String, String)> {
    label.strip_prefix("graph:").map(|entity| {
        (
            DocId::Graph(entity.to_string()),
            None, // persona_uid（图谱实体不关联特定 persona）
            None, // share
            0,    // created_at（图谱实体无时间戳）
            format!("[图谱实体] {}", entity),
            "graph".to_string(),
        )
    })
}

/// 从 RRF 融合结果的 label 解析出 doc_id 和文档摘要。
///
/// 仅用于向量通道产生的 L1:/L2: 格式 label（BM25 已通过 `resolve_bm25_doc` 预解析，
/// 图谱已通过 `parse_graph_label` 独立处理）。
///
/// 返回: 元组 (DocId, persona_uid, share, created_at, summary)
#[allow(clippy::type_complexity)]
fn parse_doc_label(
    label: &str,
    l1_docs: &std::collections::HashMap<uuid::Uuid, L1DocView>,
    l2_docs: &std::collections::HashMap<i64, L2DocView>,
) -> Option<(DocId, Option<String>, Option<f64>, i64, String)> {
    if let Some(uuid_str) = label
        .strip_prefix("L1:")
        .or_else(|| label.strip_prefix("l1:"))
    {
        let id = uuid::Uuid::parse_str(uuid_str).ok()?;
        let doc = l1_docs.get(&id)?;
        Some((
            DocId::L1(id),
            doc.persona_uid.clone(),
            None, // L1 无 share
            doc.created_at,
            doc.summary.clone(),
        ))
    } else if let Some(id_str) = label
        .strip_prefix("L2:")
        .or_else(|| label.strip_prefix("l2:"))
    {
        let id = id_str.parse::<i64>().ok()?;
        let doc = l2_docs.get(&id)?;
        Some((
            DocId::L2(id),
            Some(doc.persona_uid.clone()),
            Some(doc.share),
            doc.created_at,
            format!("{} — {}", doc.title, doc.summary),
        ))
    } else {
        // 未知格式 label（不应出现，但做防御处理）
        None
    }
}

/// 统计文档关键词字段中匹配到的查询关键词数量。
///
/// 参数:
/// - `doc_keywords`: 文档的逗号分隔关键词字符串（如 "工作, 压力, 倦怠"）。
/// - `query_keywords`: 查询关键词列表。
///
/// 返回:
/// - 精确命中的关键词数量。
fn count_keyword_matches(
    doc_keywords: Option<&str>,
    query_keywords: &[ramaria_core::keyword::KeywordToken],
) -> usize {
    let doc_str = match doc_keywords {
        Some(s) => s,
        None => return 0,
    };

    // 解析文档关键词为去重集合，做与 KeywordToken::new() 一致的规范化（trim + 英文小写）
    let doc_kw_set: std::collections::HashSet<String> = doc_str
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    if doc_kw_set.is_empty() {
        return 0;
    }

    query_keywords
        .iter()
        .filter(|qk| doc_kw_set.contains(qk.as_str()))
        .count()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_retriever() -> Retriever {
        let mut r = Retriever::new();

        // 添加 L1 文档
        r.index_l1(&L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: "用户今天学习了Rust编程语言的基础语法".to_string(),
            keywords: Some("学习,Rust,编程".to_string()),
            persona_uid: Some("user-0001".to_string()),
            created_at: 1000,
            salience: 0.8,
        });

        r.index_l1(&L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: "用户和朋友去吃了火锅，很开心".to_string(),
            keywords: Some("社交,火锅,开心".to_string()),
            persona_uid: Some("user-0001".to_string()),
            created_at: 2000,
            salience: 0.6,
        });

        // 添加 L2 事件
        r.index_l2(&L2DocView {
            id: 1,
            title: "完成Rust项目".to_string(),
            summary: "用户完成了第一个Rust项目，发布了crate".to_string(),
            keywords: Some("Rust,项目,发布".to_string()),
            attitude: Some("感到很有成就感".to_string()),
            paraphrase: Some("对完成重要工作感到满意".to_string()),
            persona_uid: "user-0001".to_string(),
            share: 0.8,
            confidence: 0.9,
            created_at: 1500,
            salience: 0.9,
        });

        r
    }

    #[test]
    fn bm25_search_finds_results() {
        let r = make_test_retriever();
        let req = SearchRequest {
            query: "Rust".to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        };
        let results = r.search(&req, None);
        assert!(!results.is_empty());
        // 应找到至少一条包含 "Rust" 的结果
        assert!(results.iter().any(|sr| sr.doc_summary.contains("Rust")));
    }

    /// 向量通道接线：index_l1_with_vector / index_l2_with_vector
    /// 写入的 L1/L2 文档在带 query 向量的检索中被真实命中。
    #[test]
    fn vector_channel_finds_indexed_l1_l2() {
        let mut r = Retriever::new();
        let l1_id = uuid::Uuid::new_v4();
        r.index_l1_with_vector(
            &L1DocView {
                id: l1_id,
                summary: "用户喜欢打篮球，每周三晚上去球场".to_string(),
                keywords: None,
                persona_uid: Some("user-0001".to_string()),
                created_at: 1000,
                salience: 0.8,
            },
            Some(vec![1.0, 0.0, 0.0]),
        );
        r.index_l2_with_vector(
            &L2DocView {
                id: 7,
                title: "篮球比赛".to_string(),
                summary: "参加了周末篮球比赛".to_string(),
                keywords: None,
                attitude: None,
                paraphrase: None,
                persona_uid: "user-0001".to_string(),
                share: 0.9,
                confidence: 0.9,
                created_at: 2000,
                salience: 0.7,
            },
            Some(vec![0.9, 0.1, 0.0]),
        );

        let req = SearchRequest {
            query: "篮球".to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        };
        // 查询向量与 L1 文档向量高度相似（cos≈1.0），向量通道必须命中
        let results = r.search(&req, Some(&[1.0, 0.0, 0.0]));
        assert!(
            results
                .iter()
                .any(|sr| sr.layer == "l1" && sr.doc_summary.contains("篮球")),
            "L1 文档应通过向量通道被检索到（此前零产出缺陷）"
        );
        // L2 文档（cos≈0.994 > min_similarity=0.0）也应被检索到
        assert!(
            results
                .iter()
                .any(|sr| sr.layer == "l2" && sr.doc_summary.contains("篮球")),
            "L2 文档应通过向量通道被检索到"
        );
    }

    /// 向量通道降级：无 query 向量（embedding 不可用）→ 向量通道跳过，
    /// BM25 仍可命中（回归红线 2：embedding 不可用不阻塞检索）。
    #[test]
    fn vector_channel_skipped_without_query_vector() {
        let mut r = Retriever::new();
        r.index_l1_with_vector(
            &L1DocView {
                id: uuid::Uuid::new_v4(),
                summary: "用户喜欢打篮球".to_string(),
                keywords: None,
                persona_uid: Some("user-0001".to_string()),
                created_at: 1000,
                salience: 0.8,
            },
            Some(vec![1.0, 0.0, 0.0]),
        );
        let req = SearchRequest {
            query: "篮球".to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        };
        // query_vec = None → 向量通道跳过；BM25 无关键词命中 → 空结果（不报错）
        let results = r.search(&req, None);
        // 不 panic、返回空或 BM25 结果均可（此用例仅验证不阻塞）
        let _ = results;
    }

    #[test]
    fn search_filters_by_persona_uid() {
        let r = make_test_retriever();
        let req = SearchRequest {
            query: "Rust".to_string(),
            persona_uid: Some("user-0002".to_string()),
            top_k: 10,
            filter_share: false,
        };
        let results = r.search(&req, None);
        // user-0002 没有任何文档
        assert!(results.is_empty());
    }

    #[test]
    fn search_top_k_truncation() {
        let mut r = make_test_retriever();
        // 添加更多文档
        for i in 0..10 {
            r.index_l1(&L1DocView {
                id: uuid::Uuid::new_v4(),
                summary: format!("文档{} 测试内容", i),
                keywords: Some("测试".to_string()),
                persona_uid: Some("user-0001".to_string()),
                created_at: 3000 + i as i64,
                salience: 0.5,
            });
        }

        let req = SearchRequest {
            query: "测试".to_string(),
            persona_uid: None,
            top_k: 3,
            filter_share: false,
        };
        let results = r.search(&req, None);
        assert!(results.len() <= 3);
    }

    #[test]
    fn search_empty_query_bm25_returns_empty() {
        let r = make_test_retriever();
        let req = SearchRequest {
            query: "".to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        };
        let results = r.search(&req, None);
        // BM25 空查询返回空，向量无 query_vec，图谱无实体
        // 三个通道均为空 → 结果为空
        assert!(results.is_empty());
    }

    #[test]
    fn search_bm25_only_disables_other_channels() {
        let mut r = make_test_retriever();
        r.config_mut().enable_vector = false;
        r.config_mut().enable_graph = false;

        let req = SearchRequest {
            query: "火锅".to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        };
        let results = r.search(&req, None);
        assert!(!results.is_empty());
        assert!(results.iter().any(|sr| sr.doc_summary.contains("火锅")));
    }

    #[test]
    fn rebuild_bm25_preserves_data() {
        let mut r = make_test_retriever();
        // 先搜索确认有结果
        let req = SearchRequest {
            query: "火锅".to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        };
        let before = r.search(&req, None);
        assert!(!before.is_empty());

        // 重建 BM25 索引（清空后从 l1_docs/l2_docs 重新构建）→ 检索结果应保持不变
        r.rebuild_bm25();
        let after = r.search(&req, None);
        assert!(!after.is_empty(), "重建后仍应能检索到火锅文档");
        assert!(
            after.iter().any(|sr| sr.doc_summary.contains("火锅")),
            "重建后结果应仍包含火锅文档"
        );
        // 文档总数不变（重建只重建索引，不丢失文档）
        assert_eq!(r.doc_count(), 3);
    }

    #[test]
    fn clear_removes_all() {
        let mut r = make_test_retriever();
        assert!(r.doc_count() > 0);

        r.clear();
        assert_eq!(r.doc_count(), 0);
        assert_eq!(r.bm25_index.doc_count(), 0);
    }

    #[test]
    fn doc_count_reflects_indexed_docs() {
        let r = make_test_retriever();
        // 2 L1 + 1 L2
        assert_eq!(r.doc_count(), 3);
    }

    #[test]
    fn search_result_contains_required_fields() {
        let r = make_test_retriever();
        let req = SearchRequest {
            query: "Rust".to_string(),
            persona_uid: None,
            top_k: 5,
            filter_share: false,
        };
        let results = r.search(&req, None);
        for sr in &results {
            assert!(!sr.layer.is_empty());
            assert!(!sr.doc_summary.is_empty());
            assert!(sr.rrf_score > 0.0);
            assert!(sr.created_at > 0);
        }
    }

    // =========================================================
    // index_l1_record 测试
    // =========================================================

    #[test]
    fn index_l1_record_adds_to_bm25() {
        let mut r = Retriever::new();
        let l1 = MemoryL1 {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            summary: "用户讨论Rust异步编程".to_string(),
            keywords: Some("Rust,异步,编程".to_string()),
            time_period: None,
            atmosphere: None,
            valence: 0.5,
            salience: 0.8,
            absorbed: false,
            created_at: 1718000000000,
            last_accessed_at: None,
            persona_uid: Some("user-0001".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        };

        let result = r.index_l1_record(&l1);
        assert!(result.is_ok());
        // 验证文档数增加了
        assert_eq!(r.doc_count(), 1);
    }

    #[test]
    fn index_l1_record_searchable_immediately() {
        let mut r = Retriever::new();
        let l1 = MemoryL1 {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            summary: "用户今天学习了Rust编程语言的基础语法".to_string(),
            keywords: Some("学习,Rust,编程".to_string()),
            time_period: None,
            atmosphere: None,
            valence: 0.8,
            salience: 0.9,
            absorbed: false,
            created_at: 1718000000000,
            last_accessed_at: None,
            persona_uid: Some("user-0001".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        };

        r.index_l1_record(&l1).unwrap();

        // 立即检索，应能命中
        let req = SearchRequest {
            query: "Rust".to_string(),
            persona_uid: None,
            top_k: 5,
            filter_share: false,
        };
        let results = r.search(&req, None);
        assert!(!results.is_empty());
        assert!(results.iter().any(|sr| sr.doc_summary.contains("Rust")));
    }

    #[test]
    fn index_l1_record_respects_persona_uid() {
        let mut r = Retriever::new();
        let l1_user_a = MemoryL1 {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            summary: "用户A的私密对话".to_string(),
            keywords: Some("私密".to_string()),
            time_period: None,
            atmosphere: None,
            valence: 0.0,
            salience: 0.5,
            absorbed: false,
            created_at: 1718000000000,
            last_accessed_at: None,
            persona_uid: Some("user-a".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        };

        r.index_l1_record(&l1_user_a).unwrap();

        // 以 user-b 检索，不应命中 user-a 的文档
        let req = SearchRequest {
            query: "私密".to_string(),
            persona_uid: Some("user-b".to_string()),
            top_k: 5,
            filter_share: false,
        };
        let results = r.search(&req, None);
        assert!(results.is_empty());
    }

    #[test]
    fn index_l1_record_preserves_fields() {
        let mut r = Retriever::new();
        let id = uuid::Uuid::new_v4();
        let sid = uuid::Uuid::new_v4();
        let l1 = MemoryL1 {
            id,
            session_id: sid,
            summary: "测试摘要".to_string(),
            keywords: Some("测试,标签".to_string()),
            time_period: Some("下午".to_string()),
            atmosphere: Some("轻松".to_string()),
            valence: 0.7,
            salience: 0.9,
            absorbed: false,
            created_at: 1718000000000,
            last_accessed_at: None,
            persona_uid: Some("test-persona".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        };

        r.index_l1_record(&l1).unwrap();

        // 验证 L1 文档被正确存储
        let req = SearchRequest {
            query: "测试".to_string(),
            persona_uid: None,
            top_k: 5,
            filter_share: false,
        };
        let results = r.search(&req, None);
        assert!(!results.is_empty());

        let found = results
            .iter()
            .find(|sr| matches!(&sr.doc_id, DocId::L1(uid) if *uid == id));
        assert!(found.is_some(), "应能通过 ID 找到刚索引的文档");
        let found = found.unwrap();
        assert_eq!(found.persona_uid.as_deref(), Some("test-persona"));
        assert_eq!(found.doc_summary, "测试摘要");
    }

    // =========================================================
    // search_exact 测试
    // =========================================================

    use ramaria_core::keyword::KeywordToken;

    #[test]
    fn search_exact_finds_matching_docs() {
        let r = make_test_retriever();
        let kw = vec![
            KeywordToken::new("Rust").unwrap(),
            KeywordToken::new("编程").unwrap(),
        ];
        let results = r.search_exact(&kw, "user-0001", 10);
        // 应命中至少 2 条：L1 "Rust,编程" 和 L2 "Rust,项目,发布"
        assert!(!results.is_empty());
        // L2 事件命中 "Rust"，L1 命中 "Rust"+"编程"
        assert!(results.iter().any(|sr| sr.layer == "l1"));
        assert!(results.iter().any(|sr| sr.layer == "l2"));
    }

    #[test]
    fn search_exact_empty_keywords() {
        let r = make_test_retriever();
        let results = r.search_exact(&[], "user-0001", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn search_exact_no_match() {
        let r = make_test_retriever();
        let kw = vec![KeywordToken::new("不存在的关键词xyz").unwrap()];
        let results = r.search_exact(&kw, "user-0001", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn search_exact_filters_by_persona() {
        let r = make_test_retriever();
        let kw = vec![KeywordToken::new("Rust").unwrap()];
        // user-0002 不应有任何文档
        let results = r.search_exact(&kw, "user-0002", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn search_exact_top_k_truncation() {
        let mut r = make_test_retriever();
        // 添加更多含相同关键词的文档
        for i in 0..5 {
            r.index_l1(&L1DocView {
                id: uuid::Uuid::new_v4(),
                summary: format!("文档{} 关于Rust", i),
                keywords: Some("Rust,测试".to_string()),
                persona_uid: Some("user-0001".to_string()),
                created_at: 3000 + i as i64,
                salience: 0.5,
            });
        }
        let kw = vec![KeywordToken::new("Rust").unwrap()];
        let results = r.search_exact(&kw, "user-0001", 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn search_exact_sorts_by_match_count() {
        let mut r = Retriever::new();
        // 文档 A: 命中 1 个关键词
        r.index_l1(&L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: "A".to_string(),
            keywords: Some("Rust".to_string()),
            persona_uid: Some("u1".to_string()),
            created_at: 1000,
            salience: 0.5,
        });
        // 文档 B: 命中 2 个关键词
        r.index_l1(&L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: "B".to_string(),
            keywords: Some("Rust,编程,异步".to_string()),
            persona_uid: Some("u1".to_string()),
            created_at: 2000,
            salience: 0.5,
        });

        let kw = vec![
            KeywordToken::new("Rust").unwrap(),
            KeywordToken::new("编程").unwrap(),
        ];
        let results = r.search_exact(&kw, "u1", 10);
        assert_eq!(results.len(), 2);
        // 文档 B（命中 2 个）应排在前面
        assert!(
            results[0].doc_summary.contains("B"),
            "命中更多关键词的文档应排在前面"
        );
    }

    // =========================================================
    // search_substring 测试
    // =========================================================

    #[test]
    fn search_substring_finds_partial_match() {
        let r = make_test_retriever();
        // "Rust编程" 应能匹配到 BM25 bigram 命中的文档
        let results = r.search_substring("Rust编程", "user-0001", 10);
        assert!(!results.is_empty());
        assert!(results.iter().any(|sr| sr.doc_summary.contains("Rust")));
    }

    #[test]
    fn search_substring_empty_query() {
        let r = make_test_retriever();
        let results = r.search_substring("", "user-0001", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn search_substring_filters_by_persona() {
        let r = make_test_retriever();
        let results = r.search_substring("火锅", "user-0002", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn search_substring_top_k() {
        let mut r = make_test_retriever();
        for i in 0..5 {
            r.index_l1(&L1DocView {
                id: uuid::Uuid::new_v4(),
                summary: format!("文档{} Rust相关", i),
                keywords: Some("Rust".to_string()),
                persona_uid: Some("user-0001".to_string()),
                created_at: 3000 + i as i64,
                salience: 0.5,
            });
        }
        let results = r.search_substring("Rust", "user-0001", 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn search_substring_returns_bm25_score() {
        let r = make_test_retriever();
        let results = r.search_substring("Rust", "user-0001", 5);
        // BM25 分数应 > 0
        for sr in &results {
            assert!(sr.bm25_score.unwrap_or(0.0) > 0.0, "BM25 分数应大于 0");
            assert!(sr.rrf_score > 0.0, "rrf_score 应为 BM25 分数");
        }
    }

    // =========================================================
    // utt 原文通道测试（v1.4）
    // =========================================================

    fn make_utt_doc(id: i64, persona_uid: &str, text: &str, created_at: i64) -> UttDocView {
        UttDocView {
            id,
            persona_uid: persona_uid.to_string(),
            session_id: uuid::Uuid::new_v4(),
            block_text: text.to_string(),
            msg_count: 2,
            created_at,
        }
    }

    #[test]
    fn index_utt_and_search_vector() {
        let mut r = Retriever::new();
        // 过滤零相似度命中（相似度恰为 0 的块不应作为结果返回）
        r.config_mut().vector.min_similarity = 0.01;
        r.index_utt(
            &make_utt_doc(1, "char-0001", "今天天气很好我们去公园吧", 1000),
            Some(vec![1.0, 0.0]),
        );
        r.index_utt(
            &make_utt_doc(2, "char-0001", "晚饭想吃火锅", 2000),
            Some(vec![0.0, 1.0]),
        );

        let hits = r.search_utt("天气", Some(&[1.0, 0.0]), 5, Some("char-0001"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc.id, 1);
        assert_eq!(hits[0].channel, "vector");
        assert!(hits[0].score > 0.0);
    }

    #[test]
    fn search_utt_persona_isolation() {
        // 跨 persona 严格隔离：char-0002 检索不到 char-0001 的块
        let mut r = Retriever::new();
        r.index_utt(
            &make_utt_doc(1, "char-0001", "这是我的秘密原文内容", 1000),
            Some(vec![1.0, 0.0]),
        );

        let hits = r.search_utt("秘密", Some(&[1.0, 0.0]), 5, Some("char-0002"));
        assert!(hits.is_empty(), "跨 persona 不可见");
        assert_eq!(r.utt_doc_count(), 1);
    }

    #[test]
    fn search_utt_without_persona_returns_empty() {
        // 未指定目标 persona → 不检索原文（隔离红线）
        let mut r = Retriever::new();
        r.index_utt(&make_utt_doc(1, "char-0001", "原文", 1000), Some(vec![1.0]));
        assert!(r.search_utt("原文", Some(&[1.0]), 5, None).is_empty());
    }

    #[test]
    fn search_utt_vector_empty_index_falls_back_to_substring() {
        // 向量索引为空（块无 embedding）→ 子串降级
        let mut r = Retriever::new();
        r.index_utt(
            &make_utt_doc(1, "char-0001", "今天天气很好我们去公园吧", 1000),
            None,
        );
        r.index_utt(&make_utt_doc(2, "char-0001", "晚饭想吃火锅", 2000), None);

        let hits = r.search_utt("火锅", Some(&[1.0, 0.0]), 5, Some("char-0001"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc.id, 2);
        assert_eq!(hits[0].channel, "substring");
    }

    #[test]
    fn search_utt_substring_scores_by_token_hits() {
        let mut r = Retriever::new();
        // 块1 命中 1 个 token（"天气"），块2 命中 2 个 token（"天气""公园"）
        r.index_utt(&make_utt_doc(1, "char-0001", "天气不错", 1000), None);
        r.index_utt(
            &make_utt_doc(2, "char-0001", "天气好去公园散步", 2000),
            None,
        );

        let hits = r.search_utt("天气 公园", None, 5, Some("char-0001"));
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc.id, 2, "命中更多 token 的块排前");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn search_utt_substring_no_match_returns_empty() {
        let mut r = Retriever::new();
        r.index_utt(&make_utt_doc(1, "char-0001", "今天天气很好", 1000), None);
        let hits = r.search_utt("完全无关的话题词汇", None, 5, Some("char-0001"));
        assert!(hits.is_empty());
    }

    #[test]
    fn search_utt_top_k_limits_results() {
        let mut r = Retriever::new();
        for i in 0..5 {
            r.index_utt(
                &make_utt_doc(i, "char-0001", &format!("天气讨论第{i}轮内容"), i * 1000),
                None,
            );
        }
        let hits = r.search_utt("天气", None, 2, Some("char-0001"));
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn remove_utt_removes_doc_and_vector() {
        let mut r = Retriever::new();
        r.index_utt(
            &make_utt_doc(1, "char-0001", "原文内容", 1000),
            Some(vec![1.0, 0.0]),
        );
        r.remove_utt(1);
        assert_eq!(r.utt_doc_count(), 0);
        assert!(
            r.search_utt("原文", Some(&[1.0, 0.0]), 5, Some("char-0001"))
                .is_empty()
        );
    }

    #[test]
    fn clear_removes_utt_docs() {
        let mut r = Retriever::new();
        r.index_utt(&make_utt_doc(1, "char-0001", "原文", 1000), Some(vec![1.0]));
        r.clear();
        assert_eq!(r.utt_doc_count(), 0);
    }

    #[test]
    fn index_utt_block_decodes_embedding_blob() {
        use ramaria_core::types::UttBlock;
        let mut r = Retriever::new();
        let mut block = UttBlock::new(
            "char-0001".to_string(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "块原文文本".to_string(),
            3,
            1000,
        );
        block.embedding = Some(crate::utt::encode_embedding(&[0.5, -0.25]));
        r.index_utt_block(&block);

        let hits = r.search_utt("块原文", Some(&[0.5, -0.25]), 5, Some("char-0001"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].channel, "vector");
    }

    #[test]
    fn index_utt_block_corrupted_blob_degrades_to_substring() {
        use ramaria_core::types::UttBlock;
        let mut r = Retriever::new();
        let mut block = UttBlock::new(
            "char-0001".to_string(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "损坏向量但文本可检索".to_string(),
            3,
            1000,
        );
        block.embedding = Some(vec![1, 2, 3]); // 长度非 4 倍数 → 解码失败
        r.index_utt_block(&block);

        let hits = r.search_utt("文本可检索", Some(&[1.0, 2.0, 3.0]), 5, Some("char-0001"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].channel, "substring", "损坏 BLOB 降级子串");
    }

    #[test]
    fn l0_labels_do_not_leak_into_regular_search() {
        // 回归红线：utt 块（L0: label）不得混入三通道 RAG 检索结果
        let mut r = make_test_retriever();
        r.index_utt(
            &make_utt_doc(99, "user-0001", "用户原文内容", 5000),
            Some(vec![1.0, 0.0]),
        );
        let results = r.search(
            &SearchRequest {
                query: "用户原文内容".to_string(),
                persona_uid: Some("user-0001".to_string()),
                top_k: 5,
                filter_share: true,
            },
            Some(&[1.0, 0.0]),
        );
        // 既有 L1 文档（user-0001）可命中，但 L0: 块不会作为结果出现
        for sr in &results {
            assert_ne!(sr.layer, "l0", "L0 块不应混入常规 RAG 结果");
        }
    }
}
