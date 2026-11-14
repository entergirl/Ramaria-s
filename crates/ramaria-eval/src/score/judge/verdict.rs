//! crates/ramaria-eval/src/score/judge/verdict.rs - Ramaria 评测判分票面
//!
//! 设计特点:
//! - 票面与收口同处：一票的判定值、全部票面与多数结果由一个类型承载
//! - 三类任务三种收口：要点逐项取多数、布尔单字段取多数、语气维单票定案
//! - 有效票门槛统一：不足两票即判分失败，不产出分数也不写缓存
//! - 缓存条目可直接还原为判定结果，重跑时票面与首次逐字段一致

use serde::{Deserialize, Serialize};

use super::cache::JudgeCacheEntry;
use super::{JUDGE_VOTES, JudgeTask};

/// 单个要点的票面。
///
/// 字段约定:
/// - `hit`: 该票是否判该要点命中；
/// - `reason`: 该票给出的判定依据（不复制回复原文）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PointVote {
    pub hit: bool,
    #[serde(default)]
    pub reason: String,
}

/// 一票的判定值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JudgeValue {
    /// 逐要点命中（顺序与题目要点一致）
    Points { points: Vec<PointVote> },
    /// 单个布尔判定
    Flag {
        value: bool,
        #[serde(default)]
        reason: String,
    },
    /// 单个整数打分（1~5，语气维专用）
    Score {
        value: u8,
        #[serde(default)]
        reason: String,
    },
}

/// 一道题在一个任务上的投票结果。
///
/// 字段约定:
/// - `value`: 多数结果；有效票少于两票或要点未全部判出时为 `None`；
/// - `votes`: 三票原始票面（无效票为 `None`）；
/// - `disagreements`: 票面不一致的判定单位数；
/// - `units`: 已判单位总数（要点数或 1），未判单位不进分母。
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub value: Option<JudgeValue>,
    pub votes: Vec<Option<JudgeValue>>,
    pub disagreements: usize,
    pub units: usize,
}

impl Verdict {
    /// 一行判定失败的投票结果（未调用模型）。
    pub(super) fn empty() -> Self {
        Self {
            value: None,
            votes: vec![None; JUDGE_VOTES],
            disagreements: 0,
            units: 0,
        }
    }

    /// 有效票数。
    pub fn valid_votes(&self) -> usize {
        self.votes.iter().filter(|vote| vote.is_some()).count()
    }

    /// 本任务是否判分失败。
    pub fn failed(&self) -> bool {
        self.value.is_none()
    }

    /// 第 `index` 个要点的三票明细（无效票为 `None`）。
    pub fn point_votes(&self, index: usize) -> Vec<Option<bool>> {
        self.votes
            .iter()
            .map(|vote| match vote {
                Some(JudgeValue::Points { points }) => points.get(index).map(|point| point.hit),
                _ => None,
            })
            .collect()
    }

    /// 第 `index` 个要点的多数票面。
    pub fn point(&self, index: usize) -> Option<&PointVote> {
        match &self.value {
            Some(JudgeValue::Points { points }) => points.get(index),
            _ => None,
        }
    }

    /// 布尔任务的多数取值。
    pub fn flag(&self) -> Option<bool> {
        match &self.value {
            Some(JudgeValue::Flag { value, .. }) => Some(*value),
            _ => None,
        }
    }

    /// 打分任务的取值（1~5；判分失败为 `None`）。
    pub fn score(&self) -> Option<u8> {
        match &self.value {
            Some(JudgeValue::Score { value, .. }) => Some(*value),
            _ => None,
        }
    }

    /// 由三票票面收口为多数结果。
    pub(super) fn tally(task: JudgeTask, votes: Vec<Option<JudgeValue>>, points: usize) -> Self {
        match task {
            JudgeTask::RubricPoints => tally_points(votes, points),
            JudgeTask::Unanswerable | JudgeTask::Conflict => tally_flag(votes),
            JudgeTask::Tone => tally_single(votes),
        }
    }

    /// 由单票还原判定结果（语气维专用，票面长度为 1）。
    pub(super) fn single(vote: Option<JudgeValue>) -> Self {
        let units = usize::from(vote.is_some());
        Self {
            value: vote.clone(),
            votes: vec![vote],
            disagreements: 0,
            units,
        }
    }

    /// 由缓存条目还原投票结果。
    pub(super) fn from_entry(entry: &JudgeCacheEntry) -> Self {
        Self {
            value: entry.value.clone(),
            votes: entry.votes.clone(),
            disagreements: entry.disagreements,
            units: entry.units,
        }
    }
}

/// 逐要点取多数：每个要点两票命中即算命中，全部要点判出才产出分数。
fn tally_points(votes: Vec<Option<JudgeValue>>, points: usize) -> Verdict {
    let mut agreed: Vec<Option<PointVote>> = vec![None; points];
    let mut disagreements = 0usize;
    let mut judged = 0usize;
    for (index, slot) in agreed.iter_mut().enumerate() {
        let hits: Vec<bool> = votes
            .iter()
            .filter_map(|vote| match vote {
                Some(JudgeValue::Points { points }) => points.get(index).map(|point| point.hit),
                _ => None,
            })
            .collect();
        if hits.len() < 2 {
            continue;
        }
        judged += 1;
        let hit = hits.iter().filter(|value| **value).count() * 2 > hits.len();
        if !hits.iter().all(|value| *value == hits[0]) {
            disagreements += 1;
        }
        let reason = votes
            .iter()
            .find_map(|vote| match vote {
                Some(JudgeValue::Points { points }) => points
                    .get(index)
                    .filter(|point| point.hit == hit)
                    .map(|point| point.reason.clone()),
                _ => None,
            })
            .unwrap_or_default();
        *slot = Some(PointVote { hit, reason });
    }
    let value = if agreed.iter().all(|slot| slot.is_some()) {
        Some(JudgeValue::Points {
            points: agreed.into_iter().flatten().collect(),
        })
    } else {
        None
    };
    Verdict {
        value,
        votes,
        disagreements,
        units: judged,
    }
}

/// 单票定案：取首个有效票面，无有效票即判分失败。
///
/// 说明: 单票入口不产分歧；多票传入时也只认第一票，语气维不参与投票口径。
fn tally_single(votes: Vec<Option<JudgeValue>>) -> Verdict {
    let value = votes.iter().flatten().next().cloned();
    let units = usize::from(value.is_some());
    Verdict {
        value,
        votes,
        disagreements: 0,
        units,
    }
}

/// 单字段取多数：两票一致即定案。
fn tally_flag(votes: Vec<Option<JudgeValue>>) -> Verdict {
    let flags: Vec<bool> = votes
        .iter()
        .filter_map(|vote| match vote {
            Some(JudgeValue::Flag { value, .. }) => Some(*value),
            _ => None,
        })
        .collect();
    if flags.len() < 2 {
        return Verdict {
            value: None,
            votes,
            disagreements: 0,
            units: 0,
        };
    }
    let value = flags.iter().filter(|flag| **flag).count() * 2 > flags.len();
    let disagreements = usize::from(!flags.iter().all(|flag| *flag == flags[0]));
    let reason = votes
        .iter()
        .find_map(|vote| match vote {
            Some(JudgeValue::Flag {
                value: vote_value,
                reason,
            }) if *vote_value == value => Some(reason.clone()),
            _ => None,
        })
        .unwrap_or_default();
    Verdict {
        value: Some(JudgeValue::Flag { value, reason }),
        votes,
        disagreements,
        units: 1,
    }
}
