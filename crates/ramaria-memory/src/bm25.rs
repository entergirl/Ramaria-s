//! crates/ramaria-memory/src/bm25.rs — BM25 全文检索引擎
//!
//! 设计特点:
//! - 分词默认中文字符二元组（bigram）+ 英文按字母边界切分，零外部依赖
//! - 标准 Okapi BM25 算法：k1=1.2, b=0.75
//! - 在内存中构建倒排索引（term→doc→tf），支持增量添加/移除文档
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
use std::collections::HashMap;

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
}

/// BM25 内存索引。
///
/// 职责:
/// - 维护文档集合的倒排索引
/// - 提供 BM25 评分查询
/// - 支持增量添加和移除文档
/// - 承载**实例级**分词器（默认空词典 = 纯 bigram，可注入词典增强分词）
///
/// 字段约定:
/// - `tokenizer`: 索引侧（`add_tokenized`）与查询侧（`search`）共用同一分词器，
///   保证索引/查询口径一致；空词典时与 `BigramNormalizer` 逐 token 等价。
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
    pub fn with_dictionary(keywords: &[String]) -> Self {
        Self {
            tokenizer: BigramWithDictionaryNormalizer::from_dictionary(keywords),
            ..Self::default()
        }
    }

    /// 替换实例分词词典（供重建/热更新复用同一索引实例）。
    ///
    /// 参数:
    /// - `keywords`: keyword_pool 规范词列表（空 = 清除词典，退化为纯 bigram）。
    ///
    /// 说明:
    /// - 仅影响后续 `add_tokenized` / `search` 的分词口径；已添加的文档记录
    ///   不会自动重分词，需由调用方在重建场景下清空后重新添加。
    pub fn set_dictionary(&mut self, keywords: &[String]) {
        self.tokenizer = BigramWithDictionaryNormalizer::from_dictionary(keywords);
    }

    /// 词典是否为空（空词典时索引/查询分词与纯 bigram 完全一致）。
    pub fn dictionary_is_empty(&self) -> bool {
        self.tokenizer.is_empty()
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
    /// 若 doc_id 已存在，旧记录被替换（覆盖语义）。
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
        self.docs.insert(doc_id, Bm25Doc { term_freq, doc_len });
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

    /// 对查询文本执行 BM25 评分，返回所有文档的得分。
    ///
    /// 公式: score(D,Q) = Σ_{t∈Q∩D} IDF(t) · (f(t,D)·(k1+1)) / (f(t,D) + k1·(1−b + b·|D|/avgdl))
    ///
    /// 说明:
    /// - 查询按索引实例当前分词器切分（与索引构建口径一致；默认 = 纯 bigram）。
    ///
    /// 返回按得分降序排列的列表。
    pub fn search(&self, query: &str, config: &Bm25Config) -> Vec<(DocId, f64)> {
        let query_tokens = self.tokenize_with(query);
        if query_tokens.is_empty() || self.docs.is_empty() {
            return Vec::new();
        }

        let n = self.docs.len() as f64;
        let avgdl = self.avg_doc_len();

        // 查询词频
        let mut qf: HashMap<&str, u32> = HashMap::new();
        for t in &query_tokens {
            *qf.entry(t.as_str()).or_insert(0) += 1;
        }

        let mut scores: Vec<(DocId, f64)> = Vec::with_capacity(self.docs.len());

        for (doc_id, doc) in &self.docs {
            let mut score = 0.0_f64;

            for (qt, &q_tf) in &qf {
                // 跳过不在索引中的查询词
                let df = match self.df.get(*qt) {
                    Some(&d) => d as f64,
                    None => continue,
                };

                // IDF: log((N - df + 0.5) / (df + 0.5) + 1)
                let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();

                // TF 分量
                let tf = doc.term_freq.get(*qt).copied().unwrap_or(0) as f64;
                let doc_len = doc.doc_len as f64;

                let numerator = tf * (config.k1 + 1.0);
                let denominator = tf + config.k1 * (1.0 - config.b + config.b * doc_len / avgdl);
                let term_score = idf * numerator / denominator;

                score += term_score * (q_tf as f64);
            }

            if score > 0.0 {
                scores.push((doc_id.clone(), score));
            }
        }

        // 按得分降序排序
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // 截取 top-k（默认返回全部有分的文档，上层通过 RRF 控制数量）
        scores
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- tokenize ----

    /// tokenize 各输入参数化验证：中文 bigram / 英文小写 / 混合 / 空 / 标点 / 单字符过滤。
    #[test]
    fn tokenize_cases() {
        // 中文 bigram
        let tokens = tokenize("机器学习");
        assert!(tokens.contains(&"机器".to_string()));
        assert!(tokens.contains(&"器学".to_string()));
        assert!(tokens.contains(&"学习".to_string()));
        // 英文小写，过滤单字母
        let tokens = tokenize("Machine Learning");
        assert!(tokens.contains(&"machine".to_string()));
        assert!(tokens.contains(&"learning".to_string()));
        assert!(!tokens.iter().any(|t| t.len() < 2));
        // 中英混合
        let tokens = tokenize("我在学Rust和Python");
        assert!(tokens.contains(&"我在".to_string()) || tokens.contains(&"在学".to_string()));
        assert!(tokens.contains(&"rust".to_string()));
        assert!(tokens.contains(&"python".to_string()));
        // 空输入
        assert!(tokenize("").is_empty());
        // 单中文字符不构成 bigram
        assert!(tokenize("我").is_empty());
        // 标点被移除
        let tokens = tokenize("你好！世界？");
        assert!(!tokens.contains(&"！世".to_string()));
        assert!(tokens.contains(&"你好".to_string()));
        assert!(tokens.contains(&"世界".to_string()));
        // 单字母英文被过滤
        assert!(tokenize("a b c").is_empty(), "单字母 token 应被过滤");
    }

    // ---- Bm25Index ----

    #[test]
    fn index_empty_search_returns_empty() {
        let index = Bm25Index::new();
        let config = Bm25Config::default();
        let results = index.search("测试", &config);
        assert!(results.is_empty());
    }

    #[test]
    fn index_add_and_search_single_doc() {
        let mut index = Bm25Index::new();
        let config = Bm25Config::default();

        let doc_id = DocId::L1(uuid::Uuid::new_v4());
        index.add_tokenized(doc_id.clone(), &["今天天气很好适合出门"]);
        assert_eq!(index.doc_count(), 1);

        let results = index.search("天气", &config);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, doc_id);
        assert!(results[0].1 > 0.0);
    }

    #[test]
    fn index_search_ranking() {
        let mut index = Bm25Index::new();
        let config = Bm25Config::default();

        let doc_a = DocId::L1(uuid::Uuid::new_v4());
        let doc_b = DocId::L1(uuid::Uuid::new_v4());

        // doc_a 提到"天气"一次
        index.add_tokenized(doc_a.clone(), &["今天天气很好"]);
        // doc_b 提到"天气"多次
        index.add_tokenized(doc_b.clone(), &["天气天气天气很好"]);

        let results = index.search("天气", &config);
        assert_eq!(results.len(), 2);
        // doc_b 应有更高得分
        assert!(results[0].1 > results[1].1);
    }

    #[test]
    fn index_remove_doc() {
        let mut index = Bm25Index::new();
        let config = Bm25Config::default();

        let doc_id = DocId::L1(uuid::Uuid::new_v4());
        index.add_tokenized(doc_id.clone(), &["今天天气很好"]);
        assert_eq!(index.doc_count(), 1);

        index.remove(&doc_id);
        assert_eq!(index.doc_count(), 0);

        let results = index.search("天气", &config);
        assert!(results.is_empty());
    }

    #[test]
    fn index_remove_nonexistent() {
        let mut index = Bm25Index::new();
        let doc_id = DocId::L1(uuid::Uuid::new_v4());
        // 不应 panic
        index.remove(&doc_id);
    }

    #[test]
    fn index_clear() {
        let mut index = Bm25Index::new();
        index.add_tokenized(DocId::L1(uuid::Uuid::new_v4()), &["测试"]);
        index.add_tokenized(DocId::L2(42), &["测试2"]);
        assert_eq!(index.doc_count(), 2);

        index.clear();
        assert_eq!(index.doc_count(), 0);
        assert!(index.df.is_empty());
        assert_eq!(index.total_tokens, 0);
    }

    #[test]
    fn index_avg_doc_len() {
        let mut index = Bm25Index::new();

        // 空索引 avg = 1.0
        assert!((index.avg_doc_len() - 1.0).abs() < f64::EPSILON);

        // 添加 8 token 的文档（按字符拆分作为 tokens）
        let token_count = "机器学习很有意思".chars().count() as u32;
        let tokens: Vec<String> = "机器学习很有意思".chars().map(|c| c.to_string()).collect();
        index.add(DocId::L1(uuid::Uuid::new_v4()), tokens);
        assert!((index.avg_doc_len() - token_count as f64).abs() < 0.01);
    }

    #[test]
    fn index_add_overwrite() {
        let mut index = Bm25Index::new();
        let config = Bm25Config::default();
        let doc_id = DocId::L1(uuid::Uuid::new_v4());

        index.add_tokenized(doc_id.clone(), &["天气"]);
        // 覆盖添加
        index.add_tokenized(doc_id.clone(), &["吃饭"]);

        // 只有 "吃饭" 的索引
        let results_weather = index.search("天气", &config);
        assert!(results_weather.is_empty());

        let results_eat = index.search("吃饭", &config);
        assert!(!results_eat.is_empty());
    }

    // ---- DocId Display ----

    #[test]
    fn doc_id_display() {
        let l1_id = uuid::Uuid::new_v4();
        let display = DocId::L1(l1_id).to_string();
        assert!(display.starts_with("L1:"));
        assert!(display.contains(&l1_id.to_string()));

        let display = DocId::L2(42).to_string();
        assert_eq!(display, "L2:42");
    }

    // ---- 词典增强分词迁移（默认无词典=现状回归 / 有词典=整词口径）----

    /// 无词典默认路径回归：索引实例分词与自由函数 `tokenize`/`tokenize_fields` 逐 token 一致。
    #[test]
    fn default_no_dictionary_matches_free_tokenize() {
        let index = Bm25Index::new();
        assert!(index.dictionary_is_empty(), "默认分词器应为空词典");
        assert!(
            Bm25Index::with_dictionary(&[]).dictionary_is_empty(),
            "空词典应退化"
        );

        let text = "最近工作压力很大";
        let fields = [text, "学习Rust"];
        assert_eq!(index.tokenize_with(text), tokenize(text));
        assert_eq!(
            index.tokenize_fields_with(&fields),
            tokenize_fields(&fields)
        );
    }

    /// 有词典时：文档整词命中、查询按整词切分、跨词噪声（如 "作压"）不再命中。
    #[test]
    fn dictionary_keeps_whole_word_and_removes_noise() {
        let dict = ["工作压力".to_string()];
        let mut index = Bm25Index::with_dictionary(&dict);
        let config = Bm25Config::default();
        let doc_id = DocId::L1(uuid::Uuid::new_v4());
        index.add_tokenized(doc_id.clone(), &["工作压力很大"]);

        // token 分布：词典整词 + 尾部 bigram，无跨词噪声
        let tokens = index.tokenize_fields_with(&["工作压力很大"]);
        assert!(tokens.contains(&"工作压力".to_string()));
        assert!(tokens.contains(&"很大".to_string()));
        assert!(
            !tokens.contains(&"作压".to_string()),
            "词典命中后不得产生跨词噪声 token"
        );

        // 查询 "工作压力" 整词命中；噪声 "作压" 不命中（纯 bigram 口径会命中）
        let hit = index.search("工作压力", &config);
        assert_eq!(hit.len(), 1, "整词查询应命中一篇文档");
        assert_eq!(hit[0].0, doc_id);
        assert!(
            index.search("作压", &config).is_empty(),
            "索引无噪声 token 时 '作压' 不应命中"
        );
    }

    /// 词典热更新：重建场景先 set_dictionary 再覆盖文档，口径切换后噪声消失。
    #[test]
    fn set_dictionary_switches_tokenizer_on_rebuild() {
        let mut index = Bm25Index::new();
        let config = Bm25Config::default();
        let doc_id = DocId::L1(uuid::Uuid::new_v4());

        // 旧版本（纯 bigram）口径：噪声 "作压" 可命中
        index.add_tokenized(doc_id.clone(), &["工作压力很大"]);
        assert!(
            !index.search("作压", &config).is_empty(),
            "bigram 口径下 '作压' 应能命中（旧版行为基线）"
        );

        // 迁移：注入词典 → 覆盖重建同一文档 → 口径切为词典增强
        index.set_dictionary(&["工作压力".to_string()]);
        index.add_tokenized(doc_id.clone(), &["工作压力很大"]);
        assert!(
            index.search("作压", &config).is_empty(),
            "词典口径下噪声命中应消失"
        );
        assert_eq!(index.search("工作压力", &config).len(), 1);
    }
}
