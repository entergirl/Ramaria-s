//! crates/ramaria-memory/src/retriever/search.rs — Retriever 的多通道检索编排
//!
//! 设计特点:
//! - 实现统一 `search` 入口，编排 BM25/向量/图谱通道并通过 RRF 融合
//! - 支持注入关键词镜像命中（KeywordService 在 app 层预取后传入）作为第四通道
//! - 提供关键词精确检索、脉络加权检索与 BM25 子串检索等专项入口
//! - 检索结果解析复用 helpers 中的文档/label 解析与关键词计数辅助函数
//! - 可选轨迹采集：`search_traced` 记录四通道候选与丢弃原因，不采集时不构造轨迹数据
//!
//! 融合口径:
//! - 四通道（向量 / BM25 / 图谱 / 关键词镜像）统一经 `rrf_fuse_optional` 融合；
//!   通道缺席（未启用 / 无结果 / 未注入）不产生贡献项，权重按通道身份取值。
//! - 关键词命中以 `(label, score)` 纯数据形式注入：label 复用向量通道
//!   `L1:{uuid}` / `L2:{id}` 格式，经既有 `parse_doc_label` 解析，无句柄泄漏。

use std::collections::HashSet;

use crate::bm25::DocId;
use crate::decay::{DecayConfig, calc_retention};
use crate::graph_retriever::graph_hits_to_rrf_pairs;
use crate::rrf::{ChannelResult, FusedResult, OptionalChannels, rrf_fuse_optional};
use crate::trace::{
    ChannelCandidate, ChannelTrace, DropReason, DroppedTrace, FusedEntryTrace, RetrievalTrace,
    TraceChannel,
};
use crate::vector::{VectorHit, VectorIndex};
use ramaria_core::keyword::KeywordToken;

use super::Retriever;
use super::helpers::{
    count_keyword_matches, label_time_ms, parse_doc_label, parse_graph_label, resolve_bm25_doc,
};
use super::types::{L1DocView, SearchRequest, SearchResult};

/// 向量检索失败告警节流窗口（按累计次数）：首次失败立即告警，其后每 100 次告警一次。
const VECTOR_SEARCH_WARN_EVERY: u64 = 100;

impl Retriever {
    /// 执行多通道组合检索（不含关键词镜像通道）。
    ///
    /// 说明:
    /// - 等价于 `search_with_keyword_hits(request, query_vec, None)`：
    ///   关键词镜像通道缺席，BM25 + 向量 + 图谱按启用情况参与统一融合入口，
    ///   缺席通道不产生贡献项，行为语义与既有三通道一致。
    /// - 调用方如需关键词镜像作为第四通道，请改用 `search_with_keyword_hits`。
    ///
    /// 参数:
    /// - `request`: 检索请求
    /// - `query_vec`: 可选的 query 向量（若未提供则跳过向量通道）
    pub fn search(&self, request: &SearchRequest, query_vec: Option<&[f32]>) -> Vec<SearchResult> {
        self.search_at(request, query_vec, None)
    }

    /// 执行多通道组合检索，并按查询时钟剔除未来单元（不含关键词镜像通道）。
    ///
    /// 参数:
    /// - `request` / `query_vec`: 同 `search`；
    /// - `as_of_ms`: 查询时钟（Unix 毫秒）；`None` = 不过滤（产品路径）。
    ///
    /// 返回:
    /// - 与 `search` 逐项一致的结果；传 `Some` 时时间晚于该时钟的 L1 / L2 单元
    ///   在融合前被剔除。
    pub fn search_at(
        &self,
        request: &SearchRequest,
        query_vec: Option<&[f32]>,
        as_of_ms: Option<i64>,
    ) -> Vec<SearchResult> {
        self.search_with_keyword_hits_at(request, query_vec, None, as_of_ms)
    }

    /// 执行多通道组合检索（可含关键词镜像通道）。
    ///
    /// 流程:
    /// 1. 各通道独立检索（BM25/向量/图谱 + 可选注入的关键词镜像命中）
    /// 2. 各通道结果转为统一的 ChannelResult<String>（label 作为 key）
    /// 3. RRF 统一融合（通道可选、按通道身份取权重、缺席不产生贡献项）
    /// 4. 融合截断后解析 label 为 SearchResult（文档已被移除则跳过）
    ///
    /// 参数:
    /// - `request`: 检索请求
    /// - `query_vec`: 可选的 query 向量（若未提供则跳过向量通道）
    /// - `keyword_hits`: 关键词镜像命中（label + score 列表；None/空 = 无关键词通道）。
    ///   是否进入融合另受 `self.config.enable_keyword_channel` 控制。
    pub fn search_with_keyword_hits(
        &self,
        request: &SearchRequest,
        query_vec: Option<&[f32]>,
        keyword_hits: Option<Vec<(String, f64)>>,
    ) -> Vec<SearchResult> {
        self.search_with_keyword_hits_at(request, query_vec, keyword_hits, None)
    }

    /// 执行多通道组合检索，并按查询时钟剔除未来单元（可含关键词镜像通道）。
    ///
    /// 参数:
    /// - `request` / `query_vec` / `keyword_hits`: 同 `search_with_keyword_hits`；
    /// - `as_of_ms`: 查询时钟（Unix 毫秒）；`None` = 不过滤（产品路径）。
    ///
    /// 返回:
    /// - 与 `search_with_keyword_hits` 逐项一致的结果；传 `Some` 时剔除未来单元。
    pub fn search_with_keyword_hits_at(
        &self,
        request: &SearchRequest,
        query_vec: Option<&[f32]>,
        keyword_hits: Option<Vec<(String, f64)>>,
        as_of_ms: Option<i64>,
    ) -> Vec<SearchResult> {
        self.search_inner(request, query_vec, keyword_hits, None, as_of_ms)
    }

    /// 执行多通道组合检索并采集检索阶段轨迹。
    ///
    /// 参数:
    /// - `request`: 检索请求；
    /// - `query_vec`: 可选的 query 向量（若未提供则跳过向量通道）；
    /// - `keyword_hits`: 关键词镜像命中（label + score 列表；None/空 = 无关键词通道）。
    ///
    /// 返回:
    /// - 与 `search_with_keyword_hits` 逐项一致的检索结果，以及本次检索的阶段轨迹。
    ///
    /// 说明:
    /// - 轨迹只记 label、通道原始分与名次，不含查询文本与文档摘要。
    pub fn search_traced(
        &self,
        request: &SearchRequest,
        query_vec: Option<&[f32]>,
        keyword_hits: Option<Vec<(String, f64)>>,
    ) -> (Vec<SearchResult>, RetrievalTrace) {
        self.search_traced_at(request, query_vec, keyword_hits, None)
    }

    /// 执行多通道组合检索并采集轨迹，同时按查询时钟剔除未来单元。
    ///
    /// 参数:
    /// - `request` / `query_vec` / `keyword_hits`: 同 `search_traced`；
    /// - `as_of_ms`: 查询时钟（Unix 毫秒）；`None` = 不过滤（产品路径）。
    ///
    /// 返回:
    /// - 检索结果与阶段轨迹；被剔除的未来单元记 `DropReason::Future`，
    ///   分数取该 label 在各通道的最大原始分。
    pub fn search_traced_at(
        &self,
        request: &SearchRequest,
        query_vec: Option<&[f32]>,
        keyword_hits: Option<Vec<(String, f64)>>,
        as_of_ms: Option<i64>,
    ) -> (Vec<SearchResult>, RetrievalTrace) {
        let mut trace = RetrievalTrace::default();
        let results =
            self.search_inner(request, query_vec, keyword_hits, Some(&mut trace), as_of_ms);
        (results, trace)
    }

    /// 多通道检索的统一实现（可含关键词镜像通道），可选写入检索阶段轨迹。
    ///
    /// 参数:
    /// - `request` / `query_vec` / `keyword_hits`: 同 `search_with_keyword_hits`；
    /// - `trace`: 传 `Some` 时按通道 / 融合 / 丢弃三个阶段写入；
    ///   `None` 时不构造任何轨迹数据，行为与开销与无轨迹路径一致；
    /// - `as_of_ms`: 查询时钟（Unix 毫秒）；`None` = 不过滤。
    ///
    /// 未来过滤:
    /// - L1 取 `occurred_at`、L2 取事件 `end`；时间晚于时钟的候选在融合前逐通道剔除，
    ///   否则未来单元会先占融合名次与 top_k 截断名额，把可用的历史单元挤出结果。
    /// - 图谱实体与无法解析的 label 没有可比时间，不参与过滤（保留）。
    fn search_inner(
        &self,
        request: &SearchRequest,
        query_vec: Option<&[f32]>,
        keyword_hits: Option<Vec<(String, f64)>>,
        mut trace: Option<&mut RetrievalTrace>,
        as_of_ms: Option<i64>,
    ) -> Vec<SearchResult> {
        // ---- BM25 通道 ----
        // 只取融合所需的候选量：按 request.top_k 的固定倍数超取（下限 64），
        // 避免对全库命中逐条构造 label 与解析文档；label 解析延后到融合截断之后。
        const BM25_OVERFETCH_MULTIPLIER: usize = 8;
        const BM25_MIN_LIMIT: usize = 64;
        let mut bm25_channel = if self.config.enable_bm25 {
            let limit = request
                .top_k
                .saturating_mul(BM25_OVERFETCH_MULTIPLIER)
                .max(BM25_MIN_LIMIT);
            let raw_results =
                self.bm25_index
                    .search_limited(&request.query, &self.config.bm25, limit);
            if raw_results.is_empty() {
                None
            } else {
                let results: Vec<(String, f64)> = raw_results
                    .into_iter()
                    .map(|(doc_id, score)| (doc_id.to_string(), score))
                    .collect();
                Some(ChannelResult { results })
            }
        } else {
            None
        };

        // ---- 向量通道 ----
        let mut vector_channel: Option<ChannelResult<String>> = if self.config.enable_vector {
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
                    Err(e) => {
                        // 失败按“无向量通道”降级，但必须可观测：逐条召回都会走到这里，
                        // 按累计次数节流告警，避免日志刷屏。
                        let failures = self
                            .vector_search_failures
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                            + 1;
                        if failures == 1 || failures.is_multiple_of(VECTOR_SEARCH_WARN_EVERY) {
                            tracing::warn!(
                                error = %e,
                                failures,
                                "向量检索失败，本轮按无向量通道降级（同类失败已节流）"
                            );
                        }
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        // ---- 图谱通道 ----
        let mut graph_channel: Option<ChannelResult<String>> = if self.config.enable_graph {
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

        // ---- 关键词镜像通道（外部注入；有数据且开关开启才进入融合） ----
        let mut keyword_channel: Option<ChannelResult<String>> =
            if self.config.enable_keyword_channel {
                match keyword_hits {
                    Some(hits) if !hits.is_empty() => Some(ChannelResult { results: hits }),
                    _ => None,
                }
            } else {
                None
            };

        // ---- 未来过滤（查询时钟） ----
        // 必须发生在融合之前：未来单元若进入融合，会先占名次与 top_k 截断名额，
        // 把真正可用的历史单元挤出结果。
        // L1 取 occurred_at、L2 取事件 end；图谱实体与无法解析的 label 没有可比时间，保留。
        // 同一 label 命中多个通道时只记一次，分数取各通道最大原始分；无轨迹时不构造条目。
        let mut future_drops: Vec<DroppedTrace> = Vec::new();
        if let Some(as_of) = as_of_ms {
            let collect_drops = trace.is_some();
            for channel in [
                &mut vector_channel,
                &mut bm25_channel,
                &mut graph_channel,
                &mut keyword_channel,
            ] {
                let emptied = {
                    let Some(result) = channel.as_mut() else {
                        continue;
                    };
                    result.results.retain(|(label, score)| {
                        let Some(at_ms) = label_time_ms(label, &self.l1_docs, &self.l2_docs) else {
                            return true;
                        };
                        if at_ms <= as_of {
                            return true;
                        }
                        if collect_drops {
                            match future_drops.iter_mut().find(|d| &d.id == label) {
                                Some(existing) => existing.score = existing.score.max(*score),
                                None => future_drops.push(DroppedTrace {
                                    id: label.clone(),
                                    reason: DropReason::Future,
                                    score: *score,
                                }),
                            }
                        }
                        false
                    });
                    result.results.is_empty()
                };
                // 被过滤空的通道按缺席处理：融合只对"参与的通道"计权，空结果若留在
                // Some 分支会给所有候选加同一份惩罚项，改变衰减后的相对次序。
                if emptied {
                    *channel = None;
                }
            }
        }

        // ---- 轨迹：四通道候选（缺席通道也记一条，active=false、候选为空） ----
        if let Some(t) = trace.as_deref_mut() {
            t.channels = vec![
                channel_trace(TraceChannel::Vector, vector_channel.as_ref()),
                channel_trace(TraceChannel::Bm25, bm25_channel.as_ref()),
                channel_trace(TraceChannel::Graph, graph_channel.as_ref()),
                channel_trace(TraceChannel::Keyword, keyword_channel.as_ref()),
            ];
        }

        // ---- RRF 融合 ----
        // 通道缺席不产生惩罚项；权重按通道身份取值（修复"向量缺席时图谱按 bm25_weight 计权"）。
        // 融合阶段即按本次请求的 top_k 截断：否则 RrfConfig 的默认 top_k（5）会把
        // core 配置的 l1_retrieve_top_k（如 12）静默截断。
        let mut rrf_config = self.config.rrf.clone();
        rrf_config.top_k = request.top_k;

        let fused: Vec<FusedResult<String>> = rrf_fuse_optional(
            &OptionalChannels {
                vector: vector_channel.as_ref(),
                bm25: bm25_channel.as_ref(),
                graph: graph_channel.as_ref(),
                keyword: keyword_channel.as_ref(),
            },
            &rrf_config,
        );
        // ---- 轨迹：融合条目与融合截断丢弃 ----
        // 衰减因子与衰减后分由召回装配在衰减阶段回填，此处先记衰减前口径。
        if let Some(t) = trace.as_deref_mut() {
            t.fused = fused
                .iter()
                .map(|f| FusedEntryTrace {
                    id: f.doc_id.clone(),
                    layer: String::new(),
                    rrf_score: f.rrf_score,
                    decay_factor: 1.0,
                    decayed_score: f.rrf_score,
                    vector_raw: f.vector_raw_score,
                    bm25_raw: f.bm25_raw_score,
                    graph_raw: f.graph_raw_score,
                    keyword_raw: keyword_channel.as_ref().and_then(|c| {
                        c.results
                            .iter()
                            .find(|(label, _)| label == &f.doc_id)
                            .map(|(_, score)| *score)
                    }),
                })
                .collect();

            // 通道候选里未出现在融合结果中的 label 记一次融合截断；
            // 同一 label 命中多个通道时取最大原始分。
            let fused_ids: HashSet<&str> = fused.iter().map(|f| f.doc_id.as_str()).collect();
            let mut cut: Vec<DroppedTrace> = Vec::new();
            for channel in [
                &vector_channel,
                &bm25_channel,
                &graph_channel,
                &keyword_channel,
            ] {
                let Some(result) = channel.as_ref() else {
                    continue;
                };
                for (label, score) in &result.results {
                    if fused_ids.contains(label.as_str()) {
                        continue;
                    }
                    match cut.iter_mut().find(|d| &d.id == label) {
                        Some(existing) => {
                            if *score > existing.score {
                                existing.score = *score;
                            }
                        }
                        None => cut.push(DroppedTrace {
                            id: label.clone(),
                            reason: DropReason::FusionCut,
                            score: *score,
                        }),
                    }
                }
            }
            t.dropped = future_drops;
            t.dropped.extend(cut);
        }

        if fused.is_empty() {
            return Vec::new();
        }

        // ---- 解析为 SearchResult ----
        let mut results: Vec<SearchResult> = Vec::with_capacity(fused.len());

        for (index, f) in fused.iter().enumerate() {
            // 图谱实体 label（`graph:{entity}`）优先解析；BM25 与向量通道共用
            // `L1:{uuid}` / `L2:{id}` label 格式，解析统一由 `parse_doc_label` 承担；
            // 文档已被移除则跳过。
            let (doc_id, persona_uid, share, created_at, last_accessed_at, summary, layer) =
                if let Some(data) = parse_graph_label(&f.doc_id) {
                    // 图谱实体：实体名作为摘要
                    data
                } else {
                    match parse_doc_label(&f.doc_id, &self.l1_docs, &self.l2_docs) {
                        Some((did, puid, sh, ca, la, sum)) => {
                            let lyr = match &did {
                                DocId::L1(_) => "l1".to_string(),
                                DocId::L2(_) => "l2".to_string(),
                                DocId::Graph(_) => "graph".to_string(),
                            };
                            (did, puid, sh, ca, la, sum, lyr)
                        }
                        None => {
                            // 文档已被移除，跳过
                            if let Some(t) = trace.as_deref_mut() {
                                t.dropped.push(DroppedTrace {
                                    id: f.doc_id.clone(),
                                    reason: DropReason::DocMissing,
                                    score: f.rrf_score,
                                });
                            }
                            continue;
                        }
                    }
                };

            // 轨迹：分层在解析成功后才有值，解析失败的条目保持空字符串。
            if let Some(t) = trace.as_deref_mut() {
                if let Some(entry) = t.fused.get_mut(index) {
                    entry.layer = layer.clone();
                }
            }

            // persona_uid 过滤
            let persona_mismatch = request
                .persona_uid
                .as_ref()
                .zip(persona_uid.as_ref())
                .is_some_and(|(target_uid, puid)| puid != target_uid);
            if persona_mismatch {
                if let Some(t) = trace.as_deref_mut() {
                    t.dropped.push(DroppedTrace {
                        id: f.doc_id.clone(),
                        reason: DropReason::PersonaMismatch,
                        score: f.rrf_score,
                    });
                }
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
                        created_at: doc.occurred_at,
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
                        created_at: doc.start,
                        last_accessed_at: None, // L2 事件暂不追踪访问时间
                        doc_summary: format!("{} — {}", doc.title, doc.summary),
                    },
                ));
            }
        }

        // 按命中关键词数降序排列，同等命中数按时间基准（发生时间）降序
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
}

// =========================================================
// 轨迹辅助
// =========================================================

/// 把通道结果转为轨迹条目。
///
/// 参数:
/// - `channel`: 通道标识；
/// - `channel_result`: 该通道的检索结果（`None` = 通道关闭 / 无结果）。
///
/// 返回:
/// - 通道轨迹；候选按通道原始顺序编号（`rank` 从 1 起），缺席通道 `active=false`。
fn channel_trace(
    channel: TraceChannel,
    channel_result: Option<&ChannelResult<String>>,
) -> ChannelTrace {
    match channel_result {
        Some(result) if !result.results.is_empty() => ChannelTrace {
            channel,
            active: true,
            candidates: result
                .results
                .iter()
                .enumerate()
                .map(|(index, (id, raw_score))| ChannelCandidate {
                    id: id.clone(),
                    raw_score: *raw_score,
                    rank: index + 1,
                })
                .collect(),
        },
        _ => ChannelTrace {
            channel,
            active: false,
            candidates: Vec::new(),
        },
    }
}

mod specialized;

impl Retriever {
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
