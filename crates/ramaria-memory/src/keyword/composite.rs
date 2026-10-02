//! crates/ramaria-memory/src/keyword/composite.rs — 关键词两级编排 + 语义扩展
//!
//! 设计特点（keyword-design §5.3/§5.4，P5 解决）:
//! - `CompositeIndex`: 精确倒排 → 子串回退 → 语义扩展的三级编排器
//! - `FuzzyKeywordIndex`: 词级语义模糊索引——对词典规范词预计算 embedding，
//!   查询时把查询词向量化后找语义相似词，再走精确倒排查（词级扩展而非文档级检索）
//!
//! 编排策略:
//! ```text
//! query(keywords, persona, top_k)
//!   ├─ 1. KeywordIndex.exact_query      （精确命中优先）
//!   │      └─ 结果不足且 enable_substring_fallback → 2
//!   ├─ 2. KeywordIndex.substring_query  （子串回退，填补剩余）
//!   │      └─ 结果不足且 enable_semantic_fallback 且 Fuzzy 就绪 + embedder 可用 → 3
//!   └─ 3. FuzzyKeywordIndex 词级扩展后按扩展词做精确检索（语义补充）
//! ```
//!
//! 设计决策（精确结果与语义结果为何分栏编排而非混排）:
//! - 精确/子串得分的量纲是 TF-IDF，语义扩展的词级相似度是 cosine——两者不可直接比较；
//!   编排模式保证字面命中优先，语义结果仅作降级补充。
//!
//! 降级矩阵（keyword-design §9.3）:
//! - Fuzzy 未构建 / 构建失败 / embedder 不可用 → 仅两层（精确 + 子串）
//! - 词典为空（无规范词）→ Fuzzy 不构建
//! - 全程纯内存零 I/O；embedding 依赖经 `&dyn EmbeddingProvider` 注入，便于 mock

use ramaria_core::keyword::{KeywordQuery, KeywordRef, KeywordSet, KeywordToken, MatchStrategy};
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::{RamariaError, RamariaResult};

use crate::similarity::cosine_similarity;

use super::index::{DefaultScoringStrategy, KeywordIndex};

// =========================================================
// CompositeIndexConfig
// =========================================================

/// CompositeIndex 编排配置。
#[derive(Debug, Clone)]
pub struct CompositeIndexConfig {
    /// 是否启用第 2 级子串回退（默认 true）
    pub enable_substring_fallback: bool,
    /// 是否启用第 3 级语义回退（默认 true；Fuzzy 未就绪 / embedder 不可用时自动跳过）
    pub enable_semantic_fallback: bool,
    /// 语义扩展每查询词最多扩展的相似词数量（默认 3）
    pub semantic_top_k: usize,
}

impl Default for CompositeIndexConfig {
    fn default() -> Self {
        Self {
            enable_substring_fallback: true,
            enable_semantic_fallback: true,
            semantic_top_k: 3,
        }
    }
}

// =========================================================
// FuzzyKeywordIndex
// =========================================================

/// 语义相似词的最低余弦阈值（低于视为不相关，不参与扩展）。
const DEFAULT_SEMANTIC_THRESHOLD: f64 = 0.7;

/// 词级语义模糊索引。
///
/// # 解决的问题
///
/// "职场焦虑" 与 "工作压力" 在字面上无交集，但语义高度相关。精确/子串都无法发现
/// 这种关联，本索引在**词向量空间**里找相似词，实现跨字面关联。
///
/// # 数据结构
///
/// - `entries`: `(KeywordToken, Vec<f32>)`，词 → 预计算向量
/// - 词向量维度由 embedder 决定，构建时统一校验长度
///
/// # 异步构建与就绪门控
///
/// 构建依赖 embedder 对词典全量 `embed_batch`，应在后台异步执行；构建完成前
/// `is_ready() == false`，CompositeIndex 不会进入语义层。
#[derive(Debug, Clone)]
pub struct FuzzyKeywordIndex {
    /// 词 → 向量
    entries: Vec<(KeywordToken, Vec<f32>)>,
    /// 是否已成功构建（就绪）
    ready: bool,
    /// 语义相似度阈值
    similarity_threshold: f64,
}

impl FuzzyKeywordIndex {
    /// 从规范词词典构建词向量索引（异步，依赖 embedder）。
    ///
    /// # 失败语义
    ///
    /// embedder 不可用 / 词典为空 / 返回向量数与词数不一致 → `Err`，
    /// 由调用方降级为"无 Fuzzy 层"（不阻塞主流程）。
    pub async fn build(
        canonical_terms: &[KeywordToken],
        embedder: &dyn EmbeddingProvider,
    ) -> RamariaResult<Self> {
        if canonical_terms.is_empty() {
            return Err(RamariaError::validation(
                "FuzzyKeywordIndex 构建：词典为空，无法构建词向量索引",
            ));
        }

        let texts: Vec<&str> = canonical_terms.iter().map(|t| t.as_str()).collect();
        let vectors = embedder
            .embed_batch(&texts)
            .await
            .map_err(|e| RamariaError::llm(format!("FuzzyKeywordIndex 词向量批量生成失败: {e}")))?;

        if vectors.len() != canonical_terms.len() {
            return Err(RamariaError::validation(format!(
                "FuzzyKeywordIndex 构建：embedder 返回 {} 条向量，期望 {} 条",
                vectors.len(),
                canonical_terms.len()
            )));
        }

        // 维度一致性校验（同一 embedder 输出维度应一致）
        let dim = vectors[0].len();
        if dim == 0 || vectors.iter().any(|v| v.len() != dim) {
            return Err(RamariaError::validation(format!(
                "FuzzyKeywordIndex 构建：向量维度非法（dim={dim}）"
            )));
        }

        let entries: Vec<(KeywordToken, Vec<f32>)> =
            canonical_terms.iter().cloned().zip(vectors).collect();

        tracing::info!(
            entry_count = entries.len(),
            dim,
            "FuzzyKeywordIndex 词向量索引构建完成"
        );

        Ok(Self {
            entries,
            ready: true,
            similarity_threshold: DEFAULT_SEMANTIC_THRESHOLD,
        })
    }

    /// 语义层是否已就绪（构建完成前 CompositeIndex 不进入语义层）。
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// 返回词条数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空（无词条）。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 向量维度（未就绪返回 0）。
    pub fn dim(&self) -> usize {
        self.entries.first().map(|(_, v)| v.len()).unwrap_or(0)
    }

    /// 查询词的语义扩展：找出与查询词向量最相似的 top-k 词典词。
    ///
    /// # 参数
    ///
    /// - `query_token`: 待扩展的查询词。
    /// - `embedder`: 查询词向量生成器（必须与构建时同一模型，否则维度不匹配返回空）。
    /// - `top_k`: 最多返回的相似词数量。
    ///
    /// # 返回
    ///
    /// `Vec<(KeywordToken, f64)>`，按余弦相似度降序。查询词自身（字面相同）不返回，
    /// 过滤低于阈值的弱相关词。
    pub async fn expand(
        &self,
        query_token: &KeywordToken,
        embedder: &dyn EmbeddingProvider,
        top_k: usize,
    ) -> Vec<(KeywordToken, f64)> {
        if !self.ready || self.entries.is_empty() || top_k == 0 {
            return Vec::new();
        }

        let query_vec = match embedder.embed(query_token.as_str()).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error=%e, keyword=%query_token, "查询词向量生成失败，跳过语义扩展");
                return Vec::new();
            }
        };

        let mut scored: Vec<(KeywordToken, f64)> = self
            .entries
            .iter()
            .filter(|(token, _)| token.as_str() != query_token.as_str()) // 自身不做扩展
            .map(|(token, vec)| {
                let sim = cosine_similarity(&query_vec, vec);
                (token.clone(), sim)
            })
            .filter(|(_, sim)| *sim >= self.similarity_threshold)
            .collect();

        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.as_str().cmp(b.0.as_str()))
        });
        scored.truncate(top_k);
        scored
    }
}

// =========================================================
// CompositeIndex
// =========================================================

/// 两级关键词索引编排器（精确 → 子串 → 语义）。
///
/// # 职责
///
/// - 持有底层精确/子串索引（`KeywordIndex`）与可选语义层（`FuzzyKeywordIndex`）
/// - 提供与 `KeywordIndex` 一致的索引维护入口（index/remove/clear），对上层透明
/// - `query` 按"精确优先、逐步回退"编排并去重
#[derive(Debug, Clone)]
pub struct CompositeIndex {
    /// 底层精确/子串倒排索引
    exact: KeywordIndex,
    /// 语义扩展层（未构建 / 失败时为 None）
    fuzzy: Option<FuzzyKeywordIndex>,
    /// 编排配置
    config: CompositeIndexConfig,
}

impl CompositeIndex {
    /// 创建仅含字面层（精确 + 子串）的编排器。
    pub fn new(config: CompositeIndexConfig) -> Self {
        Self {
            exact: KeywordIndex::new(),
            fuzzy: None,
            config,
        }
    }

    /// 挂载语义扩展层（构建失败时调用方可传 None 保持两层降级）。
    pub fn set_fuzzy(&mut self, fuzzy: Option<FuzzyKeywordIndex>) {
        self.fuzzy = fuzzy;
    }

    /// 返回语义层引用（供上层检查就绪状态）。
    pub fn fuzzy(&self) -> Option<&FuzzyKeywordIndex> {
        self.fuzzy.as_ref()
    }

    /// 返回编排配置引用。
    pub fn config(&self) -> &CompositeIndexConfig {
        &self.config
    }

    // =========================================================
    // 索引维护（委托底层 KeywordIndex）
    // =========================================================

    /// 索引一篇文档（幂等：同 reff 覆盖）。
    pub fn index(
        &mut self,
        reff: KeywordRef,
        keywords: KeywordSet,
        salience: f64,
        created_at: i64,
    ) {
        self.exact.index(reff, keywords, salience, created_at);
    }

    /// 解析原始关键词串后索引（返回索引文档总数）。
    pub fn index_parsed(
        &mut self,
        reff: KeywordRef,
        raw_keywords: Option<&str>,
        salience: f64,
        created_at: i64,
    ) -> usize {
        self.exact
            .index_parsed(reff, raw_keywords, salience, created_at)
    }

    /// 移除单文档。
    pub fn remove(&mut self, reff: &KeywordRef) -> bool {
        self.exact.remove(reff)
    }

    /// 批量移除 L1/L2 文档。
    pub fn remove_doc_batch(&mut self, l1_ids: &[uuid::Uuid], l2_ids: &[i64]) -> usize {
        self.exact.remove_doc_batch(l1_ids, l2_ids)
    }

    /// 清空索引（含语义层）。
    pub fn clear(&mut self) {
        self.exact.clear();
        self.fuzzy = None;
    }

    /// 返回已索引文档总数。
    pub fn doc_count(&self) -> usize {
        self.exact.doc_count()
    }

    // =========================================================
    // 查询（三级编排）
    // =========================================================

    /// 三级编排检索。
    ///
    /// # 参数
    ///
    /// - `query`: 检索请求（含关键词集合、persona、策略与 top_k）。
    /// - `embedder`: 语义扩展用向量生成器。传 `None`（embedding 不可用）时
    ///   自动跳过语义层，退化为"精确 + 子串"两层（符合静默降级红线）。
    ///
    /// # 返回
    ///
    /// 去重后的 `(KeywordRef, score)` 列表，最多 `query.top_k` 条。
    pub async fn query(
        &self,
        query: &KeywordQuery,
        embedder: Option<&dyn EmbeddingProvider>,
    ) -> Vec<(KeywordRef, f64)> {
        let needed = query.top_k;
        if query.keywords.is_empty() || needed == 0 {
            return Vec::new();
        }

        // recency 衰减的"当前时间"统一取自系统时钟（与 KeywordIndex 检索口径一致）
        let now_ms = ramaria_core::types::now_ms();
        let scorer = DefaultScoringStrategy::default();

        // ---- 第 1 级：精确匹配 ----
        let mut results: Vec<(KeywordRef, f64)> = Vec::with_capacity(needed);
        let exact_query = KeywordQuery {
            keywords: query.keywords.clone(),
            persona_uid: query.persona_uid.clone(),
            top_k: needed,
            strategy: MatchStrategy::Exact,
        };
        results.extend(self.exact.query_with_scorer(&exact_query, now_ms, &scorer));

        // ---- 第 2 级：子串回退 ----
        if results.len() < needed && self.config.enable_substring_fallback {
            let substring_query = KeywordQuery {
                keywords: query.keywords.clone(),
                persona_uid: query.persona_uid.clone(),
                top_k: needed,
                strategy: MatchStrategy::Substring,
            };
            let extra = self
                .exact
                .query_with_scorer(&substring_query, now_ms, &scorer);
            append_unique(&mut results, extra, needed);
        }

        // ---- 第 3 级：语义扩展（词级） ----
        if results.len() < needed
            && self.config.enable_semantic_fallback
            && let Some(fuzzy) = self.fuzzy.as_ref()
            && fuzzy.is_ready()
            && let Some(embedder) = embedder
        {
            let mut expansion_tokens: Vec<KeywordToken> = Vec::new();
            for query_token in query.keywords.iter() {
                let expanded = fuzzy
                    .expand(query_token, embedder, self.config.semantic_top_k)
                    .await;
                expansion_tokens.extend(expanded.into_iter().map(|(t, _)| t));
            }
            if !expansion_tokens.is_empty() {
                let mut expansions: KeywordSet = KeywordSet::new();
                for t in expansion_tokens {
                    expansions.insert(t);
                }
                let semantic_query = KeywordQuery {
                    keywords: expansions,
                    persona_uid: query.persona_uid.clone(),
                    top_k: needed,
                    strategy: MatchStrategy::Exact,
                };
                let extra = self
                    .exact
                    .query_with_scorer(&semantic_query, now_ms, &scorer);
                append_unique(&mut results, extra, needed);
            }
        }

        // 由于 KeywordIndex 的排序为"分数降序 + 同分 created_at 降序"，编排追加的结果
        // 依层级保持优先级：先精确命中、再子串、最后语义，保证字面命中优先。
        results.truncate(needed);
        results
    }
}

/// 把 `extra` 中未出现的文档追加到 `results`（去重，保留 extra 的分数顺序）。
fn append_unique(
    results: &mut Vec<(KeywordRef, f64)>,
    extra: Vec<(KeywordRef, f64)>,
    needed: usize,
) {
    if extra.is_empty() || results.len() >= needed {
        return;
    }
    for (reff, score) in extra {
        if results.iter().any(|(r, _)| r == &reff) {
            continue;
        }
        results.push((reff, score));
        if results.len() >= needed {
            break;
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
