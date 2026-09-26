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
mod tests {
    use super::*;
    use ramaria_core::keyword::{KeywordQuery, MatchStrategy};

    /// 测试基准时间（Unix 毫秒）。
    const NOW_MS: i64 = 2_000_000_000_000;

    fn l1_ref(id: uuid::Uuid, persona: &str) -> KeywordRef {
        KeywordRef::L1 {
            id,
            persona_uid: persona.to_string(),
        }
    }

    fn l2_ref(id: i64, persona: &str) -> KeywordRef {
        KeywordRef::L2 {
            id,
            persona_uid: persona.to_string(),
        }
    }

    fn q(
        keywords: &[&str],
        persona: Option<&str>,
        strategy: MatchStrategy,
        top_k: usize,
    ) -> KeywordQuery {
        KeywordQuery::builder(persona.map(|s| s.to_string()))
            .keywords_from(keywords.iter().filter_map(|s| KeywordToken::new(s)))
            .strategy(strategy)
            .top_k(top_k)
            .build()
    }

    /// 构造含 3 条文档的小型索引。
    fn sample_index() -> KeywordIndex {
        let mut index = KeywordIndex::new();
        // L1 高情感近期
        index.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "user-0001"),
            Some("工作压力, 加班, 失眠"),
            0.9,
            NOW_MS - 86_400_000, // 1 天前
        );
        // L1 中性早期
        index.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "user-0001"),
            Some("运动, 爬山, 放松"),
            0.5,
            NOW_MS - 30 * 86_400_000, // 30 天前
        );
        // L2 其他 persona
        index.index_parsed(
            l2_ref(1, "user-0002"),
            Some("工作压力, 职场"),
            0.7,
            NOW_MS - 86_400_000,
        );
        index
    }

    // ---- 基础功能 ----

    #[test]
    fn empty_index_query_empty() {
        let index = KeywordIndex::new();
        assert_eq!(index.doc_count(), 0);
        assert!(index.is_empty());
        let results = index.query(&q(&["工作"], None, MatchStrategy::Exact, 10));
        assert!(results.is_empty());
    }

    /// 精确匹配 + persona 隔离 + top_k 截断
    #[test]
    fn exact_query_persona_filter() {
        let index = sample_index();
        let results = index.query(&q(
            &["工作压力"],
            Some("user-0001"),
            MatchStrategy::Exact,
            10,
        ));
        // 只命中 user-0001 的 L1（user-0002 的 L2 被隔离）
        assert_eq!(results.len(), 1);
        match &results[0].0 {
            KeywordRef::L1 { persona_uid, .. } => assert_eq!(persona_uid, "user-0001"),
            other => panic!("应为 L1 引用，得到 {other:?}"),
        }
    }

    /// 无 persona 限定（全局）时命中所有文档
    #[test]
    fn exact_query_global() {
        let index = sample_index();
        let results = index.query(&q(&["工作压力"], None, MatchStrategy::Exact, 10));
        assert_eq!(results.len(), 2, "两个 persona 的文档都应命中");
    }

    /// 子串匹配：查"工作"命中"工作压力"
    #[test]
    fn substring_query_hits_superstring() {
        let index = sample_index();
        let exact = index.query(&q(&["工作"], Some("user-0001"), MatchStrategy::Exact, 10));
        assert!(exact.is_empty(), "精确匹配不命中子串");
        let sub = index.query(&q(
            &["工作"],
            Some("user-0001"),
            MatchStrategy::Substring,
            10,
        ));
        assert!(!sub.is_empty(), "子串应命中工作压力");
        assert_eq!(sub.len(), 1);
    }

    /// 子串查询对无命中词返回空（不 panic）
    #[test]
    fn substring_no_match_empty() {
        let index = sample_index();
        let results = index.query(&q(&["不存在词xyz"], None, MatchStrategy::Substring, 10));
        assert!(results.is_empty());
    }

    /// 子串候选 bigram 倒排与"全词表 contains 扫描"逐位等价的基准实现。
    fn substring_hits_full_scan(index: &KeywordIndex, query_token: &str) -> Vec<String> {
        let mut hits: Vec<String> = index
            .exact_inverted
            .keys()
            .filter(|t| t.as_str().contains(query_token))
            .map(|t| t.as_str().to_string())
            .collect();
        hits.sort();
        hits
    }

    /// 子串候选（bigram 倒排 + contains 过滤）与全扫描命中集合逐位一致。
    ///
    /// 覆盖：多字符命中 / 长查询词 / 单字符退化（无 bigram）/ 无命中 /
    /// 含全部 bigram 但非子串的假阳性（必须经 contains 过滤剔除）。
    #[test]
    fn substring_bigram_candidates_match_full_scan() {
        let mut index = KeywordIndex::new();
        index.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "p1"),
            Some("工作压力, 爬山, 职场, aabbcc, 996"),
            0.5,
            NOW_MS,
        );

        let queries = [
            "工作",
            "工作压力",
            "作压",
            "压力",
            "职场",
            "工",
            "爬",
            "a",
            "aabb",
            "abc",
            "996",
            "不存在词xyz",
        ];
        for text in queries {
            let qt = KeywordToken::new(text).expect("查询词应有效");
            let mut expected = substring_hits_full_scan(&index, qt.as_str());
            let mut actual: Vec<String> = match index.substring_candidates(&qt) {
                Some(candidates) => candidates
                    .into_iter()
                    .filter(|t| t.as_str().contains(qt.as_str()))
                    .map(|t| t.as_str().to_string())
                    .collect(),
                // 单字符查询词无 bigram → 子串层退化为全词表扫描
                None => index
                    .exact_inverted
                    .keys()
                    .filter(|t| t.as_str().contains(qt.as_str()))
                    .map(|t| t.as_str().to_string())
                    .collect(),
            };
            actual.sort();
            expected.sort();
            assert_eq!(actual, expected, "query={text:?} 命中集合应逐位等价");
        }

        // 假阳性：aabbcc 含 "abc" 的全部 bigram 但非其子串——候选为超集，
        // 由 contains 过滤剔除（证明 bigram 倒排不是精确命中集）
        let abc = KeywordToken::new("abc").unwrap();
        let candidates = index
            .substring_candidates(&abc)
            .expect("多字符查询词应有候选");
        assert!(
            candidates.iter().any(|t| t.as_str() == "aabbcc"),
            "超集候选应含假阳性 aabbcc"
        );
        assert_eq!(
            substring_hits_full_scan(&index, "abc"),
            Vec::<String>::new()
        );
    }

    // ---- 评分 ----

    /// recency：同为命中时，近期文档得分高于久远文档
    #[test]
    fn recency_boosts_newer_doc() {
        let mut index = KeywordIndex::new();
        let old_id = uuid::Uuid::new_v4();
        let new_id = uuid::Uuid::new_v4();
        index.index_parsed(
            l1_ref(old_id, "p1"),
            Some("爬山"),
            0.5,
            NOW_MS - 60 * 86_400_000,
        );
        index.index_parsed(l1_ref(new_id, "p1"), Some("爬山"), 0.5, NOW_MS - 86_400_000);
        let results =
            index.query_with_time(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 10), NOW_MS);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, l1_ref(new_id, "p1"), "近期文档应排前");
    }

    /// salience：同时期文档，高显著性命中得分更高
    #[test]
    fn salience_boosts_higher_significance() {
        let mut index = KeywordIndex::new();
        let low_id = uuid::Uuid::new_v4();
        let high_id = uuid::Uuid::new_v4();
        index.index_parsed(l1_ref(low_id, "p1"), Some("爬山"), 0.2, NOW_MS - 86_400_000);
        index.index_parsed(
            l1_ref(high_id, "p1"),
            Some("爬山"),
            0.9,
            NOW_MS - 86_400_000,
        );
        let results =
            index.query_with_time(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 10), NOW_MS);
        assert_eq!(results[0].0, l1_ref(high_id, "p1"), "高显著性命中应排前");
    }

    /// IDF：文档频率越高信息量越低（验证 idf 单调性 + 倒排 df 统计正确）
    #[test]
    fn idf_prefers_rare_term() {
        let mut index = KeywordIndex::new();
        // 文档 A 只含稀有词"量子"
        index.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "p1"),
            Some("量子"),
            0.5,
            NOW_MS - 86_400_000,
        );
        // 再加 9 篇含高频词"工作"的文档（含初始 1 篇共 9 篇命中）
        for _ in 0..9 {
            index.index_parsed(
                l1_ref(uuid::Uuid::new_v4(), "p1"),
                Some("工作"),
                0.5,
                NOW_MS - 86_400_000,
            );
        }
        let df_work = index
            .exact_inverted
            .get(&KeywordToken::new("工作").unwrap())
            .map(|v| v.len())
            .unwrap_or(0);
        assert_eq!(df_work, 9);
        let n = index.doc_count() as f64;
        let idf_rare = KeywordIndex::idf(n, 1.0);
        let idf_common = KeywordIndex::idf(n, df_work as f64);
        assert!(idf_rare > idf_common, "稀有词 IDF 应更高");
    }

    // ---- 维护 ----

    /// remove 单文档
    #[test]
    fn remove_doc() {
        let mut index = sample_index();
        let id = uuid::Uuid::new_v4();
        let reff = l1_ref(id, "user-0001");
        index.index_parsed(reff.clone(), Some("独有词"), 0.5, NOW_MS);
        assert_eq!(index.doc_count(), 4);
        assert!(index.remove(&reff));
        assert!(!index.remove(&reff), "重复移除返回 false");
        assert_eq!(index.doc_count(), 3);
        let results = index.query(&q(&["独有词"], None, MatchStrategy::Exact, 10));
        assert!(results.is_empty());
    }

    /// remove_doc_batch 批量移除 L1/L2
    #[test]
    fn remove_doc_batch() {
        let mut index = sample_index();
        let l1_a = uuid::Uuid::new_v4();
        let l1_b = uuid::Uuid::new_v4();
        index.index_parsed(l1_ref(l1_a, "p"), Some("甲"), 0.5, NOW_MS);
        index.index_parsed(l1_ref(l1_b, "p"), Some("乙"), 0.5, NOW_MS);
        let removed = index.remove_doc_batch(&[l1_a, l1_b], &[]);
        assert_eq!(removed, 2);
        assert!(
            index
                .query(&q(&["甲"], None, MatchStrategy::Exact, 5))
                .is_empty()
        );
    }

    /// 精准移除的倒排维护：swap_remove 下标修正、空 postings 连键清理、bigram 清理、
    /// 共享 token 的 postings 重映射（被删文档与搬移文档共享 token）。
    #[test]
    fn remove_keeps_inverted_consistent() {
        let mut index = KeywordIndex::new();
        let keep_a = l1_ref(uuid::Uuid::new_v4(), "p1");
        let remove = l1_ref(uuid::Uuid::new_v4(), "p1");
        let keep_b = l1_ref(uuid::Uuid::new_v4(), "p1");

        index.index_parsed(keep_a.clone(), Some("工作压力, 爬山"), 0.5, NOW_MS);
        index.index_parsed(remove.clone(), Some("独有词"), 0.5, NOW_MS);
        index.index_parsed(keep_b.clone(), Some("爬山, 露营"), 0.5, NOW_MS);

        // 移除中间文档：末尾文档被 swap 到它的位置，剩余两篇仍可命中；
        // "爬山" 为跨 3 篇的共享 token，其 postings 需从 {0,1,2} 修正为 {0,1}
        assert!(index.remove(&remove));
        assert_eq!(
            index
                .query(&q(&["工作压力"], Some("p1"), MatchStrategy::Exact, 5))
                .len(),
            1
        );
        assert_eq!(
            index
                .query(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 5))
                .len(),
            2,
            "共享 token 的两篇文档都应命中"
        );
        assert_eq!(
            index
                .query(&q(&["露营"], Some("p1"), MatchStrategy::Exact, 5))
                .len(),
            1,
            "搬移文档的独有 token 应以新下标命中"
        );

        // 被删文档的独有词：精确倒排与 bigram 倒排均清理
        let unique = KeywordToken::new("独有词").unwrap();
        assert!(!index.exact_inverted.contains_key(&unique));
        assert!(
            !index
                .token_bigrams
                .values()
                .any(|set| set.contains(&unique))
        );

        // 重索引被搬移过的文档（幂等路径 + 下标修正后仍正确）
        index.index_parsed(keep_b.clone(), Some("爬山, 露营"), 0.5, NOW_MS);
        assert_eq!(index.doc_count(), 2);
        assert_eq!(
            index
                .query(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 5))
                .len(),
            2
        );
        assert_eq!(
            index
                .query(&q(&["露营"], Some("p1"), MatchStrategy::Exact, 5))
                .len(),
            1
        );
    }

    /// 容量上限：默认 None 不限制；设置上限后按 created_at 最旧驱逐。
    #[test]
    fn max_docs_evicts_oldest() {
        let mut index = KeywordIndex::new();
        assert_eq!(index.doc_capacity(), None, "默认不限制");

        index.set_max_docs(Some(2));
        let old_id = uuid::Uuid::new_v4();
        let mid_id = uuid::Uuid::new_v4();
        index.index_parsed(
            l1_ref(old_id, "p1"),
            Some("旧词"),
            0.5,
            NOW_MS - 20 * 86_400_000,
        );
        index.index_parsed(
            l1_ref(mid_id, "p1"),
            Some("中词"),
            0.5,
            NOW_MS - 10 * 86_400_000,
        );
        assert_eq!(index.doc_count(), 2, "未超上限不驱逐");
        assert_eq!(index.doc_capacity(), Some(2));

        // 第 3 篇写入触发驱逐：最旧的"旧词"文档被移除
        index.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "p1"),
            Some("新词"),
            0.5,
            NOW_MS,
        );
        assert_eq!(index.doc_count(), 2);
        assert!(
            index
                .query(&q(&["旧词"], Some("p1"), MatchStrategy::Exact, 5))
                .is_empty(),
            "最旧文档应被驱逐"
        );
        assert_eq!(
            index
                .query(&q(&["中词"], Some("p1"), MatchStrategy::Exact, 5))
                .len(),
            1
        );
        assert_eq!(
            index
                .query(&q(&["新词"], Some("p1"), MatchStrategy::Exact, 5))
                .len(),
            1
        );

        // 解除上限后恢复不限制
        index.set_max_docs(None);
        index.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "p1"),
            Some("追加词"),
            0.5,
            NOW_MS,
        );
        assert_eq!(index.doc_count(), 3);
        assert_eq!(index.doc_capacity(), None);
    }

    /// 幂等重建：同 reff 重复 index 覆盖而非累积
    #[test]
    fn reindex_is_idempotent() {
        let mut index = KeywordIndex::new();
        let reff = l1_ref(uuid::Uuid::new_v4(), "p1");
        index.index_parsed(reff.clone(), Some("旧词"), 0.5, NOW_MS);
        assert_eq!(index.doc_count(), 1);
        // 同文档更新关键词
        index.index_parsed(reff.clone(), Some("新词"), 0.9, NOW_MS);
        assert_eq!(index.doc_count(), 1, "重复 index 不累积文档");
        assert!(
            index
                .query(&q(&["旧词"], None, MatchStrategy::Exact, 5))
                .is_empty()
        );
        assert_eq!(
            index
                .query(&q(&["新词"], None, MatchStrategy::Exact, 5))
                .len(),
            1
        );
    }

    /// clear 清空全部
    #[test]
    fn clear_resets() {
        let mut index = sample_index();
        index.clear();
        assert_eq!(index.doc_count(), 0);
        assert!(
            index
                .query(&q(&["工作压力"], None, MatchStrategy::Exact, 5))
                .is_empty()
        );
    }

    /// top_k 截断
    #[test]
    fn top_k_truncates() {
        let mut index = KeywordIndex::new();
        for i in 0..5 {
            index.index_parsed(
                l1_ref(uuid::Uuid::new_v4(), "p1"),
                Some("爬山"),
                0.5,
                NOW_MS - i * 86_400_000,
            );
        }
        let results = index.query(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 3));
        assert_eq!(results.len(), 3);
    }

    /// 同文档命中多个查询词时不重复计分（每个文档只出现一次）
    #[test]
    fn one_result_per_doc() {
        let mut index = KeywordIndex::new();
        index.index_parsed(
            l1_ref(uuid::Uuid::new_v4(), "p1"),
            Some("工作压力, 加班"),
            0.5,
            NOW_MS - 86_400_000,
        );
        let results = index.query(&q(
            &["工作压力", "加班"],
            Some("p1"),
            MatchStrategy::Exact,
            5,
        ));
        assert_eq!(results.len(), 1, "一篇文档只应出现在结果中一次");
    }

    /// Pool 词条不入索引（debug_assert 仅在测试构建可验证，此处验证 index 忽略语义由上层保证）
    #[test]
    fn query_empty_keywords() {
        let index = sample_index();
        let empty = q(&[], None, MatchStrategy::Exact, 5);
        assert!(index.query(&empty).is_empty());
    }

    /// created_at 同分排序稳定性
    #[test]
    fn same_score_tiebreak_by_newest() {
        let mut index = KeywordIndex::new();
        let newer = l1_ref(uuid::Uuid::new_v4(), "p1");
        let older = l1_ref(uuid::Uuid::new_v4(), "p1");
        index.index_parsed(older.clone(), Some("爬山"), 0.5, NOW_MS - 10 * 86_400_000);
        index.index_parsed(newer.clone(), Some("爬山"), 0.5, NOW_MS - 86_400_000);
        let results =
            index.query_with_time(&q(&["爬山"], Some("p1"), MatchStrategy::Exact, 5), NOW_MS);
        assert_eq!(results[0].0, newer);
        assert_eq!(results[1].0, older);
    }
}
