//! crates/ramaria-memory/src/graph_retriever/entities.rs - 查询实体识别
//!
//! 设计特点:
//! - bigram 倒排生成候选实体，避免全量扫描节点名
//! - 匹配分按字符覆盖率计，长实体不因名字长而虚高
//! - 倒排不可用时回退全量扫描，结果一致

use super::*;

impl GraphRetriever {
    /// 从查询文本中提取候选实体。
    ///
    /// 策略:
    /// - 查询长度 ≥ 2 时，先用相邻字符对查 bigram 倒排、查询字符查单字符索引生成候选，
    ///   再对候选做精确子串匹配校验，避免每次查询扫描全部节点名
    /// - 支持中文长实体（如"机器学习项目"能匹配到"机器学习"和"项目"）
    /// - 单字符查询无法用 bigram 生成候选，退回全量扫描
    /// - 返回匹配的实体名列表，按匹配比例降序
    pub fn extract_entities(&self, query: &str) -> Vec<(&str, f64)> {
        if query.is_empty() || self.nodes.is_empty() {
            return Vec::new();
        }

        let q_chars: Vec<char> = query.chars().collect();
        let q_len = q_chars.len();

        // 单字符查询无法用 bigram 生成候选：查询是单个字符时，
        // "查询是实体子串"等价于该字符出现在实体名任意位置，需要全量扫描。
        if q_len < 2 {
            return self.extract_entities_full_scan(&q_chars);
        }

        let candidates = self.candidate_entities(&q_chars);
        let mut matches: Vec<(&str, f64)> = Vec::new();

        for entity_name in candidates {
            let e_chars: Vec<char> = entity_name.chars().collect();
            let e_len = e_chars.len();

            if e_len == 0 {
                continue;
            }

            // 候选只保证部分字符命中索引，仍需按原匹配公式做双向子串校验。
            let match_len = if q_len >= e_len {
                // 查询比实体长：实体是否是查询的子串
                contains_subsequence(&q_chars, &e_chars) as usize * e_len
            } else {
                // 实体比查询长：查询是否是实体的子串
                contains_subsequence(&e_chars, &q_chars) as usize * q_len
            };

            if match_len > 0 {
                let ratio = match_len as f64 / e_len.max(q_len) as f64;
                matches.push((entity_name, ratio));
            }
        }

        // 按匹配比例降序
        matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        matches
    }

    /// 由倒排索引生成候选实体名集合（引用 `nodes` 的 key，天然去重）。
    ///
    /// 两路召回并集:
    /// - bigram 倒排: 查询的每个相邻字符对命中"实体名包含该 bigram"
    /// - 单字符索引: 查询的每个字符命中"单字符实体名"
    fn candidate_entities(&self, q_chars: &[char]) -> HashSet<&str> {
        let mut candidate_names: HashSet<&str> = HashSet::new();

        for pair in q_chars.windows(2) {
            if let Some(names) = self.entity_bigrams.get(&(pair[0], pair[1])) {
                candidate_names.extend(names.iter().map(|n| n.as_str()));
            }
        }
        for c in q_chars {
            if let Some(names) = self.single_char_entities.get(c) {
                candidate_names.extend(names.iter().map(|n| n.as_str()));
            }
        }

        // 索引中的名字统一映射回节点集合的 key 引用，顺带过滤索引与集合的潜在不同步。
        candidate_names
            .into_iter()
            .filter_map(|name| self.nodes.get_key_value(name).map(|(key, _)| key.as_str()))
            .collect()
    }

    /// 全量扫描实体名做子串匹配（无法通过倒排生成候选时的回退路径）。
    ///
    /// 单字符查询无法用 bigram 生成候选：查询是单个字符时，
    /// "查询是实体子串"等价于该字符出现在实体名中，必须检查全部实体名。
    fn extract_entities_full_scan(&self, q_chars: &[char]) -> Vec<(&str, f64)> {
        let q_len = q_chars.len();
        let mut matches: Vec<(&str, f64)> = Vec::new();

        for entity_name in self.nodes.keys() {
            let e_chars: Vec<char> = entity_name.chars().collect();
            let e_len = e_chars.len();

            if e_len == 0 {
                continue;
            }

            let match_len = if q_len >= e_len {
                contains_subsequence(q_chars, &e_chars) as usize * e_len
            } else {
                contains_subsequence(&e_chars, q_chars) as usize * q_len
            };

            if match_len > 0 {
                let ratio = match_len as f64 / e_len.max(q_len) as f64;
                matches.push((entity_name.as_str(), ratio));
            }
        }

        matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        matches
    }
}
