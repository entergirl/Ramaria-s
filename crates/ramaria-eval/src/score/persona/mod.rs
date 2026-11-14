//! crates/ramaria-eval/src/score/persona/mod.rs - Ramaria 评测人格表达三维
//!
//! 设计特点:
//! - 三维共用一份参考：该题的 gold_answer，产物里记 `reference` 标明来源
//! - 事实维与情感维是确定性规则，无模型也能出分，装配在判分之前；语气维走单票 judge 并可缓存
//! - 逐题结果落 scored.jsonl 的 persona 列，条件级只收三个均值、口径标记与失败计数
//! - 三维只作回归线：缺值留空不进主证据，也不单独记缺项
//! - 题数按参与判分的题计一次，多标签题不按类别重复计入

mod emotion;
mod fact;
mod tone;

#[cfg(test)]
mod tests;

pub use tone::parse_judge_score;

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::EmbeddingProvider;
use serde::{Deserialize, Serialize};

use super::artifact::PersonaSummary;
use super::judge::Judge;
use crate::run::RunItemRecord;

/// 三个维度统一使用的参考文本标识（写在产物里供读方核对口径）。
pub const PERSONA_REFERENCE: &str = "gold_answer";

// =========================================================
// 逐题结果
// =========================================================

/// 一题的人格表达三维结果（`scored.jsonl` 的 `persona` 列）。
///
/// 字段约定:
/// - `tone`: 语气维 1~5 分；未启用判分或判分失败为 `None`；
/// - `fact`: 事实维明细；参考为空时为 `None`；
/// - `emotion`: 情感维明细；有回复即产出；
/// - `reference`: 参考文本口径标识（`gold_answer`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaScore {
    pub tone: Option<u8>,
    pub fact: Option<fact::FactScore>,
    pub emotion: Option<emotion::EmotionScore>,
    pub reference: String,
}

// =========================================================
// 单题打分
// =========================================================

/// 判分一题的人格表达确定性两维（事实 / 情感）。
///
/// 参数:
/// - `embedder`: 嵌入 provider；`None` 表示向量通道不可用（事实维降级为纯关键词）；
/// - `record`: 逐题产物（取题目、参考与回复）。
///
/// 返回:
/// - 无回复或执行失败的题返回 `None`（不参与三维均值）；
/// - 有回复时返回三维骨架：`tone` 留空待语气趟填入，参考为空时事实维留空、只保留情感维。
pub async fn score_deterministic(
    embedder: Option<&dyn EmbeddingProvider>,
    record: &RunItemRecord,
) -> Option<PersonaScore> {
    if record.error.is_some() {
        return None;
    }
    let reply = record.reply.as_deref()?;
    let reference = record.gold_answer.trim();

    let fact = if reference.is_empty() {
        None
    } else {
        Some(fact::score_fact_item(reply, reference, embedder).await)
    };
    let emotion = Some(emotion::score_emotion_item(reply, &record.question));

    Some(PersonaScore {
        tone: None,
        fact,
        emotion,
        reference: PERSONA_REFERENCE.to_string(),
    })
}

/// 判分一题的语气维（单票，参考取 gold_answer）。
///
/// 参数:
/// - `judge`: 判分句柄；
/// - `record`: 逐题产物（取题目、参考与回复）。
///
/// 返回:
/// - 参考为空时不比较语气，返回 `None`（不调用模型）；
/// - 否则返回 1~5 分；模型调用失败或解析不出分值时同样为 `None`；
/// - 缓存读写失败返回对应错误。
pub async fn score_tone(judge: &Judge, record: &RunItemRecord) -> RamariaResult<Option<u8>> {
    let Some(reply) = record.reply.as_deref() else {
        return Ok(None);
    };
    let reference = record.gold_answer.trim();
    if reference.is_empty() {
        return Ok(None);
    }
    let tone = tone::score_tone_item(
        judge,
        &record.question_id,
        &record.condition_id,
        &record.question,
        reference,
        reply,
    )
    .await?;
    Ok(tone.map(|tone| tone.score))
}

// =========================================================
// 条件汇总
// =========================================================

/// 单条件人格三维的累加器。
///
/// 字段约定:
/// - `questions`: 参与三维判分的题数；
/// - `tone_sum` / `tone_count` 等三组分别记录和与有效题数，均值只在有效题上取；
/// - `tone_failed`: 语气维启用判分却拿不到分值的题数；
/// - `fact_variant`: 该条件首个事实维样本的口径，无样本时为空串。
#[derive(Debug, Default, Clone)]
pub struct PersonaSums {
    questions: usize,
    tone_sum: f64,
    tone_count: usize,
    tone_failed: usize,
    fact_sum: f64,
    fact_count: usize,
    emotion_sum: f64,
    emotion_count: usize,
    fact_variant: String,
}

impl PersonaSums {
    /// 吸收一题的逐题结果。
    ///
    /// 参数:
    /// - `score`: 该题的三维结果；
    /// - `tone_attempted`: 本次是否启用了语气维判分（未启用时语气维缺值不算失败）。
    pub fn absorb(&mut self, score: &PersonaScore, tone_attempted: bool) {
        self.questions += 1;
        match score.tone {
            Some(tone) => {
                self.tone_sum += f64::from(tone);
                self.tone_count += 1;
            }
            None if tone_attempted => self.tone_failed += 1,
            None => {}
        }
        if let Some(fact) = &score.fact {
            self.fact_sum += fact.score;
            self.fact_count += 1;
            if self.fact_variant.is_empty() {
                self.fact_variant = fact.variant.to_string();
            }
        }
        if let Some(emotion) = &score.emotion {
            self.emotion_sum += emotion.score;
            self.emotion_count += 1;
        }
    }

    /// 收口为条件汇总；没有参与三维判分的题时返回 `None`。
    pub fn finish(&self) -> Option<PersonaSummary> {
        if self.questions == 0 {
            return None;
        }
        Some(PersonaSummary {
            questions: self.questions,
            tone: mean(self.tone_sum, self.tone_count),
            fact: mean(self.fact_sum, self.fact_count),
            emotion: mean(self.emotion_sum, self.emotion_count),
            fact_variant: self.fact_variant.clone(),
            tone_failed: self.tone_failed,
        })
    }
}

/// 有效题数为 0 时返回 `None`，否则取算术均值。
fn mean(sum: f64, count: usize) -> Option<f64> {
    (count > 0).then(|| sum / count as f64)
}
