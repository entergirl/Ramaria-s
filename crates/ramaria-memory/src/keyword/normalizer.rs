//! crates/ramaria-memory/src/keyword/normalizer.rs — 关键词统一标准化器
//!
//! 设计特点（keyword-design §4，解决 P1「解析规则分散重复」）:
//! - 提供 `KeywordNormalizer` trait，将任意文本输入转换为标准 `KeywordToken` 列表
//! - 三种标准实现按场景取用：
//!   - `CommaSeparatedNormalizer`：LLM 输出的逗号分隔关键词（L1 摘要 / L2 事件 /
//!     降级合并 / 主分类提取）
//!   - `BigramNormalizer`：自由文本 CJK bigram + 英文按字母边界切分（无词典）
//!   - `BigramWithDictionaryNormalizer`：词典增强分词——先最大正向匹配 keyword_pool
//!     词典中的完整复合词，未命中再回退 bigram（补偿 bigram 拆散组合关键词的损失，P4）
//! - 统一收敛原分散在 bm25 / l1/summarizer / inference/stats / example_selector /
//!   event/degrade 的 5 处重复解析逻辑
//!
//! 设计决策:
//! - `normalize` 返回 `KeywordToken` 而非 `String`：编译期保证下游消费者拿到已验证的
//!   标准化 token（trim + ASCII 小写 + 非空）
//! - `normalize` 不返回 `Result`：标准化操作不会失败，无效输入静默返回空列表
//! - bigram 系标准化器**不去重**：BM25 词频统计需要保留同一 token 的多次出现；
//!   去重/排序由消费方按场景自行处理（如 example_selector 排序去重）
//! - 纯内存纯函数，零 I/O，零异步

use std::collections::HashSet;

use ramaria_core::keyword::KeywordToken;

// =========================================================
// KeywordNormalizer trait
// =========================================================

/// 关键词标准化器——将任意文本输入转换为标准 `KeywordToken` 列表。
///
/// # 场景 → 实现 映射（keyword-design §4.5）
///
/// - L1 摘要 / L2 事件 / 降级事件的 keywords 字段、主分类提取 → `CommaSeparatedNormalizer`
/// - BM25 全文索引、示例筛选自由文本 → `BigramNormalizer`（无词典）或
///   `BigramWithDictionaryNormalizer`（词典增强，M3 T-V20-3-007 BM25 迁移后启用）
pub trait KeywordNormalizer {
    /// 标准化输入文本，返回顺序稳定的 token 列表。
    fn normalize(&self, input: &str) -> Vec<KeywordToken>;
}

/// CJK 统一表意文字基本区段 U+4E00–U+9FFF 与扩展 A 区 U+3400–U+4DBF。
fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

// =========================================================
// 实现 1：CommaSeparatedNormalizer
// =========================================================

/// 逗号分隔关键词标准化器。
///
/// # 标准化规则
///
/// 1. 按英文逗号 `,` 或中文逗号 `，` 分割
/// 2. trim 首尾空白
/// 3. ASCII 字母转小写（`KeywordToken::new` 保证，中文不变）
/// 4. 过滤空字符串与纯标点（`KeywordToken::new` 返回 None 的片段）
/// 5. 去重（保留首次出现顺序）
#[derive(Debug, Clone, Default)]
pub struct CommaSeparatedNormalizer;

impl KeywordNormalizer for CommaSeparatedNormalizer {
    fn normalize(&self, input: &str) -> Vec<KeywordToken> {
        let mut seen: HashSet<String> = HashSet::with_capacity(8);
        let mut tokens: Vec<KeywordToken> = Vec::with_capacity(8);

        for raw in input.split([',', '，']) {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            // 纯标点片段显式过滤（KeywordToken::new 不保证字母数字约束，此处补强）
            if !trimmed.chars().any(|c| c.is_alphanumeric()) {
                continue;
            }
            let Some(token) = KeywordToken::new(trimmed) else {
                continue; // 空 / 超长，静默过滤
            };
            // 去重：按标准化后文本判重（大小写不敏感）
            if seen.insert(token.as_str().to_string()) {
                tokens.push(token);
            }
        }
        tokens
    }
}

// =========================================================
// 实现 2：BigramNormalizer
// =========================================================

/// Bigram 分词标准化器——CJK 相邻字符二元组 + 英文按连续字母/数字边界切分。
///
/// # 语义（与 bm25::tokenize 逐字一致，M3 收拢重复解析）
///
/// - CJK 字符（含扩展 A 区）生成相邻字符 bigram，如 "机器学习" → [机器, 器学, 学习]
/// - 英文/数字按连续字母数字段切分并小写化，**过滤长度 < 2 字节**的段
/// - 标点/空白丢弃
/// - **不去重**：同一 token 出现多次时原样保留（供 BM25 词频统计）
///
/// # 边界
///
/// - 空输入 / 纯标点 → 空列表
/// - 单 CJK 字符不构成 bigram（需 ≥2 个相邻 CJK 字符）→ 不输出
#[derive(Debug, Clone, Default)]
pub struct BigramNormalizer;

impl KeywordNormalizer for BigramNormalizer {
    fn normalize(&self, input: &str) -> Vec<KeywordToken> {
        bigram_normalize(input, None)
    }
}

/// 无词典 bigram 分词核心（词典增强的实现复用同一扫描逻辑）。
fn bigram_normalize(input: &str, dict: Option<&DictState>) -> Vec<KeywordToken> {
    if input.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = input.chars().collect();
    let mut tokens: Vec<KeywordToken> = Vec::with_capacity(chars.len());
    let mut alpha_buf = String::with_capacity(32);

    // 刷出英文段：小写化后长度 < 2 字节的段丢弃（与 bm25::tokenize 的字节口径一致）
    let flush_alpha = |buf: &mut String, out: &mut Vec<KeywordToken>| {
        if buf.len() >= 2
            && let Some(t) = KeywordToken::new(buf)
        {
            out.push(t);
        }
        buf.clear();
    };

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        if is_cjk(c) {
            flush_alpha(&mut alpha_buf, &mut tokens);

            // 词典增强：在 CJK 位置优先尝试最长词典匹配（≥2 字符、命中即整体输出并跳过）
            if let Some(dict) = dict
                && let Some((word, next)) = dict.try_match_longest(&chars, i)
            {
                // 词典条目本身已由构造期标准化校验，直接装箱
                tokens.push(KeywordToken::from_validated(word));
                i = next;
                continue;
            }

            // 兜底 bigram：当前字符与下一相邻 CJK 字符构成二元组
            if i + 1 < chars.len() && is_cjk(chars[i + 1]) {
                let bigram: String = [c, chars[i + 1]].iter().collect();
                // bigram 恒为两个 CJK 字符，必能通过 KeywordToken 校验
                tokens.push(KeywordToken::from_validated(bigram));
            }
            i += 1;
        } else if c.is_alphanumeric() {
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

// =========================================================
// 实现 3：BigramWithDictionaryNormalizer
// =========================================================

/// 词典状态——keyword_pool 词条集合与最长词长缓存。
///
/// 单独成结构以支持 `BigramWithDictionaryNormalizer` 的多实例复用（词典可在热更新后重建）。
#[derive(Debug, Clone, Default)]
struct DictState {
    /// 词典条目（标准化后的文本 → 字符数），供最长匹配检索
    entries: HashSet<String>,
    /// 词典中最长词条的字符数（0 = 空词典）
    max_word_len: usize,
}

impl DictState {
    fn from_keywords(keywords: &[String]) -> Self {
        let mut entries: HashSet<String> = HashSet::with_capacity(keywords.len());
        let mut max_word_len = 0usize;
        for kw in keywords {
            let kw = kw.trim();
            if kw.is_empty() {
                continue;
            }
            // 词典仅用于 CJK 位置的整词匹配：含非 CJK 字符的词条（如含空格/英文的混合词）
            // 不可能在单个 CJK 连续段内命中，只会浪费最长匹配的探测，故直接排除。
            if !kw.chars().all(is_cjk) {
                continue;
            }
            // 仅收录 ≥2 字符的词典条目：单字符条目无法补偿 bigram 的完整性损失，
            // 且会让单字位置脱离 bigram 流，反而降低召回
            if kw.chars().count() >= 2 {
                // 以 ASCII 小写化后的规范化文本入典（KeywordToken 构造校验）
                if let Some(t) = KeywordToken::new(kw) {
                    let text = t.as_str().to_string();
                    max_word_len = max_word_len.max(text.chars().count());
                    entries.insert(text);
                }
            }
        }
        Self {
            entries,
            max_word_len,
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 在 CJK 序列中从位置 `start` 尝试最长正向词典匹配。
    ///
    /// 返回 `Some((匹配文本, 下一扫描位置))`；无命中返回 None。
    fn try_match_longest(&self, chars: &[char], start: usize) -> Option<(String, usize)> {
        if self.is_empty() {
            return None;
        }
        let max_len = self.max_word_len.min(chars.len() - start);
        // 从最长候选向下尝试（正向最大匹配，keyword-design §4.4）
        for len in (2..=max_len).rev() {
            let cand: String = chars[start..start + len].iter().collect();
            if self.entries.contains(&cand) {
                return Some((cand, start + len));
            }
        }
        None
    }
}

/// 词典增强 Bigram 分词标准化器。
///
/// # 解决的问题（P4）
///
/// 纯 bigram 会把组合关键词拆散并产生噪声 token：如 "工作压力" 被拆成
/// [工作, 作压, 压力]，其中 "作压" 是噪声。词典增强在每个 CJK 位置先尝试最大正向
/// 匹配 keyword_pool 中的完整词条，命中则整体输出，避免噪声并恢复组合词完整性。
///
/// # 算法（keyword-design §4.4）
///
/// 1. 构造期把 keyword_pool 全量词条载入词典（≥2 字符、标准化文本）
/// 2. 扫描文本：CJK 位置先试最长词典匹配（O(1) HashSet 判定，最多回看 max_word_len）
/// 3. 命中 → 输出完整关键词并跳过其字符；未命中 → 输出该位置 bigram 并前进一个字符
/// 4. 英文按字母边界切分（与无词典版一致）
///
/// # 降级
///
/// 词典为空（keyword_pool 为空）时退化为纯 `BigramNormalizer` 行为。
#[derive(Debug, Clone, Default)]
pub struct BigramWithDictionaryNormalizer {
    dict: DictState,
}

impl BigramWithDictionaryNormalizer {
    /// 从关键词词典构建标准化器。
    ///
    /// 参数:
    /// - `keywords`: keyword_pool 词条文本列表（可为别名未归一 / 含噪声，构造期统一清洗）。
    pub fn from_dictionary(keywords: &[String]) -> Self {
        Self {
            dict: DictState::from_keywords(keywords),
        }
    }

    /// 词典是否为空（空词典时行为与 `BigramNormalizer` 完全一致）。
    pub fn is_empty(&self) -> bool {
        self.dict.is_empty()
    }

    /// 返回词典中最大词条字符数（0 = 空词典）。
    pub fn max_word_len(&self) -> usize {
        self.dict.max_word_len
    }
}

impl KeywordNormalizer for BigramWithDictionaryNormalizer {
    fn normalize(&self, input: &str) -> Vec<KeywordToken> {
        bigram_normalize(input, Some(&self.dict))
    }
}

// =========================================================
// 便捷函数
// =========================================================

/// 将标准化器输出统一去重为 `Vec<String>`（保留首次出现顺序）。
///
/// 用途: 需要「去重 + 文本列表」输出的调用方（如词典候选、UI 展示）。
pub fn dedup_to_strings(tokens: Vec<KeywordToken>) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::with_capacity(tokens.len());
    tokens
        .into_iter()
        .filter_map(|t| {
            let s = t.into_inner();
            seen.insert(s.clone()).then_some(s)
        })
        .collect()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(tokens: Vec<KeywordToken>) -> Vec<String> {
        tokens.into_iter().map(|t| t.into_inner()).collect()
    }

    // ── CommaSeparatedNormalizer ──

    /// 中英文逗号、trim、小写、去重、顺序保持
    #[test]
    fn comma_basic() {
        let n = CommaSeparatedNormalizer;
        let out = strs(n.normalize("工作压力, 职场焦虑，WORK , 工作压力"));
        assert_eq!(out, vec!["工作压力", "职场焦虑", "work"]);
    }

    /// 空 / 纯标点 / 空白 → 空列表（不 panic）
    #[test]
    fn comma_invalid_inputs() {
        let n = CommaSeparatedNormalizer;
        assert!(n.normalize("").is_empty());
        assert!(n.normalize("   ").is_empty());
        assert!(n.normalize("，, ，，").is_empty());
        assert!(n.normalize("！！！").is_empty());
    }

    /// 单个 CJK / 数字 / 英文均可作合法关键词
    #[test]
    fn comma_single_items() {
        let n = CommaSeparatedNormalizer;
        assert_eq!(strs(n.normalize("家")), vec!["家"]);
        assert_eq!(strs(n.normalize("996")), vec!["996"]);
        assert_eq!(strs(n.normalize("ChatGPT")), vec!["chatgpt"]);
    }

    /// 大小写不敏感去重
    #[test]
    fn comma_dedup_case_insensitive() {
        let n = CommaSeparatedNormalizer;
        let out = strs(n.normalize("AI, ai, Ai"));
        assert_eq!(out, vec!["ai"]);
    }

    // ── BigramNormalizer ──

    /// CJK bigram / 英文切分 / 混合 / 单字符边界（与 bm25::tokenize 口径一致）
    #[test]
    fn bigram_basic() {
        let n = BigramNormalizer;
        let out = strs(n.normalize("机器学习"));
        assert_eq!(out, vec!["机器", "器学", "学习"]);

        let out = strs(n.normalize("Machine Learning"));
        assert_eq!(out, vec!["machine", "learning"]);

        let out = strs(n.normalize("我在学Rust和Python"));
        assert!(out.contains(&"rust".to_string()));
        assert!(out.contains(&"python".to_string()));
        // 中文 bigram 相邻存在即可
        assert!(out.iter().any(|t| t == "我在" || t == "在学"));

        // 空 / 单 CJK / 纯标点 / 单字母 → 空
        assert!(n.normalize("").is_empty());
        assert!(n.normalize("我").is_empty());
        assert!(n.normalize("！？。").is_empty());
        assert!(n.normalize("a b c").is_empty());
    }

    /// bigram 不去重（BM25 词频依赖重复 token）
    #[test]
    fn bigram_keeps_duplicates() {
        let n = BigramNormalizer;
        let out = strs(n.normalize("天气天气"));
        assert_eq!(out, vec!["天气", "气天", "天气"]);
    }

    // ── BigramWithDictionaryNormalizer ──

    /// 词典命中完整词条，避免噪声 bigram
    #[test]
    fn dict_word_kept_whole() {
        let n = BigramWithDictionaryNormalizer::from_dictionary(&["工作压力".to_string()]);
        let out = strs(n.normalize("工作压力很大"));
        assert!(out.contains(&"工作压力".to_string()));
        assert!(
            !out.contains(&"作压".to_string()),
            "词典命中后不应产生跨词噪声 bigram"
        );
        // 剩余部分按 bigram 切分：很大
        assert!(out.contains(&"很大".to_string()));
    }

    /// 空词典退化为纯 bigram
    #[test]
    fn dict_empty_falls_back_to_bigram() {
        let n = BigramWithDictionaryNormalizer::from_dictionary(&[]);
        assert!(n.is_empty());
        let plain = BigramNormalizer;
        let a = strs(n.normalize("机器学习"));
        let b = strs(plain.normalize("机器学习"));
        assert_eq!(a, b);
    }

    /// 多词长匹配：取最长命中词条
    #[test]
    fn dict_longest_match_wins() {
        let n = BigramWithDictionaryNormalizer::from_dictionary(&[
            "工作".to_string(),
            "工作压力".to_string(),
            "压力".to_string(),
        ]);
        let out = strs(n.normalize("工作压力"));
        // 最长词条优先 → 整体输出一次
        assert_eq!(out, vec!["工作压力"]);
    }

    /// 词典只影响 CJK；非纯 CJK 词条不入典，英文切分与无词典版一致
    #[test]
    fn dict_alpha_unchanged() {
        let n = BigramWithDictionaryNormalizer::from_dictionary(&["rust".to_string()]);
        // "rust" 含非 CJK 字符 → 不入典（英文段本就不会被 bigram 拆散，无需词典补偿）
        assert!(n.is_empty());
        let out = strs(n.normalize("使用Rust开发"));
        assert!(out.contains(&"rust".to_string()));
        assert!(out.contains(&"使用".to_string()));
        assert!(out.contains(&"开发".to_string()));
    }

    /// 单字符词典条目被忽略（不脱离 bigram 流）
    #[test]
    fn dict_ignores_single_char() {
        let n = BigramWithDictionaryNormalizer::from_dictionary(&[
            "家".to_string(),
            "工作".to_string(),
        ]);
        assert_eq!(n.max_word_len(), 2);
        let out = strs(n.normalize("回家工作"));
        // 家 不成词 → bigram；工作 → 词典整词
        assert!(out.contains(&"工作".to_string()));
        assert!(out.contains(&"回家".to_string()));
    }

    /// 构造期清洗：trim / 小写 / 无效词条过滤 / 非纯 CJK 词条过滤
    #[test]
    fn dict_construction_cleans_input() {
        let n = BigramWithDictionaryNormalizer::from_dictionary(&[
            "  Work Stress ".to_string(),
            "".to_string(),
            "！".to_string(),
            "学习 工作".to_string(),
        ]);
        assert!(n.is_empty(), "上述词条（含空格/标点/非纯 CJK）均不应入典");
        let out = strs(n.normalize("work stress"));
        assert_eq!(out, vec!["work", "stress"]);
    }

    // ── 便捷函数 ──

    #[test]
    fn dedup_to_strings_keeps_order() {
        let n = CommaSeparatedNormalizer;
        let tokens = n.normalize("b, a, b, c");
        let out = dedup_to_strings(tokens);
        assert_eq!(out, vec!["b", "a", "c"]);
    }
}
