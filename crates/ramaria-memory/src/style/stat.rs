//! crates/ramaria-memory/src/style/stat.rs - 五维风格指标统计模块
//!
//! 设计特点:
//! - 统计单元：目标 persona 的全部 messages（表达层层次 2，v3.1 §7.2）
//! - 五维指标：口癖词 / 句式句长 / 标点 / 情感表达 / 话题词汇偏好
//! - 输出结构化参数（统计值 + 样本量），不含原文消息文本（隐私红线）
//! - 纯函数计算，零 I/O：输入消息列表，输出 StyleStats 摘要
//! - 频率口径统一：计数 / 有效字符数（概率形式供二项 z 检验；展示 ×100）
//! - 分词复用 BM25 bigram tokenize；停用词内置常量过滤通用高频词

use std::collections::HashMap;

use ramaria_core::config::StyleConfig;
use ramaria_core::types::Message;
use serde::{Deserialize, Serialize};

use crate::behavior::sentiment::SentimentLexicon;
use crate::bm25::tokenize;

// =========================================================
// 停用词与感叹词表
// =========================================================

/// 中文常用功能词（bigram 形态，与 `bm25::tokenize` 的切分粒度一致）。
///
/// 用途:
/// - 口癖词/话题词统计前过滤通用高频词（排除后剩余的词才有区分度）。
/// - 该表是"通用高频词"的近似，不追求完备（漏网词由显著性检验排除）。
const STOP_WORDS: &[&str] = &[
    // 代词/指代
    "我们",
    "你们",
    "他们",
    "她们",
    "它们",
    "自己",
    "别人",
    "大家",
    "这个",
    "那个",
    "这些",
    "那些",
    "这里",
    "那里",
    "这边",
    "那边",
    "这种",
    "那种",
    "这么",
    "那么",
    "什么",
    "怎么",
    "为什么",
    "多少",
    "哪里",
    "哪个",
    "哪些",
    "谁呀",
    "谁的",
    // 助词/语气词
    "的了",
    "了呀",
    "呢吧",
    "吧啊",
    "啊呀",
    "呀嘛",
    "嘛哦",
    "哦嗯",
    "嗯哈",
    "哈啦",
    "啦哎",
    "唉呀",
    "哇哦",
    "噢噢",
    "嗯嗯",
    "好的",
    "是的",
    "对呀",
    "对的",
    "没错",
    "可以",
    "应该",
    // 副词/连词
    "非常",
    "特别",
    "比较",
    "有点",
    "可能",
    "大概",
    "然后",
    "但是",
    "因为",
    "所以",
    "如果",
    "虽然",
    "不过",
    "而且",
    "或者",
    "还有",
    "只是",
    "就是",
    "不是",
    "没有",
    "真的",
    "其实",
    "反正",
    "当然",
    "确实",
    "的确",
    "显然",
    "一定",
    "完全",
    "终于",
    "突然",
    "原来",
    "接着",
    "继续",
    "开始",
    "结束",
    "最后",
    "首先",
    "比如",
    "例如",
    "关于",
    "对于",
    "相对",
    "通过",
    "根据",
    "按照",
    "只要",
    "只有",
    "无论",
    "不管",
    "除了",
    "另外",
    "同时",
    // 动词/状态（高频无区分度）
    "知道",
    "觉得",
    "感觉",
    "希望",
    "喜欢",
    "看到",
    "听到",
    "想到",
    "说到",
    "做到",
    "需要",
    "想要",
    "打算",
    "准备",
    "认为",
    "以为",
    "发现",
    "了解",
    "明白",
    "记得",
    "忘记",
    // 时间/空间/通用名词
    "现在",
    "今天",
    "明天",
    "昨天",
    "上午",
    "下午",
    "晚上",
    "中午",
    "早上",
    "平时",
    "最近",
    "之前",
    "以后",
    "后来",
    "时候",
    "地方",
    "东西",
    "事情",
    "问题",
    "情况",
    "方式",
    "方法",
    "结果",
    "原因",
    "关系",
    "方面",
    "时间",
    "一天",
    "一次",
    "一下",
    "一点",
    "一些",
    "一个",
    "一种",
    "一直",
    "一起",
    "一样",
];

/// 感叹词表（情感表达维度：感叹词频率）。
///
/// 单字/双字混合，直接在原文中做子串匹配计数（与 bigram 分词粒度无关）。
const INTERJECTIONS: &[&str] = &[
    "哎", "唉", "哇", "嗯", "哦", "啊", "哈", "呀", "哟", "喔", "噢", "嘿", "诶", "嘛", "啦", "吧",
    "呢", "哇塞", "嗯哼", "唉呀", "哎呀",
];

// =========================================================
// 五维统计结果
// =========================================================

/// 五维风格统计结果（结构化参数 + 样本量）。
///
/// 字段约定:
/// - `sample_count`: 统计样本量 n_p（消息条数）。
/// - `total_chars`: 有效字符数（去空白/换行，作频率分母）。
/// - 计数类字段（slash_count/exclaim_count 等）为原始计数，
///   频率由 [`StyleStats::freq`] 派生（计数 / total_chars）。
/// - `word_freq`/`topic_freq`: 去停用词后的词频（降序），供口癖词/话题词显著性检验。
///
/// 安全约束:
/// - 本结构只含统计参数，**不含任何原文消息文本**。
/// - `sentence_len_mean/p25/p75` 为句长分布摘要（原始句长序列不落库）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StyleStats {
    /// 统计样本量 n_p（消息条数）
    pub sample_count: u32,
    /// 有效字符数（去除空白与换行）
    pub total_chars: u32,
    /// 句数（按句号/感叹/问号/换行切分）
    pub total_sentences: u32,
    // ---- 口癖词（去停用词后词频，降序） ----
    pub word_freq: Vec<(String, u32)>,
    // ---- 句式句长 ----
    pub sentence_len_mean: f64,
    pub sentence_len_p25: f64,
    pub sentence_len_p75: f64,
    /// `||` 断句符出现次数
    pub slash_count: u32,
    /// 逗号（全角/半角）出现次数
    pub comma_count: u32,
    /// 换行符出现次数
    pub newline_count: u32,
    // ---- 标点 ----
    /// 感叹号（！!）出现次数
    pub exclaim_count: u32,
    /// 问号（？?）出现次数
    pub question_count: u32,
    /// 省略号（…… 按 2 次计 / ...）出现次数
    pub ellipsis_count: u32,
    /// 括号（（）()）出现次数（按对计数）
    pub paren_count: u32,
    /// 波浪号（~～）出现次数
    pub tilde_count: u32,
    // ---- 情感表达 ----
    /// 每条消息情感极性得分均值（-1..1）
    pub sentiment_mean: f64,
    /// 情感极性得分标准差（情感强度分布）
    pub sentiment_std: f64,
    /// 参与情感极性统计的消息数
    pub sentiment_n: u32,
    /// 感叹词出现次数
    pub interjection_count: u32,
    /// 命中情感词典的消息数（sentiment_word_rate 的分子）
    pub sentiment_word_messages: u32,
    // ---- 话题词汇偏好（去停用词高频词，排除口癖词，降序） ----
    pub topic_freq: Vec<(String, u32)>,
}

impl Default for StyleStats {
    fn default() -> Self {
        Self {
            sample_count: 0,
            total_chars: 0,
            total_sentences: 0,
            word_freq: Vec::new(),
            sentence_len_mean: 0.0,
            sentence_len_p25: 0.0,
            sentence_len_p75: 0.0,
            slash_count: 0,
            comma_count: 0,
            newline_count: 0,
            exclaim_count: 0,
            question_count: 0,
            ellipsis_count: 0,
            paren_count: 0,
            tilde_count: 0,
            sentiment_mean: 0.0,
            sentiment_std: 0.0,
            sentiment_n: 0,
            interjection_count: 0,
            sentiment_word_messages: 0,
            topic_freq: Vec::new(),
        }
    }
}

impl StyleStats {
    /// 计算指定 persona 的五维风格统计。
    ///
    /// 参数:
    /// - `messages`: 该 persona 的全部消息（按 persona_uid 过滤后的发言）。
    /// - `config`: 风格统计配置（Top-N 等阈值）。
    ///
    /// 返回:
    /// - 五维统计摘要；消息为空时返回全零默认值（样本量 0，标注数据不足）。
    pub fn compute(messages: &[Message], config: &StyleConfig) -> Self {
        if messages.is_empty() {
            return Self::default();
        }
        let mut stats = Self {
            sample_count: messages.len() as u32,
            ..Self::default()
        };

        let mut word_counts: HashMap<String, u32> = HashMap::new();
        let mut sentence_lens: Vec<usize> = Vec::new();
        let mut polarities: Vec<f64> = Vec::new();
        let lexicon = SentimentLexicon::builtin();
        let mut topic_counts: HashMap<String, u32> = HashMap::new();

        for msg in messages {
            let text = msg.content.trim();
            if text.is_empty() {
                continue;
            }
            let chars = count_effective_chars(text);
            stats.total_chars += chars;

            // 断句与句长
            let sentences = split_sentences(text);
            stats.total_sentences += sentences.len() as u32;
            for s in &sentences {
                sentence_lens.push(s.chars().count());
            }

            // 断句符/换行
            stats.slash_count += count_occurrences(text, "||");
            stats.comma_count += count_any(text, &[',', '，']);
            stats.newline_count += count_occurrences(text, "\n");

            // 标点
            stats.exclaim_count += count_any(text, &['!', '！']);
            stats.question_count += count_any(text, &['?', '？']);
            stats.ellipsis_count +=
                count_occurrences(text, "……") * 2 + count_occurrences(text, "...");
            stats.paren_count += count_any(text, &['（', '(', ')', '）']) / 2;
            stats.tilde_count += count_any(text, &['~', '～']);

            // 感叹词
            for w in INTERJECTIONS {
                stats.interjection_count += count_occurrences(text, w);
            }

            // 情感极性（每条消息一个得分）与情感词典命中
            let polarity = lexicon.score(text);
            polarities.push(polarity);
            if polarity != 0.0 {
                stats.sentiment_word_messages += 1;
            }

            // 口癖词/话题词候选：bigram 分词后过滤停用词
            for tok in tokenize(text) {
                if is_stop_word(&tok) {
                    continue;
                }
                *word_counts.entry(tok.clone()).or_insert(0) += 1;
                *topic_counts.entry(tok).or_insert(0) += 1;
            }
        }

        // 句长分布摘要
        sentence_lens.sort_unstable();
        if !sentence_lens.is_empty() {
            stats.sentence_len_mean =
                sentence_lens.iter().sum::<usize>() as f64 / sentence_lens.len() as f64;
            stats.sentence_len_p25 = percentile(&sentence_lens, 0.25);
            stats.sentence_len_p75 = percentile(&sentence_lens, 0.75);
        }

        // 情感极性分布摘要
        if !polarities.is_empty() {
            stats.sentiment_n = polarities.len() as u32;
            stats.sentiment_mean = polarities.iter().sum::<f64>() / polarities.len() as f64;
            stats.sentiment_std = std_deviation(&polarities, stats.sentiment_mean);
        }

        // 口癖词 Top-N 与话题词 Top-N
        let top_n = config.top_n.max(1) as usize;
        let mut word_sorted: Vec<(String, u32)> = word_counts.into_iter().collect();
        word_sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        stats.word_freq = word_sorted.iter().take(top_n).cloned().collect();

        // 话题词 = 排除口癖词（word_freq）后的高频词（"常聊什么"≠"口癖"）
        let catch_set: std::collections::HashSet<&str> =
            stats.word_freq.iter().map(|(w, _)| w.as_str()).collect();
        let mut topic_sorted: Vec<(String, u32)> = topic_counts
            .into_iter()
            .filter(|(w, _)| !catch_set.contains(w.as_str()))
            .collect();
        topic_sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        stats.topic_freq = topic_sorted.into_iter().take(top_n).collect();

        stats
    }

    /// 概率频率（0..1）：计数 / 有效字符数，供二项 z 检验。
    pub fn freq(&self, count: u32) -> f64 {
        count as f64 / self.total_chars.max(1) as f64
    }

    /// 每 100 字频率（展示口径）：`freq × 100`。
    pub fn per_100(&self, count: u32) -> f64 {
        self.freq(count) * 100.0
    }

    /// 是否存在足够的样本量（n_p ≥ 阈值）。
    pub fn has_enough_sample(&self, min_sample: u32) -> bool {
        self.sample_count >= min_sample
    }
}

// =========================================================
// 统计辅助函数
// =========================================================

/// 有效字符数（去除空白字符后）。
fn count_effective_chars(text: &str) -> u32 {
    text.chars().filter(|c| !c.is_whitespace()).count() as u32
}

/// 按句末标点/换行切分句子。
///
/// 切分符: 句号/感叹号/问号（全角半角）与换行。
///
/// 注意: 标点为多字节 UTF-8 字符（如中文句号 3 字节），
/// 切片终点必须是 `idx + c.len_utf8()`（而非 `idx`，否则非 char 边界 panic）。
fn split_sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (idx, c) in text.char_indices() {
        if matches!(c, '。' | '！' | '？' | '!' | '?' | '\n') {
            let end = idx + c.len_utf8();
            let seg = &text[start..end];
            if !seg.trim().is_empty() {
                out.push(seg.trim());
            }
            start = end;
        }
    }
    let tail = &text[start..];
    if !tail.trim().is_empty() {
        out.push(tail.trim());
    }
    out
}

/// 子串出现次数。
fn count_occurrences(text: &str, needle: &str) -> u32 {
    if needle.is_empty() {
        return 0;
    }
    text.matches(needle).count() as u32
}

/// 任一字符的出现总次数。
fn count_any(text: &str, chars: &[char]) -> u32 {
    text.chars().filter(|c| chars.contains(c)).count() as u32
}

/// 升序序列的分位数（线性插值口径，同常用统计约定）。
fn percentile(sorted: &[usize], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0] as f64;
    }
    let rank = p * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo] as f64;
    }
    let frac = rank - lo as f64;
    sorted[lo] as f64 + (sorted[hi] as f64 - sorted[lo] as f64) * frac
}

/// 样本标准差（ddof=1；单样本时为 0）。
fn std_deviation(values: &[f64], mean: f64) -> f64 {
    let n = values.len();
    if n <= 1 {
        return 0.0;
    }
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    var.sqrt()
}

/// 是否为停用词（通用高频词过滤）。
fn is_stop_word(word: &str) -> bool {
    STOP_WORDS.contains(&word)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
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
        let mut stats = StyleStats::default();
        stats.sample_count = 199;
        assert!(!stats.has_enough_sample(200));
        stats.sample_count = 200;
        assert!(stats.has_enough_sample(200));
    }
}
