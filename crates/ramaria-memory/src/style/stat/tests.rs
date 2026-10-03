//! crates/ramaria-memory/src/style/stat/tests.rs - //! crates/ramaria-memory/src/style/stat.rs - 五维风格指标统计模块单元测试
//!
//! 设计特点:
//! - 位于 style::stat 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 stat.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::types::{MessageRole, MessageSource};
use uuid::Uuid;

fn msg(content: &str) -> Message {
    Message::new(
        Uuid::new_v4(),
        MessageRole::Assistant,
        content.to_string(),
        MessageSource::Local,
    )
}

fn config() -> StyleConfig {
    StyleConfig::default()
}

#[test]
fn empty_messages_yield_default_stats() {
    let stats = StyleStats::compute(&[], &config());
    assert_eq!(stats.sample_count, 0);
    assert_eq!(stats.total_chars, 0);
    assert!(!stats.has_enough_sample(200));
}

#[test]
fn sample_count_and_chars_are_correct() {
    let messages = [msg("你好呀！"), msg("今天很开心。")];
    let stats = StyleStats::compute(&messages, &config());
    assert_eq!(stats.sample_count, 2);
    // "你好呀！"(4) + "今天很开心。"(6) = 10 有效字符
    assert_eq!(stats.total_chars, 10);
    assert_eq!(stats.total_sentences, 2);
}

/// 主动消息口径：主动消息照常计入风格统计样本（零来源过滤）。
#[test]
fn proactive_messages_are_counted_in_style_stats() {
    let messages = [
        msg("主动说一句问候呀。").with_proactive(true),
        msg("今天很开心。"),
    ];
    let stats = StyleStats::compute(&messages, &config());
    assert_eq!(stats.sample_count, 2, "主动消息应计入风格样本");
    assert_eq!(stats.total_sentences, 2);
}

#[test]
fn punctuation_counts_are_correct() {
    let messages = [msg("哇！真的吗？？好棒～（开心）……哎||嗯嗯")];
    let stats = StyleStats::compute(&messages, &config());
    assert_eq!(stats.exclaim_count, 1);
    assert_eq!(stats.question_count, 2);
    assert_eq!(stats.tilde_count, 1);
    assert_eq!(stats.paren_count, 1);
    assert!(
        stats.ellipsis_count >= 1,
        "省略号计数: {}",
        stats.ellipsis_count
    );
    assert_eq!(stats.slash_count, 1);
    assert!(
        stats.interjection_count >= 2,
        "感叹词计数: {}",
        stats.interjection_count
    );
}

#[test]
fn sentence_len_summary_is_ordered() {
    let messages = [
        msg("好。"),
        msg("今天天气真的很好，我们出去走走吧！"),
        msg("嗯嗯。"),
    ];
    let stats = StyleStats::compute(&messages, &config());
    assert!(stats.sentence_len_p25 <= stats.sentence_len_mean);
    assert!(stats.sentence_len_mean <= stats.sentence_len_p75);
}

#[test]
fn sentiment_stats_reflect_lexicon() {
    // 两条积极 + 一条消极 → 均值 > 0（积极方向）
    let messages = [msg("太棒了，真好！"), msg("今天很开心"), msg("我很难过")];
    let stats = StyleStats::compute(&messages, &config());
    assert_eq!(stats.sentiment_n, 3);
    assert!(
        stats.sentiment_mean > 0.0,
        "积极消息应拉高均值: {}",
        stats.sentiment_mean
    );
    assert!(stats.sentiment_std >= 0.0);
    assert_eq!(stats.sentiment_word_messages, 3, "三条消息均命中情感词典");
}

#[test]
fn word_freq_excludes_stop_words() {
    let messages = [
        msg("我真的非常喜欢看书，看书很有意思"),
        msg("我也很喜欢看书，一起看书吧！"),
    ];
    let stats = StyleStats::compute(&messages, &config());
    // 停用词（真的/非常/喜欢/这个）被过滤；"看书"作为高频非停用词应出现在词频中
    assert!(
        stats.word_freq.iter().any(|(w, _)| w == "看书"),
        "词频应含'看书': {:?}",
        stats.word_freq
    );
    for stop in ["真的", "非常", "喜欢"] {
        assert!(
            !stats.word_freq.iter().any(|(w, _)| w == stop),
            "停用词'{stop}'应被过滤: {:?}",
            stats.word_freq
        );
    }
}

#[test]
fn topic_freq_excludes_catchphrases() {
    let messages = [
        msg("喜欢看书，喜欢电影"),
        msg("看书很有意思，电影也好"),
        // 引入更多词，使口癖 Top-N 只占高频部分，剩余词作为话题偏好
        msg("周末去看展，顺便逛街"),
    ];
    let stats = StyleStats::compute(&messages, &config());
    // "喜欢"是停用词（被过滤）；高频词"看书"/"电影"进入口癖或话题，
    // 话题词与口癖词不应重复
    let catch_set: Vec<&str> = stats.word_freq.iter().map(|(w, _)| w.as_str()).collect();
    for (w, _) in &stats.topic_freq {
        assert!(
            !catch_set.contains(&w.as_str()),
            "话题词不应与口癖词重复: {w}"
        );
    }
    assert!(
        !stats.topic_freq.is_empty(),
        "应识别出话题词: {:?}",
        stats.topic_freq
    );
}

#[test]
fn freq_and_per_100_are_consistent() {
    let stats = StyleStats {
        total_chars: 200,
        ..Default::default()
    };
    assert!((stats.freq(10) - 0.05).abs() < 1e-9);
    assert!((stats.per_100(10) - 5.0).abs() < 1e-9);
}

#[test]
fn has_enough_sample_boundary() {
    let stats = StyleStats {
        sample_count: 199,
        ..Default::default()
    };
    assert!(!stats.has_enough_sample(200));
    let stats = StyleStats {
        sample_count: 200,
        ..Default::default()
    };
    assert!(stats.has_enough_sample(200));
}

// ---- 关键词衔接：canonical 词典增强 / 空词表回退 ----

#[test]
fn compute_with_empty_canonical_matches_plain_compute() {
    // 空词表回退 = 纯 bigram（与无词表入口行为一致）
    let messages = [msg("工作压力很大"), msg("工作压力真的很大，最近压力好大")];
    let plain = StyleStats::compute(&messages, &config());
    let with_empty = StyleStats::compute_with_keywords(&messages, &config(), &[]);
    assert_eq!(plain, with_empty);
    assert!(
        plain
            .word_freq
            .iter()
            .any(|(w, _)| w == "工作" || w == "压力"),
        "纯 bigram 应产出相邻二元组"
    );
}

#[test]
fn canonical_dict_keeps_whole_word_as_candidate() {
    // 词典增强：canonical 词"工作压力"整词进入候选，不拆出"作压"噪声
    let messages = [msg("最近工作压力很大"), msg("工作压力让人睡不好")];
    let canonical = vec!["工作压力".to_string()];
    let stats = StyleStats::compute_with_keywords(&messages, &config(), &canonical);
    assert!(
        stats.word_freq.iter().any(|(w, _)| w == "工作压力"),
        "canonical 词应整体成为风格候选: {:?}",
        stats.word_freq
    );
    assert!(
        !stats.word_freq.iter().any(|(w, _)| w == "作压"),
        "不应出现词典命中的噪声二元组"
    );
}

// ---- SpeakingStyle 原文样例兜底 ----

#[test]
fn pick_samples_prefers_messages_with_high_freq_words() {
    let messages = [
        msg("哇塞今天天气真好"),
        msg("哇塞这本书也太好看了吧，哇塞"),
        msg("我们一起去吃饭吧"),
    ];
    let cfg = config();
    let stats = StyleStats::compute(&messages, &cfg);
    let samples = stats.pick_style_samples(&messages, 2, 48);
    assert!(!samples.is_empty(), "至少选出一条样例");
    // 含"哇塞"（非停用高频）的消息优先于普通句
    assert!(
        samples.iter().any(|s| s.contains("哇塞")),
        "样例应优先代表口癖词消息: {:?}",
        samples
    );
}

#[test]
fn pick_samples_truncates_long_messages_and_dedup() {
    let long1 = "哇塞".repeat(30);
    let messages = [msg(&format!("哇塞{long1}好长好长")), msg("哇塞短句")];
    let cfg = config();
    let stats = StyleStats::compute(&messages, &cfg);
    let samples = stats.pick_style_samples(&messages, 3, 8);
    assert!(!samples.is_empty());
    for s in &samples {
        assert!(s.chars().count() <= 8, "样例应截断到 max_chars: {s}");
    }
    // 长消息截断前缀与短句重复时去重
    let mut seen = std::collections::HashSet::new();
    for s in &samples {
        assert!(seen.insert(s.clone()), "样例去重: {s}");
    }
}

#[test]
fn pick_samples_empty_inputs() {
    let cfg = config();
    assert!(
        StyleStats::default()
            .pick_style_samples(&[], 3, 48)
            .is_empty()
    );
    let messages = [msg("   "), msg("")];
    let stats = StyleStats::compute(&messages, &cfg);
    assert!(stats.pick_style_samples(&messages, 3, 48).is_empty());
}
