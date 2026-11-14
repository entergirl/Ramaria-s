//! crates/ramaria-eval/src/score/rubric.rs - Ramaria 评测分项要点判分
//!
//! 设计特点:
//! - 一次调用判全部要点：同一题的要点共享一次投票，比分点调用省三分之二请求
//! - 分值与要点同源：权重取自语料要点，命中求和，不写死满分
//! - 失败不产分：有效票不足两票时 `ratio` 留空，只把票面与理由留在逐题行
//! - 无回复与无要点的题不参与：整列留空，不进任何均值
//! - 分歧率按要点计：同一要点三票不一致即记一条，与条件汇总同口径

use ramaria_core::error::RamariaResult;
use serde::{Deserialize, Serialize};

use super::artifact::{CategoryScore, RubricSummary};
use super::judge::{Judge, JudgeTask, Verdict, prompt};
use crate::run::RunItemRecord;

/// 类别数（LifeBench 九类，0..=8）。
pub const CATEGORY_COUNT: usize = 9;

/// 单题分项判分结果（`scored.jsonl` 的 `rubric` 列）。
///
/// 字段约定:
/// - `points`: 逐要点明细，顺序与语料要点一致；
/// - `score`: 命中要点权重之和；判分失败时只累计已判出的命中要点，`ratio` 留空；
/// - `max`: 全部要点权重之和（按实际求和，不写死满分）；
/// - `ratio`: `score / max`；`max` 为 0 或判分失败时为 `None`；
/// - `full_hit`: 全部要点命中（判分失败为 `false`）；
/// - `judge_failed`: 有效票不足两票，本题不产出分数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RubricScore {
    pub points: Vec<RubricPointScore>,
    pub score: f64,
    pub max: f64,
    pub ratio: Option<f64>,
    pub full_hit: bool,
    pub judge_failed: bool,
}

/// 单个要点的判分结果。
///
/// 字段约定:
/// - `index`: 要点序号（与语料数组下标一致）；
/// - `description` / `weight`: 要点原文与分值；
/// - `hit`: 多数判定；该要点判分失败时为 `None`；
/// - `votes`: 三票明细（无效票为 `None`）；
/// - `reason`: 多数票给出的判定依据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RubricPointScore {
    pub index: usize,
    pub description: String,
    pub weight: f64,
    pub hit: Option<bool>,
    pub votes: Vec<Option<bool>>,
    pub reason: Option<String>,
}

impl RubricPointScore {
    /// 该要点是否已判（有效票不少于两票）。
    pub fn judged(&self) -> bool {
        self.votes.iter().flatten().count() >= 2
    }

    /// 有效票是否完全一致（未判的要点按一致处理）。
    pub fn unanimous(&self) -> bool {
        let mut seen = None;
        for vote in self.votes.iter().flatten() {
            match seen {
                Some(first) if first != *vote => return false,
                Some(_) => {}
                None => seen = Some(*vote),
            }
        }
        true
    }
}

// =========================================================
// 单题判分
// =========================================================

/// 判分一道题的分项要点。
///
/// 参数:
/// - `judge`: 判分句柄；
/// - `record`: 逐题产物（取题目、参考答案、回复与要点）。
///
/// 返回:
/// - 无回复或无要点时返回 `None`（不参与分项判分）；
/// - 否则返回逐要点结果，判分失败体现在 `judge_failed`。
pub async fn score(judge: &Judge, record: &RunItemRecord) -> RamariaResult<Option<RubricScore>> {
    let Some(reply) = record.reply.as_deref() else {
        return Ok(None);
    };
    if record.score_points.is_empty() {
        return Ok(None);
    }
    let user = prompt::rubric_user(
        &record.question,
        &record.gold_answer,
        reply,
        &record.score_points,
    );
    let verdict = judge
        .ask(
            JudgeTask::RubricPoints,
            &record.question_id,
            &record.condition_id,
            reply,
            &user,
            Some(record.score_points.len()),
        )
        .await?;
    Ok(Some(assemble(record, &verdict)))
}

/// 把投票结果装配成单题分项结果。
fn assemble(record: &RunItemRecord, verdict: &Verdict) -> RubricScore {
    let points: Vec<RubricPointScore> = record
        .score_points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let value = verdict.point(index);
            RubricPointScore {
                index,
                description: point.description.clone(),
                weight: point.score,
                hit: value.map(|value| value.hit),
                votes: verdict.point_votes(index),
                reason: value
                    .map(|value| value.reason.clone())
                    .filter(|reason| !reason.is_empty()),
            }
        })
        .collect();
    // f64 求和的空集初值是 −0.0，统一收敛为正零，避免产物里出现「-0.0 分」。
    let max: f64 = points.iter().map(|point| point.weight).sum::<f64>() + 0.0;
    let score: f64 = points
        .iter()
        .filter(|point| point.hit == Some(true))
        .map(|point| point.weight)
        .sum::<f64>()
        + 0.0;
    let full_hit = !points.is_empty() && points.iter().all(|point| point.hit == Some(true));
    RubricScore {
        ratio: (max > 0.0).then(|| score / max),
        judge_failed: verdict.failed(),
        points,
        score,
        max,
        full_hit,
    }
}

// =========================================================
// 条件汇总
// =========================================================

/// 单个类别的累加器。
#[derive(Debug, Default, Clone)]
struct CategorySums {
    questions: usize,
    ratio_sum: f64,
}

/// 单条件分项判分的累加器。
///
/// 说明: 得分率与全要点命中只在判分成功的题上累加；分歧率在所有已判要点上累加。
#[derive(Debug, Clone)]
pub struct RubricSums {
    questions: usize,
    judged: usize,
    ratio_sum: f64,
    full_hits: usize,
    disagreements: usize,
    units: usize,
    categories: [CategorySums; CATEGORY_COUNT],
}

impl Default for RubricSums {
    fn default() -> Self {
        Self {
            questions: 0,
            judged: 0,
            ratio_sum: 0.0,
            full_hits: 0,
            disagreements: 0,
            units: 0,
            categories: std::array::from_fn(|_| CategorySums::default()),
        }
    }
}

impl RubricSums {
    /// 吸收一道题的分项结果。
    ///
    /// 参数:
    /// - `score`: 单题分项结果；
    /// - `categories`: 该题的类别码数组（多标签题每个类别各计一次）。
    pub fn absorb(&mut self, score: &RubricScore, categories: &[u8]) {
        self.questions += 1;
        for point in &score.points {
            if !point.judged() {
                continue;
            }
            self.units += 1;
            if !point.unanimous() {
                self.disagreements += 1;
            }
        }
        if score.judge_failed {
            return;
        }
        self.judged += 1;
        let ratio = score.ratio.unwrap_or(0.0);
        self.ratio_sum += ratio;
        if score.full_hit {
            self.full_hits += 1;
        }
        for category in categories {
            if let Some(sums) = self.categories.get_mut(usize::from(*category)) {
                sums.questions += 1;
                sums.ratio_sum += ratio;
            }
        }
    }

    /// 收口为条件汇总；没有参与分项判分的题时返回 `None`。
    pub fn finish(&self) -> Option<RubricSummary> {
        if self.questions == 0 {
            return None;
        }
        let judged = self.judged;
        let divisor = judged as f64;
        let macro_by_category = self
            .categories
            .iter()
            .enumerate()
            .map(|(index, sums)| CategoryScore {
                category: index as u8,
                questions: sums.questions,
                mean_ratio: (sums.questions > 0).then(|| sums.ratio_sum / sums.questions as f64),
            })
            .collect();
        Some(RubricSummary {
            questions: self.questions,
            judged,
            micro_mean_ratio: (judged > 0).then(|| self.ratio_sum / divisor),
            micro_full_hit_rate: (judged > 0).then(|| self.full_hits as f64 / divisor),
            macro_by_category,
            disagreement_rate: (self.units > 0)
                .then(|| self.disagreements as f64 / self.units as f64),
        })
    }
}
