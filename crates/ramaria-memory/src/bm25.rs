//! crates/ramaria-memory/src/bm25.rs — BM25 全文检索引擎
//!
//! 设计特点:
//! - 分词默认中文字符二元组（bigram）+ 英文按字母边界切分，零外部依赖
//! - 标准 Okapi BM25 算法：k1=1.2, b=0.75
//! - 在内存中构建倒排索引（term→doc→tf），支持增量添加/移除文档
//! - 支持带结果上限的截断检索（最小堆 top-k）与词典代次观测（陈旧文档计数）
//! - 分词器为**实例级配置**：`Bm25Index` 默认空词典（=纯 bigram），可注入
//!   keyword_pool 规范词启用词典增强分词（整词命中、消除跨词噪声）
//! - 纯计算模块，零 I/O，不依赖数据库或异步运行时
//!
//! 设计决策（不依赖 jieba-rs）:
//! - jieba-rs 依赖 C 编译环境，跨平台打包复杂
//! - 中文 bigram 分词在 BM25 场景下效果与 jieba 分词相当（信息检索领域已验证）
//! - 英文 token 按 Unicode 字母边界切分并小写化
//! - 分词统一委托 `keyword::normalizer` 标准化器，与 example_selector 共用同一
//!   解析实现；`BigramWithDictionaryNormalizer` 空词典时与 `BigramNormalizer`
//!   逐 token 等价，故默认路径行为恒定。

use crate::keyword::normalizer::{
    BigramNormalizer, BigramWithDictionaryNormalizer, KeywordNormalizer,
};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

// =========================================================
// 分词器
// =========================================================

/// 中文/英文混合分词器（统一标准化器入口，keyword-design §4.5）。
///
/// # 语义（由 `BigramNormalizer` 保证，与原内联实现逐字等价）
///
/// - 中文（ CJK 统一表意文字区段 U+4E00–U+9FFF，扩展 A 区 U+3400–U+4DBF）：
///   生成相邻字符二元组（bigram），如 "机器学习" → ["机器", "器学", "学习"]
/// - 英文/数字：按 Unicode 字母/数字边界切分，小写化，过滤长度 < 2 **字节**的 token
/// - 标点/空白：丢弃
/// - 输出保持原始顺序且**不去重**（供 BM25 tf 统计）
///
/// 与 `prompt::example_selector::extract_keywords` 的关系（v1.5 审查批 2 / M3 收拢）:
/// - 两者分词均委托 `BigramNormalizer`；差异仅保留在**消费侧后处理**：
///   `extract_keywords` 排序去重并按字符数过滤（≥2），本函数保留字节数口径且不去重。
///
/// 示例:
/// ```rust
/// use ramaria_memory::bm25::tokenize;
/// let tokens = tokenize("我在学习Rust编程");
/// assert!(tokens.contains(&"我在".to_string()));
/// assert!(tokens.contains(&"学习".to_string()));
/// assert!(tokens.contains(&"编程".to_string()));
/// assert!(tokens.contains(&"rust".to_string()));
/// ```
pub fn tokenize(text: &str) -> Vec<String> {
    BigramNormalizer
        .normalize(text)
        .into_iter()
        .map(|t| t.into_inner())
        .collect()
}

/// 对文本字段列表进行分词并合并去重。
///
/// 用于构建文档的 term frequency 映射。
pub fn tokenize_fields(fields: &[&str]) -> Vec<String> {
    let mut all = Vec::new();
    for field in fields {
        all.extend(tokenize(field));
    }
    all
}

// =========================================================
// BM25 索引
// =========================================================

/// 文档标识符——统一 L1（UUID）、L2（i64）和图谱实体。
///
/// 职责:
/// - 为 BM25、向量、图谱三通道提供统一的文档标识。
/// - `L1`/`L2` 对应真实存储文档，`Graph` 对应图谱检索命中的实体（无具体数据库 ID）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DocId {
    /// L1 层文档（UUID）
    L1(uuid::Uuid),
    /// L2 层文档（事件，i64 主键）
    L2(i64),
    /// 图谱检索命中的实体（无具体数据库 ID，仅有实体名）
    Graph(String),
}

impl std::fmt::Display for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocId::L1(id) => write!(f, "L1:{}", id),
            DocId::L2(id) => write!(f, "L2:{}", id),
            DocId::Graph(entity) => write!(f, "graph:{}", entity),
        }
    }
}

/// BM25 索引配置。
#[derive(Debug, Clone)]
pub struct Bm25Config {
    /// 词频饱和度参数（默认 1.2）
    pub k1: f64,
    /// 文档长度归一化参数（默认 0.75）
    pub b: f64,
}

impl Default for Bm25Config {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

/// 单个文档在 BM25 索引中的记录。
#[derive(Debug, Clone)]
struct Bm25Doc {
    /// 词 → 词频 映射
    term_freq: HashMap<String, u32>,
    /// 文档长度（总 token 数）
    doc_len: u32,
    /// 写入时的分词词典代次
    dict_epoch: u64,
}

/// BM25 内存索引。
///
/// 职责:
/// - 维护文档集合的倒排索引
/// - 提供 BM25 评分查询（全量 / 带结果上限两种模式）
/// - 支持增量添加和移除文档
/// - 承载**实例级**分词器（默认空词典 = 纯 bigram，可注入词典增强分词）
/// - 跟踪词典代次，暴露按旧口径写入的陈旧文档数量
///
/// 字段约定:
/// - `tokenizer`: 索引侧（`add_tokenized`）与查询侧（`search` / `search_limited`）
///   共用同一分词器，保证索引/查询口径一致；空词典时与 `BigramNormalizer` 逐 token 等价。
/// - `dict_epoch` / `dict_fingerprint`: 词典内容变化时推进的代次与顺序无关指纹，
///   用于识别哪些文档仍按旧口径分词（`stale_doc_count`）。
#[derive(Debug, Clone, Default)]
pub struct Bm25Index {
    /// 文档记录：doc_id → 文档内部表示
    docs: HashMap<DocId, Bm25Doc>,
    /// 倒排索引：term → 包含该词的文档数（document frequency）
    df: HashMap<String, u32>,
    /// 所有文档的总词数之和
    total_tokens: u32,
    /// 实例级分词器：空词典（默认）退化为纯 bigram
    tokenizer: BigramWithDictionaryNormalizer,
    /// 当前分词词典代次（词典内容变化时递增）
    dict_epoch: u64,
    /// 当前词典内容的顺序无关指纹（空词典为 0）
    dict_fingerprint: u64,
}

impl Bm25Index {
    /// 创建空的 BM25 索引（默认纯 bigram 分词，行为与 `tokenize` 自由函数一致）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 使用词典增强分词创建 BM25 索引。
    ///
    /// 参数:
    /// - `keywords`: keyword_pool 规范词列表（可为空；空词典退化为纯 bigram）。
    ///
    /// 说明:
    /// - 索引与查询统一按词典整词匹配，消除 bigram 跨词噪声（如 "作压"）。
    /// - 初始代次：空词典为 0，非空词典为 1；代次只增不减，供陈旧文档统计。
    pub fn with_dictionary(keywords: &[String]) -> Self {
        let fingerprint = dictionary_fingerprint(keywords);
        Self {
            tokenizer: BigramWithDictionaryNormalizer::from_dictionary(keywords),
            dict_fingerprint: fingerprint,
            dict_epoch: if fingerprint == 0 { 0 } else { 1 },
            ..Self::default()
        }
    }

    /// 替换实例分词词典（供重建/热更新复用同一索引实例）。
    ///
    /// 参数:
    /// - `keywords`: keyword_pool 规范词列表（空 = 清除词典，退化为纯 bigram）。
    ///
    /// 说明:
    /// - 仅影响后续 `add_tokenized` / `search` 的分词口径；已有文档不会自动
    ///   重分词，需由调用方在重建场景下清空后重新添加，可用 `stale_doc_count`
    ///   检查尚未按新词典重建的文档数。
    /// - 仅当词典内容（顺序无关指纹）发生变化时才推进代次并替换分词器；
    ///   重复注入同一词典不会换代次。
    pub fn set_dictionary(&mut self, keywords: &[String]) {
        let fingerprint = dictionary_fingerprint(keywords);
        if fingerprint == self.dict_fingerprint {
            // 词典内容未变化：代次与分词器保持原状
            return;
        }

        self.dict_fingerprint = fingerprint;
        self.dict_epoch = self.dict_epoch.saturating_add(1);
        self.tokenizer = BigramWithDictionaryNormalizer::from_dictionary(keywords);

        // 词典切换不触发存量重分词：存在旧口径文档时提示调用方重建
        let stale_docs = self.stale_doc_count();
        if stale_docs > 0 {
            tracing::warn!(
                stale_docs,
                "BM25 词典已切换：存量文档仍按旧口径分词，命中率可能下降，需清空后重建"
            );
        }
    }

    /// 词典是否为空（空词典时索引/查询分词与纯 bigram 完全一致）。
    pub fn dictionary_is_empty(&self) -> bool {
        self.tokenizer.is_empty()
    }

    /// 按旧分词词典代次写入、尚未重建的文档数。
    ///
    /// 说明:
    /// - `set_dictionary` 只改变后续分词口径，已有文档不会自动重分词；
    /// - 返回值大于 0 表示索引内为混合口径，命中率可能下降，调用方可据此
    ///   清空索引并按新词典重建。
    pub fn stale_doc_count(&self) -> usize {
        self.docs
            .values()
            .filter(|doc| doc.dict_epoch != self.dict_epoch)
            .count()
    }

    /// 返回索引中的文档总数。
    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    /// 平均文档长度。
    pub fn avg_doc_len(&self) -> f64 {
        if self.docs.is_empty() {
            1.0 // 避免除零
        } else {
            self.total_tokens as f64 / self.docs.len() as f64
        }
    }

    /// 增量添加一篇文档。
    ///
    /// 说明:
    /// - 若 doc_id 已存在，旧记录被替换（覆盖语义）；
    /// - 新文档盖当前词典代次，供 `stale_doc_count` 识别口径陈旧的文档。
    ///
    /// 接收 `Vec<String>` 的所有权以消除 clone 开销：
    /// 调用方（通常是 `tokenize_fields`）产出 tokens 后直接移动至此方法，
    /// 避免 `for token in tokens { token.clone }` 的逐项复制。
    pub fn add(&mut self, doc_id: DocId, tokens: Vec<String>) {
        // 移除旧文档（若存在）
        self.remove(&doc_id);

        let mut term_freq = HashMap::with_capacity(tokens.len());
        let mut doc_len = 0u32;

        // 消费 tokens 所有权，按 token 分组计数
        let mut unique_tokens: Vec<String> = Vec::with_capacity(tokens.len());
        for token in tokens {
            // 首次出现时记录到 unique_tokens（用于后续 df 更新）
            if !term_freq.contains_key(&token) {
                unique_tokens.push(token.clone());
            }
            *term_freq.entry(token).or_insert(0u32) += 1;
            doc_len += 1;
        }

        // 更新 document frequency —— 仅对唯一的 token 操作
        for token in unique_tokens {
            *self.df.entry(token).or_insert(0u32) += 1;
        }

        self.total_tokens += doc_len;
        self.docs.insert(
            doc_id,
            Bm25Doc {
                term_freq,
                doc_len,
                dict_epoch: self.dict_epoch,
            },
        );
    }

    /// 通过分词后的 token 列表添加文档。
    ///
    /// 说明:
    /// - 使用索引实例当前分词器对字段文本分词（默认 = `tokenize_fields` 自由函数
    ///   的纯 bigram 口径；注入词典后为词典增强口径），随后移动所有权到 `add`。
    pub fn add_tokenized(&mut self, doc_id: DocId, fields: &[&str]) {
        let tokens = self.tokenize_fields_with(fields);
        self.add(doc_id, tokens);
    }

    /// 按实例分词器对单段文本分词（索引与查询共用，保证口径一致）。
    fn tokenize_with(&self, text: &str) -> Vec<String> {
        self.tokenizer
            .normalize(text)
            .into_iter()
            .map(|t| t.into_inner())
            .collect()
    }

    /// 按实例分词器对字段列表分词并合并（不去重，供 BM25 词频统计）。
    fn tokenize_fields_with(&self, fields: &[&str]) -> Vec<String> {
        let mut all = Vec::new();
        for field in fields {
            all.extend(self.tokenize_with(field));
        }
        all
    }

    /// 移除一篇文档。
    ///
    /// 若 doc_id 不存在，静默返回。
    pub fn remove(&mut self, doc_id: &DocId) {
        if let Some(doc) = self.docs.remove(doc_id) {
            // 递减 document frequency
            for token in doc.term_freq.keys() {
                if let Some(cnt) = self.df.get_mut(token) {
                    if *cnt <= 1 {
                        self.df.remove(token);
                    } else {
                        *cnt -= 1;
                    }
                }
            }
            self.total_tokens = self.total_tokens.saturating_sub(doc.doc_len);
        }
    }

    /// 清空整个索引。
    pub fn clear(&mut self) {
        self.docs.clear();
        self.df.clear();
        self.total_tokens = 0;
    }
}

mod search;

// =========================================================
// 模块私有辅助
// =========================================================

/// 计算词典指纹：对每个关键词做 FNV-1a 哈希后顺序无关累加，空列表返回 0。
///
/// 说明:
/// - 仅用于判断词典内容是否变化（代次推进），不做安全用途；
/// - 顺序无关保证同一词典以不同顺序传入时指纹一致，不会误判为换代。
fn dictionary_fingerprint(keywords: &[String]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut acc: u64 = 0;
    for keyword in keywords {
        // 单个关键词独立计算 FNV-1a，再累加进总指纹
        let mut hash = FNV_OFFSET_BASIS;
        for byte in keyword.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        acc = acc.wrapping_add(hash);
    }
    acc
}

/// 堆内排序项：f64 分数不实现 `Ord`，手动用 `total_cmp` 实现（NaN 也参与比较，
/// 不会 panic）；同分按 `doc_id` 兜底，保证堆裁剪结果确定。
#[derive(Debug, Clone)]
struct ScoredDoc {
    score: f64,
    doc_id: DocId,
}

impl PartialEq for ScoredDoc {
    fn eq(&self, other: &Self) -> bool {
        self.score.total_cmp(&other.score) == std::cmp::Ordering::Equal
            && self.doc_id == other.doc_id
    }
}

impl Eq for ScoredDoc {}

impl PartialOrd for ScoredDoc {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScoredDoc {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| self.doc_id.cmp(&other.doc_id))
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
