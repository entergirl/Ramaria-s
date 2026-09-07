//! crates/ramaria-memory/src/keyword/service.rs - 关键词子系统应用层服务
//!
//! 设计特点:
//! - `KeywordService` 持有 `KeywordPool`（词典三态状态机）与 `CompositeIndex`
//!   （关键词倒排镜像，以 `Arc` 持有），对外提供装载 / 增量维护 / 查询所需的只读访问器
//! - 镜像侧增强：文档视图（L1/L2）与词典词条装载为纯内存镜像，不改动
//!   Retriever / Chat 检索主链，也不重复写库（持久化由既有 summarizer / storage 负责）
//! - `reset_docs_from_views` 与 `rebuild_retriever` 同源装载；`index_l1/index_l2/
//!   remove_*` 提供增量维护（幂等）
//! - `composite` 以 `Arc<CompositeIndex>` 持有：调用方可廉价取到只读共享引用后，
//!   在 std 锁外 `await` 异步语义查询（避免读锁跨 await），写路径经
//!   `Arc::make_mut` 在写锁内完成（多读单写安全）。
//! - `rebuild_fuzzy` 用规范词构建词级语义索引：embedding 不可用 / 词典为空 /
//!   构建失败 → 置 None 保持"精确 + 子串"两层降级（静默 warn，不抛错）
//! - 纯内存零 I/O；`KeywordPoolRow`（core 数据行）为装载边界，不依赖数据库

use std::collections::HashMap;
use std::sync::Arc;

use ramaria_core::keyword::{
    KeywordPoolRow, KeywordQuery, KeywordRef, KeywordSet, KeywordStatus, KeywordToken,
};
use ramaria_core::traits::EmbeddingProvider;

use crate::retriever::{L1DocView, L2DocView};

use super::composite::{CompositeIndex, CompositeIndexConfig, FuzzyKeywordIndex};
use super::normalizer::{BigramNormalizer, BigramWithDictionaryNormalizer, KeywordNormalizer};
use super::pool::{KeywordPool, PoolEntry};

// =========================================================
// KeywordService
// =========================================================

/// 关键词子系统应用层服务——词典池 + 关键词倒排镜像的统一持有者。
///
/// 职责:
/// - 从 keyword_pool 词条装载 `KeywordPool`（canonical / alias / pending 三态）；
/// - 从 L1/L2 文档视图装载 / 增量维护 `CompositeIndex`（文档级关键词倒排）；
/// - 对外暴露 `pool()` / `composite()` 只读访问器，供上层（M4 检索融合）查询。
///
/// 状态:
/// - 启动 / 重建路径：`load_pool_entries` + `reset_docs_from_views`（全量覆盖）。
/// - L1 摘要生成后：`index_l1` + `upsert_pool_tokens`（增量累积）。
/// - 清理路径：`remove_l1` / `remove_l2` / `remove_doc_batch`（幂等）。
///
/// 线程模型:
/// - 本结构非自身加锁的纯内存对象，由上层以 `Arc<RwLock<KeywordService>>` 共享；
///   写操作需 `&mut self`，查询经只读访问器在写锁外完成。
#[derive(Debug, Clone)]
pub struct KeywordService {
    /// 词典三态状态机（词条缓存）
    pool: KeywordPool,
    /// 关键词倒排镜像（精确 + 子串 + 可选语义层；Arc 支持锁外异步查询共享）
    composite: Arc<CompositeIndex>,
}

impl KeywordService {
    /// 创建空服务（空词典 + 空倒排镜像）。
    pub fn new() -> Self {
        Self {
            pool: KeywordPool::new(),
            composite: Arc::new(CompositeIndex::new(CompositeIndexConfig::default())),
        }
    }

    // =========================================================
    // 只读访问器（供检索融合使用）
    // =========================================================

    /// 词典池只读引用。
    pub fn pool(&self) -> &KeywordPool {
        &self.pool
    }

    /// 关键词倒排镜像只读引用。
    pub fn composite(&self) -> &CompositeIndex {
        self.composite.as_ref()
    }

    /// 关键词倒排镜像共享引用（廉价 clone Arc，供锁外异步查询）。
    ///
    /// 说明:
    /// - 镜像为不可变数据；写路径经 `Arc::make_mut` 在写锁内替换，
    ///   读侧持有的 Arc 始终指向一致快照（多读单写安全）。
    pub fn composite_arc(&self) -> Arc<CompositeIndex> {
        Arc::clone(&self.composite)
    }

    /// 词典池快照（词条文本 + 别名解析表；供锁外异步查询 / 路由查询侧规范化）。
    pub fn pool_snapshot(&self) -> KeywordPoolSnapshot {
        KeywordPoolSnapshot::from_pool(&self.pool)
    }

    /// 镜像已索引文档总数。
    pub fn doc_count(&self) -> usize {
        self.composite.doc_count()
    }

    /// 词典池词条总数。
    pub fn pool_len(&self) -> usize {
        self.pool.len()
    }

    // =========================================================
    // 词典装载与累积
    // =========================================================

    /// 全量装载词典池词条（重置式：覆盖既有词条缓存）。
    ///
    /// 说明:
    /// - 把 `keyword_pool` 表词条行映射为三态 `PoolEntry`；`alias_status` 语义：
    ///   `NULL`/`"canonical"` → 规范词；`"alias"` → 已确认别名；
    ///   `"pending"` → 待确认别名。
    /// - alias / pending 行缺少 `canonical_id`（数据异常）时兜底为 Canonical，不丢弃词条。
    pub fn load_pool_entries(&mut self, rows: &[KeywordPoolRow]) {
        let entries: Vec<PoolEntry> = rows.iter().filter_map(to_pool_entry).collect();
        self.pool = KeywordPool::from_entries(entries);
    }

    /// 增量累积词典词条（镜像 use_count +1 / 刷新最近使用时间）。
    ///
    /// 参数:
    /// - `tokens`: 新出现的关键词（已标准化、通常来自文档 keywords 字段解析）。
    /// - `now_ms`: 出现时间（Unix 毫秒）。
    ///
    /// 返回: 参与累积的词条数（`tokens.len()`）。
    ///
    /// 说明:
    /// - 仅内存镜像累积；keyword_pool 持久化由既有 summarizer / storage 负责，不重复写库。
    pub fn upsert_pool_tokens(&mut self, tokens: &[KeywordToken], now_ms: i64) -> usize {
        for token in tokens {
            self.pool.upsert(token.clone(), now_ms);
        }
        tokens.len()
    }

    /// 返回规范词列表（按 use_count 降序），供语义层构建。
    pub fn canonical_terms(&self) -> Vec<KeywordToken> {
        self.pool.list_canonicals().into_iter().cloned().collect()
    }

    // =========================================================
    // 文档视图装载与增量维护
    // =========================================================

    /// 清空倒排镜像并以视图全量装载（重置式，幂等）。
    ///
    /// 说明:
    /// - 清空 `CompositeIndex`（含既有语义层，保持与当前词典一致），
    ///   再按 L1/L2 视图解析 keywords 字段（逗号分隔串）逐条索引。
    /// - L1 persona 为 None 时以空串兜底（KeywordRef 契约为 String）；
    ///   全局检索（persona 不过滤）仍可命中，persona 限定检索按隔离规则过滤。
    pub fn reset_docs_from_views(&mut self, l1: &[L1DocView], l2: &[L2DocView]) {
        let mut composite = CompositeIndex::new(CompositeIndexConfig::default());
        for doc in l1 {
            composite.index_parsed(
                self.l1_ref(doc),
                doc.keywords.as_deref(),
                doc.salience,
                doc.created_at,
            );
        }
        for doc in l2 {
            composite.index_parsed(
                self.l2_ref(doc),
                doc.keywords.as_deref(),
                doc.salience,
                doc.created_at,
            );
        }
        self.composite = Arc::new(composite);
    }

    /// 清空倒排镜像（保留词典池；通常由下一轮全量装载接管）。
    pub fn clear_docs(&mut self) {
        self.composite = Arc::new(CompositeIndex::new(CompositeIndexConfig::default()));
    }

    /// 增量索引一篇 L1 文档（幂等：同文档覆盖）。
    pub fn index_l1(&mut self, doc: &L1DocView) -> usize {
        let reff = self.l1_ref(doc);
        Arc::make_mut(&mut self.composite).index_parsed(
            reff,
            doc.keywords.as_deref(),
            doc.salience,
            doc.created_at,
        );
        self.composite.doc_count()
    }

    /// 增量索引一篇 L2 事件文档（幂等：同文档覆盖）。
    pub fn index_l2(&mut self, doc: &L2DocView) -> usize {
        let reff = self.l2_ref(doc);
        Arc::make_mut(&mut self.composite).index_parsed(
            reff,
            doc.keywords.as_deref(),
            doc.salience,
            doc.created_at,
        );
        self.composite.doc_count()
    }

    /// 按 L1 id 移除镜像文档（跨 persona 安全：按 id 而非全等引用匹配）。
    pub fn remove_l1(&mut self, id: uuid::Uuid) -> bool {
        Arc::make_mut(&mut self.composite).remove_doc_batch(&[id], &[]) > 0
    }

    /// 按 L2 id 移除镜像文档（跨 persona 安全：按 id 而非全等引用匹配）。
    pub fn remove_l2(&mut self, id: i64) -> bool {
        Arc::make_mut(&mut self.composite).remove_doc_batch(&[], &[id]) > 0
    }

    /// 批量移除 L1/L2 镜像文档（幂等：不存在的 id 忽略）。
    pub fn remove_doc_batch(&mut self, l1_ids: &[uuid::Uuid], l2_ids: &[i64]) -> usize {
        Arc::make_mut(&mut self.composite).remove_doc_batch(l1_ids, l2_ids)
    }

    // =========================================================
    // 语义层（Fuzzy）
    // =========================================================

    /// 直接挂载语义扩展层（构建由调用方在锁外异步完成后写入，避免写锁跨 await）。
    pub fn set_fuzzy(&mut self, fuzzy: Option<FuzzyKeywordIndex>) {
        Arc::make_mut(&mut self.composite).set_fuzzy(fuzzy);
    }

    /// 用规范词构建语义扩展层并挂载。
    ///
    /// 降级语义（静默 warn，不抛错）:
    /// - embedder 为 None（embedding 不可用）→ 置 None，保持"精确 + 子串"两层。
    /// - 词典为空（无规范词）→ 置 None（无词向量可构建）。
    /// - 构建失败（批量向量化失败 / 返回数不一致 / 维度非法）→ 置 None。
    ///
    /// 说明:
    /// - 本方法接收 `&mut self` 并内部 await；若调用方以锁保护服务，应在锁外取
    ///   `canonical_terms()` + await 构建 + 锁内 `set_fuzzy`，避免 std 写锁跨 await。
    pub async fn rebuild_fuzzy(
        &mut self,
        canonical_terms: &[KeywordToken],
        embedder: Option<&dyn EmbeddingProvider>,
    ) {
        let Some(embedder) = embedder else {
            self.set_fuzzy(None);
            tracing::debug!("关键词语义层跳过：embedding 不可用（两层降级）");
            return;
        };
        if canonical_terms.is_empty() {
            self.set_fuzzy(None);
            tracing::debug!("关键词语义层跳过：词典为空（无规范词）");
            return;
        }
        match FuzzyKeywordIndex::build(canonical_terms, embedder).await {
            Ok(fuzzy) => {
                self.set_fuzzy(Some(fuzzy));
                tracing::info!(entry_count = canonical_terms.len(), "关键词语义层构建完成");
            }
            Err(e) => {
                self.set_fuzzy(None);
                tracing::warn!(error = %e, "关键词语义层构建失败（两层降级）");
            }
        }
    }

    // =========================================================
    // 内部转换
    // =========================================================

    /// L1 视图 → KeywordRef（persona None 兜底空串）。
    fn l1_ref(&self, doc: &L1DocView) -> KeywordRef {
        KeywordRef::L1 {
            id: doc.id,
            persona_uid: doc.persona_uid.clone().unwrap_or_default(),
        }
    }

    /// L2 视图 → KeywordRef。
    fn l2_ref(&self, doc: &L2DocView) -> KeywordRef {
        KeywordRef::L2 {
            id: doc.id,
            persona_uid: doc.persona_uid.clone(),
        }
    }
}

impl Default for KeywordService {
    fn default() -> Self {
        Self::new()
    }
}

// =========================================================
// 词典池快照
// =========================================================

/// 词典池快照——词条文本与别名解析表的只读值形态。
///
/// 职责:
/// - 供调用方在 std 读锁内一次性取出、释放锁后在 `await` 异步查询 / 路由评分中使用，
///   避免持锁跨 await。
/// - `dictionary`: 全量词条文本（canonical + alias + pending），驱动词典增强分词。
/// - `resolve`: 别名 / 待确认词条 → 规范词文本（canonical 自身不收录）。
#[derive(Debug, Clone, Default)]
pub struct KeywordPoolSnapshot {
    /// 全量词条文本（词典增强分词的候选完整词）
    dictionary: Vec<String>,
    /// 别名 → 规范词文本
    resolve: HashMap<String, String>,
}

impl KeywordPoolSnapshot {
    /// 从词典池构造快照。
    pub fn from_pool(pool: &KeywordPool) -> Self {
        let mut dictionary = Vec::with_capacity(pool.len());
        let mut resolve = HashMap::new();
        for entry in pool.iter() {
            let text = entry.token.as_str().to_string();
            dictionary.push(text.clone());
            if let Some(canonical) = pool.resolve(&entry.token)
                && canonical != &entry.token
            {
                // canonical 自反不收录（仅保留别名/待确认的归一映射）
                resolve.insert(text, canonical.as_str().to_string());
            }
        }
        Self {
            dictionary,
            resolve,
        }
    }

    /// 词典是否为空（为空时查询退化为纯 bigram 口径）。
    pub fn is_empty(&self) -> bool {
        self.dictionary.is_empty()
    }

    /// 全量词条文本。
    pub fn dictionary(&self) -> &[String] {
        &self.dictionary
    }

    /// 别名解析表（token 文本 → 规范词文本）。
    pub fn resolve(&self) -> &HashMap<String, String> {
        &self.resolve
    }
}

// =========================================================
// 关键词镜像自由文本查询（锁外异步）
// =========================================================

/// 用自由文本查询关键词镜像，返回可与 Retriever label 融合的 `(label, score)`。
///
/// 说明:
/// - 查询词构造：词典增强分词（词典 = 池全量词条，空池退化为纯 bigram），
///   并对命中别名/待确认词条的 token 追加其规范词（union，提升召回）；
///   无有效查询词 → 空。
/// - 检索经 `CompositeIndex.query` 三级编排（精确 → 子串 → 语义），
///   embedder 为 None（embedding 不可用）时自动两层降级。
/// - 输出 label 复用 Retriever 向量通道格式（`L1:{uuid}` / `L2:{id}`），
///   经既有 `parse_doc_label` 解析；`Pool` 词典词条不产出。
///
/// 用法:
/// - 调用方先在锁内取 `composite_arc()` 与 `pool_snapshot()`，释放锁后传入本函数
///   （避免 std 读锁跨 await），返回结果以纯数据交给检索融合。
pub async fn query_text_labels(
    composite: &CompositeIndex,
    pool: &KeywordPoolSnapshot,
    text: &str,
    persona_uid: Option<&str>,
    embedder: Option<&dyn EmbeddingProvider>,
    top_k: usize,
) -> Vec<(String, f64)> {
    if text.trim().is_empty() || top_k == 0 || composite.doc_count() == 0 {
        return Vec::new();
    }

    // 查询词集合（词典增强 + 别名归一扩展 + 去重）
    let mut set: KeywordSet = KeywordSet::new();
    if pool.is_empty() {
        // 空词典：纯 bigram 口径（与既有 bm25 tokenize 等价）
        for token in BigramNormalizer.normalize(text) {
            set.insert(token);
        }
    } else {
        let dict_normalizer = BigramWithDictionaryNormalizer::from_dictionary(pool.dictionary());
        for token in dict_normalizer.normalize(text) {
            set.insert(token.clone());
            if let Some(canonical) = pool.resolve().get(token.as_str())
                && let Some(ct) = KeywordToken::new(canonical)
            {
                set.insert(ct);
            }
        }
    }
    if set.is_empty() {
        return Vec::new();
    }

    let query = KeywordQuery::builder(persona_uid.map(|s| s.to_string()))
        .keywords_from(set)
        .top_k(top_k)
        .build();
    let hits = composite.query(&query, embedder).await;
    hits.into_iter()
        .filter_map(|(reff, score)| keyword_ref_label(&reff).map(|label| (label, score)))
        .collect()
}

/// KeywordRef → 检索 label（`L1:{uuid}` / `L2:{id}`；Pool 词典词条不产出）。
fn keyword_ref_label(reff: &KeywordRef) -> Option<String> {
    match reff {
        KeywordRef::L1 { id, .. } => Some(format!("L1:{id}")),
        KeywordRef::L2 { id, .. } => Some(format!("L2:{id}")),
        KeywordRef::Pool { .. } => None,
    }
}

// =========================================================
// 行装载辅助
// =========================================================

/// keyword_pool 行 → 池词条（无效关键词文本 → None，静默过滤）。
fn to_pool_entry(row: &KeywordPoolRow) -> Option<PoolEntry> {
    let token = KeywordToken::new(&row.keyword)?;
    Some(PoolEntry {
        rowid: row.rowid,
        token,
        use_count: row.use_count,
        last_used_at: row.created_at,
        created_at: row.created_at,
        status: pool_status_from_row(row),
    })
}

/// 别名状态文本 → 三态（防御：alias/pending 缺 canonical_id 时兜底 Canonical）。
fn pool_status_from_row(row: &KeywordPoolRow) -> KeywordStatus {
    match row.alias_status.as_deref() {
        Some("alias") => row
            .canonical_id
            .map(|canonical_id| KeywordStatus::Alias { canonical_id })
            .unwrap_or(KeywordStatus::Canonical),
        Some("pending") => row
            .canonical_id
            .map(|suggested_canonical_id| KeywordStatus::Pending {
                suggested_canonical_id,
            })
            .unwrap_or(KeywordStatus::Canonical),
        _ => KeywordStatus::Canonical,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::error::RamariaResult;
    use ramaria_core::keyword::{KeywordQuery, KeywordToken};

    const NOW_MS: i64 = 2_000_000_000_000;

    fn token(s: &str) -> KeywordToken {
        KeywordToken::new(s).unwrap()
    }

    fn row(
        rowid: i64,
        keyword: &str,
        status: Option<&str>,
        canonical_id: Option<i64>,
    ) -> KeywordPoolRow {
        KeywordPoolRow {
            rowid,
            keyword: keyword.to_string(),
            use_count: 1,
            created_at: rowid,
            alias_status: status.map(|s| s.to_string()),
            canonical_id,
            canonical_keyword: None,
        }
    }

    fn sample_rows() -> Vec<KeywordPoolRow> {
        vec![
            row(1, "工作压力", None, None),
            row(2, "职场焦虑", Some("pending"), Some(1)),
            row(3, "职业倦怠", Some("alias"), Some(1)),
            row(4, "爬山", Some("canonical"), None),
        ]
    }

    fn l1_view(id: uuid::Uuid, persona: Option<&str>, keywords: Option<&str>) -> L1DocView {
        L1DocView {
            id,
            summary: String::new(),
            keywords: keywords.map(|s| s.to_string()),
            persona_uid: persona.map(|s| s.to_string()),
            created_at: NOW_MS,
            salience: 0.8,
            last_accessed_at: None,
        }
    }

    fn l2_view(id: i64, persona: &str, keywords: Option<&str>) -> L2DocView {
        L2DocView {
            id,
            title: String::new(),
            summary: String::new(),
            keywords: keywords.map(|s| s.to_string()),
            attitude: None,
            paraphrase: None,
            persona_uid: persona.to_string(),
            share: 0.5,
            confidence: 0.7,
            created_at: NOW_MS,
            salience: 0.6,
        }
    }

    fn query_for(keywords: &[&str], persona: Option<&str>) -> KeywordQuery {
        KeywordQuery::builder(persona.map(|s| s.to_string()))
            .keywords_from(keywords.iter().filter_map(|s| KeywordToken::new(s)))
            .top_k(20)
            .build()
    }

    /// 测试用确定性 embedding（可注入失败）。
    struct MockEmbedder {
        fail_batch: bool,
    }

    impl MockEmbedder {
        fn ok() -> Self {
            Self { fail_batch: false }
        }
        fn failing() -> Self {
            Self { fail_batch: true }
        }
        fn vector(text: &str) -> Vec<f32> {
            // 同词同向量（词条间保证可区分：取哈希前 4 维）
            let hash: u64 = text.bytes().fold(0x811c9dc5u64, |acc, b| {
                (acc ^ u64::from(b)).wrapping_mul(0x01000193)
            });
            vec![
                (hash & 0xFF) as f32 / 255.0,
                ((hash >> 8) & 0xFF) as f32 / 255.0,
                ((hash >> 16) & 0xFF) as f32 / 255.0,
                ((hash >> 24) & 0xFF) as f32 / 255.0,
            ]
        }
    }

    #[async_trait::async_trait]
    impl ramaria_core::traits::EmbeddingProvider for MockEmbedder {
        async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
            Ok(Self::vector(text))
        }
        async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
            if self.fail_batch {
                return Err(ramaria_core::RamariaError::llm("mock 批量向量化失败"));
            }
            Ok(texts.iter().map(|t| Self::vector(t)).collect())
        }
        fn model_info(&self) -> ramaria_core::EmbeddingModelInfo {
            ramaria_core::EmbeddingModelInfo {
                model_id: "mock-service".into(),
                dimension: 4,
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

    // ---- 词典装载与三态映射 ----

    #[test]
    fn load_pool_entries_maps_three_states() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        assert_eq!(svc.pool_len(), 4);

        // canonical（NULL 与 "canonical" 形态）解析自身
        assert_eq!(
            svc.pool().resolve(&token("工作压力")).unwrap().as_str(),
            "工作压力"
        );
        assert_eq!(svc.pool().resolve(&token("爬山")).unwrap().as_str(), "爬山");
        // pending / alias 解析到规范词
        assert_eq!(
            svc.pool().resolve(&token("职场焦虑")).unwrap().as_str(),
            "工作压力"
        );
        assert_eq!(
            svc.pool().resolve(&token("职业倦怠")).unwrap().as_str(),
            "工作压力"
        );
        // 规范词列表仅 Canonical（排除 pending/alias）
        assert_eq!(svc.pool().list_canonicals().len(), 2);
    }

    #[test]
    fn load_pool_entries_handles_invalid_and_missing_canonical() {
        let mut svc = KeywordService::new();
        let rows = vec![
            row(1, "  ", None, None),                // 无效文本 → 过滤
            row(2, "孤儿别名", Some("alias"), None), // 缺 canonical_id → 兜底 Canonical
        ];
        svc.load_pool_entries(&rows);
        assert_eq!(svc.pool_len(), 1);
        assert!(svc.pool().entry_by_id(2).unwrap().status.is_canonical());
    }

    #[test]
    fn upsert_pool_tokens_accumulates_mirror() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        let before = svc
            .pool()
            .entry_by_token(&token("工作压力"))
            .unwrap()
            .use_count;
        svc.upsert_pool_tokens(&[token("工作压力"), token("新词")], NOW_MS);
        assert_eq!(
            svc.pool()
                .entry_by_token(&token("工作压力"))
                .unwrap()
                .use_count,
            before + 1
        );
        assert_eq!(
            svc.pool().entry_by_token(&token("新词")).unwrap().use_count,
            1
        );
    }

    // ---- 文档装载与增量维护 ----

    #[test]
    fn reset_docs_from_views_indexes_l1_and_l2() {
        let mut svc = KeywordService::new();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        svc.reset_docs_from_views(
            &[
                l1_view(id1, Some("p1"), Some("工作压力, 加班")),
                l1_view(id2, None, Some("爬山")), // persona None → 空串兜底
            ],
            &[l2_view(7, "p2", Some("职场, 内耗"))],
        );
        assert_eq!(svc.doc_count(), 3);

        // reset 幂等：重复装载不累积文档（同 id 覆盖 / 全量重建）
        svc.reset_docs_from_views(
            &[l1_view(id1, Some("p1"), Some("工作压力, 加班"))],
            &[l2_view(7, "p2", Some("职场, 内耗"))],
        );
        assert_eq!(svc.doc_count(), 2);
    }

    #[test]
    fn incremental_index_and_remove_are_idempotent() {
        let mut svc = KeywordService::new();
        let id = uuid::Uuid::new_v4();
        let doc = l1_view(id, Some("p1"), Some("失眠, 焦虑"));
        svc.index_l1(&doc);
        svc.index_l1(&doc); // 同文档覆盖
        assert_eq!(svc.doc_count(), 1);

        assert!(svc.remove_l1(id));
        assert!(!svc.remove_l1(id), "重复移除返回 false");
        assert_eq!(svc.doc_count(), 0);

        // 批量移除幂等
        svc.index_l1(&doc);
        svc.index_l2(&l2_view(9, "p2", Some("加班")));
        assert_eq!(svc.remove_doc_batch(&[id], &[9]), 2);
        assert_eq!(svc.remove_doc_batch(&[id], &[9]), 0, "幂等：不存在即忽略");
    }

    // ---- persona 过滤 ----

    #[tokio::test]
    async fn persona_filter_keeps_isolation() {
        let mut svc = KeywordService::new();
        let p1_doc = uuid::Uuid::new_v4();
        let p2_doc = uuid::Uuid::new_v4();
        svc.reset_docs_from_views(
            &[
                l1_view(p1_doc, Some("p1"), Some("工作压力")),
                l1_view(p2_doc, Some("p2"), Some("工作压力")),
            ],
            &[],
        );
        // persona 限定：只命中本 persona 文档
        let r1 = svc
            .composite()
            .query(&query_for(&["工作压力"], Some("p1")), None)
            .await;
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].0.persona_uid(), Some("p1"));
        // 全局（None）命中全部
        let rg = svc
            .composite()
            .query(&query_for(&["工作压力"], None), None)
            .await;
        assert_eq!(rg.len(), 2);
    }

    // ---- 语义层降级 ----

    #[tokio::test]
    async fn rebuild_fuzzy_embedder_none_keeps_two_layers() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        svc.rebuild_fuzzy(&svc.canonical_terms(), None).await;
        assert!(svc.composite().fuzzy().is_none());
        // 两层仍可查询（不含 fuzzy）
        let id = uuid::Uuid::new_v4();
        svc.index_l1(&l1_view(id, Some("p1"), Some("工作压力")));
        let r = svc
            .composite()
            .query(&query_for(&["工作压力"], Some("p1")), None)
            .await;
        assert_eq!(r.len(), 1);
    }

    #[tokio::test]
    async fn rebuild_fuzzy_empty_terms_keeps_two_layers() {
        let mut svc = KeywordService::new();
        let embedder = MockEmbedder::ok();
        svc.rebuild_fuzzy(&[], Some(&embedder)).await;
        assert!(svc.composite().fuzzy().is_none());
    }

    #[tokio::test]
    async fn rebuild_fuzzy_success_mounts_layer() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        let embedder = MockEmbedder::ok();
        let terms = svc.canonical_terms();
        svc.rebuild_fuzzy(&terms, Some(&embedder)).await;
        let fuzzy = svc.composite().fuzzy().expect("应挂载语义层");
        assert!(fuzzy.is_ready());
        assert_eq!(fuzzy.len(), terms.len());
    }

    #[tokio::test]
    async fn rebuild_fuzzy_failure_degrades_to_none() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        let embedder = MockEmbedder::failing();
        svc.rebuild_fuzzy(&svc.canonical_terms(), Some(&embedder))
            .await;
        assert!(svc.composite().fuzzy().is_none());
    }

    #[tokio::test]
    async fn set_fuzzy_replaces_layer() {
        let mut svc = KeywordService::new();
        let embedder = MockEmbedder::ok();
        let terms = vec![token("工作压力"), token("职场焦虑")];
        let fuzzy = FuzzyKeywordIndex::build(&terms, &embedder).await.unwrap();
        svc.set_fuzzy(Some(fuzzy));
        assert!(svc.composite().fuzzy().is_some());
        svc.set_fuzzy(None);
        assert!(svc.composite().fuzzy().is_none());
    }

    // ---- 词典快照 / 自由文本查询（检索融合接线） ----

    #[test]
    fn pool_snapshot_contains_dict_and_alias_resolve() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        let snapshot = svc.pool_snapshot();
        // 词典含全量词条（canonical + alias + pending）
        assert_eq!(snapshot.dictionary.len(), 4);
        assert!(snapshot.dictionary.iter().any(|d| d == "工作压力"));
        assert!(snapshot.dictionary.iter().any(|d| d == "职业倦怠"));
        // 解析表仅含别名/待确认 → canonical
        assert_eq!(
            snapshot.resolve().get("职场焦虑").map(String::as_str),
            Some("工作压力")
        );
        assert_eq!(
            snapshot.resolve().get("职业倦怠").map(String::as_str),
            Some("工作压力")
        );
        assert!(
            snapshot.resolve().get("工作压力").is_none(),
            "canonical 不收录自反映射"
        );
    }

    #[test]
    fn composite_arc_shares_same_mirror() {
        let mut svc = KeywordService::new();
        let id = uuid::Uuid::new_v4();
        svc.index_l1(&l1_view(id, Some("p1"), Some("爬山")));
        let arc = svc.composite_arc();
        assert_eq!(arc.doc_count(), 1);
        // Arc 与 service 指向同一份镜像（写后旧 Arc 仍指向旧快照）
        svc.clear_docs();
        assert_eq!(arc.doc_count(), 1, "旧 Arc 快照不受后续写影响");
        assert_eq!(svc.doc_count(), 0);
    }

    /// 别名短语查询：用户文本含别名词 → 命中规范词关键词文档（label 与 retriever 兼容）。
    #[tokio::test]
    async fn query_text_labels_aliases_to_canonical_doc() {
        let mut svc = KeywordService::new();
        // 词典：canonical 工作压力 + pending 职场焦虑 → 工作压力
        svc.load_pool_entries(&sample_rows());
        let doc_id = uuid::Uuid::new_v4();
        svc.index_l1(&l1_view(doc_id, Some("p1"), Some("工作压力")));

        let hits = query_text_labels(
            svc.composite(),
            &svc.pool_snapshot(),
            "最近职场焦虑",
            Some("p1"),
            None,
            10,
        )
        .await;
        assert_eq!(hits.len(), 1, "别名查询应命中规范词文档");
        assert_eq!(hits[0].0, format!("L1:{doc_id}"));
    }

    /// 空词典（无池词条）→ 纯 bigram + 子串回退仍可命中。
    #[tokio::test]
    async fn query_text_labels_empty_pool_falls_back_bigram() {
        let mut svc = KeywordService::new();
        let doc_id = uuid::Uuid::new_v4();
        // 无 pool 词条；镜像文档关键词含 "工作压力"
        svc.index_l1(&l1_view(doc_id, Some("p1"), Some("工作压力")));

        let hits = query_text_labels(
            svc.composite(),
            &svc.pool_snapshot(),
            "工作压力很大",
            Some("p1"),
            None,
            10,
        )
        .await;
        assert!(
            hits.iter().any(|(l, _)| l == &format!("L1:{doc_id}")),
            "空词典时 bigram + 子串应命中（label 兼容）"
        );
    }

    /// 无镜像文档 / 空文本 / persona 隔离下自由文本查询的降级语义。
    #[tokio::test]
    async fn query_text_labels_degrade_cases() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        // 无镜像文档 → 空
        let hits = query_text_labels(
            svc.composite(),
            &svc.pool_snapshot(),
            "工作压力",
            Some("p1"),
            None,
            10,
        )
        .await;
        assert!(hits.is_empty());

        // 空文本 → 空
        let doc_id = uuid::Uuid::new_v4();
        svc.index_l1(&l1_view(doc_id, Some("p1"), Some("工作压力")));
        let hits = query_text_labels(
            svc.composite(),
            &svc.pool_snapshot(),
            "   ",
            Some("p1"),
            None,
            10,
        )
        .await;
        assert!(hits.is_empty());

        // persona 隔离：查 p2 查不到 p1 文档
        let hits = query_text_labels(
            svc.composite(),
            &svc.pool_snapshot(),
            "工作压力",
            Some("p2"),
            None,
            10,
        )
        .await;
        assert!(hits.is_empty(), "跨 persona 隔离不命中");
    }
}
