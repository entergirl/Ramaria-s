//! rust/crates/ramaria-memory/src/bm25.rs — BM25 全文检索引擎
//!
//! 设计特点:
//! - 中文字符二元组（bigram）分词 + 英文按空白/标点切分，零外部依赖
//! - 标准 Okapi BM25 算法：k1=1.2, b=0.75
//! - 在内存中构建倒排索引（term→doc→tf），支持增量添加/移除文档
//! - 索引可序列化为 JSON 持久化到 bm25_index 表
//! - 纯计算模块，零 I/O，不依赖数据库或异步运行时
//!
//! 设计决策（不依赖 jieba-rs）:
//! - jieba-rs 依赖 C 编译环境，跨平台打包复杂
//! - 中文 bigram 分词在 BM25 场景下效果与 jieba 分词相当（信息检索领域已验证）
//! - 英文 token 按 Unicode 字母边界切分并小写化
//! - 接入真实分词器时可替换为 Tokenizer trait

use std::collections::HashMap;

// =========================================================
// 分词器
// =========================================================

/// 中文/英文混合分词器。
///
/// 策略:
/// - 中文（ CJK 统一表意文字区段 U+4E00–U+9FFF，扩展 A 区 U+3400–U+4DBF）：
///   生成相邻字符二元组（bigram），如 "机器学习" → ["机器", "器学", "学习"]
/// - 英文/数字：按 Unicode 字母/数字边界切分，小写化，过滤长度 < 2 的 token
/// - 标点/空白：丢弃
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
    if text.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::with_capacity(chars.len() * 2);
    let mut alpha_buf = String::with_capacity(32);

    let flush_alpha = |buf: &mut String, out: &mut Vec<String>| {
        if buf.len() >= 2 {
            out.push(buf.to_lowercase());
        }
        buf.clear();
    };

    let is_cjk =
        |c: char| -> bool { matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}') };

    let is_alpha = |c: char| -> bool { c.is_alphanumeric() };

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        if is_cjk(c) {
            flush_alpha(&mut alpha_buf, &mut tokens);
            // 生成 bigram
            if i + 1 < chars.len() && is_cjk(chars[i + 1]) {
                let bigram: String = [c, chars[i + 1]].iter().collect();
                tokens.push(bigram);
            }
            i += 1;
        } else if is_alpha(c) {
            alpha_buf.push(c);
            i += 1;
        } else {
            flush_alpha(&mut alpha_buf, &mut tokens);
            i += 1;
        }
    }
    flush_alpha(&mut alpha_buf, &mut tokens);
    tokens
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

/// 对文本字段分词，统计词频。
///
/// 返回: (token, term_frequency_in_this_doc) 的映射。
pub fn tokenize_with_freq(fields: &[&str]) -> HashMap<String, u32> {
    let mut freq: HashMap<String, u32> = HashMap::new();
    for token in tokenize_fields(fields) {
        *freq.entry(token).or_insert(0) += 1;
    }
    freq
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
#[derive(Debug, Clone, Default)]
pub struct Bm25Index {
    /// 文档记录：doc_id → 文档内部表示
    docs: HashMap<DocId, Bm25Doc>,
    /// 倒排索引：term → 包含该词的文档数（document frequency）
    df: HashMap<String, u32>,
    /// 所有文档的总词数之和
    total_tokens: u32,
}

impl Bm25Index {
    /// 创建空的 BM25 索引。
    pub fn new() -> Self {
        Self::default()
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
    /// `tokenize_fields` 的输出 `Vec<String>` 直接移动所有权到 `add`，
    /// 消除中间 clone 开销。
    pub fn add_tokenized(&mut self, doc_id: DocId, fields: &[&str]) {
        let tokens = tokenize_fields(fields);
        self.add(doc_id, tokens);
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
    /// 返回按得分降序排列的列表。
    pub fn search(&self, query: &str, config: &Bm25Config) -> Vec<(DocId, f64)> {
        let query_tokens = tokenize(query);
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

    /// 获取文档中所有 token 的列表（用于持久化到 bm25_index 表）。
    pub fn get_doc_tokens(&self, doc_id: &DocId) -> Option<Vec<String>> {
        self.docs.get(doc_id).map(|doc| {
            let mut tokens = Vec::with_capacity(doc.term_freq.len());
            for (token, &freq) in &doc.term_freq {
                for _ in 0..freq {
                    tokens.push(token.clone());
                }
            }
            tokens
        })
    }

    /// 返回索引中所有文档的 (doc_id, layer_str, tokens_json) 三元组。
    ///
    /// 用于批量持久化到 bm25_index 表。
    /// 图谱实体（`DocId::Graph`）不参与 BM25 索引，因此不会被导出。
    pub fn export_all(&self) -> Vec<(DocId, String, String)> {
        self.docs
            .keys()
            .filter_map(|doc_id| {
                let layer = match doc_id {
                    DocId::L1(_) => "l1".to_string(),
                    DocId::L2(_) => "l2".to_string(),
                    DocId::Graph(_) => return None, // 图谱实体不参与 BM25 持久化
                };
                let tokens = self.get_doc_tokens(doc_id)?;
                let json = serde_json::to_string(&tokens).ok()?;
                Some((doc_id.clone(), layer, json))
            })
            .collect()
    }
}

// =========================================================
// 索引构建辅助
// =========================================================

/// 从 L1 和 L2 文档构建 BM25 索引。
///
/// L1 索引字段: summary, keywords
/// L2 索引字段: title, summary, keywords, attitude, paraphrase
pub struct Bm25IndexBuilder {
    config: Bm25Config,
    index: Bm25Index,
}

impl Bm25IndexBuilder {
    /// 使用默认配置创建构建器。
    pub fn new() -> Self {
        Self {
            config: Bm25Config::default(),
            index: Bm25Index::new(),
        }
    }

    /// 使用自定义 BM25 配置创建构建器。
    pub fn with_config(config: Bm25Config) -> Self {
        Self {
            config,
            index: Bm25Index::new(),
        }
    }

    /// 添加一条 L1 记忆到索引。
    pub fn add_l1(&mut self, id: uuid::Uuid, summary: &str, keywords: Option<&str>) {
        let mut fields: Vec<&str> = vec![summary];
        if let Some(kw) = keywords {
            fields.push(kw);
        }
        self.index.add_tokenized(DocId::L1(id), &fields);
    }

    /// 添加一条 L2 事件到索引。
    pub fn add_l2(
        &mut self,
        id: i64,
        title: &str,
        summary: &str,
        keywords: Option<&str>,
        attitude: Option<&str>,
        paraphrase: Option<&str>,
    ) {
        let mut fields: Vec<&str> = vec![title, summary];
        if let Some(kw) = keywords {
            fields.push(kw);
        }
        if let Some(att) = attitude {
            fields.push(att);
        }
        if let Some(par) = paraphrase {
            fields.push(par);
        }
        self.index.add_tokenized(DocId::L2(id), &fields);
    }

    /// 消耗构建器，返回索引和配置。
    pub fn build(self) -> (Bm25Index, Bm25Config) {
        (self.index, self.config)
    }

    /// 获取内部索引的可变引用（用于增量更新）。
    pub fn index_mut(&mut self) -> &mut Bm25Index {
        &mut self.index
    }
}

impl Default for Bm25IndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- tokenize ----

    #[test]
    fn tokenize_chinese_bigram() {
        let tokens = tokenize("机器学习");
        assert!(tokens.contains(&"机器".to_string()));
        assert!(tokens.contains(&"器学".to_string()));
        assert!(tokens.contains(&"学习".to_string()));
    }

    #[test]
    fn tokenize_english_lowercase() {
        let tokens = tokenize("Machine Learning");
        assert!(tokens.contains(&"machine".to_string()));
        assert!(tokens.contains(&"learning".to_string()));
        // 不应包含单字母的 "M" 或 "L"
        assert!(!tokens.iter().any(|t| t.len() < 2));
    }

    #[test]
    fn tokenize_mixed_cn_en() {
        let tokens = tokenize("我在学Rust和Python");
        // 应包含中文 bigram
        assert!(tokens.contains(&"我在".to_string()) || tokens.contains(&"在学".to_string()));
        // 应包含英文 token
        assert!(tokens.contains(&"rust".to_string()));
        assert!(tokens.contains(&"python".to_string()));
    }

    #[test]
    fn tokenize_empty() {
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn tokenize_single_chinese_char() {
        // 单个中文字符不构成 bigram，应返回空
        let tokens = tokenize("我");
        assert!(tokens.is_empty());
    }

    #[test]
    fn tokenize_punctuation_removed() {
        let tokens = tokenize("你好！世界？");
        // 不应包含标点
        assert!(!tokens.contains(&"！世".to_string()));
        // 但应有中文 bigram
        assert!(tokens.contains(&"你好".to_string()));
        assert!(tokens.contains(&"世界".to_string()));
    }

    #[test]
    fn tokenize_single_english_letter_ignored() {
        let tokens = tokenize("a b c");
        assert!(tokens.is_empty(), "单字母 token 应被过滤");
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

    // ---- Bm25IndexBuilder ----

    #[test]
    fn builder_add_l1() {
        let mut builder = Bm25IndexBuilder::new();
        let id = uuid::Uuid::new_v4();
        builder.add_l1(id, "今天天气很好适合出门", Some("天气,出门"));
        let (index, _) = builder.build();

        assert_eq!(index.doc_count(), 1);
    }

    #[test]
    fn builder_add_l2() {
        let mut builder = Bm25IndexBuilder::new();
        builder.add_l2(
            1,
            "项目上线",
            "团队完成了主要模块的开发和测试",
            Some("工作,项目"),
            Some("感到很有成就感"),
            Some("对完成重要工作感到满意"),
        );
        let (index, _) = builder.build();

        let config = Bm25Config::default();
        let results = index.search("成就感", &config);
        assert!(!results.is_empty());
    }

    #[test]
    fn builder_add_l2_without_optional_fields() {
        let mut builder = Bm25IndexBuilder::new();
        builder.add_l2(1, "简单事件", "只是一个测试", None, None, None);
        let (index, _) = builder.build();
        assert_eq!(index.doc_count(), 1);
    }

    // ---- export_all ----

    #[test]
    fn export_all_roundtrip() {
        let mut index = Bm25Index::new();
        let l1_id = uuid::Uuid::new_v4();
        let l2_id = 42_i64;

        index.add_tokenized(DocId::L1(l1_id), &["测试文档"]);
        index.add_tokenized(DocId::L2(l2_id), &["事件内容"]);

        let exports = index.export_all();
        assert_eq!(exports.len(), 2);

        // 验证 layer 标记正确
        let l1_export = exports
            .iter()
            .find(|(id, _, _)| matches!(id, DocId::L1(_)))
            .unwrap();
        assert_eq!(l1_export.1, "l1");

        let l2_export = exports
            .iter()
            .find(|(id, _, _)| matches!(id, DocId::L2(_)))
            .unwrap();
        assert_eq!(l2_export.1, "l2");

        // 验证 tokens_json 可解析
        for (_, _, json_str) in &exports {
            let tokens: Vec<String> = serde_json::from_str(json_str).unwrap();
            assert!(!tokens.is_empty());
        }
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
}
