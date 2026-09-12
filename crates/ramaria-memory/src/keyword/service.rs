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
//! - `build_fuzzy`（无 self 关联函数）用词表在锁外构建词级语义索引：embedding 不可用 /
//!   词表为空 / 构建失败 → 返回 None 保持"精确 + 子串"两层降级（静默降级，不抛错），
//!   构建结果由调用方经 `set_fuzzy` 挂载（避免 std 写锁跨 await）
//! - `fuzzy_stale` / `warn_if_fuzzy_stale` 提供语义层陈旧判据与节流告警（词数差近似）
//! - 纯内存零 I/O；`KeywordPoolRow`（core 数据行）为装载边界，不依赖数据库

use std::collections::{HashMap, HashSet};
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
// 常量
// =========================================================

/// 语义层陈旧告警的节流窗口（毫秒）。
const FUZZY_STALE_WARN_INTERVAL_MS: i64 = 5 * 60 * 1000;

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
    /// 上次语义层陈旧告警时间（Unix 毫秒；节流用，None = 尚未告警）
    fuzzy_stale_warned_at: Option<i64>,
}

impl KeywordService {
    /// 创建空服务（空词典 + 空倒排镜像）。
    pub fn new() -> Self {
        Self {
            pool: KeywordPool::new(),
            composite: Arc::new(CompositeIndex::new(CompositeIndexConfig::default())),
            fuzzy_stale_warned_at: None,
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
    /// - `"pending"` 词条的**写入侧当前未接线**（生产链路暂无生产者，行由外部/手动写入）；
    ///   装载后 pending 不参与词表/归一（见 `KeywordPool::established_terms`）。
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
    /// 返回: 实际参与累积的唯一词条数（批内按文本去重后的数量）。
    ///
    /// 说明:
    /// - **批内幂等**：同一批内重复出现的 token 只累加一次（保序去重后逐条 upsert）。
    /// - **跨批重放非幂等**：同一批文档重复提交仍会再次累加 use_count——
    ///   调用方需保证同一文档只提交一次。
    /// - 仅内存镜像累积；keyword_pool 持久化由既有 summarizer / storage 负责，不重复写库。
    pub fn upsert_pool_tokens(&mut self, tokens: &[KeywordToken], now_ms: i64) -> usize {
        let mut seen: HashSet<&str> = HashSet::with_capacity(tokens.len());
        let mut unique = 0usize;
        for token in tokens {
            // 保序去重：按标准化文本判重（KeywordToken 构造期已统一 trim + ASCII 小写）
            if !seen.insert(token.as_str()) {
                continue;
            }
            self.pool.upsert(token.clone(), now_ms);
            unique += 1;
        }
        unique
    }

    /// 返回规范词列表（按 use_count 降序），供通用规范词读取。
    ///
    /// 说明:
    /// - 仅规范词（不含已确认别名）；语义层构建与词典增强请用
    ///   `pool().established_terms()`（已确认词表口径：canonical + alias）。
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
    /// - **语义层随之清空**（新 `CompositeIndex` 无 fuzzy）：调用方需在重建流程中
    ///   重新构建（锁外 `build_fuzzy` + 锁内 `set_fuzzy`），否则保持两层降级。
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
    ///
    /// 说明:
    /// - **语义层随之清空**（新 `CompositeIndex` 无 fuzzy）：调用方需在重建流程中
    ///   重新构建（锁外 `build_fuzzy` + 锁内 `set_fuzzy`），否则保持两层降级。
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

    /// 用词表构建语义扩展层（锁外 await 完成后由调用方经 `set_fuzzy` 挂载）。
    ///
    /// 参数:
    /// - `terms`: 词表（通常为已确认词表 token，由调用方在锁内取出后释放锁）。
    /// - `embedder`: 向量生成器；`None` 表示 embedding 不可用。
    ///
    /// 返回:
    /// - `Some(FuzzyKeywordIndex)`: 构建成功的语义层。
    /// - `None`: 降级（保持"精确 + 子串"两层，不抛错）。
    ///
    /// 降级语义（静默降级，不抛错）:
    /// - embedder 为 None → None（debug 日志）
    /// - 词表为空 → None（debug 日志）
    /// - 构建失败 → None（warn 日志，不含词条文本）
    ///
    /// 说明:
    /// - 无 `&self` 参数：可在 std 写锁外 await，避免锁跨 await；
    ///   构建结果由调用方在锁内经 `set_fuzzy` 挂载。
    pub async fn build_fuzzy(
        terms: &[KeywordToken],
        embedder: Option<&dyn EmbeddingProvider>,
    ) -> Option<FuzzyKeywordIndex> {
        let Some(embedder) = embedder else {
            tracing::debug!("关键词语义层跳过：embedding 不可用（两层降级）");
            return None;
        };
        if terms.is_empty() {
            tracing::debug!("关键词语义层跳过：词表为空（无可用词条）");
            return None;
        }
        match FuzzyKeywordIndex::build(terms, embedder).await {
            Ok(fuzzy) => {
                tracing::info!(entry_count = terms.len(), "关键词语义层构建完成");
                Some(fuzzy)
            }
            Err(e) => {
                tracing::warn!(error = %e, "关键词语义层构建失败（两层降级）");
                None
            }
        }
    }

    /// 语义层是否陈旧（以词数差为近似判据）。
    ///
    /// 判据:
    /// - `composite.fuzzy()` 为 None 且已确认词表非空 → true（未构建）。
    /// - `Some(f)` 且 `f.len()` 与已确认词表词数不一致 → true（词表已变化）。
    /// - 否则 false。
    ///
    /// 说明:
    /// - 词数一致但词表内容有增删替换时本判据发现不了——以词数差近似，
    ///   避免逐词比对向查询路径引入额外开销；精确判据应由调用方携带词表版本自行维护。
    pub fn fuzzy_stale(&self) -> bool {
        let established_len = self.pool.established_terms().len();
        match self.composite.fuzzy() {
            None => established_len > 0,
            Some(fuzzy) => fuzzy.len() != established_len,
        }
    }

    /// 语义层陈旧时按节流打 warn 并返回 true；不陈旧返回 false。
    ///
    /// 参数:
    /// - `now_ms`: 当前时间（Unix 毫秒；节流窗口以调用方时间为准）。
    ///
    /// 返回:
    /// - `true`: 当前陈旧（本次是否实际打日志由节流决定）。
    /// - `false`: 不陈旧。
    ///
    /// 说明:
    /// - 节流窗口 5 分钟：窗口内重复调用不再打日志，避免按请求频次刷屏。
    /// - 日志只记录池词表词数与语义层词数（不含词条文本）。
    pub fn warn_if_fuzzy_stale(&mut self, now_ms: i64) -> bool {
        if !self.fuzzy_stale() {
            return false;
        }
        let should_log = match self.fuzzy_stale_warned_at {
            Some(last) => now_ms.saturating_sub(last) >= FUZZY_STALE_WARN_INTERVAL_MS,
            None => true,
        };
        if should_log {
            self.fuzzy_stale_warned_at = Some(now_ms);
            tracing::warn!(
                pool_terms = self.pool.established_terms().len(),
                fuzzy_terms = self.composite.fuzzy().map(|f| f.len()).unwrap_or(0),
                "关键词语义层陈旧：词表已变化但语义扩展层未重建（保持两层降级可用）"
            );
        }
        true
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

/// 词典池快照——已确认词表与别名解析表的只读值形态。
///
/// 职责:
/// - 供调用方在 std 读锁内一次性取出、释放锁后在 `await` 异步查询 / 路由评分中使用，
///   避免持锁跨 await。
/// - `dictionary`: 已确认词表文本（canonical + alias，**排除 pending**），
///   驱动词典增强分词；与 BM25 词典装载（storage 侧 `list_established`）同源口径。
/// - `resolve`: 已确认别名 → 规范词文本（canonical 自身与 pending 不收录）。
#[derive(Debug, Clone, Default)]
pub struct KeywordPoolSnapshot {
    /// 已确认词表文本（词典增强分词的候选完整词）
    dictionary: Vec<String>,
    /// 已确认别名 → 规范词文本
    resolve: HashMap<String, String>,
}

impl KeywordPoolSnapshot {
    /// 从词典池构造快照（只收录已确认词表：canonical + alias，排除 pending）。
    pub fn from_pool(pool: &KeywordPool) -> Self {
        let dictionary: Vec<String> = pool
            .established_terms()
            .into_iter()
            .map(|token| token.as_str().to_string())
            .collect();
        let mut resolve = HashMap::new();
        for entry in pool.iter() {
            // 仅已确认别名参与归一；pending 未确认，不进入解析表
            if !matches!(entry.status, KeywordStatus::Alias { .. }) {
                continue;
            }
            if let Some(canonical) = pool.resolve(&entry.token) {
                resolve.insert(
                    entry.token.as_str().to_string(),
                    canonical.as_str().to_string(),
                );
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

    /// 已确认词表文本（canonical + alias，排除 pending）。
    pub fn dictionary(&self) -> &[String] {
        &self.dictionary
    }

    /// 已确认别名解析表（别名文本 → 规范词文本）。
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
/// - 查询词构造：词典增强分词（词典 = 池已确认词表，空池退化为纯 bigram），
///   并对命中已确认别名的 token 追加其规范词（union，提升召回）；
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

    /// 已确认词表（canonical + alias，排除 pending）token 列表。
    fn established(svc: &KeywordService) -> Vec<KeywordToken> {
        svc.pool()
            .established_terms()
            .into_iter()
            .cloned()
            .collect()
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
        let accumulated = svc.upsert_pool_tokens(&[token("工作压力"), token("新词")], NOW_MS);
        assert_eq!(accumulated, 2, "返回实际参与累积的唯一词条数");
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

    /// 批内幂等：同批重复 token 只累加一次（跨批重放不保证，调用方需按文档幂等）。
    #[test]
    fn upsert_pool_tokens_dedups_within_batch() {
        let mut svc = KeywordService::new();
        let unique = svc.upsert_pool_tokens(
            &[token("工作压力"), token("新词"), token("工作压力")],
            NOW_MS,
        );
        assert_eq!(unique, 2, "批内重复 token 只计一次");
        assert_eq!(
            svc.pool()
                .entry_by_token(&token("工作压力"))
                .unwrap()
                .use_count,
            1
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

    // ---- 语义层构建（锁外 build + 锁内 set）与降级 ----

    #[tokio::test]
    async fn build_fuzzy_embedder_none_keeps_two_layers() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        let fuzzy = KeywordService::build_fuzzy(&established(&svc), None).await;
        assert!(fuzzy.is_none(), "embedding 不可用 → 不构建语义层");
        svc.set_fuzzy(fuzzy);
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
    async fn build_fuzzy_empty_terms_returns_none() {
        let embedder = MockEmbedder::ok();
        let fuzzy = KeywordService::build_fuzzy(&[], Some(&embedder)).await;
        assert!(fuzzy.is_none(), "空词表 → 不构建语义层");
    }

    #[tokio::test]
    async fn build_fuzzy_success_mounts_layer() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        let embedder = MockEmbedder::ok();
        let terms = established(&svc);
        let fuzzy = KeywordService::build_fuzzy(&terms, Some(&embedder))
            .await
            .expect("构建应成功");
        assert!(fuzzy.is_ready());
        assert_eq!(fuzzy.len(), terms.len());
        // 挂载由调用方在锁内完成
        svc.set_fuzzy(Some(fuzzy));
        assert!(svc.composite().fuzzy().is_some());
    }

    #[tokio::test]
    async fn build_fuzzy_failure_degrades_to_none() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        let embedder = MockEmbedder::failing();
        let fuzzy = KeywordService::build_fuzzy(&established(&svc), Some(&embedder)).await;
        assert!(fuzzy.is_none(), "构建失败 → None（两层降级）");
        svc.set_fuzzy(fuzzy);
        assert!(svc.composite().fuzzy().is_none());
    }

    /// 陈旧判据：未构建且池非空 → 陈旧；构建后词数一致 → 不陈旧；池增长后 → 陈旧。
    #[tokio::test]
    async fn fuzzy_stale_reports_missing_and_outdated() {
        let mut svc = KeywordService::new();
        // 空池：无词表也无语义层 → 不算陈旧
        assert!(!svc.fuzzy_stale());
        assert!(!svc.warn_if_fuzzy_stale(NOW_MS), "空池不告警");

        svc.load_pool_entries(&sample_rows());
        assert!(svc.fuzzy_stale(), "池非空且语义层未构建 → 陈旧");

        // 构建并挂载后词数一致 → 不陈旧
        let embedder = MockEmbedder::ok();
        let terms = established(&svc);
        let fuzzy = KeywordService::build_fuzzy(&terms, Some(&embedder))
            .await
            .expect("构建应成功");
        svc.set_fuzzy(Some(fuzzy));
        assert!(!svc.fuzzy_stale(), "词数一致 → 不陈旧");

        // 池增长（新词）→ 词数不一致 → 陈旧
        svc.upsert_pool_tokens(&[token("新词")], NOW_MS);
        assert!(svc.fuzzy_stale(), "词表增长后 → 陈旧");
        assert!(svc.warn_if_fuzzy_stale(NOW_MS), "陈旧时告警返回 true");
    }

    /// 陈旧告警节流：窗口内不重复记录，超出窗口再次记录。
    #[test]
    fn warn_if_fuzzy_stale_is_throttled() {
        let mut svc = KeywordService::new();
        svc.load_pool_entries(&sample_rows());
        assert!(svc.warn_if_fuzzy_stale(NOW_MS));
        assert_eq!(svc.fuzzy_stale_warned_at, Some(NOW_MS), "首次告警记录时间");

        // 节流窗口内：返回值仍报告陈旧，但不刷新告警时间
        assert!(svc.warn_if_fuzzy_stale(NOW_MS + FUZZY_STALE_WARN_INTERVAL_MS - 1));
        assert_eq!(svc.fuzzy_stale_warned_at, Some(NOW_MS));

        // 超出窗口：再次告警
        assert!(svc.warn_if_fuzzy_stale(NOW_MS + FUZZY_STALE_WARN_INTERVAL_MS));
        assert_eq!(
            svc.fuzzy_stale_warned_at,
            Some(NOW_MS + FUZZY_STALE_WARN_INTERVAL_MS)
        );
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
        // 词典仅含已确认词表（canonical + alias），pending 排除
        assert_eq!(snapshot.dictionary.len(), 3);
        assert!(snapshot.dictionary.iter().any(|d| d == "工作压力"));
        assert!(snapshot.dictionary.iter().any(|d| d == "职业倦怠"));
        assert!(
            !snapshot.dictionary.iter().any(|d| d == "职场焦虑"),
            "pending 词条不入词典"
        );
        // 解析表仅含已确认别名 → canonical；pending / canonical 自身不收录
        assert_eq!(
            snapshot.resolve().get("职业倦怠").map(String::as_str),
            Some("工作压力")
        );
        assert!(
            snapshot.resolve().get("职场焦虑").is_none(),
            "pending 不参与归一"
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

    /// 别名短语查询：用户文本含已确认别名词 → 命中规范词关键词文档（label 与 retriever 兼容）。
    #[tokio::test]
    async fn query_text_labels_aliases_to_canonical_doc() {
        let mut svc = KeywordService::new();
        // 词典：canonical 工作压力 + 已确认 alias 职业倦怠 → 工作压力
        svc.load_pool_entries(&sample_rows());
        let doc_id = uuid::Uuid::new_v4();
        svc.index_l1(&l1_view(doc_id, Some("p1"), Some("工作压力")));

        let hits = query_text_labels(
            svc.composite(),
            &svc.pool_snapshot(),
            "最近职业倦怠",
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
