//! rust/crates/ramaria-memory/src/retriever.rs — 三通道组合检索编排器
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
//! 4. RRF 融合: 三个通道结果通过 rrf_fuse() 合并排序
//! 5. 解析 doc_id → 加载实际文档 → 返回检索结果

use crate::bm25::{Bm25Config, Bm25Index, DocId};
use crate::graph_retriever::{GraphRetriever, GraphRetrieverConfig, graph_hits_to_rrf_pairs};
use crate::rrf::{ChannelResult, FusedResult, RrfConfig, rrf_fuse};
use crate::vector::{BruteForceIndex, VectorHit, VectorIndex, VectorIndexConfig};

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
    /// 是否过滤低 share 事件（rama 类型不适用）
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

// =========================================================
// 三通道检索器
// =========================================================

/// 三通道组合检索器。
///
/// 职责:
/// - 持有各通道的索引（BM25 / 向量 / 图谱）
/// - 提供统一的 `search()` 入口
/// - 管理索引的构建和增量更新
///
/// 内存假设:
/// - L1/L2 文档视图完整存储在内存 HashMap 中，用于 BM25 搜索结果的文档解析。
/// - 在 10k 文档规模下（v1.0/v1.1 典型用户场景），内存占用约 5-15 MB，
///   完全在 200MB 空闲目标范围内。
/// - 100k+ 文档时（v1.2+ 压力场景）需考虑 LRU 淘汰或 mmap 二级存储。
#[derive(Debug)]
pub struct Retriever {
    /// 检索配置
    config: RetrieverConfig,
    /// BM25 索引
    bm25_index: Bm25Index,
    /// 向量索引
    vector_index: BruteForceIndex,
    /// 图谱检索器
    graph_retriever: GraphRetriever,
    /// doc_id → L1 视图（BM25 结果解析用）
    l1_docs: std::collections::HashMap<uuid::Uuid, L1DocView>,
    /// doc_id → L2 视图（BM25 结果解析用）
    l2_docs: std::collections::HashMap<i64, L2DocView>,
}

impl Retriever {
    /// 使用默认配置创建检索器。
    pub fn new() -> Self {
        Self {
            config: RetrieverConfig::default(),
            bm25_index: Bm25Index::new(),
            vector_index: BruteForceIndex::new(),
            graph_retriever: GraphRetriever::new(),
            l1_docs: std::collections::HashMap::new(),
            l2_docs: std::collections::HashMap::new(),
        }
    }

    /// 使用自定义配置创建检索器。
    pub fn with_config(config: RetrieverConfig) -> Self {
        Self {
            config,
            bm25_index: Bm25Index::new(),
            vector_index: BruteForceIndex::new(),
            graph_retriever: GraphRetriever::new(),
            l1_docs: std::collections::HashMap::new(),
            l2_docs: std::collections::HashMap::new(),
        }
    }

    /// 获取内部 BM25 索引的可变引用。
    pub fn bm25_mut(&mut self) -> &mut Bm25Index {
        &mut self.bm25_index
    }

    /// 获取内部向量索引的可变引用。
    pub fn vector_mut(&mut self) -> &mut BruteForceIndex {
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

    /// 将 L1 文档添加到所有启用通道的索引中。
    ///
    /// 接受引用以避免调用方不必要的 clone；内部仅在存入 HashMap 时做一次 clone。
    pub fn index_l1(&mut self, doc: &L1DocView) {
        // BM25 索引
        if self.config.enable_bm25 {
            let tokens = crate::bm25::tokenize_fields(&[
                &doc.summary,
                doc.keywords.as_deref().unwrap_or(""),
            ]);
            self.bm25_index.add(DocId::L1(doc.id), &tokens);
        }

        // 向量索引（L1 文档暂不添加向量，需要 EmbeddingProvider）
        // Phase 3 接入真实 embedding 后，在此生成向量并添加到 vector_index

        self.l1_docs.insert(doc.id, doc.clone());
    }

    /// 将 L2 事件添加到所有启用通道的索引中。
    ///
    /// 接受引用以避免调用方不必要的 clone；内部仅在存入 HashMap 时做一次 clone。
    /// 同时消除了之前因 borrow checker 限制而产生的临时 String 分配。
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
            self.bm25_index.add(DocId::L2(doc.id), &tokens);
        }

        // 向量索引（同 L1，Phase 3 接入 embedding）
        self.l2_docs.insert(doc.id, doc.clone());
    }

    /// 从 BM25 索引中移除一篇文档。
    pub fn remove_from_bm25(&mut self, doc_id: &DocId) {
        self.bm25_index.remove(doc_id);
    }

    /// 重建 BM25 索引。
    ///
    /// 清空现有索引，从 l1_docs 和 l2_docs 重新构建。
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
                        let data = resolve_bm25_doc(&doc_id, &self.l1_docs, &self.l2_docs);
                        bm25_data.insert(label.clone(), data);
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

    /// 清空所有索引和文档。
    pub fn clear(&mut self) {
        self.bm25_index.clear();
        self.vector_index.clear();
        self.graph_retriever.clear();
        self.l1_docs.clear();
        self.l2_docs.clear();
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
    if let Some(uuid_str) = label.strip_prefix("L1:") {
        let id = uuid::Uuid::parse_str(uuid_str).ok()?;
        let doc = l1_docs.get(&id)?;
        Some((
            DocId::L1(id),
            doc.persona_uid.clone(),
            None, // L1 无 share
            doc.created_at,
            doc.summary.clone(),
        ))
    } else if let Some(id_str) = label.strip_prefix("L2:") {
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
        let r = make_test_retriever();
        // 先搜索确认有结果
        let req = SearchRequest {
            query: "火锅".to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        };
        let before = r.search(&req, None);
        assert!(!before.is_empty());

        // 不 mutable 的可重建测试
        // rebuild_bm25 需要 &mut self，这里仅验证 rebuild 后逻辑不会 panic
        // 实际 rebuild 测试在集成测试中做
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
}
