//! crates/ramaria-memory/src/bm25/search.rs - BM25 检索与评分
//!
//! 设计特点:
//! - 标准 Okapi BM25 打分，查询按索引实例当前分词器切分
//! - 结果上限检索走最小堆 top-k，避免全量排序
//! - 得分降序、同分按文档标识稳定排序

use super::*;

impl Bm25Index {
    /// 对查询文本执行 BM25 评分，返回所有得分大于 0 的文档。
    ///
    /// 公式: score(D,Q) = Σ_{t∈Q∩D} IDF(t) · (f(t,D)·(k1+1)) / (f(t,D) + k1·(1−b + b·|D|/avgdl))
    ///
    /// 说明:
    /// - 查询按索引实例当前分词器切分（与索引构建口径一致；默认 = 纯 bigram）；
    /// - 需要结果数量上限时用 `search_limited`，避免对全量命中排序与分配。
    ///
    /// 返回按得分降序排列的列表。
    pub fn search(&self, query: &str, config: &Bm25Config) -> Vec<(DocId, f64)> {
        self.search_limited(query, config, 0)
    }

    /// 带结果上限的 BM25 检索。
    ///
    /// 参数:
    /// - `query`: 查询文本，按索引实例当前分词器切分（与索引构建口径一致）；
    /// - `config`: BM25 评分参数；
    /// - `limit`: 结果数量上限；`0` 表示不限制（行为与 `search` 完全一致）。
    ///
    /// 返回:
    /// - 按得分降序排列的 `(DocId, 分数)` 列表；`limit > 0` 时长度不超过 `limit`。
    ///
    /// 说明:
    /// - `limit > 0` 时用容量为 `limit` 的最小堆维护前 `limit` 名，避免对全量
    ///   命中排序与后续分配；仅得分大于 0 的文档参与排名。
    /// - 同分文档按 `doc_id` 兜底排序，保证结果确定。
    pub fn search_limited(
        &self,
        query: &str,
        config: &Bm25Config,
        limit: usize,
    ) -> Vec<(DocId, f64)> {
        let query_tokens = self.tokenize_with(query);
        if query_tokens.is_empty() || self.docs.is_empty() {
            return Vec::new();
        }

        let n = self.docs.len() as f64;
        // 全空库（所有文档均无 token）时平均长度为 0，收紧下限避免除零
        let avgdl = self.avg_doc_len().max(1e-9);

        // 查询词频
        let mut qf: HashMap<&str, u32> = HashMap::new();
        for t in &query_tokens {
            *qf.entry(t.as_str()).or_insert(0) += 1;
        }

        if limit == 0 {
            // 不限制上限：收集全部有分文档后统一降序排序（上层通过 RRF 控制数量）
            let mut scores: Vec<(DocId, f64)> = Vec::with_capacity(self.docs.len());
            for (doc_id, doc) in &self.docs {
                let score = self.score_doc(doc, &qf, n, avgdl, config);
                if score > 0.0 {
                    scores.push((doc_id.clone(), score));
                }
            }
            scores.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            return scores;
        }

        // 限制上限：最小堆维护容量为 limit 的 top-k（堆满且新分更高才替换堆顶）
        let mut heap: BinaryHeap<Reverse<ScoredDoc>> =
            BinaryHeap::with_capacity(limit.min(self.docs.len()));
        for (doc_id, doc) in &self.docs {
            let score = self.score_doc(doc, &qf, n, avgdl, config);
            if score <= 0.0 {
                continue;
            }
            let item = ScoredDoc {
                score,
                doc_id: doc_id.clone(),
            };
            if heap.len() < limit {
                heap.push(Reverse(item));
                continue;
            }
            if let Some(Reverse(min)) = heap.peek() {
                if item > *min {
                    heap.pop();
                    heap.push(Reverse(item));
                }
            }
        }

        // 堆内无固定顺序，导出后统一降序（同分按 doc_id 兜底）
        let mut scores: Vec<(DocId, f64)> = heap
            .into_iter()
            .map(|Reverse(item)| (item.doc_id, item.score))
            .collect();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scores
    }

    /// 计算单篇文档对查询词频表的 BM25 总分。
    ///
    /// 参数:
    /// - `doc`: 待评分文档；
    /// - `qf`: 查询词频表（查询分词后统计）；
    /// - `n`: 文档总数；
    /// - `avgdl`: 平均文档长度（调用方保证为正）；
    /// - `config`: BM25 参数。
    ///
    /// 说明:
    /// - 不在索引中的查询词直接跳过；词频为 0 的查询词也跳过，避免全空字段文档
    ///   （doc_len = 0）在 b = 1 时出现 0/0 = NaN 污染整篇文档总分。
    fn score_doc(
        &self,
        doc: &Bm25Doc,
        qf: &HashMap<&str, u32>,
        n: f64,
        avgdl: f64,
        config: &Bm25Config,
    ) -> f64 {
        let mut score = 0.0_f64;

        for (qt, &q_tf) in qf {
            // 跳过不在索引中的查询词
            let df = match self.df.get(*qt) {
                Some(&d) => d as f64,
                None => continue,
            };

            // 词频为 0 时跳过：全空字段文档（doc_len = 0）在 b = 1 时 BM25 分母为
            // 0，0/0 得到 NaN 会污染该文档总分并被 `score > 0.0` 静默丢弃
            let tf = doc.term_freq.get(*qt).copied().unwrap_or(0) as f64;
            if tf == 0.0 {
                continue;
            }

            // IDF: log((N - df + 0.5) / (df + 0.5) + 1)
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();

            // TF 分量
            let doc_len = doc.doc_len as f64;
            let numerator = tf * (config.k1 + 1.0);
            let denominator = tf + config.k1 * (1.0 - config.b + config.b * doc_len / avgdl);
            let term_score = idf * numerator / denominator;

            score += term_score * (q_tf as f64);
        }

        score
    }
}
