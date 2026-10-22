//! crates/ramaria-memory/src/keyword/index/query.rs - 倒排索引检索与评分
//!
//! 设计特点:
//! - 精确 / 子串 / 并集三种匹配策略；子串经索引词 bigram 倒排定位候选再过滤
//! - 评分 = TF-IDF × Salience × Recency，半衰期可配
//! - 只读查询，不改索引状态

use super::*;

impl KeywordIndex {
    /// 使用自定义评分策略检索。
    pub fn query_with_scorer(
        &self,
        query: &KeywordQuery,
        now_ms: i64,
        scorer: &dyn ScoringStrategy,
    ) -> Vec<(KeywordRef, f64)> {
        if query.keywords.is_empty() || self.docs.is_empty() || query.top_k == 0 {
            return Vec::new();
        }

        // 1. 计算每个文档命中的查询词（含对应策略扩展），得到 doc_idx → idf_sum
        let mut matched: HashMap<usize, f64> = HashMap::new();
        let n_docs = self.docs.len() as f64;

        match query.strategy {
            MatchStrategy::Exact => {
                for qt in query.keywords.iter() {
                    let Some(postings) = self.exact_inverted.get(qt) else {
                        continue;
                    };
                    let df = postings.len() as f64;
                    let idf = Self::idf(n_docs, df);
                    for &idx in postings {
                        *matched.entry(idx).or_insert(0.0) += idf;
                    }
                }
            }
            MatchStrategy::Substring => {
                // 候选词 = 唯一索引词中包含查询 token 者（含精确命中，天然覆盖）。
                // 候选定位经"索引词 bigram 倒排"：含查询词全部 bigram 的索引词（超集），
                // 再 contains 过滤；单字符查询词无 bigram → 退化为全词表扫描。
                let mut candidate_tokens: Vec<&KeywordToken> = Vec::new();
                for qt in query.keywords.iter() {
                    match self.substring_candidates(qt) {
                        Some(candidates) => candidate_tokens.extend(
                            candidates
                                .into_iter()
                                .filter(|indexed| indexed.as_str().contains(qt.as_str())),
                        ),
                        None => candidate_tokens.extend(
                            self.exact_inverted
                                .keys()
                                .filter(|indexed| indexed.as_str().contains(qt.as_str())),
                        ),
                    }
                }
                // 去重候选词（同一 token 被多个查询词命中只计一次 IDF）
                candidate_tokens.sort_by_key(|t| t.as_str());
                candidate_tokens.dedup_by_key(|t| t.as_str());

                for indexed in candidate_tokens {
                    let postings = &self.exact_inverted[indexed];
                    let df = postings.len() as f64;
                    let idf = Self::idf(n_docs, df);
                    for &idx in postings {
                        *matched.entry(idx).or_insert(0.0) += idf;
                    }
                }
            }
        }

        if matched.is_empty() {
            return Vec::new();
        }

        // 2. 评分（idf_sum × salience 加权 × recency 衰减；衰减按 occurred_at 时间基准）。
        //    同分次键（occurred_at）随评分预表化（doc_idx → occurred_at），
        //    排序比较时 O(1) 查询，避免逐对按 KeywordRef 线性扫描文档表。
        let mut occurred_at_by_idx: HashMap<usize, i64> = HashMap::with_capacity(matched.len());
        let mut scored: Vec<(usize, f64)> = Vec::with_capacity(matched.len());
        for (idx, idf_sum) in matched {
            let entry = &self.docs[idx];

            // persona 隔离：查询限定 persona 时，跳过其他 persona 的文档
            if let Some(target) = query.persona_uid.as_deref()
                && !target.is_empty()
            {
                match entry.reff.persona_uid() {
                    Some(puid) if puid != target => continue,
                    // L1 无 persona 绑定：不参与 persona 限定检索
                    None => continue,
                    _ => {}
                }
            }

            let age_days = ((now_ms - entry.occurred_at) as f64) / 86_400_000.0;
            let score = scorer.score(idf_sum, entry.salience, age_days);
            if score > 0.0 {
                occurred_at_by_idx.insert(idx, entry.occurred_at);
                scored.push((idx, score));
            }
        }

        // 3. 排序：得分降序；同分按时间基准降序（最新优先）；再按 label 兜底稳定
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let ca = occurred_at_by_idx.get(&a.0).copied().unwrap_or(0);
                    let cb = occurred_at_by_idx.get(&b.0).copied().unwrap_or(0);
                    cb.cmp(&ca)
                })
                .then_with(|| {
                    self.docs[a.0]
                        .reff
                        .label()
                        .cmp(&self.docs[b.0].reff.label())
                })
        });

        scored.truncate(query.top_k);
        scored
            .into_iter()
            .map(|(idx, score)| (self.docs[idx].reff.clone(), score))
            .collect()
    }

    /// 子串层的候选索引词：包含查询词全部相邻字符 bigram 的索引词（contains 过滤前的超集）。
    ///
    /// 返回:
    /// - `Some(candidates)`: 候选词集合（可能为空——任一 bigram 无倒排即无超集可能）。
    /// - `None`: 查询词字符数 < 2（无 bigram），调用方需退化为全词表扫描。
    ///
    /// 说明:
    /// - 候选仅是超集：任一 bigram 命中不能保证整词为子串（如索引词含全部 bigram
    ///   但不连续），调用方仍须 `contains` 过滤——保证与全扫描逐位等价。
    pub(super) fn substring_candidates<'a>(
        &'a self,
        query: &KeywordToken,
    ) -> Option<Vec<&'a KeywordToken>> {
        let pairs = adjacent_char_pairs(query.as_str());
        if pairs.is_empty() {
            return None;
        }

        let mut sets: Vec<&HashSet<KeywordToken>> = Vec::with_capacity(pairs.len());
        for pair in pairs {
            let Some(set) = self.token_bigrams.get(&pair) else {
                return Some(Vec::new());
            };
            sets.push(set);
        }

        // 从最小集合出发做交集过滤，尽量减少后续 contains 的候选规模
        sets.sort_by_key(|set| set.len());
        let Some((smallest, rest)) = sets.split_first() else {
            return Some(Vec::new());
        };
        Some(
            smallest
                .iter()
                .filter(|token| rest.iter().all(|set| set.contains(*token)))
                .collect(),
        )
    }

    /// IDF = ln(1 + (N - df + 0.5) / (df + 0.5))，df 越大信息量越低。
    pub(super) fn idf(n_docs: f64, df: f64) -> f64 {
        if df <= 0.0 {
            return 0.0;
        }
        ((n_docs - df + 0.5) / (df + 0.5) + 1.0).ln()
    }
}
