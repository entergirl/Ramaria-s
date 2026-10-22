//! crates/ramaria-memory/src/retriever/search/specialized.rs - 专项检索入口
//!
//! 设计特点:
//! - 脉络加权：BM25 相关性 × 时间衰减（含访问加成），空查询按发生时间兜底
//! - 子串检索：短查询按包含关系匹配 L1 / L2 摘要
//! - BM25 单通道：供子串检索与降级路径复用

use super::*;

impl Retriever {
    /// 基于话题相关性与时间衰减的脉络加权检索。
    ///
    /// 用途:
    /// - 替代跨会话注入中"无条件取最近 N 条"（`list_recent_l1_by_persona`）：
    ///   以当前用户消息为话题依据，按"时间（衰减 × 访问加成）× 话题相关性"
    ///   融合排序，使"刚聊过 / 相关的话题"优先进入脉络注入。
    ///
    /// 排序公式:
    /// - `score = BM25 相关性 × calc_retention(occurred_at, last_accessed_at, now, salience)`
    /// - `calc_retention` 内含访问加成：近期被检索命中的 L1（`touch` 刷新
    ///   `last_accessed_at` 后）保留率保底 `recent_boost_floor`，旧记忆更容易被召回。
    ///
    /// 兜底:
    /// - 查询为空或 BM25 无相关性命中 → 按 `occurred_at`（发生时间）降序回退最近 N 条，
    ///   与既有"无条件取最近几条"语义等价（不丢脉络）。
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
                doc.occurred_at,
                doc.last_accessed_at,
                now_ms,
                doc.salience,
                decay_config,
            );
            scored.push((bm25_raw * retention, doc));
        }

        // 4. 排序：有相关性命中 → 按融合分降序；否则按发生时间降序（最近优先兜底）
        let has_relevance = scored.iter().any(|(s, _)| *s > 0.0);
        if has_relevance {
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        } else {
            // 按发生时间降序（最近优先兜底）：sort_by_key 升序后反转
            scored.sort_by_key(|(_, doc)| doc.occurred_at);
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
                created_at: doc.occurred_at,
                last_accessed_at: doc.last_accessed_at,
                doc_summary: doc.summary.clone(),
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
            let persona_mismatch = request
                .persona_uid
                .as_ref()
                .zip(doc_data.persona_uid.as_ref())
                .is_some_and(|(target_uid, puid)| puid != target_uid);
            if persona_mismatch {
                continue;
            }

            results.push(SearchResult {
                doc_id: doc_data.doc_id,
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
}
