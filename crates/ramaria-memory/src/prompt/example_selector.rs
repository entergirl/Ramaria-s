//! crates/ramaria-memory/src/prompt/example_selector.rs - Few-shot 示例筛选器（风格代表性）
//!
//! 设计特点:
//! - 从 persona_examples 候选库中按风格代表性评分选取最优 3-5 对
//! - 评分维度: 口语度（语气词命中密度）、短度（贴合理想句长，含分条按最长段计）、
//!   分条痕迹（含「||」加分）
//! - 去重保护: 相同 reply 只保留一条，避免重复示例
//! - 排序规则: 综合评分降序（稳定排序），取前 N 条
//! - 无合适示例时返回空列表（不强制凑数）
//!
//! 评分公式:
//! score = colloquial_score * 0.45 + brevity_score * 0.35 + segment_score * 0.20
//!
//! 依赖:
//! - ramaria_core::types::PersonaExample: 对话示例结构体

use crate::keyword::normalizer::{BigramNormalizer, KeywordNormalizer};
use ramaria_core::types::PersonaExample;

// =========================================================
// 筛选配置
// =========================================================

/// Few-shot 示例筛选配置（风格代表性）。
///
/// 字段约定:
/// - `max_examples`: 最大返回示例对数。默认 5。
/// - `min_examples`: 最少返回示例对数（不满足时返回空）。默认 1。
/// - `colloquial_weight`: 口语度权重（语气词命中密度）。默认 0.45。
/// - `brevity_weight`: 短度权重（贴合理想句长）。默认 0.35。
/// - `segment_weight`: 分条痕迹权重（含「||」加分）。默认 0.20。
/// - `ideal_length`: 理想回复长度（字符数；短度为满分点附近）。默认 20。
#[derive(Debug, Clone)]
pub struct ExampleSelectorConfig {
    /// 最大返回示例数
    pub max_examples: usize,
    /// 最少返回示例数（不满足时返回空）
    pub min_examples: usize,
    /// 口语度权重（语气词命中密度）
    pub colloquial_weight: f64,
    /// 短度权重（贴合理想句长）
    pub brevity_weight: f64,
    /// 分条痕迹权重（含「||」加分）
    pub segment_weight: f64,
    /// 理想回复长度（字符数；短度满分点）
    pub ideal_length: usize,
}

/// 口语度满分密度：每 20 字符出现 1 个语气词记为满分。
const COLLOQUIAL_CHARS_PER_HIT: f64 = 20.0;

/// 风格判分使用的语气词集合（与「说话方式」语气词清单一致，含疑问/感叹收尾）。
const INTERJECTIONS: &[char] = &['啊', '呀', '哦', '嗯', '啦', '嘛', '呢', '吧'];

impl Default for ExampleSelectorConfig {
    fn default() -> Self {
        Self {
            max_examples: 5,
            min_examples: 1,
            colloquial_weight: 0.45,
            brevity_weight: 0.35,
            segment_weight: 0.20,
            ideal_length: 20,
        }
    }
}

// =========================================================
// 筛选器
// =========================================================

/// Few-shot 示例筛选器（风格代表性）。
///
/// 职责:
/// - 从候选示例列表中按风格代表性评分筛选。
/// - 返回评分最高的前 N 条。
///
/// 用法:
/// ```
/// use ramaria_memory::prompt::example_selector::{ExampleSelector, ExampleSelectorConfig};
/// // 空候选 → 返回空列表（示例仅示意调用形态，完整示例数据见库内测试）
/// let selected = ExampleSelector::select(&[], &ExampleSelectorConfig::default());
/// assert!(selected.is_empty());
/// ```
pub struct ExampleSelector;

impl ExampleSelector {
    /// 从候选示例中筛选风格代表性最高的 N 条。
    ///
    /// 参数:
    /// - `candidates`: 候选示例列表（通常从 `list_all_examples` 获取）。
    /// - `config`: 筛选配置。
    ///
    /// 返回:
    /// - 评分最高的示例列表（最多 `config.max_examples` 条）。
    /// - 若数量不足 `config.min_examples`，返回空列表。
    pub fn select(
        candidates: &[PersonaExample],
        config: &ExampleSelectorConfig,
    ) -> Vec<PersonaExample> {
        if candidates.is_empty() {
            tracing::debug!("候选示例为空，跳过筛选");
            return Vec::new();
        }

        // 1. 为每条示例计算风格代表性评分
        let mut scored: Vec<(f64, &PersonaExample)> = candidates
            .iter()
            .map(|ex| (Self::score_example(ex, config), ex))
            .collect();

        // 2. 按评分降序稳定排序
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
            "Few-shot 示例筛选完成（风格代表性）"
        );

        selected
    }

    /// 计算单条示例的风格代表性评分。
    ///
    /// 评分 = colloquial（口语度）* colloquial_weight
    ///      + brevity（短度）* brevity_weight
    ///      + segment（分条痕迹）* segment_weight
    fn score_example(example: &PersonaExample, config: &ExampleSelectorConfig) -> f64 {
        let colloquial = Self::compute_colloquial_score(example);
        let brevity = Self::compute_brevity_score(example, config.ideal_length);
        let segment = Self::compute_segment_score(example);

        colloquial * config.colloquial_weight
            + brevity * config.brevity_weight
            + segment * config.segment_weight
    }

    /// 计算口语度分（0.0 ~ 1.0）：语气词命中密度（每 20 字符 1 个记为满分）。
    fn compute_colloquial_score(example: &PersonaExample) -> f64 {
        let len = example.reply.chars().count().max(1);
        let hits = example
            .reply
            .chars()
            .filter(|c| INTERJECTIONS.contains(c))
            .count();
        let expected = len as f64 / COLLOQUIAL_CHARS_PER_HIT;
        (hits as f64 / expected.max(1.0)).clamp(0.0, 1.0)
    }

    /// 计算短度分（0.0 ~ 1.0）：贴近本人句长（含分条时按最长段计）。
    ///
    /// 规则:
    /// - 最长段长度越接近 `ideal_length` 得分越高（线性衰减）。
    fn compute_brevity_score(example: &PersonaExample, ideal_length: usize) -> f64 {
        let longest = example
            .reply
            .split("||")
            .map(|seg| seg.trim().chars().count())
            .max()
            .unwrap_or(0);
        let ideal = ideal_length.max(1);
        let diff = (longest as f64 - ideal as f64).abs();
        (1.0 - diff / ideal as f64).clamp(0.0, 1.0)
    }

    /// 计算分条痕迹分（0.0 / 1.0）：含「||」分隔记满分。
    fn compute_segment_score(example: &PersonaExample) -> f64 {
        if example.reply.contains("||") {
            1.0
        } else {
            0.0
        }
    }
}

// =========================================================
// 便捷函数
// =========================================================

/// 从用户输入中提取简单关键词。
///
/// # 实现（M3 收拢重复解析）
///
/// 分词委托 `keyword::normalizer::BigramNormalizer`（与 `bm25::tokenize` 同一来源）；
/// 本函数仅在消费侧保留差异化后处理:
///
/// - **中文 (CJK)**：字符二元组（bigram），如 "今天天气真好" → ["今天","天天","天气","气真","真好"]；
///   独立 CJK 字符（如标点后的单字）不输出。
/// - **英文/数字**：按字母/数字边界切分，小写化，过滤后按**字符数 ≥ 2** 口径保留
///   （区别于 bm25 的字节数口径——对独立多字节非 CJK 字母如 "é"，本函数丢弃）。
/// - **排序去重**：按字典序去重输出（供示例 tags / 导入关键词等消费侧集合）。
pub fn extract_keywords(input: &str) -> Vec<String> {
    let mut keywords: Vec<String> = BigramNormalizer
        .normalize(input)
        .into_iter()
        // 字符数口径过滤（v1.5 消费侧差异，见函数头注释）
        .filter(|t| t.as_str().chars().count() >= 2)
        .map(|t| t.into_inner())
        .collect();

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

    fn make_example(reply: &str) -> PersonaExample {
        PersonaExample {
            id: 1,
            persona_uid: "char-0001".into(),
            partner: "用户输入".into(),
            reply: reply.into(),
            session_id: None,
            context: None,
            valence: 0.5,
            tags: None,
            selected: true,
            length: reply.chars().count() as i32,
            created_at: 1000,
        }
    }

    #[test]
    fn select_prefers_colloquial_short_reply() {
        let candidates = vec![
            make_example("嗯嗯，我在的呀"),
            make_example("这是一段比较书面而且相对完整的回复内容"),
        ];
        let selected = ExampleSelector::select(&candidates, &ExampleSelectorConfig::default());

        assert_eq!(
            selected[0].reply, "嗯嗯，我在的呀",
            "口语化短回复应优先于书面长回复"
        );
    }

    #[test]
    fn select_prefers_segmented_reply() {
        let candidates = vec![
            make_example("同一句回复内容不带分条"),
            make_example("同一句内容||带分条"),
        ];
        let selected = ExampleSelector::select(&candidates, &ExampleSelectorConfig::default());

        assert_eq!(selected[0].reply, "同一句内容||带分条", "含分条痕迹应加分");
    }

    #[test]
    fn select_respects_max_examples() {
        let candidates: Vec<PersonaExample> = (0..10)
            .map(|i| make_example(&format!("回复内容{i}")))
            .collect();
        let config = ExampleSelectorConfig {
            max_examples: 3,
            ..Default::default()
        };
        let selected = ExampleSelector::select(&candidates, &config);
        assert_eq!(selected.len(), 3);
    }

    #[test]
    fn select_below_min_returns_empty() {
        let candidates = vec![make_example("唯一的回复")];
        let config = ExampleSelectorConfig {
            min_examples: 2,
            ..Default::default()
        };
        let selected = ExampleSelector::select(&candidates, &config);
        assert!(selected.is_empty());
    }

    #[test]
    fn select_deduplicates_by_reply() {
        let candidates = vec![make_example("一样的话"), make_example("一样的话")];
        let selected = ExampleSelector::select(&candidates, &ExampleSelectorConfig::default());
        assert_eq!(selected.len(), 1, "相同 reply 只保留一条");
    }

    #[test]
    fn select_empty_candidates() {
        let selected = ExampleSelector::select(&[], &ExampleSelectorConfig::default());
        assert!(selected.is_empty());
    }

    #[test]
    fn colloquial_score_scales_with_interjection_density() {
        let rich = make_example("嗯嗯，我在的呀");
        let plain = make_example("我在的，稍等一下");
        assert!(
            ExampleSelector::compute_colloquial_score(&rich)
                > ExampleSelector::compute_colloquial_score(&plain),
            "语气词密度高的回复口语度应更高"
        );
        assert_eq!(
            ExampleSelector::compute_colloquial_score(&plain),
            0.0,
            "无语气词应为零分"
        );
    }

    #[test]
    fn brevity_score_uses_longest_segment() {
        // 最长段 4 字符（「先说这个」）：按最长段与理想长度 20 计分
        let ex = make_example("先说这个||好");
        let score = ExampleSelector::compute_brevity_score(&ex, 20);
        assert!((score - (1.0 - 16.0 / 20.0)).abs() < 1e-9, "得分 {score}");
    }

    #[test]
    fn segment_score_detects_pipe_separator() {
        assert_eq!(
            ExampleSelector::compute_segment_score(&make_example("带||分条")),
            1.0
        );
        assert_eq!(
            ExampleSelector::compute_segment_score(&make_example("不带分条")),
            0.0
        );
    }

    /// 默认理想长度对齐 persona 真实句长（防回退到长模板口径）。
    #[test]
    fn default_ideal_length_targets_short_replies() {
        assert_eq!(
            ExampleSelectorConfig::default().ideal_length,
            20,
            "默认理想长度应为 20 字符"
        );
    }

    /// 三维权重默认值与评分公式锁定（防静默漂移）。
    #[test]
    fn default_weights_lock_style_scoring() {
        let c = ExampleSelectorConfig::default();
        assert!((c.colloquial_weight - 0.45).abs() < 1e-9);
        assert!((c.brevity_weight - 0.35).abs() < 1e-9);
        assert!((c.segment_weight - 0.20).abs() < 1e-9);
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
}
