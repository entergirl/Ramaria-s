//! crates/ramaria-eval/src/score/persona/emotion.rs - Ramaria 评测人格表达情感维
//!
//! 设计特点:
//! - 确定性 rubric：只看回复是否恰当回应了用户情绪，零模型依赖，可逐条复现
//! - 情境极性由用户消息的词表命中判定：负面要安慰共情，正面要分享喜悦
//! - 恰当标记按词表去重计数：≥2 个记 1.0，1 个记 0.5，0 个记 0.0
//! - 不比对参考字面重叠：那是事实召回口径，这一维评的是情感回应恰当性

use serde::{Deserialize, Serialize};

/// 负面情境下的恰当回应标记（安慰 / 共情）。
pub const EMOTION_COMFORT_MARKERS: [&str; 18] = [
    "别难过",
    "别伤心",
    "抱抱",
    "理解",
    "我懂",
    "会好的",
    "别担心",
    "放心",
    "支持",
    "安慰",
    "加油",
    "没事",
    "慢慢来",
    "辛苦了",
    "陪你",
    "心疼",
    "别着急",
    "别想太多",
];

/// 正面情境下的恰当回应标记（分享喜悦 / 肯定）。
pub const EMOTION_JOY_MARKERS: [&str; 14] = [
    "太好了",
    "真棒",
    "恭喜",
    "开心",
    "高兴",
    "为你高兴",
    "厉害",
    "棒",
    "赞",
    "不错",
    "真不错",
    "好耶",
    "值得",
    "分享",
];

/// 负面情绪触发词（情境侧：用户消息）。
pub const EMOTION_NEGATIVE_CUES: [&str; 24] = [
    "难过",
    "伤心",
    "哭",
    "郁闷",
    "烦躁",
    "生气",
    "愤怒",
    "气死",
    "气得",
    "担心",
    "焦虑",
    "紧张",
    "害怕",
    "怕",
    "委屈",
    "失望",
    "崩溃",
    "累",
    "烦",
    "不开心",
    "痛苦",
    "压力",
    "孤独",
    "自责",
];

/// 正面情绪触发词（情境侧：用户消息）。
pub const EMOTION_POSITIVE_CUES: [&str; 10] = [
    "开心",
    "高兴",
    "太好了",
    "兴奋",
    "中奖",
    "升职",
    "成功了",
    "通过",
    "好消息",
    "惊喜",
];

// =========================================================
// 单题结果
// =========================================================

/// 情感表达维单题评分（rubric 0 / 0.5 / 1）。
///
/// 字段约定:
/// - `score`: rubric 分（0.0 / 0.5 / 1.0）；
/// - `situation_negative` / `situation_positive`: 用户消息检出的情境极性，
///   两者皆 false 为中性情境；
/// - `marker_hit`: 回复命中的恰当标记数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmotionScore {
    pub score: f64,
    pub situation_negative: bool,
    pub situation_positive: bool,
    pub marker_hit: usize,
}

// =========================================================
// 单题评分
// =========================================================

/// 情感表达维单题评分。
///
/// 参数:
/// - `reply`: 待评回复；
/// - `question`: 用户消息（情境极性的来源）。
///
/// 返回:
/// - 负面情境统计安慰标记，正面情境统计喜悦标记，中性情境两类合计；
/// - 标记数 ≥2 记 1.0，1 个记 0.5，0 个记 0.0；空回复记 0.0。
pub fn score_emotion_item(reply: &str, question: &str) -> EmotionScore {
    let reply = reply.trim();
    let situation_negative = has_negative_cue(question);
    let situation_positive = has_positive_cue(question);

    if reply.is_empty() {
        return EmotionScore {
            score: 0.0,
            situation_negative,
            situation_positive,
            marker_hit: 0,
        };
    }

    let marker_hit = if situation_negative {
        count_marker_hits(reply, &EMOTION_COMFORT_MARKERS)
    } else if situation_positive {
        count_marker_hits(reply, &EMOTION_JOY_MARKERS)
    } else {
        count_marker_hits(reply, &EMOTION_COMFORT_MARKERS)
            + count_marker_hits(reply, &EMOTION_JOY_MARKERS)
    };

    let score = match marker_hit {
        0 => 0.0,
        1 => 0.5,
        _ => 1.0,
    };

    EmotionScore {
        score,
        situation_negative,
        situation_positive,
        marker_hit,
    }
}

/// 统计文本命中的词表条目数（每个条目最多计一次）。
pub fn count_marker_hits(text: &str, markers: &[&str]) -> usize {
    markers
        .iter()
        .filter(|marker| text.contains(**marker))
        .count()
}

/// 文本是否含负面情感触发词。
fn has_negative_cue(text: &str) -> bool {
    EMOTION_NEGATIVE_CUES.iter().any(|cue| text.contains(cue))
}

/// 文本是否含正面情感触发词。
fn has_positive_cue(text: &str) -> bool {
    EMOTION_POSITIVE_CUES.iter().any(|cue| text.contains(cue))
}
