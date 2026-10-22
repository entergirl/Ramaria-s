//! crates/ramaria-memory/src/event/paraphrase/tests.rs - 事件转述单元测试
//!
//! 设计特点:
//! - 位于 event::paraphrase 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由父文件以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use std::pin::Pin;
use std::sync::Mutex;

use async_trait::async_trait;
use ramaria_core::types::{BackendConfig, ModelCapability};
use ramaria_core::{RamariaError, RamariaResult};

/// clean_paraphrase 各输入参数化验证：去引号 / 截断 / 空输入 / 保真 / 中文弯引号。
#[test]
fn clean_paraphrase_cases() {
    // 去英文引号（双引号/单引号）
    assert_eq!(
        clean_paraphrase(r#""面对批评时容易沮丧""#, 30),
        "面对批评时容易沮丧"
    );
    assert_eq!(
        clean_paraphrase("'面对权威时倾向于退缩'", 30),
        "面对权威时倾向于退缩"
    );
    // 去中文弯引号
    assert_eq!(
        clean_paraphrase("\u{201c}面对批评容易沮丧\u{201d}", 30),
        "面对批评容易沮丧"
    );
    // 超长截断
    let long = "这是一个非常长的去情境化描述文本超过了三十个字的限制需要截断处理";
    let result = clean_paraphrase(long, 30);
    assert!(result.chars().count() <= 30);
    // 空输入
    assert_eq!(clean_paraphrase("", 30), "");
    assert_eq!(clean_paraphrase("   ", 30), "");
    // 30 字以内保留全文
    let input = "面对亲密关系中的不安全感时倾向于过度担忧";
    assert!(input.chars().count() <= 30);
    assert_eq!(clean_paraphrase(input, 30), input);
}

#[test]
fn config_defaults() {
    let config = ParaphraseConfig::default();
    assert_eq!(config.temperature, 0.2);
    assert_eq!(config.max_tokens, 128);
    assert_eq!(config.max_chars, 30);
    // 保留度校验默认开启，阈值在校准后的保守区间
    assert!(config.retention_check_enabled);
    assert!(config.retention_min > 0.0 && config.retention_min <= 1.0);
}

// =========================================================
// compute_information_retention 纯函数测试
// =========================================================

/// 完全保留（原文即重述）→ 高分。
#[test]
fn retention_full_copy_scores_high() {
    let source = "面对权威批评时容易感到沮丧";
    let score = compute_information_retention(source, source);
    assert!(
        (score - 1.0).abs() < 1e-9,
        "原文与重述完全一致时应得 1.0，实际 {score}"
    );

    // 轻微改写（词序/插入少量虚词）仍应保持高保留
    let light = "被批评后感到很沮丧和委屈";
    let light_rewrite = "面对批评后容易感到很沮丧委屈";
    let s = compute_information_retention(light, light_rewrite);
    assert!(s >= 0.5, "轻微改写应保持高保留，实际 {s}");
}

/// 关键词被清空/完全跑题 → 低分（级联失效可检出）。
#[test]
fn retention_cleared_content_scores_low() {
    let source = "被批评后感到委屈和沮丧";
    // 完全跑题：与 source 零字面共享
    let unrelated = "今天天气很好适合出门散步";
    let score = compute_information_retention(source, unrelated);
    assert!(score < 0.05, "完全跑题保留度应趋近 0，实际 {score}");

    // 信息清空为无实质内容的空泛回应
    let hollow = "嗯嗯好的";
    let score2 = compute_information_retention(source, hollow);
    assert!(score2 < 0.05, "信息清空保留度应趋近 0，实际 {score2}");
}

/// 空输入一律 0.0，不 panic。
#[test]
fn retention_empty_inputs_zero() {
    assert_eq!(compute_information_retention("", ""), 0.0);
    assert_eq!(compute_information_retention("", "abc"), 0.0);
    assert_eq!(compute_information_retention("abc", ""), 0.0);
    assert_eq!(compute_information_retention("   ", "abc"), 0.0);
    assert_eq!(compute_information_retention("abc", "   "), 0.0);
    // 单字符极短文本不 panic（bigram 集合空 → 整串相等判定）
    let s = compute_information_retention("难", "难");
    assert!(s.is_finite(), "单字相等应返回有限值");
    let s = compute_information_retention("难", "喜");
    assert!(s.is_finite(), "单字不等应返回有限值");
}

/// 保留核心情感/反应词的同义改写不误判为 0。
#[test]
fn retention_synonym_rewrite_not_zero() {
    // 去情境化：剥离"老板/周会"等实体，保留"批评/委屈/沮丧"核心反应词
    let source = "被老板在周会上批评后感到委屈和沮丧";
    let restated = "面对公开批评时容易感到委屈沮丧";
    let score = compute_information_retention(source, restated);
    assert!(
        score > 0.05,
        "保留核心反应词的重述不应被误判为 0，实际 {score}"
    );

    // 明确断言：同义改写保留度显著高于完全跑题
    let unrelated = compute_information_retention(source, "今天天气很好适合出门散步");
    assert!(
        score > unrelated,
        "同义改写应高于完全跑题: {score} vs {unrelated}"
    );
}

/// 中文与 CJK 标点健壮：标点不显著压低保留度。
#[test]
fn retention_cjk_punctuation_robust() {
    let bare = "面对批评感到沮丧";
    let punctuated_source = "面对批评，感到沮丧。";
    let score = compute_information_retention(punctuated_source, bare);
    assert!(score >= 0.5, "标点差异不应显著压低保留度，实际 {score}");
    // 纯标点 source 不 panic
    let s = compute_information_retention("。。。", "面对批评");
    assert!(s.is_finite());
}

/// 结果恒 ∈ [0,1] 且为有限值。
#[test]
fn retention_result_in_unit_range() {
    let pairs = [
        ("被老板批评后很沮丧", "面对权威批评时倾向于沮丧"),
        ("对项目成功感到自豪", "在成就被认可时产生强烈自豪感"),
        ("和朋友出去玩很开心", "在社交活动中容易获得愉悦感"),
        ("被领导否定方案时强烈抵触", "当专业性被权威否定时强烈抵触"),
        ("rust编程学习", "编程学习带来成就感"),
        ("面对冲突习惯性回避", "冲突"),
    ];
    for (s, r) in pairs {
        let score = compute_information_retention(s, r);
        assert!(
            score.is_finite() && (0.0..=1.0).contains(&score),
            "保留度应 ∈ [0,1]，({s} → {r}) 实际 {score}"
        );
    }
}

/// 校准样例（文档口径参考）：
/// 正常去情境化改写应显著高于"完全跑题/信息清空"。
/// 该对照锁定了 retention_min 默认值的校准区间（防止把失效阈值定到误伤正常改写）。
#[test]
fn retention_calibration_separates_normal_and_failure() {
    let normal_pairs = [
        ("被老板批评后很沮丧", "面对权威批评时倾向于沮丧"),
        ("对项目成功感到自豪", "在成就被认可时产生强烈自豪感"),
        ("被领导否定方案时强烈抵触", "当专业性被权威否定时强烈抵触"),
    ];
    let fail_pairs = [
        ("被老板批评后很沮丧", "今天天气很好适合出游"),
        ("对项目成功感到自豪", "没什么特别的感受"),
    ];
    let default_min = ParaphraseConfig::default().retention_min;
    for (s, r) in normal_pairs {
        let score = compute_information_retention(s, r);
        assert!(
            score >= default_min,
            "正常去情境化改写不应低于默认阈值 {default_min}，({s} → {r}) 实际 {score}"
        );
    }
    for (s, r) in fail_pairs {
        let score = compute_information_retention(s, r);
        assert!(
            score < default_min,
            "失效样例应低于默认阈值 {default_min}，({s} → {r}) 实际 {score}"
        );
    }
}

// =========================================================
// generate_paraphrase 测试（mock LLM，无真实网络）
// =========================================================

/// 测试用 stub LLM：chat 恒返回预设文本或错误。
struct StubLlm {
    reply: Mutex<Option<String>>,
}

impl StubLlm {
    fn ok(reply: &str) -> Self {
        Self {
            reply: Mutex::new(Some(reply.to_string())),
        }
    }
    /// 不预设回复 → chat 返回错误（模拟 LLM 调用失败）。
    fn failing() -> Self {
        Self {
            reply: Mutex::new(None),
        }
    }
}

#[async_trait]
impl LlmProviderTrait for StubLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        self.reply
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| RamariaError::llm("stub: 未预设 chat 返回值"))
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<
        Pin<
            Box<
                dyn futures::Stream<Item = RamariaResult<ramaria_core::traits::StreamDelta>> + Send,
            >,
        >,
    > {
        unimplemented!("stub chat_stream 未实现")
    }

    fn capability(&self) -> &ModelCapability {
        unimplemented!("stub capability 未实现")
    }

    fn config(&self) -> &BackendConfig {
        unimplemented!("stub config 未实现")
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "stub-llm"
    }
}

#[tokio::test]
async fn generate_high_retention_returns_some() {
    let llm = StubLlm::ok("面对批评时容易感到沮丧");
    let config = ParaphraseConfig::default();
    let result = generate_paraphrase(
        &llm,
        "被老板在周会上批评后感到很沮丧",
        "用户工作汇报被批评",
        &config,
    )
    .await;
    assert_eq!(
        result.as_deref(),
        Some("面对批评时容易感到沮丧"),
        "保留度达标时应返回清理后的重述文本"
    );
}

#[tokio::test]
async fn generate_low_retention_returns_none_when_check_enabled() {
    // 开关开启：LLM 输出与 attitude 完全跑题（信息清空）→ 判级联失效 → None
    let llm = StubLlm::ok("今天天气很好适合出门散步");
    let config = ParaphraseConfig {
        retention_check_enabled: true,
        ..Default::default()
    };
    let result = generate_paraphrase(
        &llm,
        "被老板在周会上批评后感到很沮丧",
        "用户工作汇报被批评",
        &config,
    )
    .await;
    assert!(result.is_none(), "低保留重述应判级联失效并返回 None");
}

#[tokio::test]
async fn generate_low_retention_still_some_when_check_disabled() {
    // 开关关闭：回退旧行为，直接采用清理结果（逐字节一致）
    let llm = StubLlm::ok("今天天气很好适合出门散步");
    let config = ParaphraseConfig {
        retention_check_enabled: false,
        ..Default::default()
    };
    let result = generate_paraphrase(
        &llm,
        "被老板在周会上批评后感到很沮丧",
        "用户工作汇报被批评",
        &config,
    )
    .await;
    assert_eq!(
        result.as_deref(),
        Some("今天天气很好适合出门散步"),
        "开关关闭时应直接采用清理结果"
    );
}

#[tokio::test]
async fn generate_quoted_high_retention_is_cleaned() {
    // LLM 输出带引号包裹且保留度高 → clean 后返回去引号文本
    let llm = StubLlm::ok("\u{201c}面对批评时容易感到沮丧\u{201d}");
    let config = ParaphraseConfig::default();
    let result = generate_paraphrase(
        &llm,
        "被老板在周会上批评后感到很沮丧",
        "用户工作汇报被批评",
        &config,
    )
    .await;
    assert_eq!(
        result.as_deref(),
        Some("面对批评时容易感到沮丧"),
        "应剥离引号并返回清理文本"
    );
}

#[tokio::test]
async fn generate_llm_error_returns_none() {
    // LLM 调用失败 → None（既有行为不变），开关开启/关闭均不受影响
    let llm = StubLlm::failing();
    let config_on = ParaphraseConfig::default();
    let result_on = generate_paraphrase(&llm, "被批评后沮丧", "context", &config_on).await;
    assert!(result_on.is_none(), "LLM 报错时应返回 None");

    let config_off = ParaphraseConfig {
        retention_check_enabled: false,
        ..Default::default()
    };
    let result_off = generate_paraphrase(&llm, "被批评后沮丧", "context", &config_off).await;
    assert!(result_off.is_none(), "开关关闭时 LLM 报错仍应返回 None");
}

#[tokio::test]
async fn generate_empty_output_returns_none() {
    // LLM 返回空/空白 → None（既有行为不变）
    let llm = StubLlm::ok("   ");
    let config = ParaphraseConfig::default();
    let result = generate_paraphrase(&llm, "被批评后沮丧", "context", &config).await;
    assert!(result.is_none(), "空白输出应返回 None");
}
