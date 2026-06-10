//! rust/crates/ramaria-memory/src/prompt/example_selector.rs - Few-shot 示例筛选器
//!
//! 设计特点:
//! - 从 persona_examples 候选库中按多维度评分选取最优 3-5 对
//! - 评分维度: 话题匹配（tag overlap）、情绪匹配（valence proximity）、长度适配
//! - 去重保护: 相同 reply 只保留一条，避免重复示例
//! - 排序规则: 综合评分降序，取前 N 条
//! - 无合适示例时返回空列表（不强制凑数）
//!
//! 评分公式:
//!   score = tag_bonus * 0.5 + valence_match * 0.3 + length_score * 0.2
//!
//! 依赖:
//! - ramaria_core::types::PersonaExample: 对话示例结构体

use ramaria_core::types::PersonaExample;

// =========================================================
// 筛选配置
// =========================================================

/// Few-shot 示例筛选配置。
///
/// 字段约定:
/// - `max_examples`: 最大返回示例对数。默认 5。
/// - `min_examples`: 最少返回示例对数（不满足时返回空）。默认 1。
/// - `tag_weight`: 话题匹配权重。默认 0.5。
/// - `valence_weight`: 情绪匹配权重。默认 0.3。
/// - `length_weight`: 长度适配权重。默认 0.2。
/// - `ideal_length`: 理想回复长度（字符数）。默认 60。
#[derive(Debug, Clone)]
pub struct ExampleSelectorConfig {
    /// 最大返回示例数
    pub max_examples: usize,
    /// 最少返回示例数（不满足时返回空）
    pub min_examples: usize,
    /// 话题匹配权重
    pub tag_weight: f64,
    /// 情绪匹配权重
    pub valence_weight: f64,
    /// 长度适配权重
    pub length_weight: f64,
    /// 理想回复长度
    pub ideal_length: usize,
}

impl Default for ExampleSelectorConfig {
    fn default() -> Self {
        Self {
            max_examples: 5,
            min_examples: 1,
            tag_weight: 0.5,
            valence_weight: 0.3,
            length_weight: 0.2,
            ideal_length: 60,
        }
    }
}

// =========================================================
// 筛选器
// =========================================================

/// Few-shot 示例筛选器。
///
/// 职责:
/// - 从候选示例列表中按多维度评分筛选。
/// - 返回评分最高的前 N 条。
///
/// 用法:
/// ```ignore
/// let selected = ExampleSelector::select(&examples, &query_keywords, query_valence, &config);
/// ```
pub struct ExampleSelector;

impl ExampleSelector {
    /// 从候选示例中筛选最优 N 条。
    ///
    /// 参数:
    /// - `candidates`: 候选示例列表（通常从 `list_selected_examples` 获取）。
    /// - `query_keywords`: 用户输入中提取的关键词（用于话题匹配）。
    /// - `query_valence`: 用户输入的情绪效价 [-1.0, 1.0]。
    /// - `config`: 筛选配置。
    ///
    /// 返回:
    /// - 评分最高的示例列表（最多 `config.max_examples` 条）。
    /// - 若数量不足 `config.min_examples`，返回空列表。
    pub fn select(
        candidates: &[PersonaExample],
        query_keywords: &[&str],
        query_valence: f64,
        config: &ExampleSelectorConfig,
    ) -> Vec<PersonaExample> {
        if candidates.is_empty() {
            tracing::debug!("候选示例为空，跳过筛选");
            return Vec::new();
        }

        // 1. 为每条示例计算综合评分
        let mut scored: Vec<(f64, &PersonaExample)> = candidates
            .iter()
            .map(|ex| {
                let score = Self::score_example(ex, query_keywords, query_valence, config);
                (score, ex)
            })
            .collect();

        // 2. 按评分降序排序
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // 3. 去重（相同 reply 只保留评分最高的那条）
        let mut seen_replies = std::collections::HashSet::new();
        let mut selected: Vec<PersonaExample> = Vec::new();

        for (_score, ex) in scored {
            if seen_replies.insert(&ex.reply) {
                selected.push(ex.clone());
                if selected.len() >= config.max_examples {
                    break;
                }
            }
        }

        // 4. 数量不足 min_examples 时返回空
        if selected.len() < config.min_examples {
            tracing::debug!(
                selected = selected.len(),
                min = config.min_examples,
                "示例数量不足最低要求，返回空"
            );
            return Vec::new();
        }

        tracing::debug!(
            candidates = candidates.len(),
            selected = selected.len(),
            "Few-shot 示例筛选完成"
        );

        selected
    }

    /// 计算单条示例的综合评分。
    ///
    /// 评分 = tag_bonus * tag_weight + valence_match * valence_weight + length_score * length_weight
    fn score_example(
        example: &PersonaExample,
        query_keywords: &[&str],
        query_valence: f64,
        config: &ExampleSelectorConfig,
    ) -> f64 {
        let tag_score = Self::compute_tag_score(example, query_keywords);
        let valence_score = Self::compute_valence_score(example, query_valence);
        let length_score = Self::compute_length_score(example, config.ideal_length);

        tag_score * config.tag_weight
            + valence_score * config.valence_weight
            + length_score * config.length_weight
    }

    /// 计算话题匹配分（0.0 ~ 1.0）。
    ///
    /// 规则:
    /// - 提取示例 tags 中逗号分隔的关键词
    /// - 与 query_keywords 做交集
    /// - 匹配数 / 总查询关键词数 = tag_score
    /// - 无查询关键词时返回 0.5（中性分）
    fn compute_tag_score(example: &PersonaExample, query_keywords: &[&str]) -> f64 {
        if query_keywords.is_empty() {
            return 0.5;
        }

        let example_tags: Vec<&str> = example
            .tags
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        if example_tags.is_empty() {
            return 0.0;
        }

        // 大小写不敏感匹配
        let matches = query_keywords
            .iter()
            .filter(|kw| {
                let kw_lower = kw.to_lowercase();
                example_tags.iter().any(|tag| {
                    let tag_lower = tag.to_lowercase();
                    tag_lower.contains(&kw_lower) || kw_lower.contains(&tag_lower)
                })
            })
            .count();

        matches as f64 / query_keywords.len() as f64
    }

    /// 计算情绪匹配分（0.0 ~ 1.0）。
    ///
    /// 规则:
    /// - 1.0 - |example.valence - query_valence| / 2.0
    /// - 越接近 query_valence 得分越高
    fn compute_valence_score(example: &PersonaExample, query_valence: f64) -> f64 {
        let diff = (example.valence - query_valence).abs();
        (1.0 - diff / 2.0).clamp(0.0, 1.0)
    }

    /// 计算长度适配分（0.0 ~ 1.0）。
    ///
    /// 规则:
    /// - 理想长度附近得分最高（高斯型）
    /// - 1.0 - |example.length - ideal_length| / ideal_length
    fn compute_length_score(example: &PersonaExample, ideal_length: usize) -> f64 {
        let len = example.length as usize;
        let ideal = ideal_length.max(1);
        let diff = (len as f64 - ideal as f64).abs();
        (1.0 - diff / ideal as f64).clamp(0.0, 1.0)
    }
}

// =========================================================
// 便捷函数
// =========================================================

/// 从用户输入中提取简单关键词。
///
/// 策略:
/// - **中文 (CJK)**：使用字符二元组（bigram）分词，与 BM25 分词策略一致。
///   例如 "今天天气真好" → ["今天","天天","天气","气真","真好"]。
///   独立 CJK 字符（如标点后的单字）**不输出**，与 BM25 tokenize 行为一致。
/// - **英文/数字**：按 Unicode 字母/数字边界切分，小写化，过滤长度 < 2 的 token。
/// - **标点/空白**：丢弃。
///
/// 返回:
/// - 去重后的小写关键词列表，按字典序排列。
///
/// 说明:
/// - Phase 3 接入真实分词器后可替换为 jieba-rs 等实现。
pub fn extract_keywords(input: &str) -> Vec<String> {
    let mut keywords: Vec<String> = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut alpha_buf = String::with_capacity(32);

    let flush_alpha = |buf: &mut String, out: &mut Vec<String>| {
        let trimmed = buf.trim().to_lowercase();
        if trimmed.chars().count() >= 2 {
            out.push(trimmed);
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
            flush_alpha(&mut alpha_buf, &mut keywords);
            // 生成 CJK bigram（仅当后续字符也是 CJK 时）
            // 独立 CJK 字符不输出，与 BM25 tokenize 行为一致
            if i + 1 < chars.len() && is_cjk(chars[i + 1]) {
                let bigram: String = [c, chars[i + 1]].iter().collect();
                keywords.push(bigram);
            }
            i += 1;
        } else if is_alpha(c) {
            alpha_buf.push(c);
            i += 1;
        } else {
            flush_alpha(&mut alpha_buf, &mut keywords);
            i += 1;
        }
    }
    flush_alpha(&mut alpha_buf, &mut keywords);

    keywords.sort();
    keywords.dedup();
    keywords
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_example(reply: &str, tags: Option<&str>, valence: f64, length: i32) -> PersonaExample {
        PersonaExample {
            id: 1,
            persona_uid: "char-0001".into(),
            partner: "用户输入".into(),
            reply: reply.into(),
            session_id: None,
            context: None,
            valence,
            tags: tags.map(|s| s.into()),
            selected: true,
            length,
            created_at: 1000,
        }
    }

    #[test]
    fn select_returns_top_by_score() {
        let candidates = vec![
            make_example("reply1", Some("编程,Python"), 0.5, 60),
            make_example("reply2", Some("游戏,娱乐"), 0.3, 100),
            make_example("reply3", Some("编程,Rust"), 0.6, 50),
        ];
        let config = ExampleSelectorConfig::default();
        let keywords = extract_keywords("编程 Rust");

        let selected = ExampleSelector::select(&candidates, &str_to_refs(&keywords), 0.5, &config);

        // reply3 应有更高分（tags 匹配 "编程"+"Rust"，valence 更近）
        assert!(!selected.is_empty());
        // 第一条应为 reply3（最佳匹配）
        assert!(selected[0].reply.contains("reply3"));
    }

    #[test]
    fn select_respects_max_examples() {
        let candidates: Vec<PersonaExample> = (0..10)
            .map(|i| make_example(&format!("reply{i}"), Some("编程"), 0.5, 60))
            .collect();
        let config = ExampleSelectorConfig {
            max_examples: 3,
            ..Default::default()
        };
        let keywords = extract_keywords("编程");

        let selected = ExampleSelector::select(&candidates, &str_to_refs(&keywords), 0.5, &config);
        assert_eq!(selected.len(), 3);
    }

    #[test]
    fn select_below_min_returns_empty() {
        let candidates = vec![make_example("only_one", Some("罕见话题"), 0.5, 60)];
        let config = ExampleSelectorConfig {
            min_examples: 2,
            ..Default::default()
        };
        let keywords = extract_keywords("罕见");

        let selected = ExampleSelector::select(&candidates, &str_to_refs(&keywords), 0.5, &config);
        assert!(selected.is_empty());
    }

    #[test]
    fn select_deduplicates_by_reply() {
        let candidates = vec![
            make_example("same_reply", Some("标签A"), 0.9, 50),
            make_example("same_reply", Some("标签B"), 0.5, 100),
        ];
        let config = ExampleSelectorConfig::default();
        let keywords = extract_keywords("标签");

        let selected = ExampleSelector::select(&candidates, &str_to_refs(&keywords), 0.5, &config);
        assert_eq!(selected.len(), 1);
        // 应保留评分更高的那条（valence=0.9 的）
        assert!((selected[0].valence - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn select_empty_candidates() {
        let config = ExampleSelectorConfig::default();
        let keywords = extract_keywords("测试");
        let selected = ExampleSelector::select(&[], &str_to_refs(&keywords), 0.0, &config);
        assert!(selected.is_empty());
    }

    #[test]
    fn tag_score_exact_match() {
        let ex = make_example("reply", Some("编程,Python"), 0.5, 60);
        let keywords = extract_keywords("编程 Python");

        let score = ExampleSelector::compute_tag_score(&ex, &str_to_refs(&keywords));
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn tag_score_no_match() {
        let ex = make_example("reply", Some("游戏"), 0.5, 60);
        let keywords = extract_keywords("编程");

        let score = ExampleSelector::compute_tag_score(&ex, &str_to_refs(&keywords));
        assert!((score - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn valence_score_perfect_match() {
        let ex = make_example("reply", None, 0.5, 60);
        let score = ExampleSelector::compute_valence_score(&ex, 0.5);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn valence_score_opposite() {
        let ex = make_example("reply", None, -1.0, 60);
        let score = ExampleSelector::compute_valence_score(&ex, 1.0);
        assert!((score - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn length_score_ideal() {
        let ex = make_example("reply", None, 0.0, 60);
        let score = ExampleSelector::compute_length_score(&ex, 60);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn extract_keywords_basic() {
        let keywords = extract_keywords("你好，今天天气怎么样？");
        // CJK bigram 分词：应包含 "你好"、"今天"、"天气" 等二元组
        assert!(keywords.iter().any(|k| k.contains("天气")));
        assert!(keywords.iter().any(|k| k == "你好"), "应包含 bigram 你好");
        assert!(keywords.iter().any(|k| k == "今天"), "应包含 bigram 今天");
        // 独立 CJK 字符不输出（如标点后的 "好"、"样" 不应出现）
        assert!(
            !keywords.contains(&"好".to_string()),
            "独立 CJK 字符不应输出"
        );
    }

    #[test]
    fn extract_keywords_cjk_bigram() {
        // 纯中文：应生成 bigram
        let keywords = extract_keywords("机器学习");
        assert!(keywords.contains(&"机器".to_string()));
        assert!(keywords.contains(&"器学".to_string()));
        assert!(keywords.contains(&"学习".to_string()));
        assert_eq!(keywords.len(), 3);
    }

    #[test]
    fn extract_keywords_mixed_cjk_english() {
        // 中英混合：CJK 用 bigram，英文按单词切分
        let keywords = extract_keywords("学习Rust编程");
        assert!(keywords.contains(&"学习".to_string()));
        assert!(keywords.contains(&"rust".to_string()));
        assert!(
            keywords.contains(&"编程".to_string()) || keywords.iter().any(|k| k.contains("编程"))
        );
    }

    // 辅助: Vec<String> → Vec<&str>
    fn str_to_refs<'a>(strings: &'a [String]) -> Vec<&'a str> {
        strings.iter().map(|s| s.as_str()).collect()
    }
}
