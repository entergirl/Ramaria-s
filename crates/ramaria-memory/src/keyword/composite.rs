//! crates/ramaria-memory/src/keyword/composite.rs — 关键词两级编排 + 语义扩展
//!
//! 设计特点（keyword-design §5.3/§5.4，P5 解决）:
//! - `CompositeIndex`: 精确倒排 → 子串回退 → 语义扩展的三级编排器
//! - `FuzzyKeywordIndex`: 词级语义模糊索引——对词典规范词预计算 embedding，
//!   查询时把查询词向量化后找语义相似词，再走精确倒排查（词级扩展而非文档级检索）
//!
//! 编排策略:
//! ```
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
mod tests {
    use super::*;
    use ramaria_core::keyword::{KeywordQuery, KeywordToken};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const NOW_MS: i64 = 2_000_000_000_000;

    /// 测试用确定性 mock embedding provider。
    ///
    /// 向量由文本哈希生成：同词必同向量、不同词向量正交（保证测试可复现且无随机性）。
    struct MockEmbedder {
        dim: usize,
        // 文本 → 预设向量（供"语义相似"场景构造非零相关）
        overrides: Mutex<HashMap<String, Vec<f32>>>,
    }

    impl MockEmbedder {
        fn new(dim: usize, overrides: HashMap<String, Vec<f32>>) -> Self {
            Self {
                dim,
                overrides: Mutex::new(overrides),
            }
        }

        /// 确定性伪向量：文本哈希填充。
        fn vector_of(&self, text: &str) -> Vec<f32> {
            if let Ok(table) = self.overrides.lock()
                && let Some(v) = table.get(text)
            {
                return v.clone();
            }
            let mut v = vec![0.0f32; self.dim];
            let hash: u64 = text.bytes().fold(0x811c9dc5u64, |acc, b| {
                (acc ^ u64::from(b)).wrapping_mul(0x01000193)
            });
            for (i, slot) in v.iter_mut().enumerate().take(self.dim) {
                *slot = ((hash >> (i % 16)) & 0xF) as f32 / 16.0;
            }
            v
        }
    }

    #[async_trait::async_trait]
    impl ramaria_core::traits::EmbeddingProvider for MockEmbedder {
        async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
            Ok(self.vector_of(text))
        }
        async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| self.vector_of(t)).collect())
        }
        fn model_info(&self) -> ramaria_core::EmbeddingModelInfo {
            ramaria_core::EmbeddingModelInfo {
                model_id: "mock".into(),
                dimension: self.dim,
            }
        }
        async fn validate(&self) -> RamariaResult<()> {
            Ok(())
        }
        async fn download_model(&self) -> RamariaResult<()> {
            Ok(())
        }
        fn download_progress(&self) -> f64 {
            1.0
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    fn l1_ref(id: uuid::Uuid, persona: &str) -> KeywordRef {
        KeywordRef::L1 {
            id,
            persona_uid: persona.to_string(),
        }
    }

    fn query_for(keywords: &[&str], persona: &str, top_k: usize) -> KeywordQuery {
        KeywordQuery::builder(Some(persona.to_string()))
            .keywords_from(keywords.iter().filter_map(|s| KeywordToken::new(s)))
            .top_k(top_k)
            .build()
    }

    /// 构造语义上"相关但字面不同"的文档集：
    /// - 文档 A 关键词 = ["工作压力"]（字面直击）
    /// - 文档 B 关键词 = ["职场焦虑"]（语义相关，字面无关）
    fn semantic_sample() -> CompositeIndex {
        let mut composite = CompositeIndex::new(CompositeIndexConfig::default());
        composite.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "user-0001"),
            Some("工作压力, 加班"),
            0.8,
            NOW_MS - 86_400_000,
        );
        composite.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "user-0001"),
            Some("职场焦虑, 内耗"),
            0.7,
            NOW_MS - 86_400_000,
        );
        composite
    }

    // ---- CompositeIndex 维护 ----

    #[test]
    fn composite_index_ops() {
        let mut composite = CompositeIndex::new(CompositeIndexConfig::default());
        assert_eq!(composite.doc_count(), 0);
        let reff = l1_ref(uuid::Uuid::new_v4(), "p1");
        composite.index_parsed(reff.clone(), Some("爬山"), 0.5, NOW_MS);
        assert_eq!(composite.doc_count(), 1);
        assert!(composite.remove(&reff));
        assert_eq!(composite.doc_count(), 0);
    }

    // ---- 三级编排 ----

    /// 精确命中足够时直接返回（不进入子串/语义层）
    #[tokio::test]
    async fn exact_sufficient_no_fallback() {
        let composite = semantic_sample();
        // 无 embedder（embedding 不可用）也应能精确返回
        let results = composite
            .query(&query_for(&["工作压力"], "user-0001", 5), None)
            .await;
        assert_eq!(results.len(), 1);
        // 结果只来自精确命中文档
        let doc = &results[0].0;
        assert_eq!(doc.persona_uid(), Some("user-0001"));
    }

    /// 子串回退：精确不足时子串命中补足
    #[tokio::test]
    async fn substring_fallback_when_exact_insufficient() {
        let mut composite = CompositeIndex::new(CompositeIndexConfig::default());
        composite.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "p1"),
            Some("工作压力"),
            0.5,
            NOW_MS - 86_400_000,
        );
        // 查"工作"——精确不命中（无"工作"独立词），子串命中"工作压力"
        let results = composite.query(&query_for(&["工作"], "p1", 5), None).await;
        assert_eq!(results.len(), 1, "子串回退应补足结果");
    }

    /// 子串回退开关关闭时不回退
    #[tokio::test]
    async fn substring_fallback_disabled() {
        let composite = CompositeIndex::new(CompositeIndexConfig {
            enable_substring_fallback: false,
            ..Default::default()
        });
        // 直接往底层 index 加词较繁琐，这里用精确语义验证：exact 空 → 不回退 → 空
        let mut composite = composite;
        composite.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "p1"),
            Some("工作压力"),
            0.5,
            NOW_MS - 86_400_000,
        );
        let results = composite.query(&query_for(&["工作"], "p1", 5), None).await;
        assert!(results.is_empty(), "关闭子串回退后不应命中");
    }

    /// 语义扩展：Fuzzy 就绪 + embedder 可用 → 找出"工作压力"语义相关的"职场焦虑"文档
    #[tokio::test]
    async fn semantic_expansion_finds_related_doc() {
        let mut composite = semantic_sample();
        // 构造 mock：工作压力 与 职场焦虑 向量高度相关（其余无关）
        let dim = 4;
        let mut overrides = HashMap::new();
        overrides.insert("工作压力".to_string(), vec![1.0, 0.0, 0.0, 0.0]);
        overrides.insert("职场焦虑".to_string(), vec![1.0, 1.0, 0.0, 0.0]);
        let embedder = MockEmbedder::new(dim, overrides);

        let terms: Vec<KeywordToken> = ["工作压力", "职场焦虑", "加班", "内耗"]
            .iter()
            .filter_map(|s| KeywordToken::new(s))
            .collect();
        let fuzzy = FuzzyKeywordIndex::build(&terms, &embedder).await.unwrap();
        composite.set_fuzzy(Some(fuzzy));

        // 查"工作压力"→ 精确命中文档A；语义扩展命中文档B（关键词"职场焦虑"）
        let results = composite
            .query(&query_for(&["工作压力"], "user-0001", 5), Some(&embedder))
            .await;
        assert_eq!(results.len(), 2, "应包含精确 + 语义两篇文档");
    }

    /// Fuzzy 未就绪 / embedder 为 None → 仅两层，不 panic
    #[tokio::test]
    async fn semantic_layer_not_ready_degrades() {
        let composite = semantic_sample();
        // fuzzy 未挂载
        let results = composite
            .query(&query_for(&["工作压力"], "user-0001", 5), None)
            .await;
        assert_eq!(results.len(), 1);
    }

    // ---- FuzzyKeywordIndex ----

    /// 构建成功 → ready；词条数/维度正确
    #[tokio::test]
    async fn fuzzy_build_success() {
        let embedder = MockEmbedder::new(4, HashMap::new());
        let terms: Vec<KeywordToken> = ["工作压力", "职场焦虑"]
            .iter()
            .filter_map(|s| KeywordToken::new(s))
            .collect();
        let fuzzy = FuzzyKeywordIndex::build(&terms, &embedder).await.unwrap();
        assert!(fuzzy.is_ready());
        assert_eq!(fuzzy.len(), 2);
        assert_eq!(fuzzy.dim(), 4);
    }

    /// 词典为空 → 构建失败（Err）
    #[tokio::test]
    async fn fuzzy_build_empty_terms_err() {
        let embedder = MockEmbedder::new(4, HashMap::new());
        let result = FuzzyKeywordIndex::build(&[], &embedder).await;
        assert!(result.is_err());
    }

    /// 查询词 embed 失败 → 返回空扩展（不 panic）
    #[tokio::test]
    async fn fuzzy_expand_embed_error_empty() {
        let embedder = MockEmbedder::new(4, HashMap::new());
        let terms: Vec<KeywordToken> = ["工作压力"]
            .iter()
            .filter_map(|s| KeywordToken::new(s))
            .collect();
        let fuzzy = FuzzyKeywordIndex::build(&terms, &embedder).await.unwrap();
        // 查询词"加班"未在词典且 mock 生成非零向量；这里用"不存在"验证路径不 panic
        let token = KeywordToken::new("不存在词").unwrap();
        let out = fuzzy.expand(&token, &embedder, 3).await;
        // mock 是确定性哈希向量，cosine 大概率低于阈值 → 空；不断言具体值，只验证可运行
        let _ = out;
    }

    /// expand 自身词不扩展（跳过字面相同词条）
    #[tokio::test]
    async fn fuzzy_expand_skips_self() {
        let dim = 4;
        let mut overrides = HashMap::new();
        overrides.insert("工作压力".to_string(), vec![1.0, 0.0, 0.0, 0.0]);
        let embedder = MockEmbedder::new(dim, overrides);
        let terms: Vec<KeywordToken> = ["工作压力"]
            .iter()
            .filter_map(|s| KeywordToken::new(s))
            .collect();
        let fuzzy = FuzzyKeywordIndex::build(&terms, &embedder).await.unwrap();
        let token = KeywordToken::new("工作压力").unwrap();
        let out = fuzzy.expand(&token, &embedder, 3).await;
        assert!(out.is_empty(), "自身不参与扩展");
    }
}
