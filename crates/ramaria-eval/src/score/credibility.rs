//! crates/ramaria-eval/src/score/credibility.rs - Ramaria 评测可信度列
//!
//! 设计特点:
//! - 只统计题型对应的题：UA 取不可回答类，CD 取冲突类，其他题不进入这两列
//! - 两条线并行：确定性列只做字面标记命中，judge 列走模型投票，前者作交叉不作判定线
//! - 判定失败不进分母：有效票不足两票的题只记失败，不把噪声算成拒答或接受
//! - 无回复的题不判分：执行失败不是拒答，也不该进 CD 分母
//! - 两次调用分别只发给对应题型：非 UA 题不调 unanswerable，非 CD 题不调 conflict

use ramaria_core::error::RamariaResult;

use super::artifact::{ConditionCredibility, ItemCredibility, JudgeRate};
use super::judge::{Judge, JudgeTask, Verdict, prompt};
use crate::run::RunItemRecord;

/// 不可回答类别码（LifeBench 九类之一）。
pub const CATEGORY_UNANSWERABLE: u8 = 4;

/// 冲突检测类别码（问题内含错误前提）。
pub const CATEGORY_CONFLICT: u8 = 7;

/// 拒答标记表（字面命中即置位）。
///
/// 说明:
/// - 这是确定性交叉列，不是拒答判定线：命中不等于答错，不命中也不等于答对；
/// - 只做子串匹配，不做分词与语义判定，保证同一回复每次判分结果一致。
pub const REFUSAL_MARKERS: [&str; 10] = [
    "不知道",
    "不清楚",
    "无法回答",
    "无法确定",
    "没有提到",
    "没有记录",
    "记不得",
    "没有相关信息",
    "不能确定",
    "没有这方面的信息",
];

/// 回复是否命中拒答标记。
pub fn ua_refusal_hint(reply: &str) -> bool {
    REFUSAL_MARKERS.iter().any(|marker| reply.contains(marker))
}

// =========================================================
// 确定性交叉列
// =========================================================

/// 单题可信度列（确定性部分）。
///
/// 参数:
/// - `record`: 逐题产物。
///
/// 返回:
/// - 非不可回答题的 `ua_refusal_hint` 为 `None`，失败题同样为 `None`，两者都不进 UA 分母；
/// - `cd_score_points` 只对冲突题取分项要点数，其他题型与旧产物为 0；
/// - judge 列初始化为 `None`，由 `apply_unanswerable` / `apply_conflict` 填入。
pub fn item_credibility(record: &RunItemRecord) -> ItemCredibility {
    let unanswerable = record.category.contains(&CATEGORY_UNANSWERABLE);
    let conflict = record.category.contains(&CATEGORY_CONFLICT);
    ItemCredibility {
        ua_refusal_hint: if unanswerable {
            record.reply.as_deref().map(ua_refusal_hint)
        } else {
            None
        },
        cd_score_points: if conflict {
            record.score_points.len()
        } else {
            0
        },
        ua_judge: None,
        cd_judge: None,
        judge_failed: false,
    }
}

// =========================================================
// judge 判定
// =========================================================

/// 单字段判定的投票结果。
///
/// 字段约定:
/// - `value`: 多数取值；有效票不足两票时为 `None`；
/// - `disagreement`: 有效票是否不一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagVerdict {
    pub value: Option<bool>,
    pub disagreement: bool,
}

impl FlagVerdict {
    /// 由投票结果收口。
    fn of(verdict: &Verdict) -> Self {
        Self {
            value: verdict.flag(),
            disagreement: verdict.disagreements > 0,
        }
    }

    /// 该字段是否判分失败。
    fn failed(&self) -> bool {
        self.value.is_none()
    }
}

/// 判分一题的 UA 字段（只发给不可回答题）。
///
/// 参数:
/// - `judge`: 判分句柄；
/// - `record`: 逐题产物（取题目、回复与类别）。
///
/// 返回:
/// - 无回复或题型不含 UA 时不调用模型，返回 `None`。
pub async fn judge_unanswerable(
    judge: &Judge,
    record: &RunItemRecord,
) -> RamariaResult<Option<FlagVerdict>> {
    if !record.category.contains(&CATEGORY_UNANSWERABLE) {
        return Ok(None);
    }
    let Some(reply) = record.reply.as_deref() else {
        return Ok(None);
    };
    let user = prompt::unanswerable_user(&record.question, reply);
    let verdict = judge
        .ask(
            JudgeTask::Unanswerable,
            &record.question_id,
            &record.condition_id,
            reply,
            &user,
            None,
        )
        .await?;
    Ok(Some(FlagVerdict::of(&verdict)))
}

/// 判分一题的 CD 字段（只发给冲突题）。
///
/// 参数:
/// - `judge`: 判分句柄；
/// - `record`: 逐题产物（取题目、参考答案、回复与类别）。
///
/// 返回:
/// - 无回复或题型不含 CD 时不调用模型，返回 `None`。
pub async fn judge_conflict(
    judge: &Judge,
    record: &RunItemRecord,
) -> RamariaResult<Option<FlagVerdict>> {
    if !record.category.contains(&CATEGORY_CONFLICT) {
        return Ok(None);
    }
    let Some(reply) = record.reply.as_deref() else {
        return Ok(None);
    };
    let user = prompt::conflict_user(&record.question, &record.gold_answer, reply);
    let verdict = judge
        .ask(
            JudgeTask::Conflict,
            &record.question_id,
            &record.condition_id,
            reply,
            &user,
            None,
        )
        .await?;
    Ok(Some(FlagVerdict::of(&verdict)))
}

/// 把 UA 判定写进逐题可信度列（判分失败只置失败位，不改取值）。
pub fn apply_unanswerable(credibility: &mut ItemCredibility, flag: &FlagVerdict) {
    credibility.ua_judge = flag.value;
    credibility.judge_failed |= flag.failed();
}

/// 把 CD 判定写进逐题可信度列（判分失败只置失败位，不改取值）。
pub fn apply_conflict(credibility: &mut ItemCredibility, flag: &FlagVerdict) {
    credibility.cd_judge = flag.value;
    credibility.judge_failed |= flag.failed();
}

// =========================================================
// 条件汇总
// =========================================================

/// 单类判定的累加器。
#[derive(Debug, Default, Clone, Copy)]
struct RateSums {
    judged: usize,
    hits: usize,
    disagreements: usize,
}

impl RateSums {
    /// 吸收一题的判定；判分失败不进分母。
    fn absorb(&mut self, flag: &FlagVerdict) {
        let Some(value) = flag.value else {
            return;
        };
        self.judged += 1;
        self.hits += usize::from(value);
        self.disagreements += usize::from(flag.disagreement);
    }

    /// 收口；无有效判定时返回 `None`。
    fn finish(&self) -> Option<JudgeRate> {
        if self.judged == 0 {
            return None;
        }
        let judged = self.judged as f64;
        Some(JudgeRate {
            judged: self.judged,
            hits: self.hits,
            rate: self.hits as f64 / judged,
            disagreement_rate: self.disagreements as f64 / judged,
        })
    }
}

/// 单条件 UA / CD 判定累加器。
#[derive(Debug, Default, Clone, Copy)]
pub struct JudgeTally {
    ua: RateSums,
    cd: RateSums,
}

impl JudgeTally {
    /// 吸收一题的 UA 判定。
    pub fn absorb_unanswerable(&mut self, flag: &FlagVerdict) {
        self.ua.absorb(flag);
    }

    /// 吸收一题的 CD 判定。
    pub fn absorb_conflict(&mut self, flag: &FlagVerdict) {
        self.cd.absorb(flag);
    }

    /// 收口为（UA, CD）两列。
    pub fn finish(&self) -> (Option<JudgeRate>, Option<JudgeRate>) {
        (self.ua.finish(), self.cd.finish())
    }
}

/// 条件级可信度汇总。
///
/// 参数:
/// - `items`: 该条件全部逐题可信度列（含失败题）；
/// - `tally`: judge 判定累加器。
///
/// 返回:
/// - `ua_refusal_rate` 以有回复的不可回答题数为分母，分母为 0 时为 `None`；
/// - `cd_false_premise_accept_rate` 取 `cd_judge.rate`，无有效判定时为 `None`。
pub fn aggregate(items: &[ItemCredibility], tally: &JudgeTally) -> ConditionCredibility {
    let answered = items
        .iter()
        .filter(|item| item.ua_refusal_hint.is_some())
        .count();
    let hits = items
        .iter()
        .filter(|item| item.ua_refusal_hint == Some(true))
        .count();
    let cd_questions = items.iter().filter(|item| item.cd_score_points > 0).count();
    let (ua_judge, cd_judge) = tally.finish();

    ConditionCredibility {
        ua_answered: answered,
        ua_refusal_hits: hits,
        ua_refusal_rate: (answered > 0).then(|| hits as f64 / answered as f64),
        cd_questions,
        cd_false_premise_accept_rate: cd_judge.as_ref().map(|rate| rate.rate),
        ua_judge,
        cd_judge,
    }
}
