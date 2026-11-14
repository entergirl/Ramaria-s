//! crates/ramaria-eval/src/score/persona/tests.rs - Ramaria 评测人格表达三维单元测试
//!
//! 设计特点:
//! - 事实维与情感维全部走纯函数断言，不联网、不加载模型
//! - 嵌入路径用零向量 mock：余弦恒 0，只验证权重编排与口径标签
//! - 语气维只断言解析与模板文本，模型行为由判分模块的 mock 用例覆盖
//! - 条件汇总用构造结果直接吸收，不依赖逐题产物文件

use ramaria_testkit::MockEmbedding;

use super::emotion::{EmotionScore, score_emotion_item};
use super::fact::{
    FactScore, content_bigrams, keyword_hit_norm_score, keyword_hit_score, score_fact_item,
};
use super::tone::{parse_judge_score, tone_system_prompt};
use super::*;

// =========================================================
// 夹具
// =========================================================

/// 构造一条逐题三维结果（事实 / 情感按分值造，判据细节不参与汇总断言）。
fn persona_of(tone: Option<u8>, fact: Option<f64>, emotion: Option<f64>) -> PersonaScore {
    PersonaScore {
        tone,
        fact: fact.map(|score| FactScore {
            score,
            cosine: None,
            keyword_hit: score,
            keyword_hit_norm: score,
            fact_point: score,
            variant: "keyword",
        }),
        emotion: emotion.map(|score| EmotionScore {
            score,
            situation_negative: false,
            situation_positive: false,
            marker_hit: 0,
        }),
        reference: PERSONA_REFERENCE.to_string(),
    }
}

const REFERENCE: &str = "用户去年收养了一只猫，取名团子";

// =========================================================
// 事实维
// =========================================================

/// 空回复记 0 分：三条关键词判据全为空，口径为纯关键词。
#[tokio::test]
async fn fact_empty_reply_scores_zero() {
    let score = score_fact_item("   ", REFERENCE, None).await;
    assert_eq!(score.score, 0.0);
    assert_eq!(score.keyword_hit, 0.0);
    assert_eq!(score.keyword_hit_norm, 0.0);
    assert_eq!(score.fact_point, 0.0);
    assert!(score.cosine.is_none());
    assert_eq!(score.variant, "keyword");
}

/// 回复与参考完全一致：旧 2-gram 覆盖率满格。
#[test]
fn fact_keyword_hit_full_when_reply_matches_reference() {
    assert!((keyword_hit_score("团子是三花猫", "团子是三花猫") - 1.0).abs() < 1e-9);
}

/// 内容 2-gram 剔除含标点与两侧皆功能字的组合。
#[test]
fn fact_content_bigrams_filter_punct_and_function_pairs() {
    assert!(content_bigrams("我，你").is_empty(), "含标点的组合应剔除");
    assert!(content_bigrams("我是").is_empty(), "两侧皆功能字应剔除");
    assert_eq!(content_bigrams("团子是").len(), 2, "单侧功能字仍保留");
    assert!(content_bigrams("猫").is_empty(), "不足 2 字无组合");
}

/// 同一输入下旧 2-gram 口径与长度归一口径不同：短回复在旧口径被压低。
#[test]
fn fact_length_normalized_differs_from_old_metric() {
    let reply = "团子";
    let old = keyword_hit_score(reply, REFERENCE);
    let norm = keyword_hit_norm_score(reply, REFERENCE);
    assert!(
        old < 0.1,
        "旧口径分母为参考全部 2-gram，短回复被压低: {old}"
    );
    assert!((norm - 1.0).abs() < 1e-9, "长度归一口径应满格: {norm}");
}

/// 余弦可用时按 0.6 / 0.4 加权；口径标签标为 `cosine+keyword`。
#[tokio::test]
async fn fact_score_uses_cosine_and_keyword_weights() {
    let embedder = MockEmbedding::new();
    let score = score_fact_item("团子是三花猫", "团子是三花猫", Some(&embedder)).await;

    let keyword_hit = keyword_hit_score("团子是三花猫", "团子是三花猫");
    assert!((keyword_hit - 1.0).abs() < 1e-9);
    assert_eq!(score.variant, "cosine+keyword");
    assert_eq!(score.cosine, Some(0.0), "零向量余弦恒为 0");
    assert!(
        (score.score - 0.4 * keyword_hit).abs() < 1e-9,
        "余弦为 0 时综合分只剩关键词项 0.4: {}",
        score.score
    );
}

/// 无嵌入时降级为纯关键词：同一输入得分高于带余弦的加权口径。
#[tokio::test]
async fn fact_score_degrades_to_keyword_only() {
    let score = score_fact_item("团子是三花猫", "团子是三花猫", None).await;
    assert_eq!(score.variant, "keyword");
    assert!(score.cosine.is_none());
    assert!((score.score - 1.0).abs() < 1e-9);
}

// =========================================================
// 情感维
// =========================================================

/// 负面情境 + 充分安慰记 1.0；单标记记 0.5；无标记记 0.0。
#[test]
fn emotion_negative_situation_rubric() {
    let question = "今天被领导当众批评，好难过";
    let full = score_emotion_item("别难过，我理解你，会好的", question);
    assert_eq!(full.score, 1.0);
    assert!(full.situation_negative && !full.situation_positive);

    let partial = score_emotion_item("别担心，睡一觉就好了", question);
    assert_eq!(partial.score, 0.5);
    assert_eq!(partial.marker_hit, 1);

    let cold = score_emotion_item("这个方案本身就有问题，明天重写吧", question);
    assert_eq!(cold.score, 0.0);

    let empty = score_emotion_item("", question);
    assert_eq!(empty.score, 0.0);
}

/// 正面情境 + 分享喜悦记 1.0；单标记记 0.5；无标记记 0.0。
#[test]
fn emotion_positive_situation_rubric() {
    let question = "我升职了，太开心了";
    let full = score_emotion_item("太好了，真棒，恭喜你", question);
    assert_eq!(full.score, 1.0);
    assert!(full.situation_positive && !full.situation_negative);

    let partial = score_emotion_item("嗯，不错", question);
    assert_eq!(partial.score, 0.5);

    let cold = score_emotion_item("下次注意保持", question);
    assert_eq!(cold.score, 0.0);
}

// =========================================================
// 语气维
// =========================================================

/// 1~5 数字提取：纯数字、带前缀文字、中文数字均可；越界为 `None`。
#[test]
fn tone_parse_judge_score_cases() {
    assert_eq!(parse_judge_score("4"), Some(4));
    assert_eq!(parse_judge_score("评分：5"), Some(5));
    assert_eq!(parse_judge_score("四"), Some(4));
    assert_eq!(parse_judge_score("0"), None);
    assert_eq!(parse_judge_score("六"), None);
}

/// 语气维系统提示是 rubric 与 few-shot 锚定的同一份文本。
#[test]
fn tone_system_prompt_is_rubric_with_anchors() {
    let system = tone_system_prompt();
    assert!(system.starts_with("请按 1~5 分评估"));
    assert!(system.contains("重要：不要按回复长短判分"));
    assert!(system.contains("【示例 4】"));
}

// =========================================================
// 条件汇总
// =========================================================

/// 两题语气 4 / 2：均值 3.0，题数按题计。
#[test]
fn sums_average_tone_over_questions() {
    let mut sums = PersonaSums::default();
    sums.absorb(&persona_of(Some(4), Some(0.5), Some(1.0)), true);
    sums.absorb(&persona_of(Some(2), Some(0.0), Some(0.0)), true);

    let summary = sums.finish().expect("有题应产出汇总");
    assert_eq!(summary.questions, 2);
    assert_eq!(summary.tone, Some(3.0));
    assert_eq!(summary.fact, Some(0.25));
    assert_eq!(summary.emotion, Some(0.5));
    assert_eq!(summary.tone_failed, 0);
    assert_eq!(summary.fact_variant, "keyword");
}

/// 一题语气解析失败：只记失败数，均值只算另一题。
#[test]
fn sums_count_tone_failure_without_mean() {
    let mut sums = PersonaSums::default();
    sums.absorb(&persona_of(Some(4), Some(1.0), Some(1.0)), true);
    sums.absorb(&persona_of(None, Some(1.0), Some(1.0)), true);

    let summary = sums.finish().expect("有题应产出汇总");
    assert_eq!(summary.questions, 2);
    assert_eq!(summary.tone, Some(4.0));
    assert_eq!(summary.tone_failed, 1);
}

/// 未启用判分时的语气缺失不算失败。
#[test]
fn sums_ignore_tone_absence_when_not_attempted() {
    let mut sums = PersonaSums::default();
    sums.absorb(&persona_of(None, Some(1.0), Some(1.0)), false);

    let summary = sums.finish().expect("有题应产出汇总");
    assert_eq!(summary.tone, None);
    assert_eq!(summary.tone_failed, 0);
}

/// 无参与题时整列留空。
#[test]
fn sums_finish_is_none_without_questions() {
    assert!(PersonaSums::default().finish().is_none());
}
