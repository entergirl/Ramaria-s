//! crates/ramaria-memory/src/keyword/index.rs — 关键词倒排索引
//!
//! 设计特点（keyword-design §5.1，P5/P6 解决）:
//! - 内存倒排索引：`exact_inverted: KeywordToken → 文档位集合`，支撑精确 + 子串两级匹配
//! - 文档元数据（salience / created_at / persona_uid）内置于文档表，
//!   评分时按 `TF-IDF × Salience × Recency` 加权（时间衰减半衰期可配）
//! - 子串匹配（查询 token 是索引 token 的子串，如查"工作"命中"工作压力"）经
//!   "索引词 bigram 倒排"定位候选（含查询词全部 bigram 的超集），contains 过滤后
//!   经精确倒排回取文档——避免逐查询词遍历全词表与全文档扫描
//! - 幂等语义：同文档重复 index 先精准移除旧记录（不再全量重建倒排）；
//!   remove 支持批删（吸收/重建场景，批删走 retain + 重建）
//! - 文档容量上限可选（默认不限制）：超出后按 created_at 最旧驱逐
//! - 纯内存纯函数，零 I/O，零异步；`ramaria-core` 的 `KeywordRef/KeywordQuery`
//!   为对外类型边界（M3 T-V20-3-001 定稿）
//!
//! 模块边界:
//! - 本文件只实现"按关键词集合检索已索引文档"，不含语义扩展（见 composite.rs）与
//!   词典状态机（见 pool.rs）

use std::collections::{HashMap, HashSet};

use ramaria_core::keyword::{KeywordQuery, KeywordRef, KeywordSet, KeywordToken, MatchStrategy};

use super::normalizer::{CommaSeparatedNormalizer, KeywordNormalizer};

// =========================================================
// 文档表
// =========================================================

/// 索引中的一篇文档记录。
///
/// `KeywordRef` 只承载文档标识（id + persona），评分所需的元数据（salience /
/// created_at）单独存放于此，避免把时间/显著性语义泄漏到 core 类型层。
#[derive(Debug, Clone)]
struct DocEntry {
    /// 文档标识（L1/L2；Pool 词条不入倒排索引）
    reff: KeywordRef,
    /// 文档关键词集合（标准化 token）
    keywords: KeywordSet,
    /// 情感显著性 0.0..=1.0（用于 salience 加权）
    salience: f64,
    /// 创建时间（Unix 毫秒，用于 recency 衰减）
    created_at: i64,
}

impl DocEntry {
    /// 构造并校验元数据边界（防御性钳制 salience）。
    fn new(reff: KeywordRef, keywords: KeywordSet, salience: f64, created_at: i64) -> Self {
        Self {
            reff,
            keywords,
            salience: salience.clamp(0.0, 1.0),
            created_at,
        }
    }
}

// =========================================================
// 可插拔评分策略
// =========================================================

/// 关键词相关性评分策略——可插拔 trait。
///
/// 输入均为已算好的分量，实现方负责组合：
/// - `idf_sum`: 命中的查询关键词 IDF 之和（TF-IDF 的 IDF 分量）
/// - `salience`: 命中文档的情感显著性（0.0..=1.0）
/// - `age_days`: 文档距今的天数（≥0，用于 recency 衰减）
pub trait ScoringStrategy: Send + Sync {
    /// 计算一条命中文档的相关性得分。
    fn score(&self, idf_sum: f64, salience: f64, age_days: f64) -> f64;
}

/// 默认评分策略：`score = idf_sum × (1 + 0.5 × salience) × exp(-age_days / halflife)`
///
/// - salience 让高情感 L1/L2 权重更高
/// - recency 用指数衰减确保优先召回近期内容
#[derive(Debug, Clone)]
pub struct DefaultScoringStrategy {
    /// 时间衰减半衰期（天），默认 30
    pub recency_halflife: f64,
}

impl Default for DefaultScoringStrategy {
    fn default() -> Self {
        Self {
            recency_halflife: 30.0,
        }
    }
}

impl ScoringStrategy for DefaultScoringStrategy {
    fn score(&self, idf_sum: f64, salience: f64, age_days: f64) -> f64 {
        if idf_sum <= 0.0 {
            return 0.0;
        }
        let salience_weight = 1.0 + 0.5 * salience.clamp(0.0, 1.0);
        let recency_weight = (-age_days.max(0.0) / self.recency_halflife.max(1e-6)).exp();
        idf_sum * salience_weight * recency_weight
    }
}

// =========================================================
// KeywordIndex
// =========================================================

/// 关键词倒排索引——精确 + 子串两级匹配。
///
/// # 数据结构
///
/// - `docs`: 全量文档记录（文档表，评分元数据所在）
/// - `exact_inverted`: `KeywordToken → HashSet<doc_idx>`（精确匹配主索引）
/// - `token_bigrams`: `(char, char) → HashSet<KeywordToken>`（子串候选定位倒排：
///   每个索引词的全部相邻字符对 → 该词；写路径维护、读路径只读）
/// - `max_docs`: 文档容量上限（`None` = 不限制）
///
/// # 复杂度
///
/// - Exact: O(Q × postings_avg)，Q = 查询关键词数
/// - Substring: O(Q × candidate + hits)，candidate = 含查询词全部 bigram 的候选词
///   （单字符查询词无 bigram → 退化为全词表扫描）
/// - remove: O(被移除文档的 token 数 + 被搬移文档的 token 数)，不再全量重建倒排
///
/// # 线程模型
///
/// 以 `&mut self` 提供写操作、`&self` 提供查询；跨线程共享时由上层用
/// `Arc<RwLock<KeywordIndex>>` 包装（写锁 index/remove，读锁 query）。
#[derive(Debug, Clone)]
pub struct KeywordIndex {
    /// 文档表（doc_idx ↔ 文档记录）
    docs: Vec<DocEntry>,
    /// 精确倒排：token → 出现该 token 的文档下标集合
    exact_inverted: HashMap<KeywordToken, HashSet<usize>>,
    /// 子串候选倒排：字符 bigram → 含该 bigram 的索引词集合
    token_bigrams: HashMap<(char, char), HashSet<KeywordToken>>,
    /// 文档容量上限（`None` = 不限制）
    max_docs: Option<usize>,
}

impl Default for KeywordIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl KeywordIndex {
    /// 创建空索引（不设文档容量上限）。
    pub fn new() -> Self {
        Self {
            docs: Vec::new(),
            exact_inverted: HashMap::new(),
            token_bigrams: HashMap::new(),
            max_docs: None,
        }
    }

    // =========================================================
    // 索引维护
    // =========================================================

    /// 索引一篇文档的关键词集合。
    ///
    /// # 幂等性
    ///
    /// 同 `reff` 文档重复添加时，先移除旧记录再写入（覆盖语义），保证重建/重试安全。
    ///
    /// 参数:
    /// - `reff`: 文档标识（L1/L2）。Pool 词条请勿入索引。
    /// - `keywords`: 该文档的标准化关键词集合。
    /// - `salience`: 情感显著性 0.0..=1.0（越界自动钳制）。
    /// - `created_at`: 创建时间（Unix 毫秒，供 recency 衰减）。
    pub fn index(
        &mut self,
        reff: KeywordRef,
        keywords: KeywordSet,
        salience: f64,
        created_at: i64,
    ) {
        debug_assert!(
            matches!(reff, KeywordRef::L1 { .. } | KeywordRef::L2 { .. }),
            "KeywordIndex 只接收 L1/L2 文档引用，Pool 词条走词典层"
        );
        // 幂等：同文档先精准移除旧记录（swap_remove + 倒排下标维护，不全量重建）
        self.remove(&reff);

        let doc_idx = self.docs.len();
        let entry = DocEntry::new(reff.clone(), keywords, salience, created_at);

        for token in entry.keywords.iter() {
            self.exact_inverted
                .entry(token.clone())
                .or_default()
                .insert(doc_idx);
            insert_token_bigrams(&mut self.token_bigrams, token);
        }
        self.docs.push(entry);
        self.evict_to_capacity();
    }

    /// 便捷入口：解析原始逗号分隔关键词串（如 memory_l1.keywords 字段）后索引。
    ///
    /// 返回值为本索引文档总数（便于调用方校验）。
    pub fn index_parsed(
        &mut self,
        reff: KeywordRef,
        raw_keywords: Option<&str>,
        salience: f64,
        created_at: i64,
    ) -> usize {
        let keywords: KeywordSet = CommaSeparatedNormalizer
            .normalize(raw_keywords.unwrap_or(""))
            .into_iter()
            .collect();
        self.index(reff, keywords, salience, created_at);
        self.docs.len()
    }

    /// 按文档标识移除索引记录（精准维护倒排，不做全量重建）。
    ///
    /// 返回是否实际移除（不存在返回 false）。
    ///
    /// 说明:
    /// - `swap_remove` 把末尾文档搬到被移除位置：先从被删文档的每个 token 的
    ///   postings 中删除 `pos`（postings 空则连同子串候选倒排一并移除该 token），
    ///   再把被搬移文档的全部 postings 下标 `last_idx` 修正为 `pos`。
    pub fn remove(&mut self, reff: &KeywordRef) -> bool {
        let Some(pos) = self.docs.iter().position(|d| &d.reff == reff) else {
            return false;
        };
        let last_idx = self.docs.len() - 1;
        let removed = self.docs.swap_remove(pos);

        // 被删文档：从其 token 的 postings 中删除 pos；postings 空则连键清理
        for token in removed.keywords.iter() {
            let emptied = match self.exact_inverted.get_mut(token) {
                Some(postings) => {
                    postings.remove(&pos);
                    postings.is_empty()
                }
                None => false,
            };
            if emptied {
                self.exact_inverted.remove(token);
                remove_token_bigrams(&mut self.token_bigrams, token);
            }
        }

        // 末尾文档被换到 pos：将其全部 postings 下标 last_idx 修正为 pos
        if pos != last_idx {
            let moved = &self.docs[pos];
            for token in moved.keywords.iter() {
                if let Some(postings) = self.exact_inverted.get_mut(token) {
                    postings.remove(&last_idx);
                    postings.insert(pos);
                }
            }
        }
        true
    }

    /// 批量移除 L1/L2 文档。
    ///
    /// 返回实际移除条数（幂等：不存在即忽略）。
    ///
    /// 说明:
    /// - 批量路径走 `retain` + 全量重建倒排（批量低频，避免逐条 swap 修正的复杂度）。
    pub fn remove_doc_batch(&mut self, l1_ids: &[uuid::Uuid], l2_ids: &[i64]) -> usize {
        let l2_set: HashSet<i64> = l2_ids.iter().copied().collect();
        let l1_set: HashSet<uuid::Uuid> = l1_ids.iter().copied().collect();

        let before = self.docs.len();
        self.docs.retain(|d| match &d.reff {
            KeywordRef::L1 { id, .. } => !l1_set.contains(id),
            KeywordRef::L2 { id, .. } => !l2_set.contains(id),
            KeywordRef::Pool { .. } => true,
        });
        let removed = before - self.docs.len();
        if removed > 0 {
            self.rebuild_inverted();
        }
        removed
    }

    /// 全量清空索引（保留容量上限配置）。
    pub fn clear(&mut self) {
        self.docs.clear();
        self.exact_inverted.clear();
        self.token_bigrams.clear();
    }

    /// 重建精确倒排与子串候选倒排（从文档表全量重算）。
    fn rebuild_inverted(&mut self) {
        self.exact_inverted.clear();
        self.token_bigrams.clear();
        for (idx, entry) in self.docs.iter().enumerate() {
            for token in entry.keywords.iter() {
                self.exact_inverted
                    .entry(token.clone())
                    .or_default()
                    .insert(idx);
                insert_token_bigrams(&mut self.token_bigrams, token);
            }
        }
    }

    /// 设置文档容量上限（`None` = 不限制）。
    ///
    /// 说明:
    /// - 默认 `None`（既有行为不变）；上层可按镜像规模自行接线。
    /// - 设置上限不会立即驱逐既有文档：由下一次 `index` 写入后统一按
    ///   created_at 最旧驱逐（批量 remove + 重建路径）。
    pub fn set_max_docs(&mut self, max: Option<usize>) {
        self.max_docs = max;
    }

    /// 返回文档容量上限（`None` = 不限制）。
    pub fn doc_capacity(&self) -> Option<usize> {
        self.max_docs
    }

    /// 超出容量上限时按 created_at 最旧驱逐（默认 `None` → 空操作）。
    fn evict_to_capacity(&mut self) {
        let Some(max) = self.max_docs else {
            return;
        };
        if self.docs.len() <= max {
            return;
        }

        // 按 created_at 升序（最旧优先）取需驱逐的下标；稳定排序保证同时间戳按插入序
        let mut order: Vec<usize> = (0..self.docs.len()).collect();
        order.sort_by_key(|&idx| self.docs[idx].created_at);
        let evict_count = self.docs.len() - max;

        let mut l1_ids: Vec<uuid::Uuid> = Vec::new();
        let mut l2_ids: Vec<i64> = Vec::new();
        for &idx in order.iter().take(evict_count) {
            match &self.docs[idx].reff {
                KeywordRef::L1 { id, .. } => l1_ids.push(*id),
                KeywordRef::L2 { id, .. } => l2_ids.push(*id),
                // Pool 词条不入倒排索引（防御：理论不可达）
                KeywordRef::Pool { .. } => {}
            }
        }
        self.remove_doc_batch(&l1_ids, &l2_ids);
    }

    /// 返回已索引文档总数。
    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    /// 索引是否为空。
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    // =========================================================
    // 查询
    // =========================================================

    /// 按 `KeywordQuery` 检索（当前时间由系统时钟给出，用于 recency 衰减）。
    pub fn query(&self, query: &KeywordQuery) -> Vec<(KeywordRef, f64)> {
        let now_ms = ramaria_core::types::now_ms();
        self.query_with_time(query, now_ms)
    }

    /// 确定性检索——显式给定"当前时间"，供单测固定时间衰减基准。
    pub fn query_with_time(&self, query: &KeywordQuery, now_ms: i64) -> Vec<(KeywordRef, f64)> {
        let scorer = DefaultScoringStrategy::default();
        self.query_with_scorer(query, now_ms, &scorer)
    }

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

        // 2. 评分（idf_sum × salience 加权 × recency 衰减）。
        //    同分次键（created_at）随评分预表化（doc_idx → created_at），
        //    排序比较时 O(1) 查询，避免逐对按 KeywordRef 线性扫描文档表。
        let mut created_at_by_idx: HashMap<usize, i64> = HashMap::with_capacity(matched.len());
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

            let age_days = ((now_ms - entry.created_at) as f64) / 86_400_000.0;
            let score = scorer.score(idf_sum, entry.salience, age_days);
            if score > 0.0 {
                created_at_by_idx.insert(idx, entry.created_at);
                scored.push((idx, score));
            }
        }

        // 3. 排序：得分降序；同分按创建时间降序（最新优先）；再按 label 兜底稳定
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let ca = created_at_by_idx.get(&a.0).copied().unwrap_or(0);
                    let cb = created_at_by_idx.get(&b.0).copied().unwrap_or(0);
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
    fn substring_candidates<'a>(&'a self, query: &KeywordToken) -> Option<Vec<&'a KeywordToken>> {
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
    fn idf(n_docs: f64, df: f64) -> f64 {
        if df <= 0.0 {
            return 0.0;
        }
        ((n_docs - df + 0.5) / (df + 0.5) + 1.0).ln()
    }
}

// =========================================================
// 子串候选倒排辅助
// =========================================================

/// 把 token 的全部相邻字符 bigram 登记到子串候选倒排（幂等）。
fn insert_token_bigrams(
    map: &mut HashMap<(char, char), HashSet<KeywordToken>>,
    token: &KeywordToken,
) {
    for pair in adjacent_char_pairs(token.as_str()) {
        map.entry(pair).or_default().insert(token.clone());
    }
}

/// 从子串候选倒排移除 token（token 已不在任何文档中时调用；集合空则连键删除）。
fn remove_token_bigrams(
    map: &mut HashMap<(char, char), HashSet<KeywordToken>>,
    token: &KeywordToken,
) {
    for pair in adjacent_char_pairs(token.as_str()) {
        let emptied = match map.get_mut(&pair) {
            Some(set) => {
                set.remove(token);
                set.is_empty()
            }
            None => false,
        };
        if emptied {
            map.remove(&pair);
        }
    }
}

/// 返回文本的全部相邻字符 bigram（如 "工作压力" → (工,作)/(作,压)/(压,力)）。
///
/// 字符数 < 2 时返回空列表（无 bigram）。
fn adjacent_char_pairs(text: &str) -> Vec<(char, char)> {
    let chars: Vec<char> = text.chars().collect();
    chars.windows(2).map(|w| (w[0], w[1])).collect()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
