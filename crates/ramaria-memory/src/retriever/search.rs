//! crates/ramaria-memory/src/retriever/search.rs — Retriever 的三通道检索编排
//!
//! 设计特点:
//! - 实现统一 `search` 入口，编排 BM25/向量/图谱三通道并通过 RRF 融合
//! - 提供关键词精确检索、脉络加权检索与 BM25 子串检索等专项入口
//! - 检索结果解析复用 helpers 中的预解析/字符串解析辅助函数

use crate::bm25::DocId;
use crate::decay::{DecayConfig, calc_retention};
use crate::graph_retriever::graph_hits_to_rrf_pairs;
use crate::rrf::{ChannelResult, FusedResult, rrf_fuse};
use crate::vector::{VectorHit, VectorIndex};
use ramaria_core::keyword::KeywordToken;

use super::Retriever;
use super::helpers::{
    Bm25Resolved, count_keyword_matches, parse_doc_label, parse_graph_label, resolve_bm25_doc,
};
use super::types::{L1DocView, SearchRequest, SearchResult};

impl Retriever {
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
            let (doc_id, persona_uid, share, created_at, last_accessed_at, summary, layer) =
                if let Some(data) = bm25_data.get(&f.doc_id) {
                    (
                        data.doc_id.clone(),
                        data.persona_uid.clone(),
                        data.share,
                        data.created_at,
                        data.last_accessed_at,
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
                        Some((did, puid, sh, ca, la, sum)) => {
                            let lyr = match &did {
                                DocId::L1(_) => "l1".to_string(),
                                DocId::L2(_) => "l2".to_string(),
                                DocId::Graph(_) => "graph".to_string(),
                            };
                            (did, puid, sh, ca, la, sum, lyr)
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
                last_accessed_at,
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
        keywords: &[KeywordToken],
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
                        last_accessed_at: doc.last_accessed_at,
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
                        last_accessed_at: None, // L2 事件暂不追踪访问时间
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
    /// 脉络加权检索（v1.7 B4，决策 D-V17-006）。
    ///
    /// 用途:
    /// - 替代跨会话注入中"无条件取最近 N 条"（`list_recent_l1_by_persona`）：
    ///   以当前用户消息为话题依据，按"时间（衰减 × 访问加成）× 话题相关性"
    ///   融合排序，使"刚聊过 / 相关的话题"优先进入脉络注入。
    ///
    /// 排序公式:
    /// - `score = BM25 相关性 × calc_retention(created_at, last_accessed_at, now, salience)`
    /// - `calc_retention` 内含访问加成：近期被检索命中的 L1（`touch` 刷新
    ///   `last_accessed_at` 后）保留率保底 `recent_boost_floor`，旧记忆更容易被召回。
    ///
    /// 兜底:
    /// - 查询为空或 BM25 无相关性命中 → 按 `created_at` 降序回退最近 N 条，
    ///   与 v1.6"无条件取最近几条"语义等价（不丢脉络）。
    /// - 目标 persona 无 L1 → 空列表。
    ///
    /// 参数:
    /// - `query`: 当前用户消息（话题相关性依据；空/空白时按时间兜底）。
    /// - `persona_uid`: 目标人格（仅检索该 persona 的 L1 摘要）。
    /// - `top_k`: 最多返回条数。
    /// - `now_ms`: 当前时间（Unix 毫秒，衰减基准）。
    /// - `decay_config`: 记忆层衰减配置（含访问加成参数）。
    ///
    /// 返回:
    /// - 按融合分降序的 L1 SearchResult 列表（最多 top_k 条）。
    pub fn search_narrative(
        &self,
        query: &str,
        persona_uid: &str,
        top_k: usize,
        now_ms: i64,
        decay_config: &DecayConfig,
    ) -> Vec<SearchResult> {
        if top_k == 0 {
            return Vec::new();
        }

        // 1. 收集目标 persona 的 L1 文档（脉络注入只针对该 persona 的摘要）
        let persona_docs: Vec<&L1DocView> = self
            .l1_docs
            .values()
            .filter(|d| d.persona_uid.as_deref() == Some(persona_uid))
            .collect();
        if persona_docs.is_empty() {
            return Vec::new();
        }

        // 2. 话题相关性：BM25 检索 query 命中 L1 文档（BM25 分数作为相关性）
        //    BM25 索引可能包含其他 persona 的文档，此处按 id 过滤到本 persona。
        let bm25_hits: std::collections::HashMap<uuid::Uuid, f64> = if !query.trim().is_empty() {
            self.bm25_index
                .search(query, &self.config.bm25)
                .into_iter()
                .filter_map(|(doc_id, score)| match doc_id {
                    DocId::L1(id) => Some((id, score)),
                    _ => None,
                })
                .collect()
        } else {
            std::collections::HashMap::new()
        };

        // 3. 融合打分：相关性 × 时间保留率（含访问加成）
        let mut scored: Vec<(f64, &L1DocView)> = Vec::with_capacity(persona_docs.len());
        for doc in persona_docs {
            let bm25_raw = bm25_hits.get(&doc.id).copied().unwrap_or(0.0);
            let retention = calc_retention(
                doc.created_at,
                doc.last_accessed_at,
                now_ms,
                doc.salience,
                decay_config,
            );
            scored.push((bm25_raw * retention, doc));
        }

        // 4. 排序：有相关性命中 → 按融合分降序；否则按创建时间降序（最近优先兜底）
        let has_relevance = scored.iter().any(|(s, _)| *s > 0.0);
        if has_relevance {
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        } else {
            // 按创建时间降序（最近优先兜底）：sort_by_key 升序后反转
            scored.sort_by_key(|(_, doc)| doc.created_at);
            scored.reverse();
        }

        // 5. 截取 top_k 并转换为 SearchResult
        scored.truncate(top_k);
        scored
            .into_iter()
            .map(|(score, doc)| SearchResult {
                doc_id: DocId::L1(doc.id),
                layer: "l1".to_string(),
                rrf_score: score,
                bm25_score: if score > 0.0 { Some(score) } else { None },
                vector_score: None,
                graph_score: None,
                persona_uid: doc.persona_uid.clone(),
                share: None,
                created_at: doc.created_at,
                last_accessed_at: doc.last_accessed_at,
                doc_summary: doc.summary.clone(),
            })
            .collect()
    }

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
                last_accessed_at: doc_data.last_accessed_at,
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
}
