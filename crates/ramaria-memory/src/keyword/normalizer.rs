//! rust/crates/ramaria-memory/src/keyword/normalizer.rs - 复合关键词归一化器
//!
//! 设计特点:
//! - `BigramWithDictionaryNormalizer`: 基于词典的最大正向匹配算法
//! - 复合关键词（如"职业倦怠"）在匹配时保持整体，不被拆分为单字
//! - 别名映射表：将同义关键词（如"职场焦虑"）解析为规范形式（"工作压力"）
//! - 双重降级：无匹配时退化为单字分词，词典为空时退化为原始文本
//!
//! 用法:
//! ```
//! use ramaria_memory::keyword::normalizer::BigramWithDictionaryNormalizer;
//! let normalizer = BigramWithDictionaryNormalizer::new(
//!     vec!["职业倦怠".into(), "工作压力".into()],
//! );
//! let tokens = normalizer.normalize("职业倦怠导致工作压力增大");
//! // "职业倦怠"和"工作压力"在词典中保持整体，其余字符被拆为单字
//! assert_eq!(tokens, vec!["职业倦怠", "导", "致", "工作压力", "增", "大"]);
//! ```

use std::collections::HashMap;

// =========================================================
// BigramWithDictionaryNormalizer
// =========================================================

/// 基于词典的复合关键词归一化器（最大正向匹配）。
///
/// 职责:
/// - 识别文本中的复合关键词（如"职业倦怠"、"工作压力"），确保不被拆散
/// - 应用别名映射：将同义表述统一为规范形式
/// - 输出归一化后的有序关键词列表，供 BM25 索引和 TopicBatcher 使用
///
/// 算法说明（最大正向匹配）:
/// 1. 词典按长度降序排列（长词优先匹配）。
/// 2. 从文本起始位置 pos=0 开始：
///    a. 对每个词典词，检查 text[pos..pos+词长] 是否匹配。
///    b. 找到最长匹配 → 输出该词典词，pos += 词长。
///    c. 无匹配 → 输出当前字符（单字），pos += 字符宽度（UTF-8）。
/// 3. 输出经别名映射表解析为规范形式。
///
/// 降级策略:
/// - 词典为空：退化为单字分词（每个中文字符输出为一个 token）
/// - 无匹配片段：输出原始单字，不丢弃信息
///
/// 性能说明:
/// - 词典通常 < 1000 条，每次匹配遍历完整词典 O(n·m) 可接受
/// - 输入文本通常 < 200 字符（关键词列表短）
pub struct BigramWithDictionaryNormalizer {
    /// 复合关键词词典（按长度降序排列，长词优先匹配）
    dictionary: Vec<String>,
    /// 别名映射表（同义别名 → 规范形式）
    alias_map: HashMap<String, String>,
}

impl BigramWithDictionaryNormalizer {
    /// 创建新的归一化器。
    ///
    /// 参数:
    /// - `dictionary`: 复合关键词列表（如 `["职业倦怠", "工作压力", "权威冲突"]`）。
    ///   内部自动按长度降序排序。
    pub fn new(dictionary: Vec<String>) -> Self {
        let mut dict = dictionary;
        // 去重 + 按长度降序（长词优先匹配）
        dict.sort_by_key(|b| std::cmp::Reverse(b.len()));
        dict.dedup();

        Self {
            dictionary: dict,
            alias_map: HashMap::new(),
        }
    }

    /// 创建带别名映射表的归一化器。
    ///
    /// 参数:
    /// - `dictionary`: 复合关键词列表。
    /// - `alias_map`: 别名映射（别名 → 规范形式，如 `"职场焦虑" → "工作压力"`）。
    pub fn with_alias_map(dictionary: Vec<String>, alias_map: HashMap<String, String>) -> Self {
        let mut dict = dictionary;
        dict.sort_by_key(|b| std::cmp::Reverse(b.len()));
        dict.dedup();

        Self {
            dictionary: dict,
            alias_map,
        }
    }

    /// 对输入文本执行归一化分词。
    ///
    /// 参数:
    /// - `text`: 待分词的文本（通常是关键词列表拼接的字符串）。
    ///
    /// 返回:
    /// - 归一化后的关键词列表（已解析别名）。
    ///
    /// 说明:
    /// - 使用最大正向匹配算法，优先匹配词典中最长的复合关键词。
    /// - 输出中的每个 token 都经过别名映射表解析。
    /// - 空文本返回空 Vec。
    pub fn normalize(&self, text: &str) -> Vec<String> {
        if text.is_empty() {
            return Vec::new();
        }

        let mut tokens: Vec<String> = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut pos = 0;

        // 预计算别名键列表（按长度降序排列），用于别名键的复合匹配
        let mut alias_keys: Vec<&String> = self.alias_map.keys().collect();
        alias_keys.sort_by_key(|k| std::cmp::Reverse(k.len()));

        while pos < len {
            let mut matched = false;

            // 最大正向匹配：先尝试词典中的复合关键词
            for dict_word in &self.dictionary {
                let word_chars: Vec<char> = dict_word.chars().collect();
                let word_len = word_chars.len();

                if pos + word_len <= len {
                    let slice: String = chars[pos..pos + word_len].iter().collect();
                    if slice == *dict_word {
                        // 解析别名 → 规范形式（词典词本身也可能有别名映射）
                        let resolved = self
                            .alias_map
                            .get(dict_word.as_str())
                            .cloned()
                            .unwrap_or(dict_word.clone());
                        tokens.push(resolved);
                        pos += word_len;
                        matched = true;
                        break;
                    }
                }
            }

            // 如果词典未匹配，尝试别名键匹配（别名键作为隐式词典词）
            if !matched {
                for alias_key in &alias_keys {
                    let key_chars: Vec<char> = alias_key.chars().collect();
                    let key_len = key_chars.len();

                    if pos + key_len <= len {
                        let slice: String = chars[pos..pos + key_len].iter().collect();
                        if slice == alias_key.as_str() {
                            // 别名键匹配 → 输出规范形式
                            let canonical = self.alias_map.get(alias_key.as_str()).unwrap();
                            tokens.push(canonical.clone());
                            pos += key_len;
                            matched = true;
                            break;
                        }
                    }
                }
            }

            if !matched {
                // 无匹配：输出当前字符作为单字 token，推进 1 个字符
                // 排除空白字符（不产生无意义 token）
                if !chars[pos].is_whitespace() {
                    let single: String = chars[pos].to_string();
                    tokens.push(single);
                }
                pos += 1;
            }
        }

        tokens
    }

    /// 解析别名——返回关键词的规范形式。
    ///
    /// 参数:
    /// - `keyword`: 原始关键词字符串。
    ///
    /// 返回:
    /// - 如果 keyword 在别名映射表中，返回规范形式。
    /// - 否则返回 keyword 本身（不进行 trim/小写——由 `KeywordToken::new()` 保证）。
    ///
    /// 说明:
    /// - 供外部模块（如 BM25 分词器）单独调用别名解析。
    /// - 不会修改非别名关键词。
    pub fn resolve(&self, keyword: &str) -> String {
        self.alias_map
            .get(keyword)
            .cloned()
            .unwrap_or_else(|| keyword.to_string())
    }

    /// 注册别名映射（运行时动态添加）。
    ///
    /// 参数:
    /// - `alias`: 别名（如 "职场焦虑"）。
    /// - `canonical`: 规范形式（如 "工作压力"）。
    ///
    /// 说明:
    /// - 覆盖已有映射。
    /// - 供别名管理模块在运行过程中动态更新映射表。
    pub fn add_alias(&mut self, alias: &str, canonical: &str) {
        self.alias_map
            .insert(alias.to_string(), canonical.to_string());
    }

    /// 批量加载别名映射。
    ///
    /// 参数:
    /// - `mappings`: 别名映射的迭代器（别名, 规范形式）对。
    pub fn load_aliases<I>(&mut self, mappings: I)
    where
        I: IntoIterator<Item = (String, String)>,
    {
        self.alias_map.extend(mappings);
    }

    /// 返回词典条目数。
    pub fn dictionary_size(&self) -> usize {
        self.dictionary.len()
    }

    /// 返回别名映射表条目数。
    pub fn alias_count(&self) -> usize {
        self.alias_map.len()
    }

    /// 返回词典引用（供调试和日志使用）。
    pub fn dictionary(&self) -> &[String] {
        &self.dictionary
    }

    /// 返回别名映射引用（供调试和日志使用）。
    pub fn aliases(&self) -> &HashMap<String, String> {
        &self.alias_map
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── 构造与基本功能 ──

    /// 空词典：退化为单字分词
    #[test]
    fn empty_dictionary_outputs_single_chars() {
        let norm = BigramWithDictionaryNormalizer::new(vec![]);
        let tokens = norm.normalize("工作压力");
        // 无词典时每个字符单独输出（排除重复字符去重问题）
        // "工作压力" → ["工", "作", "压", "力"]
        assert_eq!(tokens.len(), 4);
        assert!(tokens.contains(&"工".to_string()));
        assert!(tokens.contains(&"作".to_string()));
        assert!(tokens.contains(&"压".to_string()));
        assert!(tokens.contains(&"力".to_string()));
    }

    /// 空文本返回空列表
    #[test]
    fn empty_text_returns_empty() {
        let norm = BigramWithDictionaryNormalizer::new(vec!["测试".into()]);
        let tokens = norm.normalize("");
        assert!(tokens.is_empty());
    }

    // ── 复合关键词匹配 ──

    /// 复合关键词"职业倦怠"被整体匹配
    #[test]
    fn compound_keyword_job_burnout() {
        let norm = BigramWithDictionaryNormalizer::new(vec!["职业倦怠".into()]);
        let tokens = norm.normalize("职业倦怠");
        assert_eq!(tokens, vec!["职业倦怠"]);
    }

    /// 复合关键词不被拆分
    #[test]
    fn compound_keyword_not_split() {
        let norm = BigramWithDictionaryNormalizer::new(vec!["工作压力".into(), "职业倦怠".into()]);
        let tokens = norm.normalize("职业倦怠导致工作压力增大");
        // "职业倦怠" → 匹配, "导"→单字, "致"→单字, "工作压力"→匹配, "增"→单字, "大"→单字
        assert_eq!(tokens, vec!["职业倦怠", "导", "致", "工作压力", "增", "大"]);
    }

    /// 长词优先匹配（重叠词典词）
    #[test]
    fn longest_match_priority() {
        let norm = BigramWithDictionaryNormalizer::new(vec![
            "工作压力".into(),
            "工作压力管理".into(), // 更长
        ]);
        let tokens = norm.normalize("工作压力管理");
        // 应匹配"工作压力管理"而非"工作压力"
        assert_eq!(tokens, vec!["工作压力管理"]);
    }

    /// 多词重叠：长词覆盖部分匹配
    #[test]
    fn overlapping_keywords() {
        let norm = BigramWithDictionaryNormalizer::new(vec![
            "人际关系".into(),
            "人际".into(),
            "关系".into(),
        ]);
        let tokens = norm.normalize("人际关系");
        // 长词优先 → "人际关系"
        assert_eq!(tokens, vec!["人际关系"]);
    }

    // ── 别名映射 ──

    /// 别名映射将同义关键词统一为规范形式
    #[test]
    fn alias_resolution() {
        let mut alias_map = HashMap::new();
        alias_map.insert("职场焦虑".into(), "工作压力".into());
        alias_map.insert("职场倦怠".into(), "职业倦怠".into());

        let norm = BigramWithDictionaryNormalizer::with_alias_map(
            vec!["工作压力".into(), "职业倦怠".into()],
            alias_map,
        );
        let tokens = norm.normalize("职场焦虑导致职场倦怠");
        assert_eq!(tokens, vec!["工作压力", "导", "致", "职业倦怠"]);
    }

    /// resolve 方法只查别名，不改非别名
    #[test]
    fn resolve_non_alias_unchanged() {
        let mut alias_map = HashMap::new();
        alias_map.insert("a".into(), "b".into());
        let norm = BigramWithDictionaryNormalizer::with_alias_map(vec![], alias_map);
        assert_eq!(norm.resolve("a"), "b");
        assert_eq!(norm.resolve("c"), "c");
    }

    // ── 动态添加别名 ──

    /// 运行时动态注册别名
    #[test]
    fn dynamic_add_alias() {
        let mut norm = BigramWithDictionaryNormalizer::new(vec!["工作压力".into()]);
        norm.add_alias("职场焦虑", "工作压力");
        let tokens = norm.normalize("职场焦虑");
        assert_eq!(tokens, vec!["工作压力"]);
    }

    /// 批量加载别名
    #[test]
    fn batch_load_aliases() {
        let mut norm = BigramWithDictionaryNormalizer::new(vec![]);
        let mappings = vec![
            ("a1".to_string(), "c1".to_string()),
            ("a2".to_string(), "c2".to_string()),
        ];
        norm.load_aliases(mappings);
        assert_eq!(norm.alias_count(), 2);
    }

    // ── 边界情况 ──

    /// 文本包含空白时跳过空白
    #[test]
    fn whitespace_skipped() {
        let norm = BigramWithDictionaryNormalizer::new(vec![]);
        let tokens = norm.normalize("a b");
        // 空白被跳过，只输出非空白字符
        assert_eq!(tokens, vec!["a", "b"]);
    }

    /// 词典去重
    #[test]
    fn dictionary_dedup() {
        let norm = BigramWithDictionaryNormalizer::new(vec![
            "测试".into(),
            "测试".into(), // 重复
        ]);
        assert_eq!(norm.dictionary_size(), 1);
    }

    /// 部分匹配（词典词未完全包含输入）
    #[test]
    fn partial_match() {
        let norm = BigramWithDictionaryNormalizer::new(vec!["工作压力".into()]);
        let tokens = norm.normalize("工作压力大");
        assert_eq!(tokens, vec!["工作压力", "大"]);
    }

    /// 无匹配时全部输出单字
    #[test]
    fn no_match_all_single_chars() {
        let norm = BigramWithDictionaryNormalizer::new(vec!["xyz".into()]);
        let tokens = norm.normalize("你好世界");
        assert_eq!(tokens, vec!["你", "好", "世", "界"]);
    }

    /// 长文本性能稳定（无死循环）
    #[test]
    fn long_text_no_infinite_loop() {
        let norm = BigramWithDictionaryNormalizer::new(vec!["关键词".into()]);
        let long_text = "测试".repeat(100);
        let tokens = norm.normalize(&long_text);
        // 200 个字符 → 200 个单字（无复合关键词匹配）
        assert_eq!(tokens.len(), 200);
    }
}
