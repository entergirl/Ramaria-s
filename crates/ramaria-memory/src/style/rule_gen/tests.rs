//! crates/ramaria-memory/src/style/rule_gen/tests.rs - //! crates/ramaria-memory/src/style/rule_gen.rs - 自动风格规则文本生成模块单元测试
//!
//! 设计特点:
//! - 位于 style::rule_gen 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 rule_gen.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
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
fn threshold_reached_resumes_significance_analysis() {
    // 同一样本特征：n_p=199 无输出 → n_p=200 恢复（数据积累后自动恢复规则生成）
    let pool = BaselinePool::new();
    let mut below = StyleStats {
        sample_count: 199,
        total_chars: 2000,
        ..Default::default()
    };
    assert!(
        analyze_significance(&below, &pool, &config()).is_none(),
        "199 条仍不足"
    );
    below.sample_count = 200;
    assert!(
        analyze_significance(&below, &pool, &config()).is_some(),
        "200 条达阈值 → 恢复显著性分析（冷启动仅话题也返回 Some）"
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

// ---- LLM 翻译增强降级（静默回退模板） ----

/// 恒失败 mock LLM（模拟翻译增强服务故障）。
struct FailingTranslateLlm {
    capability: ramaria_core::types::ModelCapability,
    config: ramaria_core::types::BackendConfig,
}

impl FailingTranslateLlm {
    fn new() -> Self {
        Self {
            capability: ramaria_core::types::ModelCapability {
                provider: ramaria_core::types::LlmProvider::LmStudio,
                model_id: "mock".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: false,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
            config: ramaria_core::types::BackendConfig::lm_studio_default(),
        }
    }
}

#[async_trait::async_trait]
impl LlmProviderTrait for FailingTranslateLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        Err(ramaria_core::RamariaError::llm("mock 翻译 LLM 故障"))
    }
    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<
        std::pin::Pin<
            Box<
                dyn futures::Stream<Item = RamariaResult<ramaria_core::traits::StreamDelta>> + Send,
            >,
        >,
    > {
        Err(ramaria_core::RamariaError::unsupported("mock 不支持流式"))
    }
    fn capability(&self) -> &ramaria_core::types::ModelCapability {
        &self.capability
    }
    fn config(&self) -> &ramaria_core::types::BackendConfig {
        &self.config
    }
    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }
    fn name(&self) -> &'static str {
        "FailingTranslateLlm"
    }
}

/// LLM 存在但翻译调用失败（auto_translate=true）→ warn 回退模板（不阻塞、不报错）。
#[tokio::test]
async fn generate_with_failing_llm_falls_back_to_template() {
    let stats = stats_with(100, 2000);
    let pool = baseline_pool_with(0.5);
    let sig = analyze_significance(&stats, &pool, &config()).unwrap();
    let llm = FailingTranslateLlm::new();
    let rule = generate_style_rule(&stats, &sig, Some(&llm), true, 0.3)
        .await
        .expect("LLM 故障应静默回退模板（不报错）");
    assert_eq!(
        rule,
        render_template_rule(&stats, &sig),
        "翻译失败应返回模板文本"
    );
    assert!(!rule.trim().is_empty(), "模板含显著项时不应为空");
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
