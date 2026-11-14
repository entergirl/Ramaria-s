//! crates/ramaria-eval/src/score/persona/tone.rs - Ramaria 评测人格表达语气维
//!
//! 设计特点:
//! - 单票判分：每道题只调用一次判分模型，温度 0，不参与三次投票
//! - 系统提示统一出口：rubric 与 few-shot 锚定同源，长度中性口径不随调用漂移
//! - 解析先认 1~5 的 ASCII 数字，中文数字兜底，越界或无数值记为判分失败
//! - 失败只返回空值不报错：不阻塞其它两维与其它题，由条件级单独计数

use ramaria_core::error::RamariaResult;
use serde::{Deserialize, Serialize};

use crate::score::judge::{Judge, JudgeTask, prompt};

/// 语气维单题评分。
///
/// 字段约定:
/// - `score`: 1~5 整数分，越高表示语气风格越贴近参考。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToneScore {
    pub score: u8,
}

/// 语气维 judge 的系统提示（rubric + few-shot 锚定）。
///
/// 说明: 与判分入口共用同一份模板文本，单测据此锁定提示不被改写。
pub fn tone_system_prompt() -> String {
    prompt::TONE_SYSTEM.to_string()
}

// =========================================================
// 单题评分
// =========================================================

/// 语气维单题评分（单票）。
///
/// 参数:
/// - `judge`: 判分句柄（走缓存）；
/// - `question_id` / `condition_id`: 缓存键的复合键部分；
/// - `question`: 题目原文（只提供语境）；
/// - `reference`: 参考回复（本题取 gold_answer）；
/// - `reply`: 待评回复。
///
/// 返回:
/// - 解析出 1~5 时返回分值；
/// - 模型调用失败或输出解析不出 1~5 时返回 `None`（该题不计入语气均值）；
/// - 缓存读写失败返回对应错误。
pub async fn score_tone_item(
    judge: &Judge,
    question_id: &str,
    condition_id: &str,
    question: &str,
    reference: &str,
    reply: &str,
) -> RamariaResult<Option<ToneScore>> {
    // 判分请求的系统提示由任务自带，这里锁一次两份文本同源。
    debug_assert_eq!(
        tone_system_prompt(),
        JudgeTask::Tone.system(),
        "语气维系统提示应只有一份文本"
    );
    let user = prompt::tone_user(question, reference, reply);
    let verdict = judge
        .ask_once(JudgeTask::Tone, question_id, condition_id, reply, &user)
        .await?;
    Ok(verdict.score().map(|score| ToneScore { score }))
}

// =========================================================
// 输出解析
// =========================================================

/// 从判分输出提取 1~5 整数分。
///
/// 规则:
/// - 先扫全部 ASCII 数字，取首个落在 1~5 的数字（"4"、"评分：5" 均可）；
/// - 没有可用数字时按中文数字兜底（一~五，两按二计）；
/// - 0、6 以上或无数值返回 `None`。
pub fn parse_judge_score(raw: &str) -> Option<u8> {
    let digits: String = raw.chars().filter(|ch| ch.is_ascii_digit()).collect();
    for ch in digits.chars() {
        let value = ch.to_digit(10)?;
        if (1..=5).contains(&value) {
            return Some(value as u8);
        }
    }
    match raw.trim() {
        "一" => Some(1),
        "二" | "两" => Some(2),
        "三" => Some(3),
        "四" => Some(4),
        "五" => Some(5),
        _ => None,
    }
}
