//! crates/ramaria-memory/src/behavior/sentiment.rs - 中文情感词典极性提取
//!
//! 设计特点:
//! - 内置小型中文情感词典（积极/消极词表），供 D4 规则翻译的极性一致性校验
//! - `polarity_of_text` 统计文本中积极/消极词命中数，输出 -1.0（消极）..1.0（积极）
//! - 词典为规则校验的"轻量兜底"：仅用于极性符号比对，不替代 LLM 语义判断
//! - 纯字符串处理，零 I/O，无外部依赖

use std::collections::HashSet;

/// 内置中文情感词典（精选常用词，覆盖规则文本常见表达）。
///
/// 字段约定:
/// - `positive`: 积极词表（安慰/鼓励/喜欢等）。
/// - `negative`: 消极词表（难过/讨厌/累/烦等）。
/// - 词典是开放集合：`with_extra` 可追加领域词，不修改内置表。
pub struct SentimentLexicon {
    positive: HashSet<String>,
    negative: HashSet<String>,
}

impl Default for SentimentLexicon {
    fn default() -> Self {
        Self::builtin()
    }
}

impl SentimentLexicon {
    /// 内置词典（常用情感词，避免过度膨胀导致误判）。
    pub fn builtin() -> Self {
        let to_string = |words: &[&str]| words.iter().map(|w| w.to_string()).collect();
        Self {
            positive: to_string(&[
                "喜欢",
                "爱",
                "开心",
                "高兴",
                "快乐",
                "欣慰",
                "满意",
                "期待",
                "鼓励",
                "安慰",
                "支持",
                "放心",
                "安心",
                "温暖",
                "幸福",
                "感动",
                "感谢",
                "感激",
                "加油",
                "没事",
                "别担心",
                "会好的",
                "好起来",
                "珍惜",
                "欣赏",
                "佩服",
                "骄傲",
                "轻松",
                "舒服",
                "放心了",
                "真好",
                "太好了",
                "不错",
                "棒",
            ]),
            negative: to_string(&[
                "难过",
                "伤心",
                "痛苦",
                "悲伤",
                "沮丧",
                "失望",
                "讨厌",
                "烦",
                "烦躁",
                "焦虑",
                "担心",
                "害怕",
                "恐惧",
                "生气",
                "愤怒",
                "累",
                "疲惫",
                "委屈",
                "心酸",
                "无奈",
                "孤独",
                "寂寞",
                "迷茫",
                "压力",
                "崩溃",
                "哭",
                "后悔",
                "遗憾",
                "郁闷",
                "苦恼",
                "痛苦",
                "心疼",
                "难受",
                "糟糕",
                "太差了",
                "受不了",
                "撑不住",
            ]),
        }
    }

    /// 追加领域词（副本扩展，不修改内置表）。
    pub fn with_extra(mut self, positive: &[&str], negative: &[&str]) -> Self {
        self.positive.extend(positive.iter().map(|w| w.to_string()));
        self.negative.extend(negative.iter().map(|w| w.to_string()));
        self
    }

    /// 统计文本的极性得分。
    ///
    /// 返回:
    /// - 得分 = (积极命中 − 消极命中) / 总命中；无命中 → 0.0（中性）。
    /// - 范围 -1.0..1.0，符号即极性方向。
    pub fn score(&self, text: &str) -> f64 {
        let mut pos = 0usize;
        let mut neg = 0usize;
        for w in &self.positive {
            if text.contains(w) {
                pos += 1;
            }
        }
        for w in &self.negative {
            if text.contains(w) {
                neg += 1;
            }
        }
        let total = pos + neg;
        if total == 0 {
            return 0.0;
        }
        (pos as f64 - neg as f64) / total as f64
    }
}

/// 计算文本极性（内置词典）。
///
/// 返回:
/// - 极性得分 -1.0..1.0（0.0 = 中性/无命中）。
pub fn sentiment_polarity(text: &str) -> f64 {
    SentimentLexicon::builtin().score(text)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_text_scores_positive() {
        let s = sentiment_polarity("别担心，会好的，我支持你");
        assert!(s > 0.0, "积极文本应得正分，实际 {s}");
    }

    #[test]
    fn negative_text_scores_negative() {
        let s = sentiment_polarity("我也很难过，最近压力好大，好累");
        assert!(s < 0.0, "消极文本应得负分，实际 {s}");
    }

    #[test]
    fn neutral_text_scores_zero() {
        // 不含任何词典词的句子 → 中性
        let s = sentiment_polarity("今天天气晴朗，我们讨论一下方案");
        assert_eq!(s, 0.0);
    }

    #[test]
    fn mixed_text_sign_follows_dominant() {
        let s = sentiment_polarity("虽然很累，但很开心");
        // 累(负) 1 个 vs 开心(正) 1 个 → 0.0；加一个积极词确认方向
        let s2 = sentiment_polarity("虽然很累，但很开心，很欣慰");
        assert!(s2 > 0.0, "积极词更多应得正分，实际 {s2}");
        let _ = s;
    }

    #[test]
    fn score_range_clamped() {
        for t in ["太好了太棒了真棒", "太差了受不了崩溃哭", "普通句子"] {
            let s = sentiment_polarity(t);
            assert!((-1.0..=1.0).contains(&s), "得分应在 [-1,1]，实际 {s}");
        }
    }

    #[test]
    fn with_extra_extends_lexicon() {
        let lex = SentimentLexicon::builtin().with_extra(&["很哇塞"], &["emo"]);
        assert!(lex.score("很哇塞") > 0.0);
        assert!(lex.score("emo") < 0.0);
        // 内置表不受影响
        let builtin = SentimentLexicon::builtin();
        assert_eq!(builtin.score("很哇塞"), 0.0);
    }

    #[test]
    fn empty_text_is_neutral() {
        assert_eq!(sentiment_polarity(""), 0.0);
    }
}
