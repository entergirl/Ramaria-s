//! crates/ramaria-memory/src/style/rule_gen.rs - 自动风格规则文本生成模块
//!
//! 设计特点:
//! - 显著性分析：对五维统计与全局基线池做二项 z 检验，输出显著项（StyleSignificant）
//! - 模板拼接优先（确定性可测、零 LLM 降级）；LLM 离线翻译作为增强（`auto_translate`）
//! - 数据不足（n_p < 阈值）→ 返回 None，不生成规则文本（静默跳过，回归红线）
//! - LLM 不可用/失败 → warn 日志并回退模板（静默降级链）
//! - 安全约束：prompt 只含统计参数，不含原文消息文本；输出为风格描述文本
//! - 话题词汇偏好不参与显著性检验（"常聊什么"为偏好信息，供知识层引用）

use ramaria_core::config::StyleConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider as LlmProviderTrait};

use super::baseline::{BaselinePool, MetricKey, is_significant, z_test};
use super::stat::StyleStats;

// =========================================================
// 显著项结构
// =========================================================

/// 一条显著口癖词（通过 |z|、频次、样本量、相对超频比四条件）。
#[derive(Debug, Clone, PartialEq)]
pub struct CatchphraseHit {
    /// 口癖词
    pub word: String,
    /// 出现频次
    pub count: u32,
    /// z 值
    pub z: f64,
    /// 相对超频比（persona 频率 / 全局频率）
    pub boost: f64,
}

/// 显著性分析结果（数据足够时生成；数据不足返回 None）。
#[derive(Debug, Clone, PartialEq)]
pub struct StyleSignificant {
    /// 显著口癖词（相对超频比 > 2 且 z 显著）
    pub catchphrases: Vec<CatchphraseHit>,
    /// 句长显著偏短（f_p < f_g）
    pub short_sentences: bool,
    /// 句长显著偏长（f_p > f_g）
    pub long_sentences: bool,
    /// `||` 断句符显著高频
    pub slash_high: bool,
    /// 逗号显著高频
    pub comma_high: bool,
    /// 换行显著高频
    pub newline_high: bool,
    /// 感叹号显著高频
    pub exclaim_high: bool,
    /// 问号显著高频
    pub question_high: bool,
    /// 省略号显著高频
    pub ellipsis_high: bool,
    /// 括号显著高频
    pub paren_high: bool,
    /// 波浪号显著高频
    pub tilde_high: bool,
    /// 情感极性显著偏积极
    pub sentiment_positive: bool,
    /// 情感极性显著偏消极
    pub sentiment_negative: bool,
    /// 感叹词显著高频
    pub interjection_high: bool,
    /// 情感词典命中率显著偏高
    pub sentiment_word_high: bool,
    /// 话题词汇偏好 Top-N（无需显著性，为偏好信息）
    pub topics: Vec<String>,
}

// =========================================================
// 显著性分析
// =========================================================

/// 分析五维统计的显著性。
///
/// 返回:
/// - `Some(sig)`: 数据足够（n_p ≥ 阈值），含显著项与话题偏好。
/// - `None`: 数据不足（n_p < 阈值），不生成规则文本（静默跳过）。
///
/// 说明:
/// - 基线池为空（冷启动）时不进行显著性检验，仅输出话题偏好
///   （无 f_g 无法判定差异，静默跳过其他维度）。
pub fn analyze_significance(
    stats: &StyleStats,
    pool: &BaselinePool,
    config: &StyleConfig,
) -> Option<StyleSignificant> {
    if !stats.has_enough_sample(config.min_sample_count) {
        return None;
    }

    let mut sig = StyleSignificant {
        catchphrases: Vec::new(),
        short_sentences: false,
        long_sentences: false,
        slash_high: false,
        comma_high: false,
        newline_high: false,
        exclaim_high: false,
        question_high: false,
        ellipsis_high: false,
        paren_high: false,
        tilde_high: false,
        sentiment_positive: false,
        sentiment_negative: false,
        interjection_high: false,
        sentiment_word_high: false,
        topics: stats.topic_freq.iter().map(|(w, _)| w.clone()).collect(),
    };

    if pool.is_empty() {
        // 冷启动回退：无全局基线，仅话题偏好可用
        return Some(sig);
    }

    // ---- 口癖词（相对超频比 > 2 另加判定） ----
    for (word, count) in &stats.word_freq {
        let f_p = stats.freq(*count);
        let f_g = pool.global_word_freq(word) / 100.0; // 概率口径
        let z = z_test(f_p, f_g, stats.sample_count);
        let boost = BaselinePool::relative_boost(f_p, f_g);
        if is_significant(z, *count, stats.sample_count, config)
            && boost > config.relative_boost_ratio
        {
            sig.catchphrases.push(CatchphraseHit {
                word: word.clone(),
                count: *count,
                z,
                boost,
            });
        }
    }

    // ---- 句式句长（均值型，z 方向判定） ----
    match significant_direction(
        stats.sentence_len_mean,
        pool.global_sentence_len_mean(),
        stats.sample_count,
        config,
    ) {
        Some(Direction::High) => sig.long_sentences = true,
        Some(Direction::Low) => sig.short_sentences = true,
        None => {}
    }

    // ---- 断句符/标点/感叹词（计数型，仅偏高方向生成规则） ----
    sig.slash_high =
        count_metric_significant(stats.slash_count, stats, pool, MetricKey::Slash, config);
    sig.comma_high =
        count_metric_significant(stats.comma_count, stats, pool, MetricKey::Comma, config);
    sig.newline_high =
        count_metric_significant(stats.newline_count, stats, pool, MetricKey::Newline, config);
    sig.exclaim_high =
        count_metric_significant(stats.exclaim_count, stats, pool, MetricKey::Exclaim, config);
    sig.question_high = count_metric_significant(
        stats.question_count,
        stats,
        pool,
        MetricKey::Question,
        config,
    );
    sig.ellipsis_high = count_metric_significant(
        stats.ellipsis_count,
        stats,
        pool,
        MetricKey::Ellipsis,
        config,
    );
    sig.paren_high =
        count_metric_significant(stats.paren_count, stats, pool, MetricKey::Paren, config);
    sig.tilde_high =
        count_metric_significant(stats.tilde_count, stats, pool, MetricKey::Tilde, config);
    sig.interjection_high = count_metric_significant(
        stats.interjection_count,
        stats,
        pool,
        MetricKey::Interjection,
        config,
    );

    // 情感词典命中率（消息比例，比率型）
    sig.sentiment_word_high =
        rate_metric_significant(stats, pool, MetricKey::SentimentWordRate, config);

    // 情感极性均值（偏积极/偏消极，均值型）
    let f_g_mean = pool.global_metric(MetricKey::SentimentMean);
    match significant_direction(stats.sentiment_mean, f_g_mean, stats.sample_count, config) {
        Some(Direction::High) => sig.sentiment_positive = true,
        Some(Direction::Low) => sig.sentiment_negative = true,
        None => {}
    }

    Some(sig)
}

/// 方向枚举（高于/低于基线）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    High,
    Low,
}

/// 连续型指标的显著性方向判定（句长/情感均值等均值型指标）。
fn significant_direction(f_p: f64, f_g: f64, n_p: u32, config: &StyleConfig) -> Option<Direction> {
    let z = z_test(f_p, f_g, n_p);
    // 均值型指标无"频次"概念：用 1 作为占位计数，仅依赖 |z| 与 n_p 判定
    if !is_significant(z, 1, n_p, config) {
        return None;
    }
    if z > 0.0 {
        Some(Direction::High)
    } else {
        Some(Direction::Low)
    }
}

/// 计数型指标（每 100 字口径）的显著性判定（仅偏高方向生成规则）。
fn count_metric_significant(
    count: u32,
    stats: &StyleStats,
    pool: &BaselinePool,
    key: MetricKey,
    config: &StyleConfig,
) -> bool {
    let f_p = stats.freq(count);
    let f_g = pool.global_metric(key) / 100.0; // 概率口径
    let z = z_test(f_p, f_g, stats.sample_count);
    is_significant(z, count, stats.sample_count, config) && z > 0.0
}

/// 比率型指标（消息比例 0..1）的显著性判定（仅偏高方向生成规则）。
fn rate_metric_significant(
    stats: &StyleStats,
    pool: &BaselinePool,
    key: MetricKey,
    config: &StyleConfig,
) -> bool {
    let rate = if stats.sample_count > 0 {
        stats.sentiment_word_messages as f64 / stats.sample_count as f64
    } else {
        0.0
    };
    let f_g = pool.global_metric(key); // 已是比例口径
    let z = z_test(rate, f_g, stats.sample_count);
    is_significant(z, stats.sentiment_word_messages, stats.sample_count, config) && z > 0.0
}

// =========================================================
// 模板规则生成（确定性，零 LLM 依赖）
// =========================================================

/// 模板拼接生成自动风格规则文本（D-V17-002 模板优先）。
///
/// 输出为 `## 自动风格规则` 小节的正文（多行风格描述）。
pub fn render_template_rule(stats: &StyleStats, sig: &StyleSignificant) -> String {
    let mut lines: Vec<String> = Vec::new();

    if !sig.catchphrases.is_empty() {
        let words: Vec<String> = sig
            .catchphrases
            .iter()
            .take(5)
            .map(|h| format!("「{}」", h.word))
            .collect();
        lines.push(format!(
            "你习惯使用口癖词{}，说话时自然带出这些词。",
            words.join("、")
        ));
    }

    if sig.short_sentences {
        lines.push(format!(
            "你的句子偏短（平均 {} 字），断句明快、节奏紧凑。",
            fmt1(stats.sentence_len_mean)
        ));
    } else if sig.long_sentences {
        lines.push(format!(
            "你的句子偏长（平均 {} 字），表达绵长、娓娓道来。",
            fmt1(stats.sentence_len_mean)
        ));
    }

    if sig.slash_high {
        lines.push(format!(
            "你习惯用「||」断句（每 100 字约 {} 次），把长回复拆成多条短句。",
            fmt1(stats.per_100(stats.slash_count))
        ));
    }
    if sig.comma_high {
        lines.push(format!(
            "你常使用逗号串联多个分句（每 100 字约 {} 次），语气连贯。",
            fmt1(stats.per_100(stats.comma_count))
        ));
    }
    if sig.newline_high {
        lines.push("你习惯用换行分段表达，层次清晰。".to_string());
    }

    if sig.exclaim_high {
        lines.push(format!(
            "你每 100 字约使用 {} 个感叹号，情绪外放、表达热烈。",
            fmt1(stats.per_100(stats.exclaim_count))
        ));
    }
    if sig.question_high {
        lines.push(format!(
            "你频繁使用问句（每 100 字约 {} 个问号），喜欢用提问互动。",
            fmt1(stats.per_100(stats.question_count))
        ));
    }
    if sig.ellipsis_high {
        lines.push("你常用省略号，语气留白、带思考感。".to_string());
    }
    if sig.paren_high {
        lines.push("你常用括号补充说明，表达细致。".to_string());
    }
    if sig.tilde_high {
        lines.push("你常用波浪号（~），语气轻快、亲和。".to_string());
    }

    if sig.sentiment_positive {
        lines.push(format!(
            "你偏向积极表达（情感强度均值 {:+}），常给人温暖鼓励的感觉。",
            fmt2(stats.sentiment_mean)
        ));
    } else if sig.sentiment_negative {
        lines.push(format!(
            "你偏向消极表达（情感强度均值 {:+}），常流露疲惫或无奈。",
            fmt2(stats.sentiment_mean)
        ));
    }
    if sig.interjection_high {
        lines.push("你常带感叹词（如「哇」「哎」），口语感强。".to_string());
    }
    if sig.sentiment_word_high {
        lines.push("你较多使用情感词汇，情绪表达直接、不遮掩。".to_string());
    }

    if !sig.topics.is_empty() {
        let quoted: Vec<String> = sig
            .topics
            .iter()
            .take(5)
            .map(|t| format!("「{t}」"))
            .collect();
        lines.push(format!("你常聊{}等话题。", quoted.join("、")));
    }

    if lines.is_empty() {
        // 无显著项：不生成规则文本（静默跳过）
        return String::new();
    }
    lines.join("\n")
}

// =========================================================
// LLM 翻译增强（可选，`[style].auto_translate`）
// =========================================================

/// LLM 翻译增强的最大输出 token。
const LLM_MAX_TOKENS: u32 = 512;

/// 生成自动风格规则文本（模板优先 + LLM 翻译增强）。
///
/// 参数:
/// - `stats`: 五维统计。
/// - `sig`: 显著项（由 [`analyze_significance`] 产出）。
/// - `llm`: 可用的 LLM provider（None = 仅模板）。
/// - `auto_translate`: `[style].auto_translate` 开关（false = 仅模板）。
/// - `temperature`: LLM 生成温度（评估约定 0.3）。
///
/// 返回:
/// - 规则文本；数据不足/无显著项时为空字符串。
///
/// 降级:
/// - LLM 不可用或调用失败 → warn 日志并回退模板（静默降级链，不阻塞封存）。
pub async fn generate_style_rule(
    stats: &StyleStats,
    sig: &StyleSignificant,
    llm: Option<&dyn LlmProviderTrait>,
    auto_translate: bool,
    temperature: f64,
) -> RamariaResult<String> {
    let template = render_template_rule(stats, sig);
    if template.trim().is_empty() {
        return Ok(String::new());
    }
    if !auto_translate {
        return Ok(template);
    }
    let Some(llm) = llm else {
        return Ok(template);
    };

    let prompt = build_translate_prompt(stats, sig, &template);
    let request = ChatRequest {
        system_prompt: String::new(),
        memory_context: None,
        history: Vec::new(),
        user_message: prompt,
        temperature,
        max_tokens: LLM_MAX_TOKENS,
        request_id: uuid::Uuid::new_v4(),
        template_version: crate::prompt::PROMPT_TEMPLATE_VERSION.to_string(),
    };
    match llm.chat(&request).await {
        Ok(text) => {
            let cleaned = clean_llm_rule(&text);
            if cleaned.trim().is_empty() {
                // LLM 返回空 → 回退模板（不产生空规则）
                Ok(template)
            } else {
                Ok(cleaned)
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "风格规则 LLM 翻译增强失败，回退模板");
            Ok(template)
        }
    }
}

/// 构造 LLM 翻译增强 prompt（只含统计参数与模板，不含原文）。
fn build_translate_prompt(stats: &StyleStats, sig: &StyleSignificant, template: &str) -> String {
    format!(
        "请根据以下说话风格统计参数，把模板规则改写成自然、具体的一段中文风格描述（3~6 句）。\n\
         要求：\n\
         1. 保持统计事实不变（口癖词、句长、标点频率、情感倾向、话题）。\n\
         2. 语气像人格设定说明，不要出现'根据统计''数据显示'等表述。\n\
         3. 不要提及任何具体对话原文。\n\n\
         统计参数：\n\
         - 口癖词：{}\n\
         - 平均句长：{} 字\n\
         - 感叹号：每 100 字 {} 个\n\
         - 问号：每 100 字 {} 个\n\
         - 情感均值：{:+}\n\
         - 常聊话题：{}\n\n\
         模板规则：\n{}\n\n\
         请只输出改写后的规则文本。",
        sig.catchphrases
            .iter()
            .map(|h| h.word.as_str())
            .collect::<Vec<_>>()
            .join("、"),
        fmt1(stats.sentence_len_mean),
        fmt1(stats.per_100(stats.exclaim_count)),
        fmt1(stats.per_100(stats.question_count)),
        fmt2(stats.sentiment_mean),
        sig.topics.join("、"),
        template,
    )
}

/// 清理 LLM 输出：去除可能的包裹引号与多余空白。
fn clean_llm_rule(text: &str) -> String {
    let trimmed = text.trim().trim_matches('"');
    trimmed
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
}

// =========================================================
// 格式化辅助
// =========================================================

fn fmt1(v: f64) -> String {
    format!("{v:.1}")
}

fn fmt2(v: f64) -> String {
    format!("{v:.2}")
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> StyleConfig {
        StyleConfig::default()
    }

    fn stats_with(count: u32, total: u32) -> StyleStats {
        StyleStats {
            sample_count: 200,
            total_chars: total,
            word_freq: vec![("哇塞".to_string(), count)],
            topic_freq: vec![("电影".to_string(), count)],
            sentence_len_mean: 6.0,
            slash_count: count,
            comma_count: count,
            newline_count: 10,
            exclaim_count: count,
            question_count: count,
            ellipsis_count: 5,
            paren_count: 2,
            tilde_count: 1,
            sentiment_mean: 0.3,
            sentiment_std: 0.2,
            sentiment_n: 200,
            interjection_count: count,
            sentiment_word_messages: 100,
            ..Default::default()
        }
    }

    fn baseline_pool_with(per100: f64) -> BaselinePool {
        let mut pool = BaselinePool::new();
        // 构造一个"通用" persona：目标频率远高于全局 → 相对超频
        let generic = StyleStats {
            sample_count: 200,
            total_chars: 20000,
            word_freq: vec![("哇塞".to_string(), (per100 * 200.0) as u32)],
            topic_freq: vec![("电影".to_string(), (per100 * 200.0) as u32)],
            sentence_len_mean: 10.0,
            slash_count: (per100 * 200.0) as u32,
            comma_count: (per100 * 200.0) as u32,
            newline_count: 10,
            exclaim_count: (per100 * 200.0) as u32,
            question_count: (per100 * 200.0) as u32,
            ellipsis_count: 5,
            paren_count: 2,
            tilde_count: 1,
            sentiment_mean: 0.0,
            sentiment_std: 0.2,
            sentiment_n: 200,
            interjection_count: (per100 * 200.0) as u32,
            sentiment_word_messages: 50,
            ..Default::default()
        };
        pool.update_persona("generic", &generic);
        pool
    }

    #[test]
    fn insufficient_sample_returns_none() {
        let stats = StyleStats {
            sample_count: 199,
            total_chars: 2000,
            ..Default::default()
        };
        let pool = BaselinePool::new();
        assert!(
            analyze_significance(&stats, &pool, &config()).is_none(),
            "数据不足 → 不生成规则（回归红线 1）"
        );
    }

    #[test]
    fn cold_start_pool_returns_topics_only() {
        let stats = stats_with(100, 2000);
        let pool = BaselinePool::new();
        let sig = analyze_significance(&stats, &pool, &config()).expect("数据足够返回 Some");
        assert!(sig.catchphrases.is_empty(), "冷启动无基线不判口癖词");
        assert!(!sig.topics.is_empty(), "话题偏好总是可用");
        assert!(!sig.exclaim_high, "冷启动不判显著性");
    }

    #[test]
    fn catchphrase_detected_when_overboosted() {
        // 目标 persona 每 100 字 5 次；全局 0.5 次 → 超频比 10 > 2 → 显著口癖
        let stats = stats_with(100, 2000);
        let pool = baseline_pool_with(0.5);
        let sig = analyze_significance(&stats, &pool, &config()).expect("数据足够");
        assert!(
            sig.catchphrases.iter().any(|h| h.word == "哇塞"),
            "超频口癖词应检出: {:?}",
            sig.catchphrases
        );
        assert!(
            sig.catchphrases[0].boost > config().relative_boost_ratio,
            "相对超频比 > 2: {}",
            sig.catchphrases[0].boost
        );
    }

    #[test]
    fn no_catchphrase_when_not_overboosted() {
        // 全局频率与 persona 接近 → 超频比 ≈ 1 → 不判口癖
        let stats = stats_with(100, 2000);
        let pool = baseline_pool_with(5.0);
        let sig = analyze_significance(&stats, &pool, &config()).expect("数据足够");
        assert!(
            sig.catchphrases.is_empty(),
            "接近全局频率的口癖词不显著: {:?}",
            sig.catchphrases
        );
    }

    #[test]
    fn punctuation_high_detected() {
        // 目标 100/2000 → 5 次/100 字；全局 0.5 次/100 字 → 显著偏高
        let stats = stats_with(100, 2000);
        let pool = baseline_pool_with(0.5);
        let sig = analyze_significance(&stats, &pool, &config()).expect("数据足够");
        assert!(sig.exclaim_high, "感叹号应显著偏高");
        assert!(sig.slash_high, "断句符应显著偏高");
    }

    #[test]
    fn template_rule_contains_detected_items() {
        let stats = stats_with(100, 2000);
        let pool = baseline_pool_with(0.5);
        let sig = analyze_significance(&stats, &pool, &config()).unwrap();
        let rule = render_template_rule(&stats, &sig);
        assert!(rule.contains("哇塞"), "模板应含口癖词: {rule}");
        assert!(rule.contains("电影"), "模板应含话题: {rule}");
        assert!(rule.contains("感叹号"), "模板应含标点维度: {rule}");
    }

    #[test]
    fn template_rule_empty_when_no_significant() {
        // 无显著项（无口癖、无话题、无标点）→ 空规则
        let sig = StyleSignificant {
            catchphrases: Vec::new(),
            short_sentences: false,
            long_sentences: false,
            slash_high: false,
            comma_high: false,
            newline_high: false,
            exclaim_high: false,
            question_high: false,
            ellipsis_high: false,
            paren_high: false,
            tilde_high: false,
            sentiment_positive: false,
            sentiment_negative: false,
            interjection_high: false,
            sentiment_word_high: false,
            topics: Vec::new(),
        };
        let rule = render_template_rule(&StyleStats::default(), &sig);
        assert!(rule.is_empty(), "无显著项不生成规则: {rule}");
    }

    #[tokio::test]
    async fn generate_without_llm_returns_template() {
        // auto_translate=false 或 llm=None → 仅模板
        let stats = stats_with(100, 2000);
        let pool = baseline_pool_with(0.5);
        let sig = analyze_significance(&stats, &pool, &config()).unwrap();
        let rule = generate_style_rule(&stats, &sig, None, true, 0.3)
            .await
            .expect("generate 不应失败");
        assert_eq!(rule, render_template_rule(&stats, &sig));
    }

    #[test]
    fn build_translate_prompt_contains_no_raw_text() {
        let stats = stats_with(100, 2000);
        let pool = baseline_pool_with(0.5);
        let sig = analyze_significance(&stats, &pool, &config()).unwrap();
        assert!(!sig.catchphrases.is_empty(), "前置：应检出口癖词");
        let template = render_template_rule(&stats, &sig);
        let prompt = build_translate_prompt(&stats, &sig, &template);
        assert!(prompt.contains("哇塞"), "统计参数在 prompt 中");
        assert!(prompt.contains("模板规则"), "模板在 prompt 中");
        // prompt 不包含消息原文（本测试消息内容未传入，防御性断言）
        assert!(!prompt.contains("你好"), "不应出现原文文本");
    }
}
